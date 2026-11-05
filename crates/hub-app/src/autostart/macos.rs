//! A legacy per-user `LaunchAgent` also supports the distributed bare executable.
//! Authorization on macOS 13+ is checked with Apple's public Service Management API:
//! <https://developer.apple.com/documentation/servicemanagement/smappservice/statusforlegacyplist(at:)>

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::{Action, Registration, State};

const LABEL: &str = "io.github.aprazdnikov.agent-hub.autostart";
const MAX_OUTPUT: u64 = 64 * 1024;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const PLIST_PREFIX: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<!-- Managed by agent-hub: launch-at-login v1. -->\n\
<plist version=\"1.0\"><dict>\n\
<key>Label</key><string>io.github.aprazdnikov.agent-hub.autostart</string>\n\
<key>AssociatedBundleIdentifiers</key><array><string>io.github.aprazdnikov.agent-hub</string></array>\n\
<key>LimitLoadToSessionType</key><string>Aqua</string>\n\
<key>RunAtLoad</key><true/>\n\
<key>ProgramArguments</key><array><string>";
const PLIST_SUFFIX: &str = "</string><string>--autostart</string></array>\n</dict></plist>\n";

// The script is constant. The plist path is an argv value, never executable source.
// statusForLegacyURL: takes the plist URL, not the executable or application URL.
// Detect the API directly: JXA cannot reliably expose NSOperatingSystemVersion fields.
const AUTHORIZATION_SCRIPT: &str = include_str!("macos-authorization.js");

struct MacRegistration {
    home: PathBuf,
    plist: PathBuf,
    program: PathBuf,
    contents: String,
    launchctl: PathBuf,
    osascript: PathBuf,
    id: PathBuf,
}

#[cfg(target_os = "macos")]
pub(super) fn registration(program: &Path) -> Result<std::sync::Arc<dyn Registration>, String> {
    let home = directories::BaseDirs::new()
        .ok_or("Не удалось определить домашнюю папку пользователя")?
        .home_dir()
        .to_path_buf();
    Ok(std::sync::Arc::new(MacRegistration {
        plist: home.join("Library/LaunchAgents").join(format!("{LABEL}.plist")),
        home,
        program: program.to_path_buf(),
        contents: render_plist(program)?,
        launchctl: PathBuf::from("/bin/launchctl"),
        osascript: PathBuf::from("/usr/bin/osascript"),
        id: PathBuf::from("/usr/bin/id"),
    }))
}

impl Registration for MacRegistration {
    fn apply(&self, action: Action) -> Result<State, String> {
        let deadline = Instant::now() + OPERATION_TIMEOUT;
        let existing = self.read_plist()?;
        if action == Action::Inspect && existing.is_none() {
            return Ok(State::Disabled);
        }
        if action == Action::Inspect && existing.as_deref() != Some(&self.contents) {
            return Ok(State::NeedsAttention(
                "Путь автозапуска macOS устарел. Включите автозапуск заново для текущей копии приложения."
                    .to_owned(),
            ));
        }
        match action {
            Action::Inspect => {
                if let Err(error) = verify_executable(&self.program) {
                    return Ok(State::NeedsAttention(error));
                }
            }
            Action::Enable => verify_executable(&self.program)?,
            Action::Disable => {}
        }
        let uid = run_command(&self.id, &[OsStr::new("-u")], deadline)?.checked("id -u")?;
        let uid = uid
            .trim()
            .parse::<u32>()
            .map_err(|error| format!("Не удалось определить UID пользователя: {error}"))?;
        let domain = format!("gui/{uid}");
        let service = format!("{domain}/{LABEL}");
        match action {
            Action::Inspect => self.inspect_authorization(&domain, deadline),
            Action::Enable => {
                self.write_plist(existing.as_deref())?;
                run_command(
                    &self.launchctl,
                    &[OsStr::new("enable"), OsStr::new(&service)],
                    deadline,
                )?
                .checked("launchctl enable")?;
                // Do not unload a running Agent Hub: that would terminate the settings UI.
                // A repaired path takes effect at the next login if the job is already loaded.
                if !run_command(
                    &self.launchctl,
                    &[OsStr::new("print"), OsStr::new(&service)],
                    deadline,
                )?
                .success
                {
                    run_command(
                        &self.launchctl,
                        &[OsStr::new("bootstrap"), OsStr::new(&domain), self.plist.as_os_str()],
                        deadline,
                    )?
                    .checked("launchctl bootstrap")?;
                }
                self.inspect_authorization(&domain, deadline)
            }
            Action::Disable => {
                // The persisted override prevents future launches without killing this app.
                run_command(
                    &self.launchctl,
                    &[OsStr::new("disable"), OsStr::new(&service)],
                    deadline,
                )?
                .checked("launchctl disable")?;
                if self.read_plist()? != existing {
                    return Err(
                        "Запись автозапуска изменилась. Обновите состояние и повторите действие."
                            .to_owned(),
                    );
                }
                if existing.is_some() {
                    fs::remove_file(&self.plist).map_err(|error| {
                        format!(
                            "macOS отключила автозапуск, но удалить {} не удалось: {error}",
                            self.plist.display()
                        )
                    })?;
                }
                Ok(State::Disabled)
            }
        }
    }
}

impl MacRegistration {
    fn inspect_authorization(&self, domain: &str, deadline: Instant) -> Result<State, String> {
        let overrides = run_command(
            &self.launchctl,
            &[OsStr::new("print-disabled"), OsStr::new(domain)],
            deadline,
        )?
        .checked("launchctl print-disabled")?;
        if disabled_override(&overrides)? {
            return Ok(State::Disabled);
        }
        let authorization = run_command(
            &self.osascript,
            &[
                OsStr::new("-l"),
                OsStr::new("JavaScript"),
                OsStr::new("-e"),
                OsStr::new(AUTHORIZATION_SCRIPT),
                self.plist.as_os_str(),
            ],
            deadline,
        )?
        .checked("проверка разрешения macOS")?;
        Ok(authorization_state(&authorization))
    }

    fn read_plist(&self) -> Result<Option<String>, String> {
        if !self.directory_exists()? {
            return Ok(None);
        }
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(&self.plist)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "Не удалось безопасно прочитать {}: {error}",
                    self.plist.display()
                ));
            }
        };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_OUTPUT {
            return Err(format!(
                "{} должна быть обычным небольшим файлом, а не ссылкой или каталогом",
                self.plist.display()
            ));
        }
        let mut contents = String::new();
        file.take(MAX_OUTPUT + 1)
            .read_to_string(&mut contents)
            .map_err(|error| error.to_string())?;
        if u64::try_from(contents.len()).map_err(|error| error.to_string())? > MAX_OUTPUT {
            return Err("Запись автозапуска слишком велика".to_owned());
        }
        managed_plist(&contents).map_err(|error| format!("{}: {error}", self.plist.display()))?;
        Ok(Some(contents))
    }

    fn directory_exists(&self) -> Result<bool, String> {
        for directory in [self.home.join("Library"), self.home.join("Library/LaunchAgents")] {
            match fs::symlink_metadata(&directory) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(format!(
                        "{} должна быть каталогом, а не символической ссылкой",
                        directory.display()
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => {
                    return Err(format!("Не удалось проверить {}: {error}", directory.display()));
                }
            }
        }
        Ok(true)
    }

    fn write_plist(&self, previous: Option<&str>) -> Result<(), String> {
        if previous == Some(&self.contents) {
            return Ok(());
        }
        for directory in [self.home.join("Library"), self.home.join("Library/LaunchAgents")] {
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!("Не удалось создать {}: {error}", directory.display()));
                }
            }
            let metadata = fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "{} должна быть каталогом, а не символической ссылкой",
                    directory.display()
                ));
            }
        }
        let mut temporary = tempfile::NamedTempFile::new_in(self.home.join("Library/LaunchAgents"))
            .map_err(|error| error.to_string())?;
        temporary.write_all(self.contents.as_bytes()).map_err(|error| error.to_string())?;
        temporary.as_file().sync_all().map_err(|error| error.to_string())?;
        if self.read_plist()?.as_deref() != previous {
            return Err("Запись автозапуска изменилась. Обновите состояние и повторите действие."
                .to_owned());
        }
        if previous.is_some() {
            temporary.persist(&self.plist).map_err(|error| error.to_string())?;
        } else {
            temporary.persist_noclobber(&self.plist).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

fn render_plist(program: &Path) -> Result<String, String> {
    let text = program.to_str().ok_or("Путь приложения не является UTF-8")?;
    if !program.is_absolute()
        || text
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{fffe}' | '\u{ffff}'))
    {
        return Err("Для автозапуска нужен абсолютный путь без управляющих символов".to_owned());
    }
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    Ok(format!("{PLIST_PREFIX}{escaped}{PLIST_SUFFIX}"))
}

fn verify_executable(program: &Path) -> Result<(), String> {
    let metadata = fs::metadata(program).map_err(|error| {
        format!(
            "Исполняемый файл {} недоступен: {error}. Запустите установленную копию Agent Hub и включите автозапуск заново.",
            program.display()
        )
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!(
            "{} не является исполняемым файлом. Запустите установленную копию Agent Hub и включите автозапуск заново.",
            program.display()
        ));
    }
    Ok(())
}

fn managed_plist(contents: &str) -> Result<(), String> {
    match contents.strip_prefix(PLIST_PREFIX).and_then(|rest| rest.strip_suffix(PLIST_SUFFIX)) {
        Some(program) if !program.is_empty() && !program.contains(['<', '>']) => Ok(()),
        _ => Err("Файл не создан Agent Hub или изменён вручную; он не будет перезаписан. Проверьте его и переименуйте, чтобы включить автозапуск.".to_owned()),
    }
}

fn disabled_override(output: &str) -> Result<bool, String> {
    let body = output.trim().strip_prefix("disabled services = {").and_then(|rest| rest.strip_suffix('}'))
        .ok_or("Неизвестный формат ответа launchctl print-disabled; состояние автозапуска не подтверждено")?;
    let mut found = None;
    for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let (name, value) =
            line.split_once("=>").ok_or("Повреждён ответ launchctl print-disabled")?;
        let value = match value.trim() {
            "true" | "disabled" => true,
            "false" | "enabled" => false,
            _ => return Err("Неизвестное состояние в ответе launchctl print-disabled".to_owned()),
        };
        let name = name
            .trim()
            .strip_prefix('"')
            .and_then(|name| name.strip_suffix('"'))
            .ok_or("Неизвестная запись в ответе launchctl print-disabled")?;
        if name == LABEL && found.replace(value).is_some() {
            return Err("Повторяющаяся запись в ответе launchctl print-disabled".to_owned());
        }
    }
    Ok(found.unwrap_or(false))
}

fn authorization_state(output: &str) -> State {
    match output.trim() {
        "enabled" | "legacy" => State::Enabled,
        "requires-approval" => State::NeedsAttention("Разрешите Agent Hub в Системных настройках macOS → Основные → Объекты входа → Разрешить в фоновом режиме, затем обновите состояние.".to_owned()),
        "not-registered" | "not-found" => State::NeedsAttention("macOS не подтвердила регистрацию автозапуска. Проверьте Объекты входа в Системных настройках и включите автозапуск заново.".to_owned()),
        _ => State::NeedsAttention("macOS вернула неизвестное разрешение автозапуска. Обновите состояние и проверьте Объекты входа в Системных настройках.".to_owned()),
    }
}

struct CommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    fn checked(self, operation: &str) -> Result<String, String> {
        if self.success {
            Ok(self.stdout)
        } else {
            Err(format!(
                "{operation}: {}. Проверьте Объекты входа в Системных настройках macOS и обновите состояние.",
                self.stderr.trim()
            ))
        }
    }
}

fn run_command(
    program: &Path,
    arguments: &[&OsStr],
    deadline: Instant,
) -> Result<CommandOutput, String> {
    if Instant::now() >= deadline {
        return Err("Проверка автозапуска macOS превысила 5 секунд. Обновите состояние.".to_owned());
    }
    // Anonymous files avoid pipe-reader threads hanging when a descendant inherits stdout.
    // Polling bounds capture size and memory; only trusted system binaries run in production.
    let mut stdout = tempfile::tempfile().map_err(|error| error.to_string())?;
    let mut stderr = tempfile::tempfile().map_err(|error| error.to_string())?;
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone().map_err(|error| error.to_string())?)
        .stderr(stderr.try_clone().map_err(|error| error.to_string())?)
        .spawn()
        .map_err(|error| format!("Не удалось запустить {}: {error}", program.display()))?;
    let result = loop {
        let poll = (|| {
            if stdout.metadata()?.len() > MAX_OUTPUT || stderr.metadata()?.len() > MAX_OUTPUT {
                return Err(std::io::Error::other("слишком большой ответ команды"));
            }
            child.try_wait()
        })();
        match poll {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error.to_string()),
            Ok(None) if Instant::now() >= deadline => {
                break Err("команда превысила 5 секунд".to_owned());
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
        }
    };
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            let cleanup = child.kill().and_then(|()| child.wait().map(|_| ()));
            return Err(match cleanup {
                Ok(()) => format!("{}: {error}; обновите состояние автозапуска", program.display()),
                Err(cleanup) => format!(
                    "{}: {error}; завершить команду не удалось: {cleanup}",
                    program.display()
                ),
            });
        }
    };
    Ok(CommandOutput {
        success: status.success(),
        stdout: read_output(&mut stdout)?,
        stderr: read_output(&mut stderr)?,
    })
}

fn read_output(file: &mut File) -> Result<String, String> {
    file.seek(SeekFrom::Start(0)).map_err(|error| error.to_string())?;
    let mut output = String::new();
    file.take(MAX_OUTPUT + 1).read_to_string(&mut output).map_err(|error| error.to_string())?;
    if u64::try_from(output.len()).map_err(|error| error.to_string())? > MAX_OUTPUT {
        return Err("Слишком большой ответ команды macOS".to_owned());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        ffi::OsStr,
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::Path,
        time::{Duration, Instant},
    };

    use super::{
        LABEL, MacRegistration, authorization_state, disabled_override, render_plist, run_command,
    };
    use crate::autostart::{Action, Registration, State};

    type TestResult = Result<(), Box<dyn Error>>;

    fn executable(path: &Path, script: &str) -> TestResult {
        // Parallel fork/exec may briefly inherit another test's writable script
        // descriptor (ETXTBSY). Execute a checked-in launcher; write only its input.
        fs::write(path.with_extension("script"), script)?;
        if !path.exists() {
            symlink(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/autostart-command"),
                path,
            )?;
        }
        Ok(())
    }

    fn fixture() -> Result<(tempfile::TempDir, MacRegistration), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let home = directory.path().join("home space & '中文'");
        fs::create_dir(&home)?;
        let program = home.join("agent-hub");
        // This placeholder is inspected and chmod'ed, never executed.
        fs::write(&program, b"application fixture")?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
        let launchctl = directory.path().join("launchctl-fixture");
        executable(
            &launchctl,
            &format!(
                r#"#!/bin/sh
case "$1" in
  enable) printf 'enabled' > "$0.state" ;;
  disable) printf 'disabled' > "$0.state" ;;
  print) test -f "$0.loaded" ;;
  bootstrap) printf 'loaded' > "$0.loaded" ;;
  print-disabled)
    printf 'disabled services = {{\n"{LABEL}" => '
    if test -f "$0.state"; then cat "$0.state"; else printf 'enabled'; fi
    printf '\n}}\n'
    ;;
  *) exit 99 ;;
esac
"#
            ),
        )?;
        let osascript = directory.path().join("osascript-fixture");
        executable(
            &osascript,
            "#!/bin/sh\ntest \"$#\" -eq 5 && test -f \"$5\" || exit 98\nprintf 'enabled\\n'\n",
        )?;
        let id = directory.path().join("id-fixture");
        executable(&id, "#!/bin/sh\nprintf '501\\n'\n")?;
        let registration = MacRegistration {
            plist: home.join("Library/LaunchAgents").join(format!("{LABEL}.plist")),
            home,
            contents: render_plist(&program)?,
            program,
            launchctl,
            osascript,
            id,
        };
        Ok((directory, registration))
    }

    #[test]
    fn respects_launchd_disabled_override() {
        for value in ["true", "disabled"] {
            assert_eq!(
                disabled_override(&format!("disabled services = {{\n\"{LABEL}\" => {value}\n}}")),
                Ok(true)
            );
        }
    }

    #[test]
    fn does_not_guess_when_launchctl_output_changes() {
        assert!(disabled_override("unrecognized output").is_err());
    }

    #[test]
    fn revoked_background_consent_needs_attention() {
        assert!(matches!(authorization_state("requires-approval"), State::NeedsAttention(_)));
    }

    #[test]
    fn accepts_enabled_overrides_and_an_absent_label() {
        for output in [
            "disabled services = {}".to_owned(),
            format!("disabled services = {{\n\"{LABEL}\" => enabled\n}}"),
            format!("disabled services = {{\n\"{LABEL}\" => false\n}}"),
            format!("disabled services = {{\n\"{LABEL}.other\" => disabled\n}}"),
        ] {
            assert_eq!(disabled_override(&output), Ok(false));
        }
    }

    #[test]
    fn rejects_ambiguous_launchd_overrides() {
        for output in [
            format!("disabled services = {{\n\"{LABEL}\" => enabled\n\"{LABEL}\" => disabled\n}}"),
            format!("disabled services = {{\n\"{LABEL}\" => unknown\n}}"),
        ] {
            assert!(disabled_override(&output).is_err());
        }
    }

    #[test]
    fn unknown_or_unregistered_authorization_is_not_enabled() {
        for status in ["not-registered", "not-found", "unknown", ""] {
            assert!(matches!(authorization_state(status), State::NeedsAttention(_)));
        }
    }

    #[test]
    fn plist_quotes_paths_as_data_and_passes_login_flag() -> TestResult {
        let output = render_plist(Path::new(
            "/Applications/中文 & <Hub> \"'$.app/Contents/MacOS/agent-hub",
        ))?;
        assert!(output.contains("<string>/Applications/中文 &amp; &lt;Hub&gt; &quot;&apos;$.app/Contents/MacOS/agent-hub</string><string>--autostart</string>"));
        Ok(())
    }

    #[test]
    fn plist_rejects_relative_and_control_character_paths() {
        for program in
            ["relative/agent-hub", "/Applications/agent\nhub", "/Applications/agent\rhub"]
        {
            assert!(render_plist(Path::new(program)).is_err());
        }
    }

    #[test]
    fn enable_and_disable_are_idempotent_and_inspect_reads_os_state() -> TestResult {
        let (_directory, registration) = fixture()?;
        let observed = [
            Action::Inspect,
            Action::Enable,
            Action::Enable,
            Action::Inspect,
            Action::Disable,
            Action::Disable,
            Action::Inspect,
        ]
        .map(|action| registration.apply(action));
        assert_eq!(
            observed,
            [
                Ok(State::Disabled),
                Ok(State::Enabled),
                Ok(State::Enabled),
                Ok(State::Enabled),
                Ok(State::Disabled),
                Ok(State::Disabled),
                Ok(State::Disabled)
            ]
        );
        Ok(())
    }

    #[test]
    fn existing_plist_cannot_hide_os_disabled_state() -> TestResult {
        let (_directory, registration) = fixture()?;
        registration.apply(Action::Enable)?;
        fs::write(registration.launchctl.with_extension("state"), "disabled")?;
        assert_eq!(registration.apply(Action::Inspect)?, State::Disabled);
        Ok(())
    }

    #[test]
    fn existing_plist_cannot_hide_revoked_background_consent() -> TestResult {
        let (_directory, registration) = fixture()?;
        registration.apply(Action::Enable)?;
        executable(&registration.osascript, "#!/bin/sh\nprintf 'requires-approval\\n'\n")?;
        assert!(matches!(registration.apply(Action::Inspect)?, State::NeedsAttention(_)));
        Ok(())
    }

    #[test]
    fn removed_executable_cannot_be_enabled() -> TestResult {
        let (_directory, registration) = fixture()?;
        fs::remove_file(registration.home.join("agent-hub"))?;
        assert!(registration.apply(Action::Enable).is_err());
        assert!(!registration.plist.exists());
        Ok(())
    }

    #[test]
    fn removed_executable_needs_attention_and_can_be_disabled() -> TestResult {
        let (_directory, registration) = fixture()?;
        registration.apply(Action::Enable)?;
        fs::remove_file(registration.home.join("agent-hub"))?;
        assert!(matches!(registration.apply(Action::Inspect)?, State::NeedsAttention(_)));
        assert_eq!(registration.apply(Action::Disable)?, State::Disabled);
        Ok(())
    }

    #[test]
    fn non_executable_program_is_not_enabled() -> TestResult {
        let (_directory, registration) = fixture()?;
        registration.apply(Action::Enable)?;
        fs::set_permissions(
            registration.home.join("agent-hub"),
            fs::Permissions::from_mode(0o600),
        )?;
        assert!(matches!(registration.apply(Action::Inspect)?, State::NeedsAttention(_)));
        assert!(registration.apply(Action::Enable).is_err());
        Ok(())
    }

    #[test]
    fn enable_repairs_an_owned_entry_after_moving_the_app() -> TestResult {
        let (_directory, mut registration) = fixture()?;
        registration.apply(Action::Enable)?;
        let moved = registration.home.join("moved Agent Hub");
        fs::rename(&registration.program, &moved)?;
        registration.program = moved;
        registration.contents = render_plist(&registration.program)?;
        assert!(matches!(registration.apply(Action::Inspect)?, State::NeedsAttention(_)));
        registration.apply(Action::Enable)?;
        assert_eq!(fs::read_to_string(&registration.plist)?, registration.contents);
        Ok(())
    }

    #[test]
    fn unknown_existing_file_is_preserved() -> TestResult {
        let (_directory, registration) = fixture()?;
        fs::create_dir_all(registration.home.join("Library/LaunchAgents"))?;
        fs::write(&registration.plist, "user-owned data")?;
        assert!(registration.apply(Action::Enable).is_err());
        assert!(registration.apply(Action::Disable).is_err());
        assert_eq!(fs::read_to_string(&registration.plist)?, "user-owned data");
        Ok(())
    }

    #[test]
    fn plist_symlink_cannot_overwrite_or_delete_its_target() -> TestResult {
        let (directory, registration) = fixture()?;
        fs::create_dir_all(registration.home.join("Library/LaunchAgents"))?;
        let target = directory.path().join("unrelated");
        fs::write(&target, "keep")?;
        symlink(&target, &registration.plist)?;
        assert!(registration.apply(Action::Enable).is_err());
        assert!(registration.apply(Action::Disable).is_err());
        assert_eq!(fs::read_to_string(target)?, "keep");
        Ok(())
    }

    #[test]
    fn launchagents_directory_symlink_is_refused() -> TestResult {
        let (directory, registration) = fixture()?;
        fs::create_dir(registration.home.join("Library"))?;
        symlink(directory.path(), registration.home.join("Library/LaunchAgents"))?;
        assert!(registration.apply(Action::Enable).is_err());
        assert!(!directory.path().join(format!("{LABEL}.plist")).exists());
        Ok(())
    }

    #[test]
    fn commands_are_bounded_even_when_a_descendant_keeps_stdout() -> TestResult {
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("inherited-stdout");
        executable(&script, "#!/bin/sh\nsleep 1 &\nprintf 'finished\\n'\n")?;
        let start = Instant::now();
        let output = run_command(&script, &[], start + Duration::from_millis(500))?;
        assert!(start.elapsed() < Duration::from_millis(500));
        assert_eq!(output.checked("fixture")?, "finished\n");
        Ok(())
    }

    #[test]
    fn command_timeout_terminates_the_child() {
        let start = Instant::now();
        assert!(
            run_command(
                Path::new("/bin/sleep"),
                &[OsStr::new("1")],
                start + Duration::from_millis(20)
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn excessive_command_output_is_rejected() {
        assert!(
            run_command(
                Path::new("/bin/sh"),
                &[OsStr::new("-c"), OsStr::new("head -c 70000 /dev/zero")],
                Instant::now() + Duration::from_secs(1)
            )
            .is_err()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_authorization_bridge_queries_only_a_fixture_path() -> TestResult {
        let directory = tempfile::tempdir()?;
        let plist = directory.path().join("not-a-registered-service.plist");
        let output = run_command(
            Path::new("/usr/bin/osascript"),
            &[
                OsStr::new("-l"),
                OsStr::new("JavaScript"),
                OsStr::new("-e"),
                OsStr::new(super::AUTHORIZATION_SCRIPT),
                plist.as_os_str(),
            ],
            Instant::now() + Duration::from_secs(5),
        )?
        .checked("Service Management fixture query")?;
        assert!(matches!(
            output.trim(),
            "legacy" | "enabled" | "requires-approval" | "not-registered" | "not-found"
        ));
        Ok(())
    }
}
