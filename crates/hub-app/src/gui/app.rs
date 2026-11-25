//! The desktop workspace, its inbox, and application settings.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui;
use tokio::sync::{mpsc, watch};

use crate::gui::log::LogView;
use crate::gui::look::{Banner, BannerAction, active, banner};
use crate::gui::settings::SettingsForm;
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
        let attention = if count == 0 {
            "Входящие".into()
        } else {
            format!("Входящие ({count})")
        };
        let wide = ui.available_width() >= 510.0;
        ui.horizontal_wrapped(|ui| {
            for (tab, label) in [
                (Tab::Attention, attention.as_str()),
                (Tab::Tasks, "Задачи"),
                (Tab::Topics, "Темы Telegram"),
            ] {
                if theme::tab(ui, self.tab == tab, label).clicked() {
                    self.tasks.leave_review();
                    self.tab = tab;
                }
            }
            let mut settings = |ui: &mut egui::Ui| {
                if theme::tab(ui, self.tab == Tab::Settings, "Настройки").clicked() {
                    self.tasks.leave_review();
                    self.tab = Tab::Settings;
                }
            };
            if wide {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), settings);
            } else {
                settings(ui);
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
                    if ui.button("Открыть релизы").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(
                            "https://github.com/aprazdnikov/agent-hub-rs/releases/latest",
                        ));
                    }
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

    fn settings(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        self.form.show(ui, &self.home, active(snapshot), snapshot, send);
        if !self.form.diagnostics_open() {
            return;
        }
        egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(16)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (page, label) in
                    [(DiagnosticPage::Status, "Состояние"), (DiagnosticPage::Log, "Журнал")]
                {
                    if theme::tab(ui, self.diagnostic_page == page, label).clicked() {
                        self.diagnostic_page = page;
                    }
                }
            });
            ui.add_space(12.0);
            match self.diagnostic_page {
                DiagnosticPage::Status => status::show(ui, snapshot, self.form.has_unsaved(), send),
                DiagnosticPage::Log => {
                    if let Some(action) = self.log.show(ui, &self.logs.lines(), &self.logs_dir) {
                        self.form.apply(action);
                    }
                }
            }
        });
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
        if self.tasks.take_inbox_return() && self.tab == Tab::Tasks {
            self.tab = Tab::Attention;
            send(Command::RefreshTaskIndex);
        }
        egui::Panel::top("tabs")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(14, 7)),
            )
            .show(ui, |ui| {
                self.navigation(ui, count);
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
        if self.tab != Tab::Settings
            && let Some(action) =
                self.tasks.project_navigation(ui, &snapshot, self.tab == Tab::Tasks)
        {
            self.tab = match action {
                SidebarAction::Tasks => Tab::Tasks,
                SidebarAction::Memory(project) => {
                    self.tasks.leave_review();
                    self.memory.open_project(project);
                    Tab::Memory
                }
            };
        }
        self.confirm_restart(ui.ctx(), active(&snapshot));
        self.confirm_pending_exit(ui.ctx());
        let margin = if self.tab == Tab::Settings { 0 } else { 20 };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(margin))
            .show(ui, |ui| match self.tab {
                Tab::Attention => {
                    if self.tasks.show_attention(ui, &snapshot, &mut send) {
                        self.tab = Tab::Tasks;
                    }
                    if snapshot.saved.is_none() {
                        ui.add_space(8.0);
                        if ui.button("Настроить приложение").clicked() {
                            self.tab = Tab::Settings;
                        }
                    }
                }
                Tab::Topics => self.topics.show(ui, &snapshot, &mut send),
                Tab::Tasks => self.tasks.show(ui, &snapshot, &mut send),
                Tab::Memory => {
                    egui::ScrollArea::vertical().id_salt("memory_outer").show(ui, |ui| {
                        self.memory.show(ui, &self.home);
                    });
                }
                Tab::Settings => self.settings(ui, &snapshot, &mut send),
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
