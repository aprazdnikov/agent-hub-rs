//! Hermes keeps sessions as rows of `state.db`; they move through Hermes's own `SessionDB`
//! export and import, run by an embedded script inside Hermes's Python. No shell: arguments
//! are a list (each value glued to its flag, so none can pass for an option), data travel as
//! files in a private temporary directory, the answer only through the result file. Hermes's
//! own output is discarded, never logged: it can quote session content.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use hub_agent::archive::{
    ArchiveError, Entry, Exported, SessionArchive, entry_under, file_stem, private_file,
};
use hub_agent::cli::hide_std_window;
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::render::truncate;
use hub_core::snapshot::{BackendData, RelPath};
use serde_json::Value;

/// The `state.db` schema the script was checked against (`hermes_state_common.SCHEMA_VERSION`).
pub const SCHEMA_VERSION: u32 = 31;
pub const SESSION_FILE: &str = "hermes-session.json";
const SCRIPT: &str = include_str!("archive.py");
const SCRIPT_NAME: &str = "agent_hub_snapshot.py";
const TIMEOUT: Duration = Duration::from_mins(2);
const POLL: Duration = Duration::from_millis(50);
const RESULT_LIMIT: u64 = 1024 * 1024;
const HEAD_LIMIT: u64 = 4096;
const DETAIL_LIMIT: usize = 300;
const IDENTIFIER_LIMIT: usize = 64;
const FAILED: &str = "Hermes не выполнил перенос: подробности в логе";

/// How to run a Python file inside Hermes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    /// The published launcher: `hermes --run-module cProfile -o <stats> <script> …`.
    Launcher(PathBuf),
    /// A classic console script's interpreter: `python -I <script> …`.
    Python(PathBuf),
}

/// Windows: a venv's `python.exe` next to `hermes.exe`, else the launcher; elsewhere the file's
/// first lines decide.
fn launch_for(cli: &Path) -> Result<Launch, ArchiveError> {
    let real = fs::canonicalize(cli)?;
    if cfg!(windows) {
        let python = real.with_file_name("python.exe");
        return Ok(if python.is_file() { Launch::Python(python) } else { Launch::Launcher(real) });
    }
    let mut head = Vec::new();
    File::open(&real)?.take(HEAD_LIMIT).read_to_end(&mut head)?;
    classify(&real, &String::from_utf8_lossy(&head))
}

/// Pure: the first lines of a POSIX `hermes` file → how to run Python there.
fn classify(cli: &Path, head: &str) -> Result<Launch, ArchiveError> {
    let unknown = || {
        ArchiveError::Unsupported(format!(
            "Не удалось определить Python Hermes по {}",
            cli.display()
        ))
    };
    let mut lines = head.lines();
    let interpreter =
        lines.next().and_then(|line| line.strip_prefix("#!")).map(str::trim).ok_or_else(unknown)?;
    let program = Path::new(interpreter.split_whitespace().next().ok_or_else(unknown)?);
    let rest: Vec<&str> = lines.collect();
    if program.file_name().is_some_and(|name| name == "sh") {
        if let Some(python) = rest.iter().find_map(|line| shim_python(line)) {
            return Ok(Launch::Python(PathBuf::from(python)));
        }
        return if rest.iter().any(|line| line.starts_with("exec ")) {
            Ok(Launch::Launcher(cli.to_path_buf()))
        } else {
            Err(unknown())
        };
    }
    let python =
        program.file_name().and_then(OsStr::to_str).is_some_and(|name| name.starts_with("python"));
    // A POSIX shebang is absolute by POSIX rules, whatever host parses it.
    if python && interpreter.starts_with('/') {
        Ok(Launch::Python(program.to_path_buf()))
    } else {
        Err(unknown())
    }
}

/// pip/distlib's long-path shim: `'''exec' "<python>" "$0" "$@"`.
fn shim_python(line: &str) -> Option<&str> {
    let tail = line.strip_prefix("'''exec' ")?;
    match tail.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').map(|(python, _)| python),
        None => tail.split_whitespace().next(),
    }
}

/// The script's subcommands.
#[derive(Debug, Clone, Copy)]
enum Op {
    Present,
    Export,
    Import,
}

impl Op {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Export => "export",
            Self::Import => "import",
        }
    }
}

/// `--name=value` as one argument: argparse cannot take a value such as `--x` for an option.
fn flag(name: &str, value: impl AsRef<OsStr>) -> OsString {
    let mut arg = OsString::from(format!("--{name}="));
    arg.push(value);
    arg
}

/// The running script; dropped before it exits, it is killed and reaped.
struct Script(Child);

impl Drop for Script {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Err(error) = self.0.kill() {
            tracing::warn!(kind = ?error.kind(), "Hermes snapshot script could not be killed");
        }
        if let Err(error) = self.0.wait() {
            tracing::warn!(kind = ?error.kind(), "Hermes snapshot script could not be reaped");
        }
    }
}

/// Waits up to `timeout`; past it the caller drops `script`, which kills it: a wedged Hermes
/// must not hold the session forever.
fn wait(script: &mut Script, timeout: Duration) -> Result<ExitStatus, ArchiveError> {
    let started = Instant::now();
    loop {
        if let Some(status) = script.0.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            return Err(ArchiveError::Unsupported(format!(
                "Hermes не ответил за {} с",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(POLL);
    }
}

fn read_result(path: &Path) -> Result<Option<Value>, ArchiveError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() > RESULT_LIMIT {
        return Err(ArchiveError::Malformed("ответ Hermes не похож на результат".to_owned()));
    }
    let mut text = String::new();
    File::open(path)?.take(RESULT_LIMIT).read_to_string(&mut text)?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|_| ArchiveError::Malformed("ответ Hermes — не JSON".to_owned()))
}

/// A protocol word or an exception class name, as the script reports them; any other text from
/// the result (it may quote the session) is logged as `?`.
fn identifier(text: &str) -> &str {
    let word = text.len() <= IDENTIFIER_LIMIT
        && !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if word { text } else { "?" }
}

pub struct HermesArchive {
    launch: Launch,
    profile: Option<String>,
    version: String,
}

impl HermesArchive {
    /// Reads the head of `cli` to learn how to run Python inside this Hermes.
    ///
    /// # Errors
    /// `Io` when `cli` cannot be read, `Unsupported` when it is neither a launcher nor a
    /// console script.
    pub fn new(cli: &Path, profile: Option<String>, version: String) -> Result<Self, ArchiveError> {
        Ok(Self { launch: launch_for(cli)?, profile, version })
    }

    fn command(&self, work: &Path) -> Command {
        match &self.launch {
            Launch::Launcher(cli) => {
                let mut command = Command::new(cli);
                command.args(["--run-module", "cProfile", "-o"]).arg(work.join("profile.out"));
                command
            }
            Launch::Python(python) => {
                let mut command = Command::new(python);
                command.arg("-I");
                command
            }
        }
    }

    fn run(&self, op: Op, session: &str, extra: &[OsString]) -> Result<Value, ArchiveError> {
        let work = tempfile::Builder::new().prefix("agent-hub-hermes-").tempdir()?;
        let script = work.path().join(SCRIPT_NAME);
        private_file(&script)?.write_all(SCRIPT.as_bytes())?;
        let result = work.path().join("result.json");
        let mut command = self.command(work.path());
        command
            .arg(&script)
            .arg(op.as_str())
            .arg(flag("result", &result))
            .arg(flag("schema", SCHEMA_VERSION.to_string()))
            .arg(flag("session", session))
            .args(self.profile.iter().map(|profile| flag("profile", profile)))
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_std_window(&mut command);
        // Declared after `work`, so it is killed before the directory goes away.
        let mut running = Script(command.spawn()?);
        let status = match wait(&mut running, TIMEOUT) {
            Ok(status) => status,
            Err(error) => {
                tracing::warn!(op = op.as_str(), "Hermes snapshot script timed out and is killed");
                return Err(error);
            }
        };
        let Some(value) = read_result(&result)? else {
            tracing::warn!(op = op.as_str(), %status, "Hermes snapshot script left no result");
            return Err(ArchiveError::Unsupported(FAILED.to_owned()));
        };
        self.judge(op, &value).map(|()| value)
    }

    fn judge(&self, op: Op, value: &Value) -> Result<(), ArchiveError> {
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(());
        }
        let kind = value.get("error").and_then(Value::as_str).unwrap_or("internal");
        let detail = value.get("detail").and_then(Value::as_str).unwrap_or_default();
        // Words only: a detail other than an exception class may quote the session.
        tracing::warn!(
            op = op.as_str(),
            kind = identifier(kind),
            exception = identifier(detail),
            "Hermes refused or failed the snapshot operation"
        );
        Err(match kind {
            "schema" => ArchiveError::Unsupported(format!(
                "Hermes {} не поддерживается для переноса",
                self.version
            )),
            "profile" => {
                ArchiveError::Unsupported("Профиль Hermes из настроек не найден".to_owned())
            }
            "exists" => ArchiveError::Ambiguous,
            "payload" | "import" => ArchiveError::Malformed(format!(
                "Hermes не принял выгрузку сессии: {}",
                truncate(detail, DETAIL_LIMIT)
            )),
            _ => ArchiveError::Unsupported(FAILED.to_owned()),
        })
    }
}

impl SessionArchive for HermesArchive {
    fn export(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError> {
        let out = dest.join(SESSION_FILE);
        let value = self.run(Op::Export, file_stem(session)?, &[flag("out", &out)])?;
        if value.get("found").and_then(Value::as_bool) != Some(true) {
            return Err(ArchiveError::NotFound);
        }
        let file = RelPath::parse(SESSION_FILE)?;
        if entry_under(dest, &file)? != Entry::File {
            return Err(ArchiveError::Malformed("Hermes не записал выгрузку".to_owned()));
        }
        Ok(Exported {
            files: vec![file],
            data: BackendData::Hermes { profile: self.profile.clone() },
            agent_version: self.version.clone(),
        })
    }

    fn present(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        _data: &BackendData,
    ) -> Result<bool, ArchiveError> {
        self.run(Op::Present, file_stem(session)?, &[])?
            .get("present")
            .and_then(Value::as_bool)
            .ok_or_else(|| ArchiveError::Malformed("Hermes не ответил, есть ли сессия".to_owned()))
    }

    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError> {
        match data {
            BackendData::Hermes { profile } => {
                if *profile != self.profile {
                    tracing::info!(
                        from = ?profile,
                        to = ?self.profile,
                        "Hermes session moves to the configured profile"
                    );
                }
            }
            BackendData::Claude { .. } | BackendData::Codex | BackendData::Qwen => {
                return Err(ArchiveError::Malformed("снимок другого бэкенда".to_owned()));
            }
        }
        let file = match files {
            [file] if file.as_str() == SESSION_FILE => file,
            _ => return Err(ArchiveError::Malformed(format!("ожидался один файл {SESSION_FILE}"))),
        };
        // The script opens the path itself: every component under the snapshot is checked first.
        match entry_under(src, file)? {
            Entry::File => {}
            Entry::Link => return Err(ArchiveError::Link(file.to_string())),
            Entry::Missing | Entry::Dir | Entry::Other => {
                return Err(ArchiveError::Malformed(format!("{file}: не обычный файл")));
            }
        }
        let cwd = cwd
            .as_path()
            .to_str()
            .ok_or_else(|| ArchiveError::Unsupported("рабочий каталог не в UTF-8".to_owned()))?;
        self.run(Op::Import, file_stem(session)?, &[flag("in", file.under(src)), flag("cwd", cwd)])
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::pip(
        "#!/home/me/.venv/bin/python3\nimport sys\n",
        Some(Launch::Python(PathBuf::from("/home/me/.venv/bin/python3")))
    )]
    #[case::pip_flags(
        "#!/opt/py/bin/python3.12 -E\n",
        Some(Launch::Python(PathBuf::from("/opt/py/bin/python3.12")))
    )]
    #[case::long_path_shim(
        "#!/bin/sh\n'''exec' \"/very long/bin/python\" \"$0\" \"$@\"\n' '''\n",
        Some(Launch::Python(PathBuf::from("/very long/bin/python")))
    )]
    #[case::bare_shim(
        "#!/bin/sh\n'''exec' /venv/bin/python \"$0\" \"$@\"\n",
        Some(Launch::Python(PathBuf::from("/venv/bin/python")))
    )]
    #[case::launcher(
        "#!/bin/sh\nexec /store/bin/python3 -I -c 'import os' \"$@\"\n",
        Some(Launch::Launcher(PathBuf::from("/bin/hermes")))
    )]
    #[case::forwarder(
        "#!/bin/sh\nexec /root/.hermes/bin/hermes \"$@\"\n",
        Some(Launch::Launcher(PathBuf::from("/bin/hermes")))
    )]
    #[case::env_python("#!/usr/bin/env python3\n", None)]
    #[case::relative_python("#!python3\n", None)]
    #[case::binary("\u{7f}ELF\u{2}\u{1}", None)]
    #[case::other_shell_script("#!/bin/sh\necho hi\n", None)]
    #[case::empty("", None)]
    fn launch_comes_from_the_hermes_file(#[case] head: &str, #[case] expected: Option<Launch>) {
        assert_eq!(classify(Path::new("/bin/hermes"), head).ok(), expected);
    }

    #[rstest]
    #[case::class("JSONDecodeError", "JSONDecodeError")]
    #[case::message("KeyError: 'secret text'", "?")]
    #[case::empty("", "?")]
    fn only_words_are_logged(#[case] detail: &str, #[case] logged: &str) {
        assert_eq!(identifier(detail), logged);
    }

    #[test]
    fn a_value_never_passes_for_an_option() {
        assert_eq!(flag("profile", "--evil"), OsString::from("--profile=--evil"));
    }

    #[cfg(unix)]
    #[test]
    fn a_wedged_script_is_killed_at_the_deadline() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let started = Instant::now();
        let mut script = Script(child);
        assert!(matches!(
            wait(&mut script, Duration::from_millis(200)),
            Err(ArchiveError::Unsupported(_))
        ));
        drop(script);
        assert!(started.elapsed() < Duration::from_secs(10));
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!alive.success(), "the script outlived its deadline");
    }

    #[cfg(unix)]
    #[test]
    fn a_finished_script_reports_its_status() {
        let mut script = Script(Command::new("true").spawn().unwrap());
        assert!(wait(&mut script, Duration::from_secs(10)).unwrap().success());
    }
}
