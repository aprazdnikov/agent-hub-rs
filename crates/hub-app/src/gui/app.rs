//! The desktop workspace, its inbox, and application settings.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui;
use kylig_ui::{Button, IconButton, ProgressRing, Segmented};
use tokio::sync::{mpsc, watch};

use crate::gui::log::LogView;
use crate::gui::look::{Banner, BannerAction, active, banner, update_details};
use crate::gui::settings::{SettingsForm, SystemPage};
use crate::gui::status;
use crate::gui::tasks::SidebarAction;
use crate::gui::theme;
use crate::gui::topics::TopicsView;
use crate::gui::tray::{Tray, TrayAction};
use crate::logging::LogBuffer;
use crate::supervisor::{BotStatus, Command, SaveStatus, Snapshot};
use crate::updater::{UpdateCommand, UpdateState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Attention,
    Tasks,
    Memory,
    Topics,
    Settings,
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum DiagnosticPage {
    #[default]
    Status,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Staying,
    Quitting,
    Restarting,
}

/// How the window ended, read by `main` after the event loop returns.
pub type ExitChoice = Arc<Mutex<Exit>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Hidden,
    Shown,
}

pub struct AppSetup {
    pub autostart: Result<Arc<dyn crate::autostart::Registration>, String>,
    pub initial_window: crate::autostart::Window,
    pub commands: mpsc::Sender<Command>,
    pub snapshot: watch::Receiver<Snapshot>,
    pub logs: LogBuffer,
    pub tray: Option<(Tray, std_mpsc::Receiver<TrayAction>)>,
    pub home: PathBuf,
    pub logs_dir: PathBuf,
    pub ui_settings: PathBuf,
    pub updates: watch::Receiver<UpdateState>,
    pub update_commands: mpsc::Sender<UpdateCommand>,
    pub exit: ExitChoice,
}

pub struct HubApp {
    initial_window: Option<crate::autostart::Window>,
    commands: mpsc::Sender<Command>,
    snapshot: watch::Receiver<Snapshot>,
    logs: LogBuffer,
    tray: Option<(Tray, std_mpsc::Receiver<TrayAction>)>,
    home: PathBuf,
    logs_dir: PathBuf,
    updates: watch::Receiver<UpdateState>,
    update_commands: mpsc::Sender<UpdateCommand>,
    tab: Tab,
    diagnostic_page: DiagnosticPage,
    exit: ExitChoice,
    restart: Confirm,
    tray_quit: bool,
    pending_exit: Option<Exit>,
    save_requested: Option<u64>,
    tasks: super::tasks::TasksView,
    topics: TopicsView,
    memory: super::memory::MemoryView,
    log: LogView,
    form: SettingsForm,
}

impl HubApp {
    fn navigation(&mut self, ui: &mut egui::Ui, count: usize) {
        let compact = ui.available_width() < 645.0;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new("Agent Hub")
                    .strong()
                    .size(20.0)
                    .color(ui.visuals().hyperlink_color),
            );
            let (icon, label, next) = if ui.visuals().dark_mode {
                (kylig_icons::SUN, "Светлая тема", egui::Theme::Light)
            } else {
                (kylig_icons::MOON, "Тёмная тема", egui::Theme::Dark)
            };
            let toggle = if compact {
                ui.add(IconButton::new(icon, label))
            } else {
                ui.add(Button::new(label).icon(icon).ghost())
            };
            if toggle.clicked() {
                ui.ctx().set_theme(next);
            }
            if ui.ctx().options(|options| options.theme_preference != egui::ThemePreference::System)
                && ui.link("Как в системе").clicked()
            {
                ui.ctx().set_theme(egui::ThemePreference::System);
            }
        });
        ui.add_space(6.0);
        let attention = format!("Входящие · {count}");
        ui.horizontal_wrapped(|ui| {
            if compact {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 6.0);
                let mut font = egui::TextStyle::Button.resolve(ui.style());
                font.size = font.size.min(13.0);
                ui.style_mut().text_styles.insert(egui::TextStyle::Button, font);
            }
            for (tab, label, icon) in [
                (Tab::Attention, attention.as_str(), kylig_icons::TRAY),
                (Tab::Tasks, "Задачи", kylig_icons::CHECK_SQUARE),
                (Tab::Topics, "Темы Telegram", kylig_icons::CHATS),
                (Tab::Memory, "Память", kylig_icons::DATABASE),
                (Tab::Settings, "Настройки", kylig_icons::GEAR),
            ] {
                if theme::nav_chip(ui, self.tab == tab, label, icon).clicked() && self.tab != tab {
                    if self.tab == Tab::Tasks
                        && tab == Tab::Memory
                        && let Some(project) = self.tasks.current_project()
                    {
                        self.memory.open_project(project.to_owned());
                    }
                    self.tasks.leave_review();
                    self.tab = tab;
                }
            }
        });
    }

    #[must_use]
    pub fn new(setup: AppSetup) -> Self {
        let AppSetup {
            autostart,
            initial_window,
            commands,
            snapshot,
            logs,
            tray,
            home,
            logs_dir,
            ui_settings,
            updates,
            update_commands,
            exit,
        } = setup;
        let mut form = SettingsForm::with_autostart(autostart);
        form.set_scale_path(ui_settings);
        Self {
            initial_window: Some(initial_window),
            commands,
            snapshot,
            logs,
            tray,
            home,
            logs_dir,
            updates,
            update_commands,
            tab: Tab::Attention,
            diagnostic_page: DiagnosticPage::default(),
            exit,
            restart: Confirm::Hidden,
            tray_quit: false,
            pending_exit: None,
            save_requested: None,
            tasks: super::tasks::TasksView::default(),
            topics: TopicsView::default(),
            memory: super::memory::MemoryView::default(),
            log: LogView::default(),
            form,
        }
    }

    fn send(commands: &mpsc::Sender<Command>, command: Command) -> bool {
        if commands.try_send(command).is_err() {
            tracing::warn!("command dropped: the core is busy or stopped");
            false
        } else {
            true
        }
    }

    fn exit(&self) -> Exit {
        *self.exit.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn saving(&self) -> bool {
        self.save_requested.is_some()
            || matches!(
                self.snapshot.borrow().save_status,
                SaveStatus::Saving | SaveStatus::Delayed
            )
    }

    fn leave(&mut self, ctx: &egui::Context, how: Exit) {
        if self.saving() {
            self.pending_exit = Some(how);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.request_repaint();
        } else {
            self.leave_now(ctx, how);
        }
    }

    fn leave_now(&self, ctx: &egui::Context, how: Exit) {
        *self.exit.lock().unwrap_or_else(PoisonError::into_inner) = how;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn confirm_pending_exit(&mut self, ui: &egui::Ui) {
        let Some(how) = self.pending_exit else { return };
        let saving = self.saving();
        let mut open = true;
        theme::dialog(ui, "save_pending_exit", &mut open, "Сохранение настроек").show(ui.ctx(), |ui| {
            if saving {
                ui.label("Сохранение ещё выполняется. При выходе его результат неизвестен: файл и ключи могут сохраниться частично. Проверьте настройки после следующего запуска.");
            } else {
                ui.label("Сохранение завершено. Продолжить выход?");
            }
            ui.horizontal_wrapped(|ui| {
                let label = if how == Exit::Restarting { "Перезапустить" } else { "Выйти" };
                if ui.add_enabled(!saving || how == Exit::Quitting, Button::new(label)).clicked() {
                    self.pending_exit = None;
                    self.leave_now(ui.ctx(), how);
                }
                if ui.add(Button::new("Продолжить работу")).clicked() { self.pending_exit = None; }
            });
            if saving && how == Exit::Restarting {
                ui.weak("Перезапуск доступен после завершения сохранения.");
            }
        });
        if !open {
            self.pending_exit = None;
        }
    }

    fn update_banner(&mut self, ui: &mut egui::Ui, sessions: usize) {
        let state = self.updates.borrow().clone();
        let Some(info) = banner(&state) else { return };
        if let Some(action) = update_row(ui, &state, info, !self.update_commands.is_closed()) {
            self.apply_update(ui.ctx(), sessions, action);
        }
    }

    fn apply_update(&mut self, ctx: &egui::Context, sessions: usize, action: BannerAction) {
        match action {
            BannerAction::Check | BannerAction::Install => {
                let command = if action == BannerAction::Check {
                    UpdateCommand::Check
                } else {
                    UpdateCommand::Install
                };
                if let Err(error) = self.update_commands.try_send(command) {
                    tracing::warn!(%error, "updater command could not be sent");
                }
            }
            BannerAction::Restart if sessions > 0 => self.restart = Confirm::Shown,
            BannerAction::Restart => self.leave(ctx, Exit::Restarting),
            BannerAction::Release => ctx.open_url(egui::OpenUrl::new_tab(
                "https://github.com/aprazdnikov/agent-hub-rs/releases/latest",
            )),
        }
    }

    fn confirm_tray_quit(&mut self, ui: &egui::Ui) {
        if !self.tray_quit {
            return;
        }
        let response = theme::dialog(ui, "tray-quit", &mut self.tray_quit, "Выход из Agent Hub")
            .show(ui.ctx(), |ui| {
                ui.label("Завершить работу приложения?");
                ui.horizontal_wrapped(|ui| {
                    if ui.add(Button::new("Завершить").primary()).clicked() {
                        return Some(true);
                    }
                    ui.add(Button::new("Остаться")).clicked().then_some(false)
                })
                .inner
            })
            .flatten();
        if let Some(quit) = response {
            self.tray_quit = false;
            if quit {
                self.leave(ui.ctx(), Exit::Quitting);
            }
        }
    }

    fn confirm_restart(&mut self, ui: &egui::Ui, sessions: usize) {
        if self.restart == Confirm::Hidden {
            return;
        }
        let mut open = true;
        theme::dialog(ui, "update_restart", &mut open, "Перезапуск приложения").show(
            ui.ctx(),
            |ui| {
                ui.label(format!("Перезапуск прервёт активные сессии ({sessions}). Продолжить?"));
                ui.horizontal_wrapped(|ui| {
                    if ui.add(Button::new("Перезапустить").primary()).clicked() {
                        self.restart = Confirm::Hidden;
                        self.leave(ui.ctx(), Exit::Restarting);
                    }
                    if ui.add(Button::new("Отмена")).clicked() {
                        self.restart = Confirm::Hidden;
                    }
                });
            },
        );
        if !open {
            self.restart = Confirm::Hidden;
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        if self.form.diagnostics_open() && self.diagnostic_page == DiagnosticPage::Log {
            ui.horizontal_wrapped(|ui| {
                if ui.link("‹ Диагностика").clicked() {
                    self.diagnostic_page = DiagnosticPage::Status;
                }
                ui.heading("Журнал");
            });
            if let Some(action) = self.log.show(ui, &self.logs.lines(), &self.logs_dir) {
                self.form.apply(action);
                self.diagnostic_page = DiagnosticPage::Status;
            }
            return;
        }
        let sessions = active(snapshot);
        let mut update_action = None;
        self.form.show_with_system_controls(
            ui,
            &self.home,
            sessions,
            snapshot,
            send,
            |ui, page, send| {
                match page {
                    SystemPage::Diagnostics { unsaved } => {
                        Segmented::new(
                            &mut self.diagnostic_page,
                            [
                                (DiagnosticPage::Status, "Состояние"),
                                (DiagnosticPage::Log, "Журнал"),
                            ],
                        )
                        .id_salt("diagnostics-pages")
                        .show(ui);
                        if self.diagnostic_page == DiagnosticPage::Status {
                            status::show(ui, snapshot, unsaved, send);
                        }
                    }
                    SystemPage::Updates => {
                        let state = self.updates.borrow();
                        if self.update_commands.is_closed() {
                            ui.weak("Сервис обновлений недоступен.");
                        } else {
                            update_action = update_row(ui, &state, update_details(&state), true);
                        }
                    }
                }
                None
            },
        );
        if let Some(action) = update_action {
            self.apply_update(ui.ctx(), sessions, action);
        }
        if self.form.take_setup_complete() {
            self.tab = Tab::Attention;
        }
    }

    fn tray_actions(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let actions: Vec<TrayAction> = match &self.tray {
            Some((_, actions)) => actions.try_iter().collect(),
            None => Vec::new(),
        };
        for action in actions {
            match action {
                TrayAction::Open | TrayAction::Updates | TrayAction::Quit => {
                    if action == TrayAction::Updates {
                        self.tasks.leave_review();
                        self.form.open_application();
                        self.tab = Tab::Settings;
                    } else if action == TrayAction::Quit {
                        self.tray_quit = true;
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayAction::Toggle => {
                    let command = match snapshot.bot {
                        BotStatus::Running { .. }
                        | BotStatus::Starting
                        | BotStatus::Retrying { .. } => Command::Stop,
                        BotStatus::Stopped | BotStatus::Failed(_) | BotStatus::Unconfigured => {
                            Command::Start
                        }
                    };
                    let _ = Self::send(&self.commands, command);
                }
            }
        }
    }
}

fn update_row(
    ui: &mut egui::Ui,
    state: &UpdateState,
    info: Banner,
    enabled: bool,
) -> Option<BannerAction> {
    ui.horizontal_wrapped(|ui| {
        if info.action.is_none() {
            ui.add(ProgressRing::indeterminate().size(20.0));
        }
        ui.label(info.text);
        let action = info.action?;
        let label = match action {
            BannerAction::Check if matches!(state, UpdateState::CheckFailed(_)) => {
                "Повторить проверку"
            }
            BannerAction::Check if matches!(state, UpdateState::Current) => "Проверить снова",
            BannerAction::Check => "Проверить обновления",
            BannerAction::Install => "Установить",
            BannerAction::Restart => "Перезапустить",
            BannerAction::Release => "Открыть релизы",
        };
        ui.add_enabled(enabled, Button::new(label)).clicked().then_some(action)
    })
    .inner
}

impl eframe::App for HubApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(initial) = self.initial_window.take() {
            match initial {
                crate::autostart::Window::Visible => {}
                crate::autostart::Window::Minimized => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                crate::autostart::Window::Hidden => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
            }
        }
        self.form.poll(ctx);
        self.topics.poll(ctx);
        self.tasks.update(&mut |command| {
            Self::send(&self.commands, command);
        });
        let snapshot = self.snapshot.borrow().clone();
        self.tray_actions(ctx, &snapshot);
        if let Some((tray, _)) = &mut self.tray {
            tray.show(&snapshot, &self.updates.borrow());
        }
        self.form.sync(snapshot.saved.as_ref());
        if self.save_requested.is_some_and(|revision| revision != snapshot.save_revision) {
            self.save_requested = None;
        }
        self.form.set_saving(self.saving() || snapshot.save_status == SaveStatus::Uncertain);
        let closing = ctx.input(|input| input.viewport().close_requested());
        if closing && self.exit() == Exit::Staying && self.tray.is_none() && self.saving() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_exit = Some(Exit::Quitting);
        }
        if closing && self.exit() == Exit::Staying && self.tray.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            // A Linux tray may have no host to show it, and Wayland cannot hide a window:
            // minimising keeps the window reachable from the taskbar either way.
            let hide = if cfg!(target_os = "linux") {
                egui::ViewportCommand::Minimized(true)
            } else {
                egui::ViewportCommand::Visible(false)
            };
            ctx.send_viewport_cmd(hide);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let snapshot = self.snapshot.borrow().clone();
        let commands = self.commands.clone();
        let mut save_sent = false;
        let mut send = |command| {
            let is_save = matches!(command, Command::Save(_));
            if Self::send(&commands, command) && is_save {
                save_sent = true;
            }
        };
        let count = super::tasks::attention_count(&snapshot);
        self.memory
            .set_workspace_root(super::tasks::configured_project_root(&snapshot, &self.home));
        if self.tasks.take_inbox_return() && self.tab == Tab::Tasks {
            self.tab = Tab::Attention;
            send(Command::RefreshTaskIndex);
        }
        let compact = ui.available_width() < 650.0;
        let side_margin = if compact { 16 } else { 32 };
        egui::Panel::top("tabs")
            .frame(egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .inner_margin(egui::Margin { bottom: 12, ..egui::Margin::ZERO }))
            .show_separator_line(false)
            .show(ui, |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(side_margin, if compact { 8 } else { 12 }))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                self.navigation(ui, count);
                if self.saving() {
                    let message = if snapshot.save_status == SaveStatus::Delayed {
                        "Сохранение задерживается. Повтор недоступен; при выходе результат может остаться неизвестным."
                    } else {
                        "Сохранение выполняется; при выходе результат может остаться неизвестным."
                    };
                    ui.horizontal_wrapped(|ui| { ui.add(ProgressRing::indeterminate().size(18.0)); ui.label(message); });
                }
                if let Some(notice) = &snapshot.notice {
                    ui.colored_label(ui.visuals().error_fg_color, notice);
                }
                if self.tab != Tab::Settings || !self.form.application_open() {
                    self.update_banner(ui, active(&snapshot));
                }
                    });
                ui.separator();
            });
        self.confirm_tray_quit(ui);
        self.confirm_restart(ui, active(&snapshot));
        self.confirm_pending_exit(ui);
        let margin = egui::Margin::symmetric(side_margin, if compact { 14 } else { 20 });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(margin))
            .show(ui, |ui| {
                if matches!(self.tab, Tab::Attention | Tab::Topics | Tab::Memory) {
                    ui.set_max_width(ui.available_width().min(1280.0));
                }
                match self.tab {
                    Tab::Attention => {
                        if self.tasks.show_attention(ui, &snapshot, &mut send) {
                            self.tab = Tab::Tasks;
                        }
                        if snapshot.saved.is_none() {
                            ui.add_space(8.0);
                            if ui.add(Button::new("Настроить приложение").primary()).clicked()
                            {
                                self.form.open_setup();
                                self.tab = Tab::Settings;
                            }
                        }
                    }
                    Tab::Topics => self.topics.show(ui, &snapshot, &mut send),
                    Tab::Tasks => {
                        if let Some(SidebarAction::Memory(project)) =
                            self.tasks.show_workspace(ui, &snapshot, &mut send)
                        {
                            self.tasks.leave_review();
                            self.memory.open_project(project);
                            self.tab = Tab::Memory;
                        }
                    }
                    Tab::Memory => {
                        egui::ScrollArea::vertical().id_salt("memory_outer").show(ui, |ui| {
                            self.memory.project_picker(
                                ui,
                                self.tasks.project_choices(ui.ctx(), &snapshot),
                            );
                            self.memory.show(ui, &self.home);
                        });
                    }
                    Tab::Settings => self.settings(ui, &snapshot, &mut send),
                }
            });
        if save_sent {
            self.save_requested = Some(snapshot.save_revision);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> HubApp {
        let (commands, _inbox) = mpsc::channel(4);
        let (_snapshot, watched) = watch::channel(Snapshot::default());
        let (_updates_tx, updates) = watch::channel(UpdateState::Idle);
        let (update_commands, _update_inbox) = mpsc::channel(1);
        let exit = Arc::new(Mutex::new(Exit::Staying));
        HubApp::new(AppSetup {
            autostart: Err("unavailable".to_owned()),
            initial_window: crate::autostart::Window::Visible,
            commands,
            snapshot: watched,
            logs: LogBuffer::default(),
            tray: None,
            home: std::env::temp_dir(),
            logs_dir: std::env::temp_dir(),
            ui_settings: std::env::temp_dir().join("agent-hub-ui-test.toml"),
            updates,
            update_commands,
            exit,
        })
    }

    #[test]
    fn first_launch_opens_inbox_with_settings_reachable_from_navigation() {
        let app = app();
        assert_eq!(app.tab, Tab::Attention);
    }

    #[test]
    fn narrow_desktop_navigation_keeps_all_sections_in_one_row_and_opens_memory() {
        let mut app = app();
        let ctx = egui::Context::default();
        theme::configure(&ctx);
        let mut draw = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(608.0, 480.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.navigation(ui, 12),
            );
            output.textures_delta.clear();
            for icon in [
                kylig_icons::TRAY,
                kylig_icons::CHECK_SQUARE,
                kylig_icons::CHATS,
                kylig_icons::DATABASE,
                kylig_icons::GEAR,
            ] {
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.text() == icon)));
            }
            let labels: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text)
                        if ["Входящие · 12", "Задачи", "Темы Telegram", "Память", "Настройки"]
                            .contains(&text.galley.text()) =>
                    {
                        Some((
                            text.galley.text().to_owned(),
                            text.galley.rect.translate(text.pos.to_vec2()).center(),
                        ))
                    }
                    _ => None,
                })
                .collect();
            labels
        };
        let labels = draw(Vec::new());
        assert_eq!(labels.len(), 5);
        let pos = labels.iter().find(|(text, _)| text == "Память").unwrap().1;
        assert!(labels.iter().all(|(_, center)| (center.y - pos.y).abs() < 1.0));
        draw(vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            draw(vec![egui::Event::PointerButton {
                pos,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
        assert_eq!(app.tab, Tab::Memory);
    }

    #[test]
    fn quit_and_restart_wait_for_confirmation_when_save_is_queued() {
        let mut app = app();
        let exit = Arc::clone(&app.exit);
        app.save_requested = Some(0);
        let ctx = egui::Context::default();
        app.leave(&ctx, Exit::Quitting);
        assert_eq!(app.pending_exit, Some(Exit::Quitting));
        assert_eq!(*exit.lock().unwrap(), Exit::Staying);
        app.leave(&ctx, Exit::Restarting);
        assert_eq!(app.pending_exit, Some(Exit::Restarting));
        assert_eq!(*exit.lock().unwrap(), Exit::Staying);
    }
}
