//! The human behind a topic: approvals and questions as messages with buttons, files as
//! documents. Waiting happens in the session task; button presses resolve it through the
//! shared registries.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_core::approvals::{ApprovalId, ApprovalOutcome, ApprovalRegistry, Verdict, callback_data};
use hub_core::attachments::MAX_SEND_BYTES;
use hub_core::domain::{
    AbsolutePath, Decision, Denied, FileDelivery, OutgoingFile, Question, QuestionAnswer,
    QuestionsOutcome, ToolRequest, TopicKey,
};
use hub_core::questions::{self, Answer, Button, QuestionId, QuestionRegistry};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::messenger::{Format, Outgoing, Target};
use crate::paths::outgoing_path;
use crate::sender::Sender;
use crate::texts;

#[derive(Default)]
pub struct Registries {
    pub approvals: ApprovalRegistry<oneshot::Sender<()>>,
    pub questions: QuestionRegistry<oneshot::Sender<Answer>>,
}

pub type Shared = Arc<Mutex<Registries>>;

/// Registries stay usable after a panic elsewhere: their state is plain maps, never half-updated.
pub fn lock(registries: &Shared) -> MutexGuard<'_, Registries> {
    registries.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn random_id() -> [u8; 8] {
    Uuid::new_v4().as_u64_pair().0.to_le_bytes()
}

pub struct TelegramChannel {
    sender: Sender,
    key: TopicKey,
    cwd: AbsolutePath,
    home: PathBuf,
    registries: Shared,
    timeout: Duration,
}

impl TelegramChannel {
    #[must_use]
    pub fn new(
        sender: Sender,
        key: TopicKey,
        cwd: AbsolutePath,
        home: PathBuf,
        registries: Shared,
        timeout: Duration,
    ) -> Self {
        Self { sender, key, cwd, home, registries, timeout }
    }

    async fn approval(&self, tool: ToolRequest) -> Decision {
        let id = ApprovalId::from_random(random_id());
        let (responder, notification) = oneshot::channel();
        lock(&self.registries).approvals.open(id.clone(), responder);
        let _open = OpenApproval { registries: &self.registries, id: id.clone() };
        let text = texts::approval_html(&tool);
        let keyboard = vec![vec![
            Button {
                text: "✅ Разрешить".to_owned(), data: callback_data(&id, Verdict::Allow)
            },
            Button {
                text: "❌ Запретить".to_owned(), data: callback_data(&id, Verdict::Deny)
            },
        ]];
        let outgoing = Outgoing::html(text.clone()).with_keyboard(keyboard);
        let Some(message) = self.sender.one(Target::Topic(self.key), outgoing).await else {
            return Decision::Denied(Denied::new(texts::APPROVAL_UNSENT));
        };
        // Notification is only a wake-up. Even when timeout wins the future poll, a
        // button may already have committed its answer under the registry lock.
        let _ = tokio::time::timeout(self.timeout, notification).await;
        let outcome = lock(&self.registries).approvals.finish(&id);
        let (decision, verdict) = match outcome {
            ApprovalOutcome::Answered(verdict) => {
                let decision = verdict.decision();
                let line = texts::verdict(&decision);
                (decision, line)
            }
            ApprovalOutcome::TimedOut => (
                Decision::Denied(Denied::new(texts::no_answer(self.timeout))),
                "⌛ Нет ответа — запрещено",
            ),
        };
        // Only the asker edits the final text, after the registry is closed and unlocked.
        let shown = format!("{text}\n\n{verdict}");
        self.sender.edit(self.key.chat, message, shown, Format::Html).await;
        decision
    }

    async fn questions(&self, questions: Vec<Question>) -> QuestionsOutcome {
        let mut answers = Vec::new();
        for question in questions {
            match self.question(&question).await {
                Answer::Text(answer) => {
                    answers.push(QuestionAnswer { question: question.text().to_owned(), answer });
                }
                Answer::Declined(denied) => return QuestionsOutcome::Denied(denied),
            }
        }
        QuestionsOutcome::Answered(answers)
    }

    async fn question(&self, question: &Question) -> Answer {
        let id = QuestionId::from_random(random_id());
        let (responder, answer) = oneshot::channel();
        lock(&self.registries).questions.open(id.clone(), self.key, question.clone(), responder);
        let _open = OpenQuestion { registries: &self.registries, id: id.clone() };
        let text = questions::question_html(question);
        let keyboard = questions::keyboard(&id, question, &BTreeSet::new());
        let outgoing = Outgoing::html(text.clone()).with_keyboard(keyboard);
        let Some(message) = self.sender.one(Target::Topic(self.key), outgoing).await else {
            return Answer::Declined(Denied::new(texts::QUESTION_UNSENT));
        };
        match tokio::time::timeout(self.timeout, answer).await {
            Ok(Ok(answer)) => {
                let shown = format!("{text}\n\n{}", questions::answer_line(&answer));
                self.sender.edit(self.key.chat, message, shown, Format::Html).await;
                answer
            }
            Ok(Err(_)) => Answer::Declined(Denied::new(texts::no_answer(self.timeout))),
            Err(_) => {
                let expired = format!("{text}\n\n⌛ Нет ответа");
                self.sender.edit(self.key.chat, message, expired, Format::Html).await;
                Answer::Declined(Denied::new(texts::no_answer(self.timeout)))
            }
        }
    }

    async fn file(&self, file: OutgoingFile) -> FileDelivery {
        let (cwd, home, raw) = (self.cwd.clone(), self.home.clone(), file.path.clone());
        let checked =
            tokio::task::spawn_blocking(move || outgoing_path(&cwd, &home, &raw, MAX_SEND_BYTES))
                .await;
        let path = match checked {
            Ok(Ok(path)) => path,
            Ok(Err(error)) => return FileDelivery::Denied(Denied::new(error.to_string())),
            Err(error) => {
                return FileDelivery::Denied(Denied::new(format!(
                    "Проверка файла прервана: {error}"
                )));
            }
        };
        match self.sender.document(self.key, path.clone(), &file.caption).await {
            Ok(()) => {
                tracing::info!(file = %path.display(), "file sent");
                FileDelivery::Delivered
            }
            Err(error) => {
                tracing::warn!(%error, "file delivery failed");
                FileDelivery::Denied(Denied::new(format!("Telegram не принял файл: {error}")))
            }
        }
    }
}

impl UserChannel for TelegramChannel {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(self.approval(tool))
    }

    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(self.questions(questions))
    }

    fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(self.file(file))
    }
}

/// Closes the approval however the wait ends, including the session being stopped.
struct OpenApproval<'a> {
    registries: &'a Shared,
    id: ApprovalId,
}

impl Drop for OpenApproval<'_> {
    fn drop(&mut self) {
        lock(self.registries).approvals.close(&self.id);
    }
}

struct OpenQuestion<'a> {
    registries: &'a Shared,
    id: QuestionId,
}

impl Drop for OpenQuestion<'_> {
    fn drop(&mut self) {
        lock(self.registries).questions.close(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use hub_core::domain::{
        ChatId, Denied, MessageId, QuestionOption, Selection, ThreadId, UserId,
    };

    use super::*;
    use crate::inbound::Press;
    use crate::messenger::{Messenger, SendError};
    use crate::paths::workspace_root;
    use crate::press;
    use crate::testing::{Call, FakeMessenger};

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };

    struct Setup {
        channel: Arc<TelegramChannel>,
        messenger: Arc<FakeMessenger>,
        sender: Sender,
        registries: Shared,
        dir: tempfile::TempDir,
    }

    fn setup(timeout: Duration) -> Setup {
        setup_with_messenger(timeout, FakeMessenger::default())
    }

    fn setup_with_messenger(timeout: Duration, messenger: FakeMessenger) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let cwd = workspace_root(dir.path()).unwrap();
        let messenger = Arc::new(messenger);
        let sender = Sender::new(Arc::clone(&messenger) as Arc<dyn Messenger>);
        let registries = Shared::default();
        let channel = Arc::new(TelegramChannel::new(
            sender.clone(),
            KEY,
            cwd,
            std::env::temp_dir(),
            Arc::clone(&registries),
            timeout,
        ));
        Setup { channel, messenger, sender, registries, dir }
    }

    async fn keyboard(messenger: &FakeMessenger) -> (MessageId, Vec<Vec<Button>>) {
        for _ in 0..200 {
            if let Some(found) = messenger.last_keyboard() {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no keyboard was sent");
    }

    fn press(data: &str, message: MessageId) -> Press {
        Press {
            callback: "cb".to_owned(),
            user: UserId(1),
            chat: Some(KEY.chat),
            message: Some(message),
            html: Some("🔐 <b>Bash</b>".to_owned()),
            data: data.to_owned(),
        }
    }

    fn button(rows: &[Vec<Button>], row: usize, column: usize) -> String {
        rows[row][column].data.clone()
    }

    fn question(selection: Selection) -> Question {
        Question::new(
            "Цвет?".to_owned(),
            "Цвет".to_owned(),
            vec![
                QuestionOption { label: "Красный".to_owned(), description: String::new() },
                QuestionOption { label: "Синий".to_owned(), description: String::new() },
            ],
            selection,
        )
        .unwrap()
    }

    fn bash() -> ToolRequest {
        ToolRequest { tool: "Bash".to_owned(), summary: "ls".to_owned() }
    }

    fn stale(text: &str) -> Call {
        Call::Answer { callback: "cb".to_owned(), text: Some(text.to_owned()) }
    }

    fn approval_edits(messenger: &FakeMessenger) -> Vec<String> {
        messenger
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Edit { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn approval_waiter_updates_the_original_message_without_callback_html() {
        let setup = setup(Duration::from_secs(5));
        let mut request = Box::pin(setup.channel.request(bash()));
        assert!(futures::poll!(&mut request).is_pending());
        let (message, rows) = setup.messenger.last_keyboard().unwrap();
        let mut pressed = press(&button(&rows, 0, 0), message);
        pressed.html = None;
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), pressed).await;

        assert_eq!(request.await, Decision::Allowed);
        let edits: Vec<_> = setup
            .messenger
            .calls()
            .into_iter()
            .filter(|call| matches!(call, Call::Edit { .. }))
            .collect();
        assert_eq!(
            edits,
            [Call::Edit {
                chat: KEY.chat,
                message,
                text: "🔐 <b>Bash</b>\n<pre>ls</pre>\n\n✅ Разрешено".to_owned(),
                format: Format::Html,
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn committed_approval_wins_when_timeout_fires_before_notification() {
        let setup = setup(Duration::from_secs(5));
        let mut request = Box::pin(setup.channel.request(bash()));
        assert!(futures::poll!(&mut request).is_pending());
        let (_, rows) = setup.messenger.last_keyboard().unwrap();
        let answer = hub_core::approvals::parse_callback_data(&button(&rows, 0, 0)).unwrap();

        // Stop the button path between committing the result and sending its wake-up.
        // The receiver stays pending, so the wait MUST take its timeout branch.
        let notification = lock(&setup.registries).approvals.resolve(&answer).unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;

        assert_eq!(request.await, Decision::Allowed);
        assert_eq!(
            approval_edits(&setup.messenger),
            ["🔐 <b>Bash</b>\n<pre>ls</pre>\n\n✅ Разрешено"]
        );
        assert!(notification.send(()).is_err(), "the timed-out receiver was dropped");
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_wins_and_an_already_created_press_is_stale() {
        let setup = setup(Duration::from_secs(5));
        let mut request = Box::pin(setup.channel.request(bash()));
        assert!(futures::poll!(&mut request).is_pending());
        let (message, rows) = setup.messenger.last_keyboard().unwrap();
        let pending_press = press::handle(
            Arc::clone(&setup.registries),
            setup.sender.clone(),
            press(&button(&rows, 0, 0), message),
        );
        tokio::time::advance(Duration::from_secs(5)).await;

        assert_eq!(request.await, Decision::Denied(Denied::new("Нет ответа пользователя за 5 с")));
        pending_press.await;
        assert!(setup.messenger.calls().contains(&stale("Запрос уже неактуален")));
        assert_eq!(
            approval_edits(&setup.messenger),
            ["🔐 <b>Bash</b>\n<pre>ls</pre>\n\n⌛ Нет ответа — запрещено"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_buttons_commit_only_one_decision() {
        let setup = setup(Duration::from_secs(5));
        let mut request = Box::pin(setup.channel.request(bash()));
        assert!(futures::poll!(&mut request).is_pending());
        let (message, rows) = setup.messenger.last_keyboard().unwrap();
        let mut deny = press(&button(&rows, 0, 1), message);
        deny.callback = "deny".to_owned();

        futures::join!(
            press::handle(
                Arc::clone(&setup.registries),
                setup.sender.clone(),
                press(&button(&rows, 0, 0), message)
            ),
            press::handle(Arc::clone(&setup.registries), setup.sender.clone(), deny),
        );

        assert_eq!(request.await, Decision::Allowed);
        let answers: Vec<_> = setup
            .messenger
            .calls()
            .into_iter()
            .filter(|call| matches!(call, Call::Answer { .. }))
            .collect();
        assert_eq!(
            answers,
            [
                Call::Answer { callback: "cb".to_owned(), text: None },
                Call::Answer {
                    callback: "deny".to_owned(),
                    text: Some("Запрос уже неактуален".to_owned())
                },
            ]
        );
        assert_eq!(
            approval_edits(&setup.messenger),
            ["🔐 <b>Bash</b>\n<pre>ls</pre>\n\n✅ Разрешено"]
        );
    }

    #[rstest::rstest]
    #[case::allowed(Some(0), Decision::Allowed, "✅ Разрешено")]
    #[case::denied(Some(1), Verdict::Deny.decision(), "❌ Запрещено")]
    #[case::timed_out(
        None,
        Decision::Denied(Denied::new("Нет ответа пользователя за 5 с")),
        "⌛ Нет ответа — запрещено"
    )]
    #[tokio::test(start_paused = true)]
    async fn late_press_during_a_delayed_edit_cannot_change_the_outcome(
        #[case] chosen: Option<usize>,
        #[case] expected: Decision,
        #[case] line: &str,
    ) {
        let setup = setup(Duration::from_secs(5));
        let release = setup.messenger.delay_next_edit();
        let mut request = Box::pin(setup.channel.request(bash()));
        assert!(futures::poll!(&mut request).is_pending());
        let (message, rows) = setup.messenger.last_keyboard().unwrap();
        if let Some(column) = chosen {
            press::handle(
                Arc::clone(&setup.registries),
                setup.sender.clone(),
                press(&button(&rows, 0, column), message),
            )
            .await;
        } else {
            tokio::time::advance(Duration::from_secs(5)).await;
        }

        // The outcome is committed, but the final Telegram edit is still in flight.
        assert!(futures::poll!(&mut request).is_pending());
        assert!(approval_edits(&setup.messenger).is_empty());
        press::handle(
            Arc::clone(&setup.registries),
            setup.sender.clone(),
            press(&button(&rows, 0, 0), message),
        )
        .await;
        assert!(setup.messenger.calls().contains(&stale("Запрос уже неактуален")));
        tokio::time::advance(Duration::from_secs(5)).await;
        release.send(()).unwrap();

        assert_eq!(request.await, expected);
        assert_eq!(
            approval_edits(&setup.messenger),
            [format!("🔐 <b>Bash</b>\n<pre>ls</pre>\n\n{line}")]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_wait_makes_an_already_created_press_stale() {
        let setup = setup(Duration::from_secs(5));
        let mut request = Box::pin(setup.channel.request(bash()));
        assert!(futures::poll!(&mut request).is_pending());
        let (message, rows) = setup.messenger.last_keyboard().unwrap();
        let pending_press = press::handle(
            Arc::clone(&setup.registries),
            setup.sender.clone(),
            press(&button(&rows, 0, 0), message),
        );
        drop(request);

        pending_press.await;
        assert!(setup.messenger.calls().contains(&stale("Запрос уже неактуален")));
        assert!(approval_edits(&setup.messenger).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn failed_approval_delivery_closes_the_request() {
        let setup = setup_with_messenger(
            Duration::from_secs(5),
            FakeMessenger::with_sends(vec![Err(SendError::Failed("offline".to_owned()))]),
        );
        assert_eq!(
            setup.channel.request(bash()).await,
            Decision::Denied(Denied::new("Не удалось отправить запрос подтверждения в Telegram"))
        );
        let attempted = setup.messenger.send_attempts();
        let [message] = attempted.as_slice() else { panic!("one approval send expected") };
        press::handle(
            Arc::clone(&setup.registries),
            setup.sender.clone(),
            press(&button(&message.keyboard, 0, 0), MessageId(1)),
        )
        .await;

        assert!(setup.messenger.calls().contains(&stale("Запрос уже неактуален")));
        assert!(approval_edits(&setup.messenger).is_empty());
    }

    #[tokio::test]
    async fn approval_is_asked_with_buttons_and_resolved_by_a_press() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let request = tokio::spawn(async move { channel.request(bash()).await });

        let (message, rows) = keyboard(&setup.messenger).await;
        let labels: Vec<_> = rows[0].iter().map(|b| b.text.as_str()).collect();
        assert_eq!(labels, ["✅ Разрешить", "❌ Запретить"]);
        let pressed = press(&button(&rows, 0, 0), message);
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), pressed).await;

        assert_eq!(request.await.unwrap(), Decision::Allowed);
        assert!(setup.messenger.calls().contains(&Call::Edit {
            chat: KEY.chat,
            message,
            text: "🔐 <b>Bash</b>\n<pre>ls</pre>\n\n✅ Разрешено".to_owned(),
            format: Format::Html,
        }));
    }

    #[tokio::test]
    async fn unanswered_approval_is_denied_after_the_timeout() {
        let setup = setup(Duration::from_millis(50));
        let decision = setup.channel.request(bash()).await;
        assert_eq!(decision, Decision::Denied(Denied::new("Нет ответа пользователя за 0 с")));
        assert!(setup.messenger.calls().iter().any(
            |call| matches!(call, Call::Edit { text, .. } if text.ends_with("⌛ Нет ответа — запрещено"))
        ));
    }

    #[tokio::test]
    async fn stale_approval_press_is_answered_as_stale() {
        let setup = setup(Duration::from_millis(50));
        let _ = setup.channel.request(bash()).await;
        let (message, rows) = keyboard(&setup.messenger).await;
        let pressed = press(&button(&rows, 0, 0), message);
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), pressed).await;
        assert!(setup.messenger.calls().contains(&stale("Запрос уже неактуален")));
    }

    #[tokio::test]
    async fn dropped_request_closes_its_approval() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let request = tokio::spawn(async move { channel.request(bash()).await });
        let (message, rows) = keyboard(&setup.messenger).await;
        request.abort();
        let _ = request.await;
        let pressed = press(&button(&rows, 0, 0), message);
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), pressed).await;
        assert!(setup.messenger.calls().contains(&stale("Запрос уже неактуален")));
    }

    #[tokio::test]
    async fn single_choice_question_is_answered_by_a_press() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let asked =
            tokio::spawn(async move { channel.ask(vec![question(Selection::Single)]).await });

        let (message, rows) = keyboard(&setup.messenger).await;
        let pressed = press(&button(&rows, 1, 0), message);
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), pressed).await;

        assert_eq!(
            asked.await.unwrap(),
            QuestionsOutcome::Answered(vec![QuestionAnswer {
                question: "Цвет?".to_owned(),
                answer: "Синий".to_owned()
            }])
        );
        assert!(
            setup
                .messenger
                .calls()
                .iter()
                .any(|call| matches!(call, Call::Edit { text, .. } if text.ends_with("💬 Синий")))
        );
    }

    #[tokio::test]
    async fn multi_choice_toggles_redraw_the_keyboard() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let asked =
            tokio::spawn(async move { channel.ask(vec![question(Selection::Multiple)]).await });

        let (message, rows) = keyboard(&setup.messenger).await;
        let toggle = press(&button(&rows, 0, 0), message);
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), toggle).await;
        assert!(setup.messenger.calls().iter().any(|call| matches!(call,
            Call::EditKeyboard { keyboard, .. } if keyboard[0][0].text == "☑ Красный")));
        let submit = press(&button(&rows, 2, 0), message);
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), submit).await;

        assert_eq!(
            asked.await.unwrap(),
            QuestionsOutcome::Answered(vec![QuestionAnswer {
                question: "Цвет?".to_owned(),
                answer: "Красный".to_owned()
            }])
        );
    }

    #[tokio::test]
    async fn declined_question_denies() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let asked =
            tokio::spawn(async move { channel.ask(vec![question(Selection::Single)]).await });
        let (message, rows) = keyboard(&setup.messenger).await;
        let decline = press(&button(&rows, 2, 0), message);
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), decline).await;
        assert!(matches!(asked.await.unwrap(), QuestionsOutcome::Denied(_)));
    }

    #[tokio::test]
    async fn file_inside_cwd_is_sent_and_outside_is_refused() {
        let setup = setup(Duration::from_secs(5));
        fs::write(setup.dir.path().join("r.pdf"), "%PDF").unwrap();

        let file = OutgoingFile { path: "r.pdf".to_owned(), caption: "Отчёт".to_owned() };
        assert_eq!(setup.channel.send_file(file).await, FileDelivery::Delivered);
        assert!(
            setup
                .messenger
                .calls()
                .iter()
                .any(|call| matches!(call, Call::Document { caption, .. } if caption == "Отчёт"))
        );

        let outside = OutgoingFile { path: "../x".to_owned(), caption: String::new() };
        assert!(matches!(setup.channel.send_file(outside).await, FileDelivery::Denied(_)));
    }

    #[tokio::test]
    async fn unknown_button_is_answered_as_stale() {
        let setup = setup(Duration::from_secs(5));
        let pressed = press("zz:1", MessageId(9));
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), pressed).await;
        assert!(setup.messenger.calls().contains(&stale("Вопрос уже неактуален")));
    }
}
