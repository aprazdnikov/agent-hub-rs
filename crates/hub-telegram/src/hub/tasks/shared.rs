//! Binding a transport to an already-running durable task never starts an agent.
use super::{Agents, Hub, Request, Snapshot};
use crate::execution::{Answer, Kind, Live};
use crate::inbound::{Inbound, Press};
use crate::messenger::Target;
use hub_core::{
    domain::{MessageId, TopicKey, TopicSession, UserId},
    settings::ExecutionSettings,
};
use std::{collections::HashMap, path::Path, sync::Arc};
use uuid::Uuid;

impl Followers {
    pub(in crate::hub) fn invalidate(&mut self, key: TopicKey) {
        self.entries.remove(&key);
    }
}
#[derive(Default)]
pub(in crate::hub) struct Followers {
    entries: HashMap<TopicKey, Follow>,
}
struct Follow {
    id: u64,
    user: UserId,
    session: TopicSession,
    last: Option<Live>,
    card: Option<MessageId>,
    feedback: Option<u64>,
    failures: u8,
    retry_at: Option<tokio::time::Instant>,
    delivery: Option<tokio::task::JoinHandle<Option<MessageId>>>,
}
impl Drop for Follow {
    fn drop(&mut self) {
        if let Some(job) = &self.delivery {
            job.abort();
        }
    }
}
pub const PREFIX: &str = "te:";
pub const CARD: &str = "🧭 Общая задача #";
fn result_summary(run: &hub_core::tasks::Run, id: u64) -> String {
    let report = &run.workflow_report;
    if report.is_empty() {
        return run.result.clone();
    }
    let excerpt = |value: &str| {
        let mut chars = value.chars();
        let mut text: String = chars.by_ref().take(450).collect();
        if chars.next().is_some() {
            text.push('…');
        }
        text
    };
    let mut lines = vec![excerpt(&run.result)];
    for (i, check) in report.checks.iter().enumerate() {
        lines.push(format!(
            "Проверка {}: {}",
            i + 1,
            if check.passed { "прошла" } else { "не прошла" }
        ));
    }
    if !report.review.is_empty() {
        lines.push(format!("Ревью: {}", excerpt(&report.review)));
    }
    lines.push(format!("Полный отчёт и вывод проверок: /task attempt {id} {}", run.id));
    lines.join("\n")
}
impl<A: Agents> Hub<A> {
    pub(in crate::hub) fn shared_task(
        &mut self,
        project: &Path,
        request: Request,
        session: Option<&TopicSession>,
    ) -> Result<Snapshot, String> {
        if self.shutdown.is_some() {
            return Err("Бот останавливается. Повторите запрос.".into());
        }
        if !self.running.is_empty()
            || !self.actions.is_idle()
            || !self.handoffs.is_idle()
            || !self.snapshotting.is_empty()
            || self.git_job.is_some()
        {
            return Err(
                "Дождитесь завершения текущих сессий и операций перед запуском общей задачи."
                    .into(),
            );
        }
        let canonical = crate::paths::workspace_root(project).map_err(|e| e.to_string())?;
        if canonical.as_path() != project
            || !crate::paths::inside(&self.root().map_err(|e| e.to_string())?, &canonical)
        {
            return Err("Каталог задачи недоступен или вне корня проектов.".into());
        }
        let execution = self.execution.as_ref().ok_or("Общий исполнитель недоступен")?;
        let book = self.tasks.read()?;
        let rules_revision = book.rules(project).revision;
        let (id, request) = match request {
            Request::Start { id, revision, next } => {
                (id, Request::StartChecked { id, revision, next, rules_revision })
            }
            Request::PrepareGit { id, revision, branch } => {
                (id, Request::PrepareGitChecked { id, revision, branch, rules_revision })
            }
            request @ (Request::RunQueued { id, .. }
            | Request::StartChecked { id, .. }
            | Request::PrepareGitChecked { id, .. }) => (id, request),
            _ => return Err("Операция не поддерживается общим исполнителем".into()),
        };
        let task = book.get(id)?;
        if task.project != project {
            return Err("Задача относится к другому проекту".into());
        }
        let backend =
            hub_core::domain::BackendKind::parse(&task.backend).ok_or("Неизвестный агент")?;
        let mut settings = (*self.settings.borrow()).as_ref().clone();
        if let Some(session) = session {
            if session.backend != backend {
                return Err("Выберите тему с назначенным агентом задачи".into());
            }
            session.preferences.resolve(backend, &settings)?.apply(&mut settings);
        }
        execution.dispatch(project, request, Arc::new(ExecutionSettings::from(&settings)))?;
        super::manage::manage(&self.tasks, project, backend, Request::Get(id))
    }
    pub(in crate::hub) fn pump_queue(&mut self) {
        if self.shutdown.is_some()
            || !self.running.is_empty()
            || !self.actions.is_idle()
            || !self.handoffs.is_idle()
            || !self.snapshotting.is_empty()
            || self.git_job.is_some()
        {
            return;
        }
        let Some(pending) = self.tasks.pending(true) else {
            return;
        };
        if !self.execution.as_ref().is_some_and(|r| r.capacity(pending.isolated)) {
            return;
        }
        let origin = pending.origin.map(|origin| {
            (
                TopicKey {
                    chat: hub_core::domain::ChatId(origin.chat),
                    thread: hub_core::domain::ThreadId(origin.thread),
                },
                origin.user.map(UserId),
            )
        });
        // One interactive card per topic. Start the next queued job here only after the
        // preceding job's terminal card has been delivered.
        if let Some((key, Some(_))) = origin
            && let Some(follow) = self.follows.entries.get(&key)
            && (follow.delivery.is_some()
                || self
                    .execution
                    .as_ref()
                    .and_then(|execution| {
                        execution.snapshot_for(follow.session.cwd.as_path(), follow.id)
                    })
                    .is_some_and(|live| live.active || follow.last.as_ref() != Some(&live)))
        {
            return;
        }
        let result = (|| {
            let session = if let Some((key, user)) = origin {
                let config = &self.settings.borrow().telegram;
                if key.chat != config.chat
                    || user.is_some_and(|user| {
                        !crate::telegram::allowed(config, Some(key.chat), Some(user))
                    })
                {
                    return Err("Доступ пользователя изменился".to_owned());
                }
                if user.is_some()
                    && !self.follows.entries.contains_key(&key)
                    && self.follows.entries.len() >= 32
                {
                    return Err("Достигнут лимит 32 подписок. Освободите тему и повторно поставьте задачу в очередь.".into());
                }
                let session = self
                    .topics
                    .get(&key)
                    .filter(|s| s.cwd.as_path() == pending.project)
                    .ok_or("Тема очереди больше не связана с проектом")?;
                Some(session.clone())
            } else {
                None
            };
            self.shared_task(
                &pending.project,
                Request::RunQueued { id: pending.id, revision: pending.revision },
                session.as_ref(),
            )?;
            if let Some((key, Some(user))) = origin {
                self.shared_command(key, Some(user), &format!("follow {}", pending.id));
            }
            Ok(())
        })();
        if let Err(error) = result
            && let Err(error) = self.tasks.pause_pending(&pending, &error)
        {
            tracing::warn!(%error,"cannot pause queued task");
        }
    }
    pub(in crate::hub) fn shared_stop(&self, key: TopicKey) -> bool {
        self.stop_matching(key, false)
    }
    pub(in crate::hub) fn stop_preflight(&self, key: TopicKey) -> bool {
        self.stop_matching(key, true)
    }
    fn stop_matching(&self, key: TopicKey, preflight: bool) -> bool {
        let Some(execution) = &self.execution else { return false };
        let Some(session) = self.topics.get(&key) else {
            return false;
        };
        let live = if let Some(follow) = self.follows.entries.get(&key) {
            execution.snapshot_for(session.cwd.as_path(), follow.id).filter(|live| live.active)
        } else {
            let mut jobs = execution.snapshots().into_iter().filter(|live| {
                live.active
                    && live.project == session.cwd.as_path()
                    && (!preflight || live.run.is_none())
            });
            let first = jobs.next();
            if jobs.next().is_some() {
                return false;
            }
            first
        };
        let Some(live) = live else {
            return false;
        };
        if self.topics.get(&key).is_some_and(|s| s.cwd.as_path() == live.project) {
            return if preflight {
                execution.stop_preflight(live.token).is_ok()
            } else {
                execution.stop(live.token).is_ok()
            };
        }
        false
    }
    fn follow_valid(&self, key: TopicKey, follow: &Follow) -> bool {
        self.shutdown.is_none()
            && crate::telegram::allowed(
                &self.settings.borrow().telegram,
                Some(key.chat),
                Some(follow.user),
            )
            && self.topics.get(&key) == Some(&follow.session)
            && self.root().is_ok_and(|root| crate::paths::inside(&root, &follow.session.cwd))
    }
    pub(in crate::hub) fn shared_command(
        &mut self,
        key: TopicKey,
        user: Option<UserId>,
        raw: &str,
    ) -> bool {
        let Some((command, tail)) = super::word(raw) else { return false };
        if command != "follow" && command != "unfollow" {
            return false;
        }
        let result = (|| {
            let user = user.ok_or("Неизвестный пользователь")?;
            if !crate::telegram::allowed(
                &self.settings.borrow().telegram,
                Some(key.chat),
                Some(user),
            ) {
                return Err("Доступ запрещён".into());
            }
            if command == "unfollow" {
                self.follows.entries.remove(&key);
                return Ok(
                    "Доставка общей задачи в эту тему отключена. Выполнение продолжается.".into()
                );
            }
            let id = tail.trim().parse::<u64>().map_err(|_| "Используйте /task follow ID")?;
            let session = self.topics.get(&key).ok_or("Выберите тему проекта")?.clone();
            let book = self.tasks.read()?;
            if book.get(id)?.project != session.cwd.as_path() {
                return Err("Задача относится к другому проекту".into());
            }
            if self.execution.is_none() {
                return Err("Общий исполнитель недоступен".into());
            }
            if !self.follows.entries.contains_key(&key) && self.follows.entries.len() >= 32 {
                return Err(
                    "Достигнут лимит 32 подписок. Отключите ненужную через /task unfollow.".into(),
                );
            }
            let follow = Follow {
                id,
                user,
                session,
                last: None,
                card: None,
                delivery: None,
                feedback: None,
                failures: 0,
                retry_at: None,
            };
            if !self.follow_valid(key, &follow) {
                return Err("Тема или каталог недоступны".into());
            }
            self.follows.entries.insert(key, follow);
            Ok(format!(
                "Задача #{id} связана с этой темой. Запуск не повторяется. /task get {id} — результат; /task resume — следующий шаг."
            ))
        })();
        self.say(Target::Topic(key), result.unwrap_or_else(|error: String| error));
        self.refresh_shared();
        true
    }
    pub(in crate::hub) fn refresh_shared(&mut self) {
        let invalid: Vec<_> = self
            .follows
            .entries
            .iter()
            .filter(|(key, follow)| !self.follow_valid(**key, follow))
            .map(|(key, _)| *key)
            .collect();
        for key in invalid {
            self.follows.entries.remove(&key);
        }
        let Some(execution) = self.execution.clone() else { return };
        for (key, follow) in &mut self.follows.entries {
            if follow.delivery.as_ref().is_some_and(tokio::task::JoinHandle::is_finished)
                && let Some(job) = follow.delivery.take()
            {
                use futures::FutureExt;
                follow.card = job.now_or_never().and_then(Result::ok).flatten();
                if follow.card.is_none() {
                    follow.failures = follow.failures.saturating_add(1);
                    follow.retry_at = Some(
                        tokio::time::Instant::now()
                            + std::time::Duration::from_millis(
                                250 * (1_u64 << follow.failures.min(3)),
                            ),
                    );
                } else {
                    follow.failures = 0;
                    follow.retry_at = None;
                }
            }
            if follow.delivery.is_some() {
                continue;
            }
            let Some(live) = execution.snapshot_for(follow.session.cwd.as_path(), follow.id) else {
                continue;
            };
            if follow.last.as_ref() == Some(&live) {
                if follow.card.is_some()
                    || follow.failures >= 3
                    || follow.retry_at.is_some_and(|at| tokio::time::Instant::now() < at)
                {
                    continue;
                }
            } else {
                follow.failures = 0;
                follow.retry_at = None;
            }
            follow.feedback = None;
            let (result, review_revision) = if live.active {
                (None, None)
            } else {
                self.tasks
                    .read()
                    .ok()
                    .and_then(|book| {
                        let task = book.get(live.id).ok()?;
                        let run = task.runs.iter().find(|r| Some(r.id) == live.run)?;
                        let reviewable = !task.archived
                            && task.status == hub_core::tasks::TaskStatus::NeedsReview
                            && task.runs.last() == Some(run);
                        let result = result_summary(run, task.id);
                        Some((Some(result), reviewable.then_some(task.revision)))
                    })
                    .unwrap_or_default()
            };
            let delivery = super::shared_delivery::Delivery {
                key: *key,
                user: follow.user,
                previous: follow.card.take(),
                live: live.clone(),
                result,
                review_revision,
                execution: execution.clone(),
                settings: self.settings.clone(),
                sender: self.sender.clone(),
            };
            follow.delivery = Some(tokio::spawn(delivery.send()));
            follow.last = Some(live);
        }
    }
    pub(in crate::hub) fn shared_press(&mut self, pressed: Press) {
        self.refresh_shared();
        let result = self.shared_pressed(&pressed);
        let sender = self.sender.clone();
        tokio::spawn(async move {
            sender
                .answer(pressed.callback, Some(result.as_deref().unwrap_or_else(|error| error)))
                .await;
        });
        self.refresh_shared();
    }
    fn shared_pressed(&mut self, pressed: &Press) -> Result<String, String> {
        let key = TopicKey {
            chat: pressed.chat.ok_or("Нет темы")?,
            thread: pressed.thread.ok_or("Нет темы")?,
        };
        let follow = self
            .follows
            .entries
            .get(&key)
            .ok_or("Карточка устарела. Используйте /task follow ID")?;
        if pressed.user != follow.user
            || !self.follow_valid(key, follow)
            || !crate::telegram::allowed(
                &self.settings.borrow().telegram,
                pressed.chat,
                Some(pressed.user),
            )
            || follow.card.is_none()
            || pressed.message != follow.card
        {
            return Err("Карточка или доступ устарели".into());
        }
        let execution = self.execution.as_ref().ok_or("Исполнитель недоступен")?;
        let live = execution
            .snapshot_for(follow.session.cwd.as_path(), follow.id)
            .ok_or("Запуск завершён")?;
        if !live.active {
            let (action, revision) = pressed
                .data
                .strip_prefix(PREFIX)
                .and_then(|value| value.split_once(':'))
                .and_then(|(action, rev)| Some((action, rev.parse::<u64>().ok()?)))
                .ok_or("Карточка устарела")?;
            return self.task_result_action(key, &live, action, revision, pressed.user);
        }
        if !super::shared_delivery::offered(&live, &pressed.data) {
            return Err("Кнопка больше недоступна".into());
        }
        let parts: Vec<_> = pressed.data.split(':').collect();
        match parts.as_slice() {
            ["te", "s", token] if Uuid::parse_str(token).ok() == Some(live.token) => {
                execution.stop(live.token)?;
                Ok("Остановка запрошена".into())
            }
            ["te", action, id] => {
                let request = live.interaction.as_ref().ok_or("Запрос уже закрыт")?;
                if Uuid::parse_str(id).ok() != Some(request.id) {
                    return Err("Запрос уже закрыт".into());
                }
                let answer = match *action {
                    "a" => Answer::Allow,
                    "d" => Answer::Deny,
                    _ => return Err("Неизвестное действие".into()),
                };
                execution.respond(live.token, request.id, answer)?;
                Ok("Ответ принят".into())
            }
            ["te", "o", id, index] => {
                let request = live.interaction.as_ref().ok_or("Запрос уже закрыт")?;
                if Uuid::parse_str(id).ok() != Some(request.id) {
                    return Err("Запрос уже закрыт".into());
                }
                let Kind::Questions(questions) = &request.kind else {
                    return Err("Это не вопрос".into());
                };
                let question =
                    questions.first().filter(|_| questions.len() == 1).ok_or("Ответьте текстом")?;
                let option = question
                    .options()
                    .get(index.parse::<usize>().map_err(|_| "Неверный вариант")?)
                    .ok_or("Неверный вариант")?;
                execution.respond(
                    live.token,
                    request.id,
                    Answer::Text(vec![option.label.clone()]),
                )?;
                Ok("Ответ принят".into())
            }
            _ => Err("Карточка устарела".into()),
        }
    }
    fn task_result_action(
        &mut self,
        key: TopicKey,
        live: &Live,
        action: &str,
        revision: u64,
        user: UserId,
    ) -> Result<String, String> {
        use hub_core::tasks::TaskStatus;
        let book = self.tasks.read()?;
        let task = book.get(live.id)?;
        let run = task.runs.last().filter(|r| Some(r.id) == live.run).ok_or("Попытка устарела")?;
        if task.project != live.project
            || task.revision != revision
            || task.archived
            || task.status != TaskStatus::NeedsReview
        {
            return Err("Карточка результата устарела; откройте /task get ID".into());
        }
        match action {
            "accept" => {
                self.task_snapshot(
                    key,
                    Request::Status { id: live.id, revision, status: TaskStatus::Accepted },
                )?;
                self.say(Target::Topic(key), format!("Задача #{} принята.", live.id));
                Ok("Принято".into())
            }
            "diff" => {
                if !self.task_deliveries.available(key) {
                    return Err("Дождитесь отправки текущей карточки".into());
                }
                let patch = if run.diff.is_empty() {
                    "Для этой попытки diff не сохранён"
                } else {
                    &run.diff
                };
                self.task_deliveries.send(
                    key,
                    Some(user),
                    patch.to_owned(),
                    false,
                    self.sender.clone(),
                    self.settings.clone(),
                );
                Ok("Отправляю изменения".into())
            }
            "feedback" => {
                if let Some(follow) = self.follows.entries.get_mut(&key) {
                    follow.feedback = Some(revision);
                }
                self.say(Target::Topic(key), format!("Ответьте на карточку результата задачи #{} текстом замечания. Это запустит следующую попытку.", live.id));
                Ok("Ожидается замечание".into())
            }
            _ => Err("Кнопка устарела".into()),
        }
    }
    pub(in crate::hub) fn shared_reply(&mut self, inbound: &Inbound, key: TopicKey) -> bool {
        self.refresh_shared();
        let matched = self
            .follows
            .entries
            .get(&key)
            .is_some_and(|f| f.card.is_some() && f.card == inbound.reply_to);
        if !inbound.task_reply && !matched {
            return false;
        }
        let result = (|| {
            let follow = self
                .follows
                .entries
                .get(&key)
                .ok_or("Вопрос устарел; используйте /task follow ID")?;
            if !matched
                || inbound.user != Some(follow.user)
                || !self.follow_valid(key, follow)
                || !crate::telegram::allowed(
                    &self.settings.borrow().telegram,
                    Some(key.chat),
                    inbound.user,
                )
            {
                return Err("Вопрос или доступ устарели".into());
            }
            if let Some(revision) = follow.feedback {
                let crate::inbound::Content::Text(next) = &inbound.content else {
                    return Err("Ответьте текстом".into());
                };
                let id = follow.id;
                self.task_snapshot(key, Request::Start { id, revision, next: Some(next.clone()) })?;
                if let Some(follow) = self.follows.entries.get_mut(&key) {
                    follow.feedback = None;
                }
                return Ok("Замечание передано, запущена следующая попытка".into());
            }
            let execution = self.execution.as_ref().ok_or("Исполнитель недоступен")?;
            let live = execution
                .snapshot_for(follow.session.cwd.as_path(), follow.id)
                .ok_or("Запуск завершён")?;
            let request = live.interaction.as_ref().ok_or("Вопрос уже закрыт")?;
            if follow.last.as_ref().and_then(|l| l.interaction.as_ref()).map(|r| r.id)
                != Some(request.id)
            {
                return Err("Вопрос уже изменился".into());
            }
            let Kind::Questions(questions) = &request.kind else {
                return Err("Разрешение выбирается кнопкой".into());
            };
            if !super::shared_delivery::complete(&live) {
                return Err("Полный вопрос доступен только в GUI. Ответьте там.".into());
            }
            let crate::inbound::Content::Text(text) = &inbound.content else {
                return Err("Ответьте текстом".into());
            };
            let answers = if questions.len() == 1 {
                vec![text.clone()]
            } else {
                text.lines().map(str::to_owned).collect()
            };
            execution.respond(live.token, request.id, Answer::Text(answers))?;
            Ok("Ответ принят".to_owned())
        })();
        self.say(Target::Topic(key), result.unwrap_or_else(|error: String| error));
        self.refresh_shared();
        true
    }
}
