//! Local Telegram topic fixtures and supervisor command previews.
mod data;

use crate::widgets::{Wait, action, card, choice_chips, dialog, primary, title};
use data::{Filter, MEMORY_MODES, Topic, samples};
use eframe::egui;
use kylig_ui::{Badge, InfoBar, ListItem, Severity, TextField};

#[derive(Clone, Copy)]
enum Operation {
    Sync,
    Stop(u16),
    Memory(u16, &'static str),
    Reset(u16),
    Delete(u16),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SyncReply {
    #[default]
    Online,
    Empty,
    Offline,
}

pub(crate) struct Topics {
    rows: Vec<Topic>,
    filter: Filter,
    selected: Option<u16>,
    filters_open: bool,
    reply: SyncReply,
    wait: Wait,
    pending: Option<Operation>,
    confirm: Option<Operation>,
    notice: Option<(Severity, String)>,
}

impl Default for Topics {
    fn default() -> Self {
        Self {
            rows: samples(),
            filter: Filter::default(),
            selected: None,
            filters_open: false,
            reply: SyncReply::default(),
            wait: Wait::default(),
            pending: None,
            confirm: None,
            notice: None,
        }
    }
}
impl Topics {
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        if self.wait.draw(ui)
            && let Some(op) = self.pending.take()
        {
            self.finish(op);
        }
        let selected = self.rows.iter().copied().find(|topic| Some(topic.id) == self.selected);
        if let Some(topic) = selected {
            if ui.link("‹ Все темы").clicked() {
                self.selected = None;
            }
            title(
                ui,
                topic.name,
                &format!("#{}  ·  {}  ·  {}", topic.id, topic.project, topic.backend),
            );
        } else {
            title(
                ui,
                "Темы Telegram",
                "Групповые обсуждения, привязанные проекты и агентские сессии.",
            );
        }
        if let Some((severity, message)) = &self.notice {
            let mut open = true;
            InfoBar::new(*severity, message).open(&mut open).show(ui);
            if !open {
                self.notice = None;
            }
        }
        ui.add_enabled_ui(!self.wait.busy(), |ui| {
            if let Some(topic) = selected {
                card(ui, |ui| {
                    self.status(ui, topic);
                    ui.separator();
                    self.details(ui, topic);
                });
                return;
            }
            self.controls(ui);
            let topics: Vec<_> =
                self.rows.iter().copied().filter(|t| self.filter.matches(*t)).collect();
            ui.add_space(6.0);
            if self.rows.is_empty() {
                card(ui, |ui| {
                    ui.strong("В группе пока нет тем");
                    ui.label("Темы появятся после первого сообщения боту в группе.");
                    if action(ui, "Загрузить демо-темы") {
                        self.reply = SyncReply::Online;
                        self.rows = samples();
                        self.filter = Filter::default();
                        self.notice = None;
                    }
                });
            } else if topics.is_empty() {
                ui.label("По этим условиям тем нет. Измените поиск или сбросьте фильтры.");
            } else {
                ui.weak(format!("Показано тем — {} из {}", topics.len(), self.rows.len()));
                for topic in topics {
                    ui.push_id(topic.id, |ui| self.row(ui, topic));
                }
            }
        });
        self.confirmation(ui);
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width().min(360.0);
        ui.add(
            TextField::new(&mut self.filter.query)
                .id_salt("topic-search")
                .placeholder("Найти тему по названию или каталогу")
                .clearable(true)
                .width(width),
        );
        ui.horizontal_wrapped(|ui| {
            if primary(ui, "Синхронизировать") {
                self.start(ui, Operation::Sync);
            }
            if action(ui, "Фильтры") {
                self.filters_open = !self.filters_open;
            }
            if self.filter != Filter::default() && action(ui, "Сбросить фильтры") {
                self.filter = Filter::default();
            }
        });
        if self.filters_open {
            self.filter.controls(ui);
        }
        ui.horizontal_wrapped(|ui| {
            ui.weak("Ответ при синхронизации");
            for (reply, label) in [
                (SyncReply::Online, "Темы доступны"),
                (SyncReply::Empty, "Пустая группа"),
                (SyncReply::Offline, "Нет связи"),
            ] {
                if kylig_ui::Chip::new(label)
                    .selected(self.reply == reply)
                    .clickable(true)
                    .show(ui)
                    .response
                    .clicked()
                {
                    self.reply = reply;
                }
            }
        });
    }

    fn row(&mut self, ui: &mut egui::Ui, topic: Topic) {
        card(ui, |ui| {
            let severity = if topic.running { Severity::Info } else { Severity::Success };
            let heading = ListItem::new(topic.name)
                .subtitle(format!("#{}  ·  {}  ·  {}", topic.id, topic.project, topic.backend))
                .leading(move |ui| {
                    ui.add(Badge::dot().severity(severity));
                })
                .chevron(true)
                .id_salt(topic.id)
                .show(ui);
            if heading.response.clicked() {
                self.selected = Some(topic.id);
            }
            self.status(ui, topic);
        });
    }

    fn status(&mut self, ui: &mut egui::Ui, topic: Topic) {
        ui.horizontal_wrapped(|ui| {
            ui.strong(topic.state());
            ui.weak(topic.phase);
            if topic.running && action(ui, "Остановить") {
                self.start(ui, Operation::Stop(topic.id));
            }
        });
    }

    fn details(&mut self, ui: &mut egui::Ui, topic: Topic) {
        ui.strong("Параметры и прогресс");
        ui.label(topic.event);
        if topic.running {
            ui.weak("Память текущего запуска включена");
        }
        ui.label(format!("Каталог проекта   {}", topic.path()));
        ui.label(format!("Модель   {}", topic.model));
        ui.label(format!("Сессия   {}", topic.session.unwrap_or("Ещё не создана")));
        let mut mode = topic.memory;
        ui.label("Память темы");
        choice_chips(ui, &mut mode, MEMORY_MODES.iter().copied());
        if mode != topic.memory {
            self.start(ui, Operation::Memory(topic.id, mode));
        }
        ui.horizontal_wrapped(|ui| {
            if action(ui, "Открыть в Telegram") {
                self.notice = Some((Severity::Info, format!("Адрес демо-темы  {}", topic.link())));
            }
            if action(ui, "Открыть папку") {
                self.notice =
                    Some((Severity::Info, format!("Каталог демо-проекта  {}", topic.path())));
            }
            if action(ui, "Копировать ссылку") {
                ui.ctx().copy_text(topic.link());
                self.notice = Some((Severity::Success, "Ссылка на демо-тему скопирована.".into()));
            }
            if let Some(id) = topic.session
                && action(ui, "Копировать ID сессии")
            {
                ui.ctx().copy_text(id.to_owned());
                self.notice = Some((Severity::Success, "Идентификатор сессии скопирован.".into()));
            }
        });
        if !topic.running {
            ui.horizontal_wrapped(|ui| {
                if topic.session.is_some() && action(ui, "Сбросить сессию…") {
                    self.confirm = Some(Operation::Reset(topic.id));
                }
                if topic.id > 1 && ui.add(kylig_ui::Button::new("Удалить тему…").danger()).clicked()
                {
                    self.confirm = Some(Operation::Delete(topic.id));
                }
            });
        }
    }

    fn confirmation(&mut self, ui: &egui::Ui) {
        let Some(op) = self.confirm else { return };
        let (id, heading, detail, label) = match op {
            Operation::Reset(id) => (
                id,
                "Сбросить сессию темы?",
                "Следующее поручение начнётся с новой агентской сессии.",
                "Сбросить сессию",
            ),
            Operation::Delete(id) => (
                id,
                "Удалить тему из Telegram?",
                "Тема и её сообщения исчезнут из демонстрационной группы.",
                "Удалить тему",
            ),
            Operation::Sync | Operation::Stop(_) | Operation::Memory(_, _) => return,
        };
        let Some(topic) = self.rows.iter().find(|row| row.id == id).copied() else { return };
        let mut open = true;
        dialog(ui, "demo-topic-confirm", &mut open, heading).show(ui.ctx(), |ui| {
            ui.strong(format!("{} · #{id}", topic.name));
            ui.label(detail);
            ui.horizontal_wrapped(|ui| {
                if action(ui, "Отмена") {
                    self.confirm = None;
                }
                if ui.add(kylig_ui::Button::new(label).danger()).clicked() {
                    self.confirm = None;
                    self.start(ui, op);
                }
            });
        });
        if !open {
            self.confirm = None;
        }
    }

    fn start(&mut self, ui: &egui::Ui, op: Operation) {
        let label = match op {
            Operation::Sync => "Синхронизируем темы…",
            Operation::Stop(_) => "Останавливаем запуск…",
            Operation::Memory(_, _) => "Обновляем параметры памяти…",
            Operation::Reset(_) => "Сбрасываем сессию…",
            Operation::Delete(_) => "Удаляем тему…",
        };
        self.pending = Some(op);
        self.notice = None;
        self.wait.start(ui, label);
    }

    fn finish(&mut self, op: Operation) {
        let mut severity = Severity::Success;
        let message = match op {
            Operation::Sync => match self.reply {
                SyncReply::Online => {
                    format!("Синхронизация завершена. Проверено тем — {}.", self.rows.len())
                }
                SyncReply::Empty => {
                    self.rows.clear();
                    self.selected = None;
                    "Группа синхронизирована. Список тем пуст.".into()
                }
                SyncReply::Offline => {
                    severity = Severity::Error;
                    "Не удалось связаться с Telegram. Выберите «Темы доступны» и повторите синхронизацию.".into()
                }
            },
            Operation::Delete(id) => {
                self.rows.retain(|topic| topic.id != id);
                self.selected = None;
                format!("Тема #{id} удалена из демо-группы.")
            }
            Operation::Stop(id) => {
                if let Some(topic) = self.rows.iter_mut().find(|topic| topic.id == id) {
                    topic.running = false;
                    topic.phase = "Остановлено";
                    topic.event = "Запуск остановлен пользователем.";
                }
                format!("Запуск в теме #{id} остановлен.")
            }
            Operation::Reset(id) => {
                if let Some(topic) = self.rows.iter_mut().find(|topic| topic.id == id) {
                    topic.session = None;
                    topic.phase = "Нет данных";
                    topic.event = "Агент готов к новому поручению.";
                }
                format!("Сессия темы #{id} сброшена.")
            }
            Operation::Memory(id, mode) => {
                if let Some(topic) = self.rows.iter_mut().find(|topic| topic.id == id) {
                    topic.memory = mode;
                }
                format!("Параметр памяти темы #{id} сохранён для следующего запуска.")
            }
        };
        self.notice = Some((severity, message));
    }
}
