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
use crate::supervisor::{BotStatus, Command, Snapshot};
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
    pub commands: mpsc::Sender<Command>,
    pub snapshot: watch::Receiver<Snapshot>,
    pub logs: LogBuffer,
    pub tray: Option<(Tray, std_mpsc::Receiver<TrayAction>)>,
    pub home: PathBuf,
    pub logs_dir: PathBuf,
    pub updates: watch::Receiver<UpdateState>,
    pub update_commands: mpsc::Sender<UpdateCommand>,
    pub exit: ExitChoice,
}

pub struct HubApp {
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
    topics: TopicsView,
    log: LogView,
    form: SettingsForm,
}

impl HubApp {
    #[must_use]
    pub fn new(setup: AppSetup) -> Self {
        let AppSetup {
            commands,
            snapshot,
            logs,
            tray,
            home,
            logs_dir,
            updates,
            update_commands,
            exit,
        } = setup;
        let tab = match snapshot.borrow().bot {
            BotStatus::Unconfigured => Tab::Settings,
            BotStatus::Stopped
            | BotStatus::Starting
            | BotStatus::Running { .. }
            | BotStatus::Failed(_) => Tab::Status,
        };
        Self {
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
            topics: TopicsView::default(),
            log: LogView::default(),
            form: SettingsForm::default(),
        }
    }

    fn send(commands: &mpsc::Sender<Command>, command: Command) {
        if commands.try_send(command).is_err() {
            tracing::warn!("command dropped: the core is busy or stopped");
        }
    }

    fn exit(&self) -> Exit {
        *self.exit.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn leave(&self, ctx: &egui::Context, how: Exit) {
        *self.exit.lock().unwrap_or_else(PoisonError::into_inner) = how;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayAction::Toggle => {
                    let command = match snapshot.bot {
                        BotStatus::Running { .. } | BotStatus::Starting => Command::Stop,
                        BotStatus::Stopped | BotStatus::Failed(_) | BotStatus::Unconfigured => {
                            Command::Start
                        }
                    };
                    Self::send(&self.commands, command);
                }
                TrayAction::Quit => self.leave(ctx, Exit::Quitting),
            }
        }
    }
}

impl eframe::App for HubApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let snapshot = self.snapshot.borrow().clone();
        self.tray_actions(ctx, &snapshot);
        if let Some((tray, _)) = &mut self.tray {
            tray.show(&snapshot, &self.updates.borrow());
        }
        self.form.sync(snapshot.saved.as_ref());
        let closing = ctx.input(|input| input.viewport().close_requested());
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
        let mut send = |command| Self::send(&commands, command);
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Status, "Статус");
                ui.selectable_value(&mut self.tab, Tab::Topics, "Темы");
                ui.selectable_value(&mut self.tab, Tab::Log, "Лог");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Настройки");
            });
            // Store failures matter most on «Настройки», where the app opens in exactly those cases.
            if let Some(notice) = &snapshot.notice {
                ui.colored_label(ui.visuals().error_fg_color, notice);
            }
            self.update_banner(ui, active(&snapshot));
        });
        self.confirm_restart(ui.ctx(), active(&snapshot));
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Status => status::show(ui, &snapshot, &mut send),
            Tab::Topics => self.topics.show(ui, &snapshot, &mut send),
            Tab::Log => {
                if let Some(action) = self.log.show(ui, &self.logs.lines(), &self.logs_dir) {
                    self.form.apply(action);
                    self.tab = Tab::Settings;
                }
            }
            Tab::Settings => self.form.show(ui, &self.home, active(&snapshot), &mut send),
        });
    }
}
