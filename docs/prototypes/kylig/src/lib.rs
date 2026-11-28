//! Interactive, in-memory review prototype of the Agent Hub desktop interface.

mod memory;
#[path = "../../../../crates/hub-app/src/gui/theme/palette.rs"]
mod palette;
mod settings;
mod tasks;
mod topics;
mod widgets;

use eframe::egui;
use kylig_ui::{Button, Chip, IconButton};
use widgets::{action, card, primary, title};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Page {
    #[default]
    Inbox,
    Tasks,
    Topics,
    Memory,
    Settings,
    Map,
    Setup,
    Tray,
}

#[derive(Default)]
pub struct Prototype {
    page: Page,
    tasks: tasks::Tasks,
    settings: settings::Settings,
    memory: memory::Memory,
    topics: topics::Topics,
    tray: Tray,
}

impl Prototype {
    #[must_use]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        palette::configure(&cc.egui_ctx);
        Self::default()
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_width() < 570.0;
        ui.horizontal_wrapped(|ui| {
            let accent = ui.visuals().hyperlink_color;
            ui.label(egui::RichText::new("Agent Hub").strong().size(20.0).color(accent));
            if !compact {
                ui.weak("Интерактивный макет");
                ui.separator();
            }
            let map_label = if compact { "Карта" } else { "Карта переходов" };
            if ui.add(Button::new(map_label).ghost()).clicked() {
                self.page = Page::Map;
            }
            let (next, icon, label) = if ui.visuals().dark_mode {
                (egui::Theme::Light, kylig_icons::SUN, "Светлая тема")
            } else {
                (egui::Theme::Dark, kylig_icons::MOON, "Тёмная тема")
            };
            let toggle = if compact {
                ui.add(IconButton::new(icon, label))
            } else {
                ui.add(Button::new(label).ghost())
            };
            if toggle.clicked() {
                ui.ctx().set_theme(next);
            }
        });
        ui.add_space(6.0);
        let inbox = format!("Входящие · {}", self.tasks.attention_count());
        ui.horizontal_wrapped(|ui| {
            if compact {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            }
            for (page, label, icon) in [
                (Page::Inbox, inbox.as_str(), kylig_icons::TRAY),
                (Page::Tasks, "Задачи", kylig_icons::CHECK_SQUARE),
                (Page::Topics, "Темы Telegram", kylig_icons::CHATS),
                (Page::Memory, "Память", kylig_icons::DATABASE),
                (Page::Settings, "Настройки", kylig_icons::GEAR),
            ] {
                if Chip::new(label)
                    .icon(icon)
                    .color(ui.visuals().hyperlink_color)
                    .selected(self.page == page)
                    .clickable(true)
                    .show(ui)
                    .response
                    .clicked()
                {
                    if self.page == Page::Tasks && page == Page::Memory {
                        self.memory.open_project(self.tasks.project_name());
                    }
                    self.page = page;
                }
            }
        });
    }

    fn map(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Карта переходов",
            "Выберите сценарий. Все изменения действуют только в этом сеансе макета.",
        );
        let routes = [
            (Page::Setup, "Первый запуск", "Выбор режима › настройка подключения › рабочее окно"),
            (
                Page::Inbox,
                "Разобрать входящие",
                "Вопрос или разрешение › ответ › проверка результата",
            ),
            (
                Page::Tasks,
                "Поставить задачу",
                "Проект › новая задача › запуск › чат › результат и проверки",
            ),
            (
                Page::Topics,
                "Темы Telegram",
                "Поиск › параметры темы › синхронизация или сброс сессии",
            ),
            (
                Page::Memory,
                "Память проекта",
                "Поиск › запись и ревизии › контекст поручения или обслуживание",
            ),
            (
                Page::Settings,
                "Настроить приложение",
                "Раздел › редактирование › сохранение › диагностика или журнал",
            ),
            (
                Page::Tray,
                "Меню в трее",
                "Статус бота › управление › вернуться в окно или завершить сеанс",
            ),
        ];
        let columns = if ui.available_width() >= 800.0 { 2 } else { 1 };
        for row in routes.chunks(columns) {
            ui.columns(columns, |slots| {
                for (slot, &(page, label, path)) in slots.iter_mut().zip(row) {
                    card(slot, |ui| {
                        if action(ui, label) {
                            if page == Page::Tasks {
                                self.tasks.begin_new();
                            }
                            self.page = page;
                        }
                        ui.weak(path);
                        if page == Page::Memory && ui.link("Политика очистки").clicked()
                        {
                            self.memory.open_tools();
                            self.page = Page::Memory;
                        }
                        if page == Page::Settings {
                            ui.horizontal_wrapped(|ui| {
                                if ui.link("Агенты").clicked() {
                                    self.settings.open_agents();
                                    self.page = Page::Settings;
                                }
                                if ui.link("Действия").clicked() {
                                    self.settings.open_actions();
                                    self.page = Page::Settings;
                                }
                                if ui.link("Журнал").clicked() {
                                    self.settings.open_log();
                                    self.page = Page::Settings;
                                }
                            });
                        }
                    });
                }
            });
        }
        ui.add_space(8.0);
        if action(ui, "Начать макет заново") {
            *self = Self::default();
        }
    }

    fn content(&mut self, ui: &mut egui::Ui) {
        match self.page {
            Page::Map => self.map(ui),
            Page::Inbox => {
                if self.tasks.inbox(ui) {
                    self.page = Page::Tasks;
                }
            }
            Page::Tasks => {
                if self.tasks.show(ui) {
                    self.page = Page::Inbox;
                }
            }
            Page::Topics => self.topics.show(ui),
            Page::Memory => self.memory.show(ui),
            Page::Settings => self.settings.show(ui),
            Page::Setup => {
                if self.settings.setup(ui) {
                    self.page = Page::Inbox;
                }
            }
            Page::Tray => {
                if let Some(page) = self.tray.show(ui, &mut self.settings) {
                    self.page = page;
                }
            }
        }
    }

    fn render(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_width() < 650.0;
        let margin = if compact { 16 } else { 32 };
        let body_inset = if compact { 14 } else { 20 };
        let canvas = ui.visuals().extreme_bg_color;
        let surface = ui.visuals().panel_fill;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(canvas)).show(ui, |ui| {
            egui::Frame::new()
                .fill(surface)
                .inner_margin(egui::Margin::symmetric(margin, if compact { 8 } else { 12 }))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.header(ui);
                });
            ui.separator();
            egui::Frame::new().inner_margin(egui::Margin::symmetric(margin, body_inset)).show(
                ui,
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(self.page)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if !matches!(self.page, Page::Tasks | Page::Settings) {
                                ui.set_max_width(ui.available_width().min(1280.0));
                            }
                            self.content(ui);
                        });
                },
            );
        });
    }
}

impl eframe::App for Prototype {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

#[derive(Default)]
struct Tray {
    closed: bool,
    confirm: bool,
}

impl Tray {
    fn show(&mut self, ui: &mut egui::Ui, settings: &mut settings::Settings) -> Option<Page> {
        title(ui, "Меню в трее", "Предпросмотр системного меню Agent Hub");
        if self.closed {
            ui.label("Сеанс завершён.");
            if primary(ui, "Открыть макет снова") {
                *self = Self::default();
                return Some(Page::Inbox);
            }
            return None;
        }
        card(ui, |ui| {
            if action(ui, "Открыть окно") {
                return Some(Page::Inbox);
            }
            settings.tray_bot(ui);
            if settings.tray_update(ui) {
                return Some(Page::Settings);
            }
            ui.separator();
            if self.confirm {
                ui.label("Завершить сеанс макета?");
                if action(ui, "Завершить") {
                    self.closed = true;
                }
                if action(ui, "Остаться") {
                    self.confirm = false;
                }
            } else if action(ui, "Выход") {
                self.confirm = true;
            }
            None
        })
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn start(
    canvas: eframe::web_sys::HtmlCanvasElement,
    route: String,
) -> Result<(), wasm_bindgen::JsValue> {
    eframe::WebRunner::new()
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(move |cc| {
                let mut app = Prototype::new(cc);
                app.page = match route.as_str() {
                    "#map" => Page::Map,
                    "#setup" => Page::Setup,
                    "#tasks" => Page::Tasks,
                    "#topics" => Page::Topics,
                    "#memory" => Page::Memory,
                    "#memory-tools" => {
                        app.memory.open_tools();
                        Page::Memory
                    }
                    "#settings" => Page::Settings,
                    "#agents" => {
                        app.settings.open_agents();
                        Page::Settings
                    }
                    "#actions" => {
                        app.settings.open_actions();
                        Page::Settings
                    }
                    "#logs" => {
                        app.settings.open_log();
                        Page::Settings
                    }
                    "#tray" => Page::Tray,
                    _ => Page::Inbox,
                };
                Ok(Box::new(app))
            }),
        )
        .await
}
