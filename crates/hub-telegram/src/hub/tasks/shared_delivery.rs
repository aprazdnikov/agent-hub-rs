use crate::{
    execution::{Control, Kind, Live},
    messenger::{Outgoing, Target},
    sender::Sender,
};
use hub_core::{
    domain::{MessageId, Selection, TopicKey, UserId},
    questions::Button,
    settings::Settings,
};
use std::{fmt::Write, sync::Arc};
use tokio::sync::watch;

pub(super) struct Delivery {
    pub key: TopicKey,
    pub user: UserId,
    pub previous: Option<MessageId>,
    pub live: Live,
    pub result: Option<String>,
    pub review_revision: Option<u64>,
    pub execution: Arc<dyn Control>,
    pub settings: watch::Receiver<Arc<Settings>>,
    pub sender: Sender,
}
fn context(live: &Live) -> String {
    let mut text = format!("{}{}\n{}\n", super::shared::CARD, live.id, live.status);
    if let Some(request) = &live.interaction {
        match &request.kind {
            Kind::Permission(tool) => {
                let _ = write!(text, "\nРазрешить действие: {}\n{}", tool.tool, tool.summary);
            }
            Kind::Questions(questions) => {
                for (index, question) in questions.iter().enumerate() {
                    let _ = write!(text, "\n{}. {}\n", index + 1, question.text());
                    for option in question.options() {
                        let _ = writeln!(text, "• {} — {}", option.label, option.description);
                    }
                }
                text.push_str("\nОтветьте на карточку обычным текстом. Для нескольких вопросов — один ответ на строку, в указанном порядке.");
            }
        }
    }
    text
}
fn keyboard(live: &Live, complete: bool) -> Vec<Vec<Button>> {
    let mut rows = Vec::new();
    if live.active {
        rows.push(vec![Button {
            text: "Остановить задачу".into(),
            data: format!("te:s:{}", live.token.simple()),
        }]);
    }
    if let Some(request) = &live.interaction {
        let id = request.id.simple();
        match &request.kind {
            Kind::Permission(_) if complete => rows.push(vec![Button {
                text: "Разрешить один раз".into(),
                data: format!("te:a:{id}"),
            }]),
            Kind::Questions(questions) if complete && questions.len() == 1 => {
                if let Some(question) =
                    questions.first().filter(|q| q.selection() == Selection::Single)
                {
                    for (index, option) in question.options().iter().take(10).enumerate() {
                        rows.push(vec![Button {
                            text: option.label.chars().take(60).collect(),
                            data: format!("te:o:{id}:{index}"),
                        }]);
                    }
                }
            }
            Kind::Permission(_) | Kind::Questions(_) => {}
        }
        rows.push(vec![Button {
            text: "Отклонить / не отвечать".into(),
            data: format!("te:d:{id}"),
        }]);
    }
    rows
}
fn result_buttons(revision: u64) -> Vec<Vec<Button>> {
    vec![
        vec![
            Button { text: "Принять".into(), data: format!("te:accept:{revision}") },
            Button { text: "Открыть diff".into(), data: format!("te:diff:{revision}") },
        ],
        vec![Button {
            text: "Вернуть с замечанием".into(),
            data: format!("te:feedback:{revision}"),
        }],
    ]
}
pub(super) fn complete(live: &Live) -> bool {
    context(live).len() <= 32768
}
pub(super) fn offered(live: &Live, data: &str) -> bool {
    keyboard(live, complete(live)).iter().flatten().any(|button| button.data == data)
}
impl Delivery {
    fn valid(&self) -> bool {
        let root = crate::paths::workspace_root(&self.settings.borrow().workspace_root);
        let project = crate::paths::workspace_root(&self.live.project);
        root.as_ref().ok().zip(project.as_ref().ok()).is_some_and(|(root, project)| {
            project.as_path() == self.live.project && crate::paths::inside(root, project)
        }) && crate::telegram::allowed(
            &self.settings.borrow().telegram,
            Some(self.key.chat),
            Some(self.user),
        ) && self.execution.snapshot_for(&self.live.project, self.live.id).is_some_and(|current| {
            current.token == self.live.token
                && current.active == self.live.active
                && current.interaction.as_ref().map(|i| i.id)
                    == self.live.interaction.as_ref().map(|i| i.id)
        })
    }
    pub async fn send(self) -> Option<MessageId> {
        if !self.valid() {
            return None;
        }
        if let Some(previous) = self.previous {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                self.sender.edit_keyboard(self.key.chat, previous, vec![]),
            )
            .await;
            if !self.valid() {
                return None;
            }
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                self.sender.edit(
                    self.key.chat,
                    previous,
                    format!(
                        "{}{}\nКарточка обновлена. Прежние кнопки больше не действуют.",
                        super::shared::CARD,
                        self.live.id
                    ),
                    crate::messenger::Format::Plain,
                ),
            )
            .await;
        }
        let mut text = context(&self.live);
        let complete = text.len() <= 32768;
        if !complete {
            text = format!(
                "{}{}\nЗапрос слишком большой для карточки. Прочитайте и подтвердите его в GUI. Здесь доступен только отказ или остановка.",
                super::shared::CARD,
                self.live.id
            );
        }
        if let Some(result) = &self.result {
            let _ = write!(text, "\nРезультат:\n{result}");
        }
        if !self.live.active && self.review_revision.is_none() {
            let _ = write!(
                text,
                "\nСледующий шаг: /task get {} для ревизии, затем /task resume ID РЕВИЗИЯ ТЕКСТ_ШАГА.",
                self.live.id
            );
        }
        let chunks = hub_core::render::split_message(
            &text,
            std::num::NonZeroUsize::MIN.saturating_add(3499),
        );
        let count = chunks.len();
        let mut message = None;
        for (index, chunk) in chunks.into_iter().enumerate() {
            if !self.valid() {
                return None;
            }
            let last = index + 1 == count;
            let mut outgoing = Outgoing::plain(if count > 1 {
                format!(
                    "{}{} · часть {}/{}\n{chunk}",
                    super::shared::CARD,
                    self.live.id,
                    index + 1,
                    count
                )
            } else {
                chunk
            });
            if last {
                outgoing = outgoing.with_keyboard(
                    self.review_revision
                        .map_or_else(|| keyboard(&self.live, complete), result_buttons),
                );
            }
            message = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                self.sender.messenger().send(Target::Topic(self.key), outgoing),
            )
            .await
            .ok()?
            .ok();
            message?;
        }
        message
    }
}
