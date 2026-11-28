use eframe::egui::{self, Color32};
use kylig_ui::{Button, Segmented, Select, SelectItem, Switch, TextField};

use super::{Draft, choice};
use crate::widgets::{AGENTS, Wait, action, card, dialog, primary};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Report {
    #[default]
    Idle,
    Ready,
    Offline,
    Missing,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Status,
    Log,
}

#[derive(Clone, Copy)]
pub(super) enum LogAction {
    UseChat,
    AddUser,
}

#[derive(Clone, Copy, PartialOrd, PartialEq)]
enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Self::Error => Color32::from_rgb(255, 153, 147),
            Self::Warn => Color32::from_rgb(242, 201, 118),
            Self::Info => Color32::from_rgb(132, 216, 186),
            Self::Debug => Color32::from_rgb(167, 181, 231),
        }
    }
}

const CONSOLE_BG: Color32 = Color32::from_rgb(18, 25, 25);
const CONSOLE_BAR: Color32 = Color32::from_rgb(34, 44, 43);
const CONSOLE_TEXT: Color32 = Color32::from_rgb(229, 238, 234);
const CONSOLE_MUTED: Color32 = Color32::from_rgb(159, 182, 174);

struct Entry {
    time: String,
    level: Level,
    text: String,
    rejected: bool,
}

impl Entry {
    fn new(seconds: u64, level: Level, text: &str) -> Self {
        let time =
            format!("{:02}:{:02}:{:02}", seconds / 3600 % 24, seconds / 60 % 60, seconds % 60);
        Self { time, level, text: text.into(), rejected: false }
    }

    fn matches(&self, level: Level, search: &str) -> bool {
        self.level <= level
            && (search.is_empty()
                || self.time.contains(search)
                || self.level.label().to_ascii_lowercase().contains(search)
                || self.text.to_lowercase().contains(search))
    }

    fn draw(&self, ui: &mut egui::Ui) {
        let mut line = egui::text::LayoutJob::default();
        let format = |color| egui::TextFormat::simple(egui::FontId::monospace(14.0), color);
        line.append(&self.time, 0.0, format(CONSOLE_MUTED));
        line.append(&format!("  {:<5}  ", self.level.label()), 0.0, format(self.level.color()));
        line.append(&self.text, 0.0, format(CONSOLE_TEXT));
        ui.add(egui::Label::new(line).wrap().selectable(true));
    }
}

#[cfg(target_arch = "wasm32")]
fn utc_seconds() -> u64 {
    let now = eframe::web_sys::js_sys::Date::new_0();
    u64::from(now.get_utc_hours() * 3600 + now.get_utc_minutes() * 60 + now.get_utc_seconds())
}

#[cfg(not(target_arch = "wasm32"))]
fn utc_seconds() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
        % 86_400
}

pub(super) struct Diagnostics {
    page: Page,
    result: Report,
    fixture: &'static str,
    pending: Option<Report>,
    check: Wait,
    bot: bool,
    bot_wait: Wait,
    level: Level,
    follow: bool,
    folder: bool,
    search: String,
    entries: Vec<Entry>,
}

impl Default for Diagnostics {
    fn default() -> Self {
        let now = utc_seconds();
        let entry = |age, level, text| Entry::new(now + 86_400 - age, level, text);
        let mut rejected =
            entry(3, Level::Warn, "Запрос отклонён • chat_id −1002301 • user_id 73012");
        rejected.rejected = true;
        Self {
            page: Page::Status,
            result: Report::Idle,
            fixture: "Штатная работа",
            pending: None,
            check: Wait::default(),
            bot: true,
            bot_wait: Wait::default(),
            level: Level::Info,
            follow: true,
            folder: false,
            search: String::new(),
            entries: vec![
                entry(95, Level::Info, "Запуск Agent Hub"),
                entry(94, Level::Debug, "Загружены профили проектов"),
                entry(93, Level::Info, "Рабочий каталог открыт: ~/Projects"),
                entry(92, Level::Info, "Профиль проекта agent-hub готов"),
                entry(91, Level::Debug, "Проверены пути к CLI агентов"),
                entry(90, Level::Info, "Хранилище памяти проекта открыто"),
                entry(89, Level::Info, "Бот запущен"),
                entry(87, Level::Info, "Получен список тем Telegram"),
                entry(76, Level::Error, "Предыдущая отправка задержана до восстановления связи"),
                entry(65, Level::Warn, "Ответ помещён в очередь повторной отправки"),
                entry(48, Level::Info, "Соединение с Telegram восстановлено"),
                entry(46, Level::Info, "Ответ доставлен в тему проекта"),
                entry(23, Level::Debug, "Обновлён список активных сессий"),
                rejected,
            ],
        }
    }
}

impl Diagnostics {
    pub(super) fn open_log(&mut self) {
        self.page = Page::Log;
    }

    pub(super) fn showing_log(&self) -> bool {
        self.page == Page::Log
    }

    pub(super) fn close_log(&mut self) {
        self.page = Page::Status;
    }

    pub(super) fn record(&mut self, text: &str) {
        self.entries.push(Entry::new(utc_seconds(), Level::Info, text));
    }

    pub(super) fn saved(&mut self, telegram_enabled: bool) {
        self.bot = telegram_enabled;
        self.result = Report::Idle;
        self.pending = None;
        self.check = Wait::default();
        self.bot_wait = Wait::default();
    }

    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        saved: &Draft,
        dirty: bool,
    ) -> Option<LogAction> {
        Segmented::new(&mut self.page, [(Page::Status, "Состояние"), (Page::Log, "Журнал")])
            .id_salt("diagnostic-pages")
            .show(ui);
        ui.add_space(8.0);
        match self.page {
            Page::Status => {
                self.status(ui, saved, dirty);
                None
            }
            Page::Log => self.log(ui),
        }
    }

    pub(super) fn running(&self) -> bool {
        self.bot
    }

    pub(super) fn bot(&mut self, ui: &mut egui::Ui, local: bool) {
        ui.strong("Telegram-бот");
        if self.bot_wait.draw(ui) {
            self.bot = !self.bot;
            self.record(if self.bot {
                "Бот запущен"
            } else {
                "Бот остановлен"
            });
        }
        if local {
            ui.weak("Локальный режим. Подключение Telegram выключено.");
        } else {
            let status = if self.bot {
                "Запущен • 2 активные сессии"
            } else {
                "Остановлен • 0 активных сессий"
            };
            ui.label(status);
            if !self.bot_wait.busy()
                && action(
                    ui,
                    if self.bot {
                        "Остановить бота"
                    } else {
                        "Запустить бота"
                    },
                )
            {
                self.bot_wait.start(
                    ui,
                    if self.bot {
                        "Останавливаем бота…"
                    } else {
                        "Подключаем бота…"
                    },
                );
            }
        }
    }

    fn status(&mut self, ui: &mut egui::Ui, saved: &Draft, dirty: bool) {
        card(ui, |ui| self.bot(ui, saved.telegram.local()));
        card(ui, |ui| self.report(ui, saved, dirty));
        card(ui, |ui| {
            ui.strong("Агенты");
            for name in AGENTS {
                let unavailable = self.result == Report::Missing && name == saved.agents.default;
                ui.horizontal_wrapped(|ui| {
                    ui.label(name);
                    ui.weak(if unavailable {
                        "CLI не найден"
                    } else {
                        "CLI доступен"
                    });
                });
            }
            ui.separator();
            ui.label(if self.result == Report::Offline {
                "Недоставленные ответы • 2 в очереди"
            } else {
                "Недоставленные ответы • очередь пуста"
            });
        });
    }

    fn report(&mut self, ui: &mut egui::Ui, saved: &Draft, dirty: bool) {
        ui.strong("Проверка сохранённых настроек");
        if dirty {
            ui.weak("Новые значения попадут в проверку после сохранения.");
        }
        if self.check.draw(ui)
            && let Some(result) = self.pending.take()
        {
            self.result = result;
            let (level, message) = match result {
                Report::Offline => (Level::Error, "Истекло время ожидания Telegram"),
                Report::Missing => (Level::Warn, "Исполняемый файл агента отсутствует"),
                Report::Ready | Report::Idle => (Level::Info, "Проверка настроек завершена"),
            };
            self.entries.push(Entry::new(utc_seconds(), level, message));
        }
        if self.check.busy() {
            return;
        }
        match self.result {
            Report::Idle => {
                ui.weak("Проверка ещё не запускалась.");
            }
            Report::Ready => {
                ui.label(if saved.telegram.local() {
                    "Telegram пропущен в локальном режиме."
                } else {
                    "Токен принят. Группа с темами доступна. Управление темами разрешено."
                });
                ui.label(format!(
                    "Рабочий каталог доступен. CLI агента {} найден.",
                    saved.agents.default
                ));
            }
            Report::Offline => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    "Сеть недоступна. Проверьте подключение и повторите проверку.",
                );
            }
            Report::Missing => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!(
                        "CLI агента {} не найден. Проверьте путь в настройках агента.",
                        saved.agents.default
                    ),
                );
            }
        }
        let outcomes: &[&str] = if saved.telegram.local() {
            &["Штатная работа", "Нет CLI"]
        } else {
            &["Штатная работа", "Нет сети", "Нет CLI"]
        };
        choice(ui, "Сценарий проверки", &mut self.fixture, outcomes);
        if primary(
            ui,
            if matches!(self.result, Report::Offline | Report::Missing) {
                "Повторить проверку"
            } else {
                "Проверить настройки"
            },
        ) {
            self.pending = Some(match self.fixture {
                "Нет сети" => Report::Offline,
                "Нет CLI" => Report::Missing,
                _ => Report::Ready,
            });
            self.fixture = "Штатная работа";
            self.check.start(ui, "Проверяем каталог, агента и подключение…");
        }
    }

    pub(super) fn log(&mut self, ui: &mut egui::Ui) -> Option<LogAction> {
        self.filters(ui);
        let search = self.search.trim().to_lowercase();
        let rows = self
            .entries
            .iter()
            .filter(|entry| entry.matches(self.level, &search))
            .collect::<Vec<_>>();
        let compact = ui.available_width() < 450.0;
        let resume = ui
            .horizontal_wrapped(|ui| {
                let changed =
                    ui.add(Switch::new(&mut self.follow).label("Автопрокрутка")).changed();
                if ui
                    .add_enabled(!rows.is_empty(), Button::new("Копировать"))
                    .on_hover_text("Скопировать показанные строки")
                    .clicked()
                {
                    let text = rows
                        .iter()
                        .map(|row| format!("{}  {:<5}  {}", row.time, row.level.label(), row.text))
                        .collect::<Vec<_>>()
                        .join("\n");
                    ui.ctx().copy_text(text);
                }
                let folder = if compact { "Папка" } else { "Папка журналов" };
                if ui.add(Button::new(folder)).on_hover_text("Папка журналов").clicked()
                {
                    self.folder = true;
                }
                changed && self.follow
            })
            .inner;
        let result = console(ui, &rows, self.entries.len(), &mut self.follow, resume);
        if self.folder {
            let close =
                dialog(ui, "log-folder", &mut self.folder, "Папка журналов").show(ui.ctx(), |ui| {
                    ui.monospace("~/.agent-hub/logs/");
                    ui.label("agent-hub.log");
                    action(ui, "Закрыть")
                });
            if close == Some(true) {
                self.folder = false;
            }
        }
        result
    }

    fn filters(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let level_width = (width * 0.32).clamp(100.0, 170.0);
        let search_width = width - level_width - ui.spacing().item_spacing.x;
        ui.horizontal_top(|ui| {
            ui.add(
                TextField::new(&mut self.search)
                    .id_salt("journal-search")
                    .label("Поиск в журнале")
                    .placeholder("Текст или ID")
                    .clearable(true)
                    .width(search_width),
            );
            let levels = [Level::Error, Level::Warn, Level::Info, Level::Debug]
                .map(|level| SelectItem::new(level, level.label()));
            ui.add(
                Select::new(&mut self.level, &levels)
                    .id_salt("journal-level")
                    .label("Уровень")
                    .search(false)
                    .width(level_width),
            );
        });
    }
}

fn console(
    ui: &mut egui::Ui,
    rows: &[&Entry],
    total: usize,
    follow: &mut bool,
    resume: bool,
) -> Option<LogAction> {
    let mut action = None;
    egui::Frame::new().fill(CONSOLE_BG).corner_radius(9.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::Frame::new()
            .fill(CONSOLE_BAR)
            .corner_radius(egui::CornerRadius { nw: 9, ne: 9, sw: 0, se: 0 })
            .inner_margin(egui::Margin::symmetric(14, 9))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("agent-hub.log").monospace().color(CONSOLE_TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("{} / {total}   UTC", rows.len()))
                                .monospace()
                                .color(CONSOLE_MUTED),
                        );
                    });
                });
            });
        egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 12)).show(ui, |ui| {
            *ui.visuals_mut() = egui::Visuals::dark();
            ui.visuals_mut().hyperlink_color = Level::Info.color();
            let min_height = if ui.available_width() < 450.0 { 160.0 } else { 220.0 };
            let height = ui.available_height().max(min_height);
            let scroll = egui::ScrollArea::vertical()
                .id_salt("log-lines")
                .max_height(height)
                .min_scrolled_height(height)
                .auto_shrink([false, false])
                .stick_to_bottom(*follow)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 7.0;
                    if rows.is_empty() {
                        ui.colored_label(
                            CONSOLE_MUTED,
                            "Совпадений нет. Измените поиск или уровень сообщений.",
                        );
                    }
                    for row in rows {
                        row.draw(ui);
                        if row.rejected {
                            ui.horizontal_wrapped(|ui| {
                                if ui.link("Использовать chat_id").clicked() {
                                    action = Some(LogAction::UseChat);
                                }
                                if ui.link("Добавить user_id").clicked() {
                                    action = Some(LogAction::AddUser);
                                }
                            });
                        }
                    }
                    if resume {
                        ui.scroll_to_cursor_animation(
                            Some(egui::Align::BOTTOM),
                            egui::style::ScrollAnimation::none(),
                        );
                    }
                });
            let remaining = scroll.content_size.y
                - scroll.inner_rect.height()
                - scroll.state.clamped_offset().y;
            if remaining > 1.0 && !resume {
                *follow = false;
            }
        });
    });
    action
}
