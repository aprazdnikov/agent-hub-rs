//! In-memory desktop task fixtures shared by the inbox and project workspace.
mod chat;
mod inbox;
mod manage;
mod review;
#[cfg(test)]
mod tests;
mod workspace;

use crate::widgets::Wait;
use eframe::egui;
use kylig_ui::{Badge, Severity};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum Project {
    #[default]
    Hub,
    Docs,
    Sandbox,
}

impl Project {
    const ALL: [Self; 3] = [Self::Hub, Self::Docs, Self::Sandbox];

    fn name(self) -> &'static str {
        match self {
            Self::Hub => "agent-hub",
            Self::Docs => "docs-site",
            Self::Sandbox => "sandbox",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Question,
    Permission,
    Review,
    Draft,
    Failed,
    Stopped,
    Planned,
    Accepted,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Question => "Вопрос",
            Self::Permission => "Разрешение",
            Self::Review => "На проверке",
            Self::Draft => "Черновик",
            Self::Failed => "Ошибка запуска",
            Self::Stopped => "Остановлена",
            Self::Planned => "Запланирована",
            Self::Accepted => "Принята",
        }
    }

    fn request(self) -> Option<&'static str> {
        match self {
            Self::Question => Some("Какие задачи включить в отчёт?"),
            Self::Permission => Some("Разрешить запуск проверки ссылок?"),
            _ => None,
        }
    }

    fn severity(self) -> Severity {
        match self {
            Self::Permission | Self::Stopped => Severity::Warning,
            Self::Failed => Severity::Error,
            Self::Accepted => Severity::Success,
            Self::Planned | Self::Question | Self::Draft | Self::Review => Severity::Info,
        }
    }

    fn waiting(self) -> bool {
        matches!(self, Self::Question | Self::Permission)
    }

    fn attention(self) -> bool {
        self.waiting() || matches!(self, Self::Review | Self::Failed)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Speaker {
    User,
    Agent(&'static str),
}

struct Message {
    speaker: Speaker,
    text: String,
}

struct Attempt {
    agent: &'static str,
    step: String,
    outcome: String,
}

struct Task {
    id: usize,
    project: Project,
    name: String,
    prompt: String,
    agent: &'static str,
    topic: Option<&'static str>,
    stage: Stage,
    chat: Vec<Message>,
    composer: String,
    attempts: Vec<Attempt>,
    ops: manage::Controls,
    review_attempt: Option<usize>,
}

impl Task {
    fn seed(
        id: usize,
        project: Project,
        agent: &'static str,
        stage: Stage,
        name: &str,
        prompt: &str,
    ) -> Self {
        let mut task = Self {
            id,
            project,
            name: name.to_owned(),
            prompt: prompt.to_owned(),
            agent,
            topic: None,
            stage,
            chat: Vec::new(),
            composer: String::new(),
            attempts: Vec::new(),
            ops: manage::Controls::new(id, agent),
            review_attempt: None,
        };
        if stage != Stage::Draft {
            let outcome = match stage {
                Stage::Review | Stage::Accepted => task.summary(),
                Stage::Failed => "Соединение прервалось до начала сборки.",
                Stage::Question | Stage::Permission => "Агент ждёт решения.",
                Stage::Planned | Stage::Stopped | Stage::Draft => "Запуск остановлен.",
            };
            task.attempts.push(Attempt {
                agent,
                step: prompt.to_owned(),
                outcome: outcome.to_owned(),
            });
            if let Some(question) = stage.request() {
                task.message(Speaker::Agent(agent), question);
            } else if stage == Stage::Review {
                task.message(Speaker::Agent(agent), outcome);
            }
        }
        task
    }

    fn attention(&self) -> bool {
        self.stage.attention() && !self.ops.archived && self.ops.queue.is_none()
    }

    fn summary(&self) -> &'static str {
        match self.id {
            42 => "В отчёте указаны номер задачи, исполнитель и состояние очереди.",
            39 => "Проверка ссылок завершена. Внутренние страницы руководства доступны.",
            37 => {
                "Поиск теперь находит задачи по названию без учёта регистра. Добавлены проверки для пустого запроса и кириллицы."
            }
            _ => {
                "Агент завершил демонстрационную попытку. Поручение и сообщения сохранены в чате задачи."
            }
        }
    }

    fn latest_step(&self) -> &str {
        self.chat
            .iter()
            .rev()
            .find(|m| m.speaker == Speaker::User)
            .map_or(self.prompt.as_str(), |m| &m.text)
    }

    fn message(&mut self, speaker: Speaker, text: &str) {
        self.chat.push(Message { speaker, text: text.to_owned() });
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Rules {
    enabled: bool,
    text: String,
    workflow: &'static str,
    implementer: &'static str,
    reviewer: &'static str,
    command: String,
    git: bool,
    remote: String,
    base: String,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            enabled: true,
            text: "Сохраняй существующие изменения. Перед завершением запусти тесты проекта."
                .into(),
            workflow: "Наследовать общие роли",
            implementer: "Codex",
            reviewer: "Claude Code",
            command: "cargo test".into(),
            git: false,
            remote: "origin".into(),
            base: "main".into(),
        }
    }
}

#[derive(Default)]
struct ProjectRules {
    saved: Rules,
    draft: Rules,
}

#[derive(Default)]
struct NewTask {
    name: String,
    prompt: String,
    agent: Option<&'static str>,
    topic: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
enum Page {
    #[default]
    Chat,
    Review,
    New,
    Rules,
    Context,
}

enum Effect {
    Run(usize),
    Decision { id: usize, reply: String, proceed: bool },
    SaveRules(Project),
    Accept(usize),
    SaveDraft(usize),
    Manage(usize, manage::Change),
}

struct Pending {
    effect: Effect,
    return_to_inbox: bool,
}

#[derive(Default)]
struct Inbox {
    project: Option<Project>,
    answer: String,
    permission_details: bool,
}

pub(crate) struct Tasks {
    entries: Vec<Task>,
    next_id: usize,
    project: Project,
    selected: Option<usize>,
    inbox: Inbox,
    query: String,
    page: Page,
    rules_back: Page,
    review_tab: usize,
    line_note: Option<String>,
    from_inbox: bool,
    compact_projects: bool,
    archive: bool,
    deleting: Option<usize>,
    rules: BTreeMap<Project, ProjectRules>,
    new: NewTask,
    wait: Wait,
    pending: Option<Pending>,
    notice: Option<String>,
}

impl Default for Tasks {
    fn default() -> Self {
        Self {
            next_id: 43,
            entries: vec![
                Task::seed(
                    42,
                    Project::Hub,
                    "Claude Code",
                    Stage::Question,
                    "Отчёт по очереди",
                    "Собери отчёт о задачах проекта и покажи его в чате.",
                ),
                Task::seed(
                    39,
                    Project::Docs,
                    "Codex",
                    Stage::Permission,
                    "Проверка ссылок",
                    "Проверь внутренние ссылки в руководстве по проектам.",
                ),
                Task::seed(
                    37,
                    Project::Hub,
                    "Codex",
                    Stage::Review,
                    "Поиск по задачам",
                    "Сделай поиск по названию задачи нечувствительным к регистру.",
                ),
                Task::seed(
                    28,
                    Project::Docs,
                    "Claude Code",
                    Stage::Failed,
                    "Собрать руководство",
                    "Собери документацию и проверь оглавление.",
                ),
            ],
            project: Project::Hub,
            selected: Some(37),
            inbox: Inbox::default(),
            query: String::new(),
            page: Page::Chat,
            rules_back: Page::Chat,
            review_tab: 0,
            line_note: None,
            from_inbox: false,
            compact_projects: false,
            archive: false,
            deleting: None,
            rules: BTreeMap::new(),
            new: NewTask::default(),
            wait: Wait::default(),
            pending: None,
            notice: None,
        }
    }
}

impl Tasks {
    pub(crate) fn attention_count(&self) -> usize {
        self.entries.iter().filter(|task| task.attention()).count()
    }

    fn entry(&self, id: usize) -> Option<&Task> {
        self.entries.iter().find(|task| task.id == id)
    }

    fn entry_mut(&mut self, id: usize) -> Option<&mut Task> {
        self.entries.iter_mut().find(|task| task.id == id)
    }

    fn open(&mut self, id: usize, page: Page, from_inbox: bool) {
        let Some((project, archived)) =
            self.entry(id).map(|task| (task.project, task.ops.archived))
        else {
            return;
        };
        self.archive = archived;
        self.selected = Some(id);
        self.project = project;
        self.page = page;
        self.deleting = None;
        self.review_tab = 0;
        self.line_note = None;
        self.compact_projects = false;
        self.from_inbox = from_inbox;
    }

    fn start(&mut self, ui: &egui::Ui, label: &str, effect: Effect, return_to_inbox: bool) {
        self.notice = None;
        self.wait.start(ui, label);
        self.pending = Some(Pending { effect, return_to_inbox });
    }

    fn feedback(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.wait.busy() {
            self.poll_queue(ui);
        }
        let mut return_to_inbox = false;
        if self.wait.draw(ui)
            && let Some(pending) = self.pending.take()
        {
            self.notice = Some(self.finish(pending.effect));
            ui.ctx().request_repaint();
            return_to_inbox = pending.return_to_inbox;
            if return_to_inbox {
                self.from_inbox = false;
            }
        }
        if self.wait.busy() {
            self.offer_stop(ui);
        }
        if let Some(notice) = &self.notice {
            ui.horizontal_wrapped(|ui| {
                ui.add(Badge::dot().severity(Severity::Success));
                ui.add(egui::Label::new(notice).wrap());
            });
            ui.add_space(6.0);
        }
        return_to_inbox
    }

    fn offer_stop(&mut self, ui: &mut egui::Ui) {
        let Some(pending) = &self.pending else { return };
        let (id, running, label) = match &pending.effect {
            Effect::Run(id) if self.entry(*id).is_some_and(|t| t.topic.is_none()) => {
                (*id, true, "Остановить запуск")
            }
            Effect::Manage(id, manage::Change::Prepare(_)) => {
                (*id, false, "Остановить подготовку Git")
            }
            _ => return,
        };
        if crate::widgets::action(ui, label) {
            self.wait = Wait::default();
            self.pending = None;
            if running && let Some(task) = self.entry_mut(id) {
                task.stage = Stage::Stopped;
                let outcome = "Пользователь остановил запуск.";
                let step = task.latest_step().to_owned();
                task.attempts.push(Attempt {
                    agent: task.agent,
                    step,
                    outcome: outcome.to_owned(),
                });
                task.message(Speaker::Agent(task.agent), outcome);
            }
            self.notice = Some(if running {
                format!("Запуск задачи #{id} остановлен.")
            } else {
                format!("Подготовка Git для задачи #{id} остановлена.")
            });
        }
    }

    fn finish(&mut self, effect: Effect) -> String {
        match effect {
            Effect::Run(id) => {
                if let Some(task) = self.entry_mut(id) {
                    let step = task.latest_step().to_owned();
                    task.attempts.push(Attempt {
                        agent: task.agent,
                        step,
                        outcome: task.summary().to_owned(),
                    });
                    task.stage = Stage::Review;
                    task.message(Speaker::Agent(task.agent), task.summary());
                }
                format!("Результат задачи #{id} готов к проверке.")
            }
            Effect::Decision { id, reply, proceed } => {
                if let Some(task) = self.entry_mut(id) {
                    task.message(Speaker::User, &reply);
                    task.stage = if proceed { Stage::Review } else { Stage::Stopped };
                    let outcome = if proceed {
                        task.summary()
                    } else {
                        "Пользователь отклонил запрос агента."
                    };
                    if let Some(attempt) = task.attempts.last_mut() {
                        outcome.clone_into(&mut attempt.outcome);
                    }
                    if proceed {
                        task.message(Speaker::Agent(task.agent), task.summary());
                    }
                }
                if proceed {
                    format!("Ответ передан. Результат задачи #{id} готов к проверке.")
                } else {
                    format!("Запрос отклонён. Задача #{id} остановлена.")
                }
            }
            Effect::SaveRules(project) => {
                let rules = self.rules.entry(project).or_default();
                rules.saved.clone_from(&rules.draft);
                format!("Правила проекта {} сохранены.", project.name())
            }
            Effect::Accept(id) => {
                if let Some(task) = self.entry_mut(id) {
                    task.stage = Stage::Accepted;
                }
                format!("Результат задачи #{id} принят.")
            }
            Effect::Manage(id, change) => self.apply_change(id, change),
            Effect::SaveDraft(id) => format!("Черновик задачи #{id} сохранён."),
        }
    }
}
