use super::{Effect, NewTask, Page, Project, Stage, Task, Tasks};
use crate::widgets::{
    action, agent_choice, card, primary, remaining_height, title, workspace_panes,
};
use eframe::egui;
use kylig_ui::{
    Badge, Checkbox, IconButton, SearchField, Select, SelectItem, SourceItem, SourceList,
    SourceSection, TextArea, TextField,
};

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum Selection {
    Project(Project),
    Task(usize),
}

impl Tasks {
    pub(crate) fn project_name(&self) -> &'static str {
        self.project.name()
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) -> bool {
        if self.feedback(ui) {
            return true;
        }
        let mut back = false;
        ui.add_enabled_ui(!self.wait.busy(), |ui| {
            if self.from_inbox && ui.link("‹  Во входящие").clicked() {
                self.from_inbox = false;
                back = true;
            }
            let compact = ui.available_width() < 770.0;
            if compact {
                ui.horizontal_wrapped(|ui| {
                    let caption = if self.compact_projects {
                        "Закрыть список"
                    } else {
                        "Проекты и задачи"
                    };
                    if ui.link(kylig_icons::label(ui, kylig_icons::FOLDERS, caption)).clicked() {
                        self.compact_projects = !self.compact_projects;
                    }
                    if !self.compact_projects
                        && matches!(self.page, Page::Chat | Page::Review | Page::Context)
                        && let Some(task) = self.selected.and_then(|id| self.entry(id))
                    {
                        ui.weak(format!("{} / #{}", task.project.name(), task.id));
                    }
                });
                if self.compact_projects {
                    self.sidebar(ui);
                } else {
                    self.body(ui, true);
                }
            } else {
                workspace_panes(ui, "task-panes", |ui, sidebar| {
                    if sidebar {
                        self.sidebar(ui);
                    } else {
                        egui::ScrollArea::vertical()
                            .id_salt(("task-body", self.selected, self.page))
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.body(ui, false));
                    }
                });
            }
        });
        back
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Проекты");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(IconButton::new(kylig_icons::PLUS, "Новая задача")).clicked() {
                    self.begin_new();
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(!self.archive, "Текущие").clicked() {
                self.switch_archive(false);
            }
            if ui.selectable_label(self.archive, "Архив").clicked() {
                self.switch_archive(true);
            }
        });
        SearchField::new(&mut self.query)
            .id_salt("tasks.search")
            .placeholder("Найти задачу или проект")
            .width(ui.available_width())
            .show(ui);
        let query = self.query.trim().to_lowercase();
        let mut roots = Vec::new();
        for project in Project::ALL {
            let include_project = project.name().contains(&query);
            let tasks: Vec<_> = self
                .entries
                .iter()
                .filter(|t| {
                    t.project == project
                        && t.ops.archived == self.archive
                        && (include_project
                            || t.name.to_lowercase().contains(&query)
                            || format!("#{}", t.id).contains(&query))
                })
                .map(|t| {
                    SourceItem::new(Selection::Task(t.id), &t.name).icon(kylig_icons::CHAT_TEXT)
                })
                .collect();
            if include_project || !tasks.is_empty() {
                let count =
                    self.entries.iter().filter(|t| t.project == project && t.attention()).count();
                let mut root = SourceItem::new(Selection::Project(project), project.name())
                    .icon(kylig_icons::FOLDER)
                    .children(tasks)
                    .open(project == self.project || !query.is_empty());
                if count > 0 {
                    root = root.badge(count);
                }
                roots.push(root);
            }
        }
        if roots.is_empty() {
            ui.weak("Совпадений нет. Измените запрос.");
            return;
        }
        let sections = [SourceSection::new("").items(roots)];
        let mut selection = self.selected.map_or(Selection::Project(self.project), Selection::Task);
        let height = remaining_height(ui).max(80.0);
        let result = ui
            .allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                SourceList::new(&mut selection, &sections)
                    .id_salt(("tasks.tree", &query))
                    .show(ui)
                    .selected
            })
            .inner;
        match result {
            Some(Selection::Task(id)) => self.open(id, Page::Chat, false),
            Some(Selection::Project(p)) => self.select_project(p),
            None => {}
        }
    }

    pub(super) fn select_project(&mut self, p: Project) {
        self.project = p;
        self.selected = self
            .entries
            .iter()
            .find(|t| t.project == p && t.ops.archived == self.archive)
            .map(|t| t.id);
        self.page = Page::Chat;
        self.compact_projects = false;
        self.from_inbox = false;
    }

    fn body(&mut self, ui: &mut egui::Ui, compact: bool) {
        match self.page {
            Page::New => self.new_task(ui),
            Page::Rules => self.project_rules(ui),
            Page::Chat | Page::Review | Page::Context => {
                if let Some(id) = self.selected {
                    self.task(ui, id, compact);
                } else {
                    title(ui, self.project.name(), "В этом проекте пока нет задач.");
                    if primary(ui, "Создать первую задачу") {
                        self.page = Page::New;
                    }
                    if action(ui, "Правила проекта") {
                        self.open_rules();
                    }
                }
            }
        }
    }

    fn task(&mut self, ui: &mut egui::Ui, id: usize, compact: bool) {
        let Some(task) = self.entry(id) else { return };
        let stage = task.stage;
        if !compact {
            ui.weak(format!("{}   /   #{}", task.project.name(), id));
        }
        ui.heading(&task.name);
        ui.horizontal_wrapped(|ui| {
            ui.add(Badge::dot().severity(stage.severity()));
            ui.label(if task.ops.archived {
                "В архиве"
            } else if task.ops.queue.is_some() {
                "В очереди"
            } else {
                stage.label()
            });
            ui.weak(task.agent);
            ui.weak(task.topic.unwrap_or("Локально"));
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(self.page == Page::Chat, "Чат").clicked() {
                self.page = Page::Chat;
            }
            if !matches!(stage, Stage::Draft | Stage::Question | Stage::Permission)
                && ui.selectable_label(self.page == Page::Review, "Результат").clicked()
            {
                self.page = Page::Review;
            }
            let context = if compact {
                "Параметры"
            } else {
                "Параметры и контекст"
            };
            if ui.selectable_label(self.page == Page::Context, context).clicked() {
                self.page = Page::Context;
            }
            if ui.link(if compact { "Правила" } else { "Правила проекта" }).clicked()
            {
                self.open_rules();
            }
        });
        ui.separator();
        match self.page {
            Page::Chat => self.chat(ui, id),
            Page::Review => self.review(ui, id),
            Page::Context => self.context(ui, id),
            Page::New | Page::Rules => {}
        }
    }

    fn new_task(&mut self, ui: &mut egui::Ui) {
        title(ui, "Новая задача", "Выберите исполнителя и опишите ожидаемый результат.");
        card(ui, |ui| {
            ui.add(
                Select::new(&mut self.project, &Project::ALL.map(|p| SelectItem::new(p, p.name())))
                    .label("Проект")
                    .id_salt("new.project")
                    .width(ui.available_width().min(320.0)),
            );
            mode_and_agent(
                ui,
                self.project,
                &mut self.new.topic,
                self.new.agent.get_or_insert("Codex"),
            );
            ui.add(
                TextArea::new(&mut self.new.prompt)
                    .label("Что поручить агенту")
                    .placeholder("Опишите задачу и условия готовности")
                    .rows(3)
                    .id_salt("new.prompt")
                    .width(ui.available_width()),
            );
            ui.add(
                TextField::new(&mut self.new.name)
                    .label("Название (необязательно)")
                    .placeholder("Первая строка поручения")
                    .id_salt("new.name")
                    .width(ui.available_width()),
            );
            let dirty = self.rules.get(&self.project).is_some_and(|r| r.saved != r.draft);
            let valid = !self.new.prompt.trim().is_empty()
                && self.new.prompt.len() <= 16384
                && self.new.name.len() <= 256;
            if dirty {
                ui.weak("Сохраните правила проекта перед запуском.");
            }
            if self.new.prompt.len() > 16384 || self.new.name.len() > 256 {
                ui.weak("Сократите поручение до 16 КиБ и название до 256 байт.");
            }
            let git = self.rules.get(&self.project).is_some_and(|r| r.saved.git);
            if git {
                ui.weak("Сохраните черновик и подготовьте ветку Git в параметрах задачи.");
            }
            let choice = ui
                .horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled_ui(valid && !dirty && !git, |ui| primary(ui, "Начать задачу"))
                        .inner
                    {
                        return Some(true);
                    }
                    if ui.add_enabled_ui(valid, |ui| action(ui, "Сохранить черновик")).inner
                    {
                        return Some(false);
                    }
                    if ui.link("Правила проекта").clicked() {
                        self.open_rules();
                    }
                    None
                })
                .inner;
            if let Some(start) = choice {
                self.create(ui, start);
            }
        });
    }

    fn create(&mut self, ui: &egui::Ui, start: bool) {
        let NewTask { name, prompt, agent, topic } = std::mem::take(&mut self.new);
        let id = self.next_id;
        self.next_id += 1;
        let name = if name.trim().is_empty() {
            prompt.trim().lines().next().unwrap_or_default().chars().take(70).collect::<String>()
        } else {
            name
        };
        let mut task = Task::seed(
            id,
            self.project,
            agent.unwrap_or("Codex"),
            Stage::Draft,
            &name,
            prompt.trim(),
        );
        task.topic = topic;
        self.entries.insert(0, task);
        self.query.clear();
        self.archive = false;
        self.open(id, Page::Chat, false);
        if start {
            self.start(ui, "Агент выполняет поручение…", Effect::Run(id), false);
        } else {
            self.start(ui, "Сохраняем черновик…", Effect::SaveDraft(id), false);
        }
    }

    fn open_rules(&mut self) {
        self.rules_back = self.page;
        self.page = Page::Rules;
    }

    fn project_rules(&mut self, ui: &mut egui::Ui) {
        if ui.link("‹  Назад").clicked() {
            self.page = self.rules_back;
        }
        title(ui, "Правила проекта", self.project.name());
        let rules = self.rules.entry(self.project).or_default();
        let save = card(ui, |ui| {
            ui.add(Checkbox::new(&mut rules.draft.enabled, "Добавлять правила к каждому запуску"));
            ui.add(
                TextArea::new(&mut rules.draft.text)
                    .label("Инструкции проекта")
                    .rows(4)
                    .width(ui.available_width()),
            );
            let modes = ["Выключено", "Наследовать общие роли", "Свой выбор"]
                .map(|m| SelectItem::new(m, m));
            ui.add(
                Select::new(&mut rules.draft.workflow, &modes)
                    .label("Исправить и проверить автоматически")
                    .width(ui.available_width().min(320.0)),
            );
            if rules.draft.workflow == "Свой выбор" {
                for (label, agent) in [
                    ("Исполнитель", &mut rules.draft.implementer),
                    ("Ревьюер", &mut rules.draft.reviewer),
                ] {
                    agent_choice(ui, label, agent);
                }
            }
            if rules.draft.workflow != "Выключено" {
                ui.add(
                    TextField::new(&mut rules.draft.command)
                        .label("Доверенная команда проверки")
                        .width(ui.available_width()),
                );
            }
            ui.add(Checkbox::new(
                &mut rules.draft.git,
                "Требовать подготовку Git перед новой задачей",
            ));
            if rules.draft.git {
                ui.add(
                    TextField::new(&mut rules.draft.remote)
                        .label("Remote")
                        .width(ui.available_width()),
                );
                ui.add(
                    TextField::new(&mut rules.draft.base)
                        .label("Базовая ветка")
                        .width(ui.available_width()),
                );
            }
            let dirty = rules.draft != rules.saved;
            if dirty {
                ui.weak("Есть несохранённые изменения.");
            }
            ui.add_enabled_ui(dirty, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let save = primary(ui, "Сохранить правила");
                    if action(ui, "Отменить изменения") {
                        rules.draft.clone_from(&rules.saved);
                    }
                    save
                })
                .inner
            })
            .inner
        });
        if save {
            self.start(ui, "Сохраняем правила проекта…", Effect::SaveRules(self.project), false);
        }
    }

    fn context(&mut self, ui: &mut egui::Ui, id: usize) {
        let rules = self.rules.entry(self.project).or_default().saved.clone();
        let Some(task) = self.entry_mut(id) else { return };
        card(ui, |ui| {
            ui.strong("Параметры задачи");
            ui.weak(format!("Назначен {}", task.agent));
            ui.add_enabled_ui(
                !task.ops.archived
                    && task.ops.queue.is_none()
                    && !task.stage.waiting()
                    && task.stage != Stage::Accepted,
                |ui| mode_and_agent(ui, task.project, &mut task.topic, &mut task.ops.agent),
            );
        });
        let assign = task.ops.agent != task.agent
            && !task.ops.archived
            && task.ops.queue.is_none()
            && !task.stage.waiting()
            && task.stage != Stage::Accepted;
        if assign && primary(ui, "Назначить") {
            let agent = task.ops.agent;
            self.start(
                ui,
                "Передаём задачу другому агенту…",
                Effect::Manage(id, super::manage::Change::Assign(agent)),
                false,
            );
            return;
        }
        self.manage(ui, id, &rules);
        let Some(task) = self.entry(id) else { return };
        card(ui, |ui| {
            ui.strong("Контекст следующего запуска");
            ui.weak(format!("Проект {}   /   Исполнитель {}", task.project.name(), task.agent));
            ui.label(&task.prompt);
            if !task.composer.trim().is_empty() {
                ui.label(&task.composer);
            }
            ui.separator();
            ui.strong("Правила проекта");
            if let Some(topic) = task.topic {
                ui.weak(format!("Тема Telegram — {topic}"));
            }
            if rules.enabled && !rules.text.is_empty() {
                ui.label(rules.text);
            } else {
                ui.weak("Правила выключены или пусты.");
            }
            ui.weak(format!("Автоматическая проверка — {}", rules.workflow));
            if rules.workflow != "Выключено" && !rules.command.trim().is_empty() {
                ui.monospace(rules.command);
            }
            if rules.workflow == "Свой выбор" {
                ui.weak(format!(
                    "Исполнитель {}   /   Ревьюер {}",
                    rules.implementer, rules.reviewer
                ));
            }
        });
    }
}

fn mode_and_agent(
    ui: &mut egui::Ui,
    project: Project,
    topic: &mut Option<&'static str>,
    agent: &mut &'static str,
) {
    let (available, backend): (&[&'static str], _) = match project {
        Project::Hub => (&["Проверка обновления", "Подготовка релиза"], "Codex"),
        Project::Docs => (&["Руководство пользователя"], "Claude Code"),
        Project::Sandbox => (&["Общая"], "Qwen Code"),
    };
    ui.horizontal_wrapped(|ui| {
        if ui.selectable_label(topic.is_none(), "Локально").clicked() {
            *topic = None;
        }
        if ui.selectable_label(topic.is_some(), "Тема Telegram").clicked() {
            *topic = topic.or(available.first().copied());
        }
    });
    if topic.is_some() {
        if !available.iter().any(|t| Some(*t) == *topic) {
            *topic = available.first().copied();
        }
        let topics =
            available.iter().map(|name| SelectItem::new(Some(*name), *name)).collect::<Vec<_>>();
        ui.add(
            Select::new(topic, &topics)
                .label("Тема проекта")
                .id_salt("task.topic")
                .width(ui.available_width().min(320.0)),
        );
        *agent = backend;
        ui.weak(format!("Исполнитель темы — {backend}"));
    } else {
        agent_choice(ui, "Исполнитель", agent);
    }
}
