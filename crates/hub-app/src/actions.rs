//! Executes locally configured shell commands as managed foreground processes.
//!
//! Unix uses `/bin/sh -c`; Windows uses the system `cmd.exe /D /S /C` (not
//! PowerShell). Commands inherit the application's environment and privileges,
//! with null stdin and no terminal. Only trusted, locally configured command
//! strings belong here: this API deliberately interprets shell syntax.
//!
//! There is no runtime deadline or retry. Unix cancellation sends SIGTERM to the
//! process group with up to two seconds for cleanup, then kills any leftovers.
//! Future drop and shell exit force-stop the group; Windows always terminates
//! the job object. Unix children
//! that deliberately detach into a new session/group escape this containment;
//! commands must stay in the foreground. Abrupt termination of the application
//! cannot run Unix drop cleanup. Windows jobs also terminate on handle closure.

use std::{collections::VecDeque, io, path::Path, process::Stdio, time::Duration};

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

const OUTPUT_LIMIT: usize = 4096;
const TRUNCATED: &str = "[вывод обрезан]\n";
#[cfg(unix)]
const GRACE_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// The shell's exit status, explicit cancellation, or a runner failure.
#[derive(Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    /// The shell exited; `None` means a signal ended it on Unix.
    Completed(Option<i32>),
    /// Cancellation was requested and process-tree termination was initiated.
    Stopped,
    /// A spawn, wait, cleanup, or output collection operation failed.
    Failed(String),
}

/// Final status and the last 4 KiB of each output stream, including any marker.
#[must_use]
#[derive(Debug)]
pub struct ProcessResult {
    pub status: ProcessStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Runs one trusted command in `cwd` until its shell exits or `cancel` is set.
///
/// Output is drained continuously and decoded as UTF-8 with replacement for
/// invalid bytes. Unix cancellation allows up to two seconds for SIGTERM hooks.
/// Reaping then waits at most two seconds, followed by at most 500 ms for output
/// pipes to close. Dropping this future immediately requests tree
/// termination; Tokio handles child reaping on a best-effort basis.
pub async fn run(command: &str, cwd: &Path, cancel: CancellationToken) -> ProcessResult {
    if cancel.is_cancelled() {
        return ProcessResult {
            status: ProcessStatus::Stopped,
            stdout: String::new(),
            stderr: String::new(),
        };
    }

    let process = match spawn(command, cwd) {
        Ok(process) => process,
        Err(error) => {
            return ProcessResult {
                status: failure("Не удалось запустить действие", &error),
                stdout: String::new(),
                stderr: String::new(),
            };
        }
    };
    let mut tree = ProcessTree { process, armed: true };
    let stdout = tree.process.stdout().take();
    let stderr = tree.process.stderr().take();
    let finished = CancellationToken::new();
    let (status, stdout, stderr) = tokio::join!(
        async {
            let status = tokio::select! {
                biased;
                () = cancel.cancelled() => ProcessStatus::Stopped,
                // Only wait for the shell here. Wrapper waits can wait indefinitely
                // for background descendants, including via spawn_blocking.
                result = tree.process.inner_mut().wait() => match result {
                    Ok(status) => ProcessStatus::Completed(status.code()),
                    Err(error) => failure("Не удалось дождаться завершения действия", &error),
                },
            };
            let status = tree.finish(status).await;
            finished.cancel();
            status
        },
        capture(stdout, &finished),
        capture(stderr, &finished),
    );
    let status = match (&stdout.error, &stderr.error) {
        (Some(error), _) => failure("Не удалось прочитать вывод действия", error),
        (None, Some(error)) => failure("Не удалось прочитать поток ошибок действия", error),
        (None, None) => status,
    };
    ProcessResult { status, stdout: stdout.text, stderr: stderr.text }
}

fn spawn(command: &str, cwd: &Path) -> io::Result<Box<dyn ChildWrapper>> {
    #[cfg(unix)]
    let mut command = {
        let mut shell = Command::new("/bin/sh");
        shell.args(["-c", command]);
        shell
    };
    #[cfg(windows)]
    let mut command = shell(command)?;
    command.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut command = CommandWrap::from(command);
    command.wrap(KillOnDrop);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command
        .wrap(process_wrap::tokio::CreationFlags(
            windows::Win32::System::Threading::CREATE_NO_WINDOW,
        ))
        .wrap(process_wrap::tokio::JobObject);
    command.spawn()
}

#[cfg(windows)]
fn shell(command: &str) -> io::Result<Command> {
    let system_root = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "SystemRoot is missing"))?;
    let mut shell = Command::new(Path::new(&system_root).join("System32").join("cmd.exe"));
    // /S removes exactly these enclosing quotes. The configured text remains
    // shell syntax, including quoted program paths and operators.
    shell.args(["/D", "/S", "/C"]).raw_arg(format!("\"{command}\""));
    Ok(shell)
}

struct ProcessTree {
    process: Box<dyn ChildWrapper>,
    armed: bool,
}

impl ProcessTree {
    fn stop(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        #[cfg(unix)]
        let result = self.signal_group(nix::sys::signal::Signal::SIGKILL);
        #[cfg(windows)]
        let result = self.process.start_kill();
        if result.is_ok() {
            self.armed = false;
        }
        result
    }

    async fn finish(&mut self, status: ProcessStatus) -> ProcessStatus {
        #[cfg(unix)]
        let status = if matches!(status, ProcessStatus::Stopped) {
            match self.terminate().await {
                Ok(()) => status,
                Err(error) => failure("Не удалось мягко остановить действие", &error),
            }
        } else {
            status
        };
        if let Err(error) = self.stop() {
            return failure("Не удалось остановить процессы действия", &error);
        }
        match tokio::time::timeout(CLEANUP_TIMEOUT, self.process.inner_mut().wait()).await {
            Ok(Ok(_)) => status,
            Ok(Err(error)) => failure("Не удалось завершить процесс действия", &error),
            Err(_) => ProcessStatus::Failed("Истекло время ожидания остановки действия".into()),
        }
    }

    #[cfg(unix)]
    fn signal_group(&mut self, signal: nix::sys::signal::Signal) -> io::Result<()> {
        let result = self.process.signal(signal as i32);
        // Darwin can return EPERM when the group's only member is a zombie.
        // Reap our exited shell and retry the group (descendants may remain).
        // Never ignore EPERM: a second denial is still a real cleanup failure.
        let result = match result {
            Err(error)
                if error.raw_os_error() == Some(nix::errno::Errno::EPERM as i32)
                    && self.process.inner_mut().try_wait()?.is_some() =>
            {
                self.process.signal(signal as i32)
            }
            result => result,
        };
        result.or_else(ignore_exited_group)
    }

    #[cfg(unix)]
    async fn terminate(&mut self) -> io::Result<()> {
        self.signal_group(nix::sys::signal::Signal::SIGTERM)?;
        // The shell can exit before a VPN child finishes its cleanup hooks.
        // Preserve the whole grace period for descendants too; waiting only
        // for the shell would immediately kill a still-cleaning-up child.
        tokio::time::sleep(GRACE_TIMEOUT).await;
        Ok(())
    }
}

#[cfg(unix)]
fn ignore_exited_group(error: io::Error) -> io::Result<()> {
    if error.raw_os_error() == Some(nix::errno::Errno::ESRCH as i32) { Ok(()) } else { Err(error) }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            // Drop cannot report a return value. Do not log the command, its
            // output, or error strings that might contain application data.
            tracing::warn!(kind = ?error.kind(), os_code = error.raw_os_error(), "Could not stop action process tree on drop");
        }
    }
}

fn failure(context: &str, error: &io::Error) -> ProcessStatus {
    ProcessStatus::Failed(format!(
        "{context} ({:?}, код ОС {:?})",
        error.kind(),
        error.raw_os_error()
    ))
}

struct Captured {
    text: String,
    error: Option<io::Error>,
}

async fn capture<R: AsyncRead + Unpin>(
    reader: Option<R>,
    finished: &CancellationToken,
) -> Captured {
    let mut tail = Tail { bytes: VecDeque::new(), truncated: false };
    let result = if let Some(mut reader) = reader {
        let mut read = std::pin::pin!(tail.read(&mut reader));
        tokio::select! {
            result = &mut read => result,
            () = finished.cancelled() => {
                tokio::time::timeout(DRAIN_TIMEOUT, read).await
                    .unwrap_or_else(|_| Err(io::Error::from(io::ErrorKind::TimedOut)))
            },
        }
    } else {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    };
    Captured { text: tail.into_text(), error: result.err() }
}

struct Tail {
    bytes: VecDeque<u8>,
    truncated: bool,
}

impl Tail {
    async fn read(&mut self, reader: &mut (impl AsyncRead + Unpin)) -> io::Result<()> {
        let mut buffer = [0_u8; 1024];
        loop {
            let count = reader.read(&mut buffer).await?;
            if count == 0 {
                return Ok(());
            }
            for byte in buffer.iter().take(count) {
                if self.bytes.len() == OUTPUT_LIMIT {
                    self.bytes.pop_front();
                    self.truncated = true;
                }
                self.bytes.push_back(*byte);
            }
        }
    }

    fn into_text(mut self) -> String {
        let mut text = String::from_utf8_lossy(self.bytes.make_contiguous()).into_owned();
        // Lossy decoding can expand invalid UTF-8, so enforce the byte bound
        // after decoding too, preserving valid character boundaries.
        if self.truncated || text.len() > OUTPUT_LIMIT {
            let start = text.len().saturating_sub(OUTPUT_LIMIT - TRUNCATED.len());
            text.drain(..text.ceil_char_boundary(start));
            text.insert_str(0, TRUNCATED);
        }
        text
    }
}
