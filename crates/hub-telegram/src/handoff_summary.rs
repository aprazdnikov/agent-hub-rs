//! One isolated summary turn; it never enters the ordinary delivery or result pipeline.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::{FutureExt, future::BoxFuture};
use hub_agent::channel::UserChannel;
use hub_agent::conversation::{Conversation, Limits};
use hub_core::domain::{
    AgentEvent, Decision, Denied, FileDelivery, Finished, OutgoingFile, Prompt, Question,
    QuestionsOutcome, SessionId, ToolRequest, TopicSession,
};
use hub_core::handoff::{MAX_SUMMARY_UNITS, SUMMARY_REQUEST, Summary};
use hub_core::settings::Settings;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::hub::Agents;
use crate::progress::Progress;

const EVENTS: usize = 32;
const GENERATION_TIMEOUT: Duration = Duration::from_mins(3);
// Qwen's normal process shutdown alone may take ten seconds.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(12);
const FAILED: &str = "Агент не завершил подготовку сводки. Попробуйте снова.";
const CRASHED: &str = "Подготовка сводки прервана внутренней ошибкой. Попробуйте снова.";
const CANCELLED: &str = "Подготовка сводки отменена.";
const EXPIRED: &str = "Истекло время подготовки сводки. Попробуйте снова.";
const INTERACTION: &str = "При подготовке сводки агент запросил действие. Сводка отклонена.";
const CHANGED_SESSION: &str = "Агент изменил исходную сессию. Сводка отклонена.";
const BACKGROUND: &str = "При подготовке сводки обнаружены фоновые задачи. Сводка отклонена.";
const OVERFLOW: &str = "Сводка длиннее 3000 единиц UTF-16. Попробуйте создать её снова.";

#[derive(Debug)]
pub struct Prepared {
    pub summary: Summary,
    pub finished: Finished,
}

pub struct SummaryRun<A> {
    pub agents: Arc<A>,
    pub session: TopicSession,
    pub settings: Arc<Settings>,
    pub cancel: CancellationToken,
    pub progress: Progress,
}

impl<A: Agents> SummaryRun<A> {
    pub async fn execute(self) -> Result<Prepared, String> {
        let parent_cancel = self.cancel.clone();
        let run = Self { cancel: self.cancel.child_token(), ..self };
        // The supervisor keeps polling cleanup even if its caller drops this future.
        let _cancel_on_drop = run.cancel.clone().drop_guard();
        tokio::spawn(async move {
            let _cancel_on_exit = run.cancel.clone().drop_guard();
            let progress = run.progress.clone();
            let prepared =
                AssertUnwindSafe(run.prepare()).catch_unwind().await.unwrap_or(Err(CRASHED));
            let result = if parent_cancel.is_cancelled() { Err(CANCELLED) } else { prepared };
            if result.is_err() {
                progress.observe(&AgentEvent::Failed(String::new()));
            }
            progress.finish(matches!(result, Err(CANCELLED)));
            result.map_err(str::to_owned)
        })
        .await
        .map_err(|_| CRASHED.to_owned())?
    }

    async fn prepare(self) -> Result<Prepared, &'static str> {
        let Self { agents, session, settings, cancel, progress } = self;
        let source = session.session.clone().ok_or("У исходной темы ещё нет сессии агента.")?;
        if cancel.is_cancelled() {
            return Err(CANCELLED);
        }
        let prompt = Prompt::new(SUMMARY_REQUEST.to_owned(), Vec::new()).ok_or(FAILED)?;
        let (input, inbox) = mpsc::channel(1);
        drop(input);
        let (events, received) = mpsc::channel(EVENTS);
        let channel =
            Arc::new(SummaryChannel { cancel: cancel.clone(), attempted: AtomicBool::new(false) });
        let conversation = Conversation {
            channel: channel.clone(),
            events,
            cancel: cancel.clone(),
            limits: Limits::new(Duration::ZERO),
        };
        let backend = AssertUnwindSafe(agents.run(&session, prompt, inbox, conversation, settings))
            .catch_unwind()
            .map(|result| result.map_err(|_| CRASHED))
            .boxed();
        let mut stream = Stream { backend, received, ended: false };
        let mut capture =
            Capture { source, text: String::new(), units: 0, finished: None, turns: 0 };
        let deadline = sleep(GENERATION_TIMEOUT);
        tokio::pin!(deadline);
        progress.executing();
        let outcome = loop {
            let event = tokio::select! {
                biased;
                () = cancel.cancelled() => break Err(CANCELLED),
                () = &mut deadline => break Err(EXPIRED),
                event = stream.next() => event,
            };
            match event {
                Ok(Some(event)) => {
                    progress.observe(&event);
                    if let Err(error) = capture.observe(event) {
                        break Err(error);
                    }
                    if capture.finished.is_some() {
                        break Ok(());
                    }
                }
                Ok(None) => break Err(FAILED),
                Err(error) => break Err(error),
            }
        };
        cancel.cancel();
        let cleanup = timeout(CLEANUP_TIMEOUT, stream.drain(&mut capture)).await;
        if channel.attempted.load(Ordering::SeqCst) {
            return Err(INTERACTION);
        }
        outcome?;
        cleanup.map_err(|_| "Агент не завершил остановку после подготовки сводки.")??;
        Ok(Prepared {
            summary: Summary::parse(&capture.text)?,
            finished: capture.finished.ok_or(FAILED)?,
        })
    }
}

struct Stream<'a> {
    backend: BoxFuture<'a, Result<mpsc::Receiver<Prompt>, &'static str>>,
    received: mpsc::Receiver<AgentEvent>,
    ended: bool,
}

impl Stream<'_> {
    async fn next(&mut self) -> Result<Option<AgentEvent>, &'static str> {
        if self.ended {
            return Ok(self.received.try_recv().ok());
        }
        tokio::select! {
            event = self.received.recv() => Ok(event),
            result = &mut self.backend => {
                self.ended = true;
                result?;
                Ok(self.received.try_recv().ok())
            }
        }
    }

    async fn drain(&mut self, capture: &mut Capture) -> Result<(), &'static str> {
        let mut outcome = Ok(());
        let mut events_open = true;
        while !self.ended {
            tokio::select! {
                result = &mut self.backend => {
                    self.ended = true;
                    outcome = outcome.and(result.map(|_| ()));
                }
                event = self.received.recv(), if events_open => {
                    match event {
                        Some(event) => outcome = outcome.and(capture.observe(event)),
                        None => events_open = false,
                    }
                }
            }
        }
        // A backend can retain an event sender after returning. Closing prevents that
        // sender from extending cleanup; only the bounded buffered tail is consumed.
        self.received.close();
        while let Ok(event) = self.received.try_recv() {
            outcome = outcome.and(capture.observe(event));
        }
        outcome
    }
}

struct Capture {
    source: SessionId,
    text: String,
    units: usize,
    finished: Option<Finished>,
    turns: usize,
}

impl Capture {
    fn observe(&mut self, event: AgentEvent) -> Result<(), &'static str> {
        match event {
            AgentEvent::SessionStarted(session) => {
                if session != self.source {
                    return Err(CHANGED_SESSION);
                }
            }
            AgentEvent::AssistantText(text) => {
                if self.finished.is_some() {
                    return Err(FAILED);
                }
                let units = text.encode_utf16().take(MAX_SUMMARY_UNITS + 1).count();
                if units > MAX_SUMMARY_UNITS - self.units {
                    return Err(OVERFLOW);
                }
                self.text.push_str(&text);
                self.units += units;
            }
            AgentEvent::Finished(finished) => {
                if finished.session != self.source {
                    return Err(CHANGED_SESSION);
                }
                if finished.background != 0 {
                    return Err(BACKGROUND);
                }
                if self.finished.replace(finished).is_some() {
                    return Err(FAILED);
                }
            }
            AgentEvent::TurnStarted => {
                if self.turns != 0 || self.finished.is_some() {
                    return Err(FAILED);
                }
                self.turns = 1;
            }
            AgentEvent::ToolCall(_) => return Err(INTERACTION),
            AgentEvent::Failed(_) => return Err(FAILED),
            AgentEvent::BackgroundAbandoned(_) => return Err(BACKGROUND),
            AgentEvent::BackgroundChanged(count) => {
                if count != 0 {
                    return Err(BACKGROUND);
                }
            }
            AgentEvent::Quota(_) => {}
        }
        Ok(())
    }
}

struct SummaryChannel {
    cancel: CancellationToken,
    attempted: AtomicBool,
}

impl SummaryChannel {
    fn deny(&self) -> Denied {
        self.attempted.store(true, Ordering::SeqCst);
        self.cancel.cancel();
        Denied::new("Для сводки разрешён только текст из уже известной истории.")
    }
}

impl UserChannel for SummaryChannel {
    fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(std::future::ready(Decision::Denied(self.deny())))
    }

    fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(std::future::ready(QuestionsOutcome::Denied(self.deny())))
    }

    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(std::future::ready(FileDelivery::Denied(self.deny())))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::archive::SessionArchive;
    use hub_agent::conversation::Conversation;
    use hub_core::domain::{
        AbsolutePath, AgentEvent, BackendKind, Decision, FileDelivery, OutgoingFile, Prompt,
        QuestionsOutcome, SessionId, ToolRequest, ToolUse, Usage,
    };
    use hub_core::handoff::SUMMARY_REQUEST;
    use hub_core::quota::QuotaReport;
    use hub_core::settings::Draft;
    use rstest::rstest;
    use tokio::sync::{Notify, mpsc};

    use super::*;

    #[derive(Clone, Copy)]
    enum Ending {
        Return,
        WaitForCancel,
        HoldCleanup,
        IgnoreCancel,
        ClosedEvents,
        RetainEvents,
        Panic,
        Permission,
        Question,
        File,
    }

    struct Invocation {
        session: TopicSession,
        prompt: Prompt,
        settings: Arc<Settings>,
        cancel: CancellationToken,
    }

    struct FakeAgents {
        events: Vec<AgentEvent>,
        ending: Ending,
        invocation: Mutex<Option<Invocation>>,
        started: Notify,
        cleanup_started: Notify,
        release_cleanup: Notify,
        retained_events: Mutex<Option<mpsc::Sender<AgentEvent>>>,
        exited: AtomicBool,
    }

    struct Exited<'a>(&'a AtomicBool);
    impl Drop for Exited<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl Agents for FakeAgents {
        fn run<'a>(
            &'a self,
            session: &'a TopicSession,
            prompt: Prompt,
            mut inbox: mpsc::Receiver<Prompt>,
            conversation: Conversation,
            settings: Arc<Settings>,
        ) -> BoxFuture<'a, mpsc::Receiver<Prompt>> {
            Box::pin(async move {
                let _exited = Exited(&self.exited);
                *self.invocation.lock().unwrap() = Some(Invocation {
                    session: session.clone(),
                    prompt,
                    settings,
                    cancel: conversation.cancel.clone(),
                });
                self.started.notify_one();
                assert!(inbox.recv().await.is_none(), "summary must have a closed inbox");
                for event in &self.events {
                    if conversation.events.send(event.clone()).await.is_err() {
                        return inbox;
                    }
                }
                match self.ending {
                    Ending::Return => {}
                    Ending::WaitForCancel => conversation.cancel.cancelled().await,
                    Ending::HoldCleanup => {
                        conversation.cancel.cancelled().await;
                        self.cleanup_started.notify_one();
                        self.release_cleanup.notified().await;
                    }
                    Ending::IgnoreCancel => std::future::pending::<()>().await,
                    Ending::ClosedEvents => {
                        drop(conversation.events);
                        std::future::pending::<()>().await;
                    }
                    Ending::RetainEvents => {
                        *self.retained_events.lock().unwrap() = Some(conversation.events.clone());
                    }
                    Ending::Panic => panic!("fake backend crashed"),
                    Ending::Permission => {
                        assert!(matches!(
                            conversation
                                .channel
                                .request(ToolRequest {
                                    tool: "write".to_owned(),
                                    summary: "secret".to_owned(),
                                })
                                .await,
                            Decision::Denied(_)
                        ));
                        let _ = conversation.events.send(finished()).await;
                    }
                    Ending::Question => {
                        assert!(matches!(
                            conversation.channel.ask(Vec::new()).await,
                            QuestionsOutcome::Denied(_)
                        ));
                        let _ = conversation.events.send(finished()).await;
                    }
                    Ending::File => {
                        assert!(matches!(
                            conversation
                                .channel
                                .send_file(OutgoingFile {
                                    path: "secret".to_owned(),
                                    caption: String::new(),
                                })
                                .await,
                            FileDelivery::Denied(_)
                        ));
                        let _ = conversation.events.send(finished()).await;
                    }
                }
                inbox
            })
        }

        fn archive<'a>(
            &'a self,
            _backend: BackendKind,
            _home: &'a Path,
        ) -> BoxFuture<'a, Result<Arc<dyn SessionArchive>, String>> {
            Box::pin(async { Err("unused".to_owned()) })
        }

        fn quotas(&self) -> BoxFuture<'_, Vec<(BackendKind, Result<QuotaReport, String>)>> {
            Box::pin(async { Vec::new() })
        }
    }

    fn finished() -> AgentEvent {
        AgentEvent::Finished(Finished {
            session: SessionId::parse("source-session").unwrap(),
            usage: Usage::Codex { tokens: Some(123) },
            background: 0,
        })
    }

    fn run(events: Vec<AgentEvent>, ending: Ending) -> SummaryRun<FakeAgents> {
        let root = std::env::temp_dir();
        let settings = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        }
        .parse(&root)
        .unwrap();
        SummaryRun {
            agents: Arc::new(FakeAgents {
                events,
                ending,
                invocation: Mutex::new(None),
                started: Notify::new(),
                cleanup_started: Notify::new(),
                release_cleanup: Notify::new(),
                retained_events: Mutex::new(None),
                exited: AtomicBool::new(false),
            }),
            session: TopicSession::fresh(BackendKind::Codex, AbsolutePath::new(root).unwrap())
                .with_session(SessionId::parse("source-session")),
            settings: Arc::new(settings),
            cancel: CancellationToken::new(),
            progress: Progress::new(),
        }
    }

    #[tokio::test]
    async fn successful_summary_preserves_context_settings_usage_and_parent_token() {
        let run = run(
            vec![
                AgentEvent::SessionStarted(SessionId::parse("source-session").unwrap()),
                AgentEvent::AssistantText("Цель: задача\n".to_owned()),
                AgentEvent::AssistantText("Далее: тесты".to_owned()),
                finished(),
            ],
            Ending::WaitForCancel,
        );
        let (agents, session, settings, cancel) =
            (run.agents.clone(), run.session.clone(), run.settings.clone(), run.cancel.clone());
        let prepared = run.execute().await.unwrap();
        let invocation = agents.invocation.lock().unwrap();
        let invocation = invocation.as_ref().unwrap();
        assert_eq!(
            (
                prepared.summary.text(),
                prepared.finished.session.as_str(),
                prepared.finished.usage,
                &invocation.session,
                invocation.prompt.text()
            ),
            (
                "Цель: задача\nДалее: тесты",
                "source-session",
                Usage::Codex { tokens: Some(123) },
                &session,
                SUMMARY_REQUEST
            )
        );
        assert!(Arc::ptr_eq(&settings, &invocation.settings));
        assert!(invocation.cancel.is_cancelled());
        assert!(!cancel.is_cancelled());
        assert!(agents.exited.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn event_drain_does_not_block_on_channel_capacity() {
        let events = (0..100)
            .map(|_| AgentEvent::AssistantText("x".to_owned()))
            .chain([finished()])
            .collect();
        assert_eq!(
            run(events, Ending::Return).execute().await.unwrap().summary.text(),
            "x".repeat(100)
        );
    }

    #[rstest]
    #[case(vec![AgentEvent::Failed("backend-secret".to_owned())])]
    #[case(vec![finished()])]
    #[case(vec![AgentEvent::AssistantText("🦀".repeat(1501)), finished()])]
    #[case(vec![AgentEvent::SessionStarted(SessionId::parse("other").unwrap()), finished()])]
    #[case(vec![AgentEvent::Finished(Finished { session: SessionId::parse("other").unwrap(), usage: Usage::Codex { tokens: None }, background: 0 })])]
    #[case(vec![AgentEvent::ToolCall(ToolUse { tool: "write".to_owned(), summary: "secret".to_owned() })])]
    #[case(vec![AgentEvent::BackgroundChanged(1)])]
    #[case(vec![AgentEvent::BackgroundAbandoned(vec!["secret".to_owned()])])]
    #[case(vec![AgentEvent::Finished(Finished { session: SessionId::parse("source-session").unwrap(), usage: Usage::Codex { tokens: None }, background: 1 })])]
    #[case(vec![AgentEvent::AssistantText("unfinished".to_owned())])]
    #[tokio::test]
    async fn invalid_backend_outcomes_are_rejected_without_exposing_output(
        #[case] events: Vec<AgentEvent>,
    ) {
        let error = run(events, Ending::Return).execute().await.unwrap_err();
        assert!(!error.contains("secret"));
        assert!(!error.contains("unfinished"));
        assert!(!error.contains("other"));
    }

    #[rstest]
    #[case(Ending::Permission)]
    #[case(Ending::Question)]
    #[case(Ending::File)]
    #[tokio::test]
    async fn hub_interactions_are_denied_and_disqualify_the_summary(#[case] ending: Ending) {
        let run = run(vec![AgentEvent::AssistantText("summary".to_owned())], ending);
        let agents = run.agents.clone();
        assert!(run.execute().await.is_err());
        assert!(agents.exited.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn missing_source_session_never_starts_a_backend() {
        let mut run = run(Vec::new(), Ending::Return);
        run.session.session = None;
        let agents = run.agents.clone();
        assert!(run.execute().await.is_err());
        assert!(agents.invocation.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn already_cancelled_run_never_starts_a_backend() {
        let run = run(Vec::new(), Ending::Return);
        let agents = run.agents.clone();
        run.cancel.cancel();
        assert!(run.execute().await.is_err());
        assert!(agents.invocation.lock().unwrap().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn retained_event_sender_does_not_delay_a_completed_backend() {
        let started = tokio::time::Instant::now();
        let result = run(
            vec![AgentEvent::AssistantText("summary".to_owned()), finished()],
            Ending::RetainEvents,
        )
        .execute()
        .await;
        assert!(result.is_ok());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn closed_event_channel_does_not_spin_or_wait_for_generation_deadline() {
        let started = tokio::time::Instant::now();
        let run = run(Vec::new(), Ending::ClosedEvents);
        let agents = run.agents.clone();
        assert!(run.execute().await.is_err());
        assert!(agents.exited.load(Ordering::SeqCst));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[tokio::test(start_paused = true)]
    async fn successful_turn_without_backend_cleanup_is_rejected() {
        let run = run(
            vec![AgentEvent::AssistantText("summary".to_owned()), finished()],
            Ending::IgnoreCancel,
        );
        let agents = run.agents.clone();
        assert!(run.execute().await.is_err());
        assert!(agents.exited.load(Ordering::SeqCst));
    }

    #[rstest]
    #[case(AgentEvent::ToolCall(ToolUse { tool: "write".to_owned(), summary: "secret".to_owned() }))]
    #[case(AgentEvent::Failed("secret".to_owned()))]
    #[case(AgentEvent::BackgroundChanged(1))]
    #[case(AgentEvent::SessionStarted(SessionId::parse("changed").unwrap()))]
    #[case(AgentEvent::AssistantText("extra turn".to_owned()))]
    #[case(finished())]
    #[tokio::test]
    async fn violations_after_finished_are_still_rejected(#[case] tail: AgentEvent) {
        let run = run(
            vec![AgentEvent::AssistantText("summary".to_owned()), finished(), tail],
            Ending::WaitForCancel,
        );
        assert!(run.execute().await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn generation_expires_at_three_minutes_and_drops_an_uncooperative_backend() {
        let run = run(Vec::new(), Ending::IgnoreCancel);
        let agents = run.agents.clone();
        let task = tokio::spawn(run.execute());
        agents.started.notified().await;
        tokio::time::advance(Duration::from_secs(179)).await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_secs(20)).await;
        assert!(task.await.unwrap().unwrap_err().contains("время"));
        assert!(agents.exited.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_waits_for_backend_cleanup() {
        let run = run(Vec::new(), Ending::WaitForCancel);
        let (agents, cancel) = (run.agents.clone(), run.cancel.clone());
        let task = tokio::spawn(run.execute());
        agents.started.notified().await;
        cancel.cancel();
        assert!(task.await.unwrap().is_err());
        assert!(agents.exited.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancellation_during_successful_cleanup_still_rejects_the_summary() {
        let run = run(
            vec![AgentEvent::AssistantText("summary".to_owned()), finished()],
            Ending::HoldCleanup,
        );
        let (agents, cancel) = (run.agents.clone(), run.cancel.clone());
        let task = tokio::spawn(run.execute());
        agents.cleanup_started.notified().await;
        cancel.cancel();
        agents.release_cleanup.notify_one();
        assert!(task.await.unwrap().is_err());
        assert!(agents.exited.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_execute_cancels_and_bounds_backend_cleanup() {
        let run = run(Vec::new(), Ending::IgnoreCancel);
        let (agents, parent, progress) =
            (run.agents.clone(), run.cancel.clone(), run.progress.clone());
        let task = tokio::spawn(run.execute());
        agents.started.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        progress.finished().await;
        assert!(agents.exited.load(Ordering::SeqCst));
        assert!(agents.invocation.lock().unwrap().as_ref().unwrap().cancel.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[tokio::test]
    async fn backend_panic_becomes_a_safe_failure_and_cancels_child() {
        let run = run(Vec::new(), Ending::Panic);
        let agents = run.agents.clone();
        let error = run.execute().await.unwrap_err();
        assert!(!error.contains("fake backend"));
        assert!(agents.invocation.lock().unwrap().as_ref().unwrap().cancel.is_cancelled());
        assert!(agents.exited.load(Ordering::SeqCst));
    }
}
