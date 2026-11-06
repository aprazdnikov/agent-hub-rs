//! The window: four tabs over the latest snapshot; closing it hides it into the tray.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui;
use tokio::sync::{mpsc, watch};

use crate::gui::log::LogView;
use crate::gui::look::{Banner, BannerAction, active, banner};
use crate::gui::settings::SettingsForm;
use crate::gui::status;
use crate::gui::topics::TopicsView;
use crate::gui::tray::{Tray, TrayAction};
use crate::logging::LogBuffer;
use crate::supervisor::{BotStatus, Command, SaveStatus, Snapshot};
use crate::updater::{UpdateCommand, UpdateState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Status,
    Topics,
    Log,
    Settings,
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
    exit: ExitChoice,
    restart: Confirm,
    pending_exit: Option<Exit>,
    save_requested: Option<u64>,
    topics: TopicsView,
    log: LogView,
    form: SettingsForm,
}

impl HubApp {
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
        let tab = match snapshot.borrow().bot {
            BotStatus::Unconfigured => Tab::Settings,
            BotStatus::Stopped
            | BotStatus::Starting
            | BotStatus::Retrying { .. }
            | BotStatus::Running { .. }
            | BotStatus::Failed(_) => Tab::Status,
        };
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
            tab,
            exit,
            restart: Confirm::Hidden,
            pending_exit: None,
            save_requested: None,
            topics: TopicsView::default(),
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

    fn confirm_pending_exit(&mut self, ctx: &egui::Context) {
        let Some(how) = self.pending_exit else { return };
        let saving = self.saving();
        let modal = egui::Modal::new(egui::Id::new("save_pending_exit")).show(ctx, |ui| {
            if saving {
                ui.label("Сохранение ещё выполняется. При выходе его результат неизвестен: файл и ключи могут сохраниться частично. Проверьте настройки после следующего запуска.");
            } else {
                ui.label("Сохранение завершено. Продолжить выход?");
            }
            ui.horizontal(|ui| {
                let label = if how == Exit::Restarting { "Перезапустить" } else { "Выйти" };
                if ui.add_enabled(!saving || how == Exit::Quitting, egui::Button::new(label)).clicked() {
                    self.pending_exit = None;
                    self.leave_now(ctx, how);
                }
                if ui.button("Продолжить работу").clicked() { self.pending_exit = None; }
            });
            if saving && how == Exit::Restarting {
                ui.weak("Перезапуск доступен после завершения сохранения.");
            }
        });
        if modal.should_close() {
            self.pending_exit = None;
        }
    }

    fn update_banner(&mut self, ui: &mut egui::Ui, sessions: usize) {
        let state = self.updates.borrow().clone();
        let Some(Banner { text, action }) = banner(&state) else { return };
        ui.horizontal(|ui| {
            ui.label(text);
            match action {
                Some(BannerAction::Install) => {
                    if ui.button("Установить").clicked()
                        && self.update_commands.try_send(UpdateCommand::Install).is_err()
                    {
                        tracing::warn!("install request dropped: the updater is busy");
                    }
                }
                Some(BannerAction::Restart) => {
                    if ui.button("Перезапустить").clicked() {
                        if sessions > 0 {
                            self.restart = Confirm::Shown;
                        } else {
                            self.leave(ui.ctx(), Exit::Restarting);
                        }
                    }
                }
                Some(BannerAction::Release) => {
                    ui.hyperlink_to(
                        "Открыть релизы",
                        "https://github.com/aprazdnikov/agent-hub-rs/releases/latest",
                    );
                }
                None => {
                    ui.spinner();
                }
            }
        });
    }

    fn confirm_restart(&mut self, ctx: &egui::Context, sessions: usize) {
        if self.restart == Confirm::Hidden {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("update_restart")).show(ctx, |ui| {
            ui.label(format!("Перезапуск прервёт активные сессии ({sessions}). Продолжить?"));
            ui.horizontal(|ui| {
                if ui.button("Перезапустить").clicked() {
                    self.restart = Confirm::Hidden;
                    self.leave(ctx, Exit::Restarting);
                }
                if ui.button("Отмена").clicked() {
                    self.restart = Confirm::Hidden;
                }
            });
        });
        if modal.should_close() {
            self.restart = Confirm::Hidden;
        }
    }

    fn tray_actions(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let actions: Vec<TrayAction> = match &self.tray {
            Some((_, actions)) => actions.try_iter().collect(),
            None => Vec::new(),
        };
        for action in actions {
            match action {
                TrayAction::Open => {
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
                TrayAction::Quit => self.leave(ctx, Exit::Quitting),
            }
        }
    }
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
        egui::Panel::top("tabs")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(20, 12)),
            )
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.available_width() >= 500.0 {
                        ui.label(egui::RichText::new("agent-hub").size(20.0).strong());
                        ui.add_space(20.0);
                    }
                    ui.selectable_value(&mut self.tab, Tab::Status, "Статус");
                    ui.selectable_value(&mut self.tab, Tab::Topics, "Темы");
                    ui.selectable_value(&mut self.tab, Tab::Log, "Лог");
                    ui.selectable_value(&mut self.tab, Tab::Settings, "Настройки");
                });
                // Store failures matter most on «Настройки», where the app opens in exactly those cases.
                if self.saving() {
                    let message = if snapshot.save_status == SaveStatus::Delayed {
                        "Сохранение задерживается. Повтор недоступен; при выходе результат может остаться неизвестным."
                    } else {
                        "Сохранение выполняется; при выходе результат может остаться неизвестным."
                    };
                    ui.colored_label(ui.visuals().warn_fg_color, message);
                }
                if let Some(notice) = &snapshot.notice {
                    ui.colored_label(ui.visuals().error_fg_color, notice);
                }
                self.update_banner(ui, active(&snapshot));
            });
        self.confirm_restart(ui.ctx(), active(&snapshot));
        self.confirm_pending_exit(ui.ctx());
        let margin = if self.tab == Tab::Settings { 0 } else { 24 };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(margin))
            .show(ui, |ui| match self.tab {
                Tab::Status => status::show(ui, &snapshot, self.form.has_unsaved(), &mut send),
                Tab::Topics => self.topics.show(ui, &snapshot, &mut send),
                Tab::Log => {
                    if let Some(action) = self.log.show(ui, &self.logs.lines(), &self.logs_dir) {
                        self.form.apply(action);
                        self.tab = Tab::Settings;
                    }
                }
                Tab::Settings => self.form.show(ui, &self.home, active(&snapshot), &mut send),
            });
        if save_sent {
            self.save_requested = Some(snapshot.save_revision);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_and_restart_wait_for_confirmation_when_save_is_queued() {
        let (commands, _inbox) = mpsc::channel(4);
        let (_snapshot, watched) = watch::channel(Snapshot::default());
        let (_updates_tx, updates) = watch::channel(UpdateState::Idle);
        let (update_commands, _update_inbox) = mpsc::channel(1);
        let exit = Arc::new(Mutex::new(Exit::Staying));
        let mut app = HubApp::new(AppSetup {
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
            exit: Arc::clone(&exit),
        });
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
