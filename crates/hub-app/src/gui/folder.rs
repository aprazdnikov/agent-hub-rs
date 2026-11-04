//! Opens a directory in the system file manager.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui;
use hub_core::domain::TopicKey;

use crate::gui::look::topic_url;

pub fn open(path: &Path) -> io::Result<()> {
    launch(path.as_os_str()).map(drop)
}

fn launch(target: &OsStr) -> io::Result<Child> {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    spawn(Command::new(program), target)
}

fn spawn(mut command: Command, target: &OsStr) -> io::Result<Child> {
    command.arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command.spawn()
}

fn directory(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "ожидался абсолютный путь"));
    }
    if !path.metadata()?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "путь не является каталогом"));
    }
    Ok(path.to_owned())
}

#[derive(Clone, Copy)]
enum ExitPolicy {
    Check,
    // Explorer can exit with a nonzero code after successfully delegating an open request.
    Delegated,
}

fn finished(child: &mut Child, policy: ExitPolicy) -> io::Result<bool> {
    match child.try_wait()? {
        None => Ok(false),
        Some(status) if status.success() || matches!(policy, ExitPolicy::Delegated) => Ok(true),
        Some(status) => Err(io::Error::other(format!("системная программа завершилась: {status}"))),
    }
}

pub enum Destination {
    Folder(PathBuf),
    Topic(TopicKey),
}

impl Destination {
    fn label(&self) -> String {
        match self {
            Self::Folder(path) => path.display().to_string(),
            Self::Topic(key) => {
                topic_url(*key).unwrap_or_else(|| format!("тема #{}", key.thread.0))
            }
        }
    }

    fn launch(self) -> io::Result<Child> {
        match self {
            Self::Folder(path) => launch(directory(&path)?.as_os_str()),
            Self::Topic(key) => {
                let url = topic_url(key).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "невозможно построить ссылку на тему",
                    )
                })?;
                launch(OsStr::new(&url))
            }
        }
    }
}

const MAX_LAUNCHES: usize = 8;

struct Pending {
    label: String,
    result: Receiver<io::Result<Child>>,
}

#[derive(Default)]
pub struct Opener {
    pending: Option<Pending>,
    children: Vec<(String, Child)>,
    error: Option<String>,
}

impl Opener {
    pub fn start(&mut self, destination: Destination, ctx: &egui::Context) {
        self.start_with(destination.label(), ctx, move || destination.launch());
    }

    fn start_with(
        &mut self,
        label: String,
        ctx: &egui::Context,
        work: impl FnOnce() -> io::Result<Child> + Send + 'static,
    ) {
        if self.pending.is_some() || self.children.len() >= MAX_LAUNCHES {
            self.error = Some("Предыдущие запросы ещё выполняются. Дождитесь ответа системы или закройте открытые окна.".to_owned());
            return;
        }
        self.error = None;
        let (send, result) = mpsc::channel();
        let ctx = ctx.clone();
        match std::thread::Builder::new().name("open-topic".to_owned()).spawn(move || {
            let _ = send.send(work());
            ctx.request_repaint();
        }) {
            Ok(_) => self.pending = Some(Pending { label, result }),
            Err(error) => self.error = Some(format!("Не удалось открыть {label}: {error}")),
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(pending) = self.pending.take() {
            match pending.result.try_recv() {
                Ok(Ok(child)) => self.children.push((pending.label, child)),
                Ok(Err(error)) => {
                    self.error = Some(format!("Не удалось открыть {}: {error}", pending.label));
                }
                Err(TryRecvError::Empty) => self.pending = Some(pending),
                Err(TryRecvError::Disconnected) => {
                    self.error =
                        Some(format!("Не удалось открыть {}: запуск прерван", pending.label));
                }
            }
        }
        let mut index = 0;
        let policy = if cfg!(windows) { ExitPolicy::Delegated } else { ExitPolicy::Check };
        while let Some((label, child)) = self.children.get_mut(index) {
            match finished(child, policy) {
                Ok(false) => index += 1,
                result => {
                    if let Err(error) = result {
                        self.error = Some(format!("Не удалось открыть {label}: {error}"));
                    }
                    self.children.swap_remove(index);
                }
            }
        }
        if self.pending.is_some() || !self.children.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_non_directory_paths_are_reported() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Проект с пробелами");
        assert_eq!(directory(&path).unwrap_err().kind(), io::ErrorKind::NotFound);
        std::fs::write(&path, "file").unwrap();
        assert_eq!(directory(&path).unwrap_err().kind(), io::ErrorKind::NotADirectory);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(directory(&path).unwrap(), path);
    }

    #[test]
    fn failed_system_launcher_is_reported() {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/D", "/S", "/C", "exit 7"]);
            command
        } else {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "exit 7"]);
            command
        };
        let mut child = command.spawn().unwrap();
        child.wait().unwrap();
        assert!(finished(&mut child, ExitPolicy::Check).unwrap_err().to_string().contains('7'));
        assert!(finished(&mut child, ExitPolicy::Delegated).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn launch_passes_the_whole_target_as_one_literal_argument() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("argument");
        let target = "Проект с пробелами $(touch executed); 'quotes'";
        let mut command = Command::new("/bin/sh");
        command.current_dir(temp.path()).env("HUB_TEST_OUTPUT", &output).args([
            "-c",
            r#"printf '%s' "$1" > "$HUB_TEST_OUTPUT""#,
            "fixture",
        ]);
        let mut child = spawn(command, OsStr::new(target)).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(std::fs::read_to_string(output).unwrap(), target);
        assert!(!temp.path().join("executed").exists());
    }

    fn drain(opener: &mut Opener, ctx: &egui::Context) {
        let end = std::time::Instant::now() + Duration::from_secs(2);
        while opener.is_pending() || !opener.children.is_empty() {
            opener.poll(ctx);
            assert!(std::time::Instant::now() < end, "launcher did not complete");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn spawn_error_is_visible_and_the_next_request_can_run() {
        let ctx = egui::Context::default();
        let mut opener = Opener::default();
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-launcher");
        opener.start_with("project".to_owned(), &ctx, move || {
            spawn(Command::new(missing), OsStr::new("target"))
        });
        drain(&mut opener, &ctx);
        assert!(opener.error().unwrap().contains("project"));
        opener.start(
            Destination::Topic(TopicKey {
                chat: hub_core::domain::ChatId(1),
                thread: hub_core::domain::ThreadId(7),
            }),
            &ctx,
        );
        drain(&mut opener, &ctx);
        assert!(opener.error().unwrap().contains("невозможно построить ссылку"));
    }

    #[test]
    fn a_pending_launch_does_not_start_a_second_worker() {
        let ctx = egui::Context::default();
        let mut opener = Opener::default();
        let (release, wait) = mpsc::channel();
        opener.start_with("first".to_owned(), &ctx, move || {
            wait.recv().unwrap();
            Err(io::Error::other("fixture"))
        });
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let second = called.clone();
        opener.start_with("second".to_owned(), &ctx, move || {
            second.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(io::Error::other("must not run"))
        });
        assert!(opener.error().is_some());
        release.send(()).unwrap();
        drain(&mut opener, &ctx);
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
