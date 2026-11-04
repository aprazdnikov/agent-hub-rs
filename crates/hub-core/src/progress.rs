//! The last observed execution state; never inferred from model output text.

use std::fmt::Write;
use std::time::{Duration, Instant};

use crate::domain::AgentEvent;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Unknown,
    Preparing,
    Running,
    AwaitingApproval,
    AwaitingAnswer,
    Background,
    Finishing,
    Stopping,
    Completed,
    Stopped,
    Failed,
}
impl Phase {
    pub const ALL: [Self; 11] = [
        Self::Unknown,
        Self::Preparing,
        Self::Running,
        Self::AwaitingApproval,
        Self::AwaitingAnswer,
        Self::Background,
        Self::Finishing,
        Self::Stopping,
        Self::Completed,
        Self::Stopped,
        Self::Failed,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Нет данных",
            Self::Preparing => "Подготовка",
            Self::Running => "Выполняется",
            Self::AwaitingApproval => "Ожидает разрешения",
            Self::AwaitingAnswer => "Ожидает ответа",
            Self::Background => "Фоновые задачи",
            Self::Finishing => "Завершается",
            Self::Stopping => "Останавливается",
            Self::Completed => "Завершено",
            Self::Stopped => "Остановлено",
            Self::Failed => "Ошибка",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitKind {
    Approval,
    Answer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activity {
    Preparing,
    Started,
    Answer,
    Tool,
    ApprovalOpened,
    ApprovalClosed,
    QuestionOpened,
    QuestionClosed,
    TurnFinished,
    BackgroundChanged,
    BackgroundExpired,
    Failed,
    StopRequested,
    Stopped,
    Unconfirmed,
}
impl Activity {
    fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Подготовка запуска",
            Self::Started => "Запуск выполнения",
            Self::Answer => "Получен ответ",
            Self::Tool => "Вызван инструмент",
            Self::ApprovalOpened => "Отправлен запрос разрешения",
            Self::ApprovalClosed => "Запрос разрешения закрыт",
            Self::QuestionOpened => "Отправлен вопрос",
            Self::QuestionClosed => "Вопрос закрыт",
            Self::TurnFinished => "Получен результат хода",
            Self::BackgroundChanged => "Обновлено число фоновых задач",
            Self::BackgroundExpired => "Истекло время фоновых задач",
            Self::Failed => "Получена ошибка",
            Self::StopRequested => "Запрошена остановка",
            Self::Stopped => "Запуск остановлен",
            Self::Unconfirmed => "Запуск завершён без подтверждённого результата",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskProgress {
    base: Phase,
    started: Option<Instant>,
    ended: Option<Instant>,
    activity: Option<(Activity, Instant)>,
    approvals: usize,
    questions: usize,
    background: usize,
    completed: bool,
}
impl TaskProgress {
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.ended.is_some()
    }
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            base: Phase::Preparing,
            started: Some(now),
            activity: Some((Activity::Preparing, now)),
            ..Self::default()
        }
    }
    #[must_use]
    pub fn phase(&self) -> Phase {
        if self.ended.is_some() || matches!(self.base, Phase::Stopping | Phase::Failed) {
            self.base
        } else if self.approvals > 0 {
            Phase::AwaitingApproval
        } else if self.questions > 0 {
            Phase::AwaitingAnswer
        } else {
            self.base
        }
    }
    fn accepts_updates(&self) -> bool {
        self.started.is_some() && self.ended.is_none() && self.base != Phase::Stopping
    }
    pub fn executing(&mut self, now: Instant) {
        if self.accepts_updates() {
            self.base = Phase::Running;
            self.completed = false;
            self.activity = Some((Activity::Started, now));
        }
    }
    pub fn observe(&mut self, event: &AgentEvent, now: Instant) {
        if !self.accepts_updates() {
            return;
        }
        if self.base == Phase::Failed && !matches!(event, AgentEvent::TurnStarted) {
            return;
        }
        let activity = match event {
            AgentEvent::TurnStarted | AgentEvent::SessionStarted(_) => {
                self.executing(now);
                Activity::Started
            }
            AgentEvent::AssistantText(_) | AgentEvent::ToolCall(_) => {
                self.base = Phase::Running;
                self.completed = false;
                if matches!(event, AgentEvent::ToolCall(_)) {
                    Activity::Tool
                } else {
                    Activity::Answer
                }
            }
            AgentEvent::Finished(finished) => {
                self.background = finished.background;
                self.completed = true;
                self.base = if self.background > 0 { Phase::Background } else { Phase::Finishing };
                Activity::TurnFinished
            }
            AgentEvent::BackgroundChanged(count) => {
                self.background = *count;
                if matches!(self.base, Phase::Background | Phase::Finishing) {
                    self.base = if *count > 0 { Phase::Background } else { Phase::Finishing };
                }
                Activity::BackgroundChanged
            }
            AgentEvent::Failed(_) | AgentEvent::BackgroundAbandoned(_) => {
                self.base = Phase::Failed;
                self.completed = false;
                if matches!(event, AgentEvent::Failed(_)) {
                    Activity::Failed
                } else {
                    Activity::BackgroundExpired
                }
            }
            AgentEvent::Quota(_) => return,
        };
        self.activity = Some((activity, now));
    }
    pub fn wait_started(&mut self, kind: WaitKind, now: Instant) {
        if !self.accepts_updates() {
            return;
        }
        let activity = match kind {
            WaitKind::Approval => {
                self.approvals = self.approvals.saturating_add(1);
                Activity::ApprovalOpened
            }
            WaitKind::Answer => {
                self.questions = self.questions.saturating_add(1);
                Activity::QuestionOpened
            }
        };
        self.activity = Some((activity, now));
    }
    pub fn wait_ended(&mut self, kind: WaitKind, now: Instant) {
        if !self.accepts_updates() {
            return;
        }
        let activity = match kind {
            WaitKind::Approval => {
                self.approvals = self.approvals.saturating_sub(1);
                Activity::ApprovalClosed
            }
            WaitKind::Answer => {
                self.questions = self.questions.saturating_sub(1);
                Activity::QuestionClosed
            }
        };
        self.activity = Some((activity, now));
    }
    pub fn stopping(&mut self, now: Instant) {
        if self.accepts_updates() {
            self.base = Phase::Stopping;
            self.approvals = 0;
            self.questions = 0;
            self.activity = Some((Activity::StopRequested, now));
        }
    }
    pub fn finish(&mut self, cancelled: bool, now: Instant) {
        if self.started.is_none() || self.ended.is_some() {
            return;
        }
        self.ended = Some(now);
        self.approvals = 0;
        self.questions = 0;
        self.base = if cancelled || self.base == Phase::Stopping {
            self.activity = Some((Activity::Stopped, now));
            Phase::Stopped
        } else if self.base == Phase::Failed {
            Phase::Failed
        } else if self.completed && self.background == 0 {
            Phase::Completed
        } else {
            self.activity = Some((Activity::Unconfirmed, now));
            Phase::Unknown
        };
    }
    #[must_use]
    pub fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.started.map(|started| self.ended.unwrap_or(now).saturating_duration_since(started))
    }
    #[must_use]
    pub fn describe(&self, now: Instant) -> String {
        let mut text = self.phase().label().to_owned();
        if let Some(duration) = self.elapsed(now) {
            let _ = write!(text, "\nДлительность запуска: {} с", duration.as_secs());
        }
        if let Some((activity, at)) = self.activity {
            let _ = write!(
                text,
                "\n{} · {} с назад",
                activity.label(),
                now.saturating_duration_since(at).as_secs()
            );
        }
        if self.approvals > 0 || self.questions > 0 {
            let _ = write!(text, "\nРазрешения: {}; вопросы: {}", self.approvals, self.questions);
        }
        if self.background > 0 {
            let _ = write!(text, "\nФоновых задач по последнему событию: {}", self.background);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finished, SessionId, Usage};

    fn finished(background: usize) -> AgentEvent {
        AgentEvent::Finished(Finished {
            session: SessionId::parse("s").unwrap(),
            usage: Usage::Codex { tokens: None },
            background,
        })
    }

    #[test]
    fn progress_failure_stays_visible_until_an_explicit_new_turn() {
        let now = Instant::now();
        let mut progress = TaskProgress::new(now);
        progress.observe(&AgentEvent::Failed("failure".to_owned()), now);
        progress.observe(&AgentEvent::AssistantText("late output".to_owned()), now);
        progress.observe(&finished(0), now);
        assert_eq!(progress.phase(), Phase::Failed);
        progress.observe(&AgentEvent::TurnStarted, now);
        assert_eq!(progress.phase(), Phase::Running);
    }

    #[test]
    fn progress_next_turn_resumes_execution_before_output_and_requires_its_own_result() {
        let now = Instant::now();
        for background in [0, 1] {
            let mut progress = TaskProgress::new(now);
            progress.observe(&finished(background), now);
            progress.observe(&AgentEvent::TurnStarted, now);
            assert_eq!(progress.phase(), Phase::Running);
            progress.finish(false, now);
            assert_eq!(progress.phase(), Phase::Unknown);
        }
    }

    #[test]
    fn progress_tracks_waits_and_freezes_completed_duration() {
        let now = Instant::now();
        let mut progress = TaskProgress::new(now);
        assert_eq!(progress.phase(), Phase::Preparing);
        progress.executing(now + Duration::from_secs(1));
        progress.wait_started(WaitKind::Approval, now + Duration::from_secs(2));
        assert_eq!(progress.phase(), Phase::AwaitingApproval);
        progress.wait_ended(WaitKind::Approval, now + Duration::from_secs(4));
        assert_eq!(progress.phase(), Phase::Running);
        progress.observe(&finished(0), now + Duration::from_secs(6));
        assert_eq!(progress.phase(), Phase::Finishing);
        progress.finish(false, now + Duration::from_secs(7));
        assert_eq!(progress.phase(), Phase::Completed);
        assert_eq!(progress.elapsed(now + Duration::from_secs(90)), Some(Duration::from_secs(7)));
        assert!(progress.describe(now + Duration::from_secs(9)).contains("3 с назад"));
    }

    #[test]
    fn progress_counts_concurrent_waits_and_returns_to_background() {
        let now = Instant::now();
        let mut progress = TaskProgress::new(now);
        progress.observe(&finished(2), now);
        progress.wait_started(WaitKind::Answer, now);
        progress.wait_started(WaitKind::Approval, now);
        progress.wait_started(WaitKind::Approval, now);
        progress.wait_ended(WaitKind::Approval, now);
        assert_eq!(progress.phase(), Phase::AwaitingApproval);
        assert!(progress.describe(now).contains("Разрешения: 1; вопросы: 1"));
        progress.wait_ended(WaitKind::Approval, now);
        assert_eq!(progress.phase(), Phase::AwaitingAnswer);
        progress.wait_ended(WaitKind::Answer, now);
        assert_eq!(progress.phase(), Phase::Background);
        progress.observe(&AgentEvent::BackgroundChanged(0), now);
        assert_eq!(progress.phase(), Phase::Finishing);
        progress.finish(false, now);
        assert_eq!(progress.phase(), Phase::Completed);
    }

    #[test]
    fn progress_stop_clears_waiting_and_rejects_late_updates() {
        let now = Instant::now();
        let mut progress = TaskProgress::new(now);
        progress.wait_started(WaitKind::Answer, now);
        progress.stopping(now);
        assert_eq!(progress.phase(), Phase::Stopping);
        progress.observe(&finished(0), now);
        progress.finish(true, now + Duration::from_secs(3));
        let stopped = progress.clone();
        progress.wait_ended(WaitKind::Answer, now + Duration::from_secs(8));
        progress.observe(&AgentEvent::Failed("late".to_owned()), now);
        progress.executing(now);
        progress.finish(false, now);
        assert_eq!(progress, stopped);
        assert_eq!(progress.phase(), Phase::Stopped);
    }

    #[test]
    fn progress_distinguishes_failure_from_unconfirmed_completion() {
        let now = Instant::now();
        let mut progress = TaskProgress::new(now);
        progress.finish(false, now);
        assert_eq!(progress.phase(), Phase::Unknown);
        assert!(progress.describe(now).contains("без подтверждённого результата"));
        assert!(TaskProgress::default().describe(now).contains("Нет данных"));
        let mut failed = TaskProgress::new(now);
        failed.wait_started(WaitKind::Approval, now);
        failed.observe(&AgentEvent::Failed("private error text".to_owned()), now);
        failed.finish(false, now);
        assert_eq!(failed.phase(), Phase::Failed);
        assert!(!failed.describe(now).contains("private"));
    }

    #[test]
    fn progress_background_timeout_is_failure_and_unconfirmed_background_is_unknown() {
        let now = Instant::now();
        let mut progress = TaskProgress::new(now);
        progress.observe(&finished(1), now);
        let mut unknown = progress.clone();
        unknown.finish(false, now);
        assert_eq!(unknown.phase(), Phase::Unknown);
        progress.observe(&AgentEvent::BackgroundAbandoned(vec!["private command".to_owned()]), now);
        progress.finish(false, now);
        assert_eq!(progress.phase(), Phase::Failed);
        assert!(progress.describe(now).contains("Истекло время фоновых задач"));
        assert!(!progress.describe(now).contains("private"));
    }

    #[test]
    fn progress_observes_activity_without_retaining_the_answer_or_claiming_new_success() {
        let now = Instant::now();
        let mut progress = TaskProgress::new(now);
        progress.observe(&finished(0), now);
        progress
            .observe(&AgentEvent::AssistantText("secret".to_owned()), now + Duration::from_secs(1));
        assert_eq!(progress.phase(), Phase::Running);
        assert!(progress.describe(now + Duration::from_secs(4)).contains("3 с назад"));
        assert!(!progress.describe(now).contains("secret"));
        progress.finish(false, now);
        assert_eq!(progress.phase(), Phase::Unknown);
    }
}
