#![windows_subsystem = "windows"]

use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use eframe::egui;
use hub_app::config::FileSettings;
use hub_app::connector::TelegramConnector;
use hub_app::dirs::AppDirs;
use hub_app::github::{GitHub, SelfReplace};
use hub_app::gui::app::{AppSetup, Exit, ExitChoice, HubApp};
use hub_app::gui::home_or_root;
use hub_app::gui::tray::{Tray, app_icon};
use hub_app::instance::{InstanceError, InstanceLock};
use hub_app::logging::{self, LogBuffer, Repaint};
use hub_app::secrets::Keyring;
use hub_app::supervisor::{Command, STOP_TIMEOUT, Snapshot, Supervisor, SupervisorSetup};
use hub_app::updater::{self, UpdateCommand, UpdateState, UpdaterSetup};
use hub_app::updates::{Target, Version};
use tokio::sync::{mpsc, oneshot, watch};

const COMMANDS: usize = 32;
const UPDATE_COMMANDS: usize = 4;
const RUNTIME_GRACE: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "agent-hub failed");
            alert(&format!("{error:#}"));
            ExitCode::FAILURE
        }
    }
}

fn alert(text: &str) {
    rfd::MessageDialog::new()
        .set_title("agent-hub")
        .set_description(text)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

fn run() -> anyhow::Result<()> {
    // Taken before an update can replace the file: on Linux the path of a replaced running
    // binary reads "<path> (deleted)" afterwards.
    let program = std::env::current_exe()?;
    let dirs = AppDirs::locate().context("не найден домашний каталог пользователя")?;
    let lock = match InstanceLock::acquire(&dirs.lock()) {
        Ok(lock) => lock,
        Err(InstanceError::Running) => {
            alert("agent-hub уже запущен — его значок в трее.");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let logs = LogBuffer::default();
    let _guard = logging::init(&dirs.logs(), logs.clone())?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "agent-hub started");
    let home = home_or_root();
    let store = FileSettings::new(dirs.settings(), Box::new(Keyring));
    let loaded = store.load(&home);
    if let Err(error) = &loaded {
        tracing::error!(%error, "settings not loaded");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("agent-hub-core")
        .build()?;
    let (commands, inbox) = mpsc::channel(COMMANDS);
    let (snapshot, watched) = watch::channel(Snapshot::default());
    let checks = watched.clone();
    let (update_commands, update_inbox) = mpsc::channel(UPDATE_COMMANDS);
    let (update_state, updates) = watch::channel(UpdateState::Idle);
    let exit: ExitChoice = Arc::new(Mutex::new(Exit::Staying));
    let window_exit = Arc::clone(&exit);
    let core = runtime.handle().clone();
    let window = commands.clone();
    let connector = TelegramConnector { home: home.clone(), topics: dirs.topics() };
    let logs_dir = dirs.logs();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("agent-hub")
            .with_inner_size([960.0, 640.0])
            .with_icon(Arc::new(window_icon())),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "agent-hub",
        options,
        Box::new(move |creation| {
            let context = creation.egui_ctx.clone();
            let repaint: Repaint = Arc::new(move || context.request_repaint());
            logs.on_change(Arc::clone(&repaint));
            let supervisor = Supervisor::new(SupervisorSetup {
                connector,
                store: Box::new(store),
                home: home.clone(),
                loaded,
                snapshot,
                repaint: Arc::clone(&repaint),
            });
            core.spawn(supervisor.run(inbox));
            spawn_updater(&core, checks, update_state, update_inbox, Arc::clone(&repaint));
            let tray = match Tray::create(repaint) {
                Ok(tray) => Some(tray),
                Err(error) => {
                    tracing::warn!(%error, "no tray: closing the window quits");
                    None
                }
            };
            Ok(Box::new(HubApp::new(AppSetup {
                commands: window,
                snapshot: watched,
                logs,
                tray,
                home,
                logs_dir,
                updates,
                update_commands,
                exit: window_exit,
            })))
        }),
    )
    .map_err(|error| anyhow::anyhow!("окно не открылось: {error}"))?;
    stop_core(&runtime, &commands);
    runtime.shutdown_timeout(RUNTIME_GRACE);
    let restart = *exit.lock().unwrap_or_else(PoisonError::into_inner) == Exit::Restarting;
    // The new process must find the lock free, so it goes before the spawn.
    drop(lock);
    if restart {
        std::process::Command::new(program).spawn()?;
        tracing::info!("restarting into the new version");
    }
    tracing::info!("agent-hub stopped");
    Ok(())
}

fn spawn_updater(
    core: &tokio::runtime::Handle,
    checks: watch::Receiver<Snapshot>,
    state: watch::Sender<UpdateState>,
    inbox: mpsc::Receiver<UpdateCommand>,
    repaint: Repaint,
) {
    match (GitHub::new(), Version::parse(env!("CARGO_PKG_VERSION"))) {
        (Ok(source), Some(current)) => {
            core.spawn(updater::run(
                UpdaterSetup {
                    source,
                    installer: SelfReplace,
                    current,
                    target: Target::current(),
                    checks,
                    state,
                    repaint,
                },
                inbox,
            ));
        }
        (Err(error), _) => tracing::warn!(%error, "updates are off"),
        (Ok(_), None) => tracing::warn!("version is not x.y.z, updates are off"),
    }
}

/// Stops the bot so topics get «⏹ Остановлено», within a bounded wait.
fn stop_core(runtime: &tokio::runtime::Runtime, commands: &mpsc::Sender<Command>) {
    let (done, stopped) = oneshot::channel();
    if commands.blocking_send(Command::Quit(done)).is_ok() {
        runtime.block_on(async {
            // Past the deadline we exit anyway; the log says what did not stop.
            let _ = tokio::time::timeout(STOP_TIMEOUT + Duration::from_secs(1), stopped).await;
        });
    }
}

fn window_icon() -> egui::IconData {
    let (rgba, side) = app_icon();
    egui::IconData { rgba, width: side, height: side }
}
