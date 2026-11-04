use super::{Action, State};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunEntry {
    Absent,
    Current,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Approval {
    Allowed,
    Blocked,
    Unknown,
}

struct Snapshot {
    run: RunEntry,
    approval: Approval,
    executable: Result<(), String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Effect {
    None,
    Write,
    Delete,
}

fn command_line(program: &[u16]) -> Result<Vec<u16>, String> {
    if program.is_empty() || program.iter().any(|unit| *unit < 32 || *unit == 34) {
        return Err(
            "Путь к исполняемому файлу содержит символы, несовместимые с автозапуском Windows."
                .to_owned(),
        );
    }
    let command: Vec<_> = std::iter::once(34)
        .chain(program.iter().copied())
        .chain("\" --autostart".encode_utf16())
        .collect();
    // Microsoft's Run-key contract limits the complete command to 260 characters.
    if command.len() > 260 {
        return Err("Путь к исполняемому файлу слишком длинный для автозапуска Windows. Переместите Agent Hub в каталог с более коротким путём.".to_owned());
    }
    Ok(command)
}

fn approval(bytes: Option<&[u8]>) -> Approval {
    // StartupApproved is owned by Windows, not a documented writable app API.
    // Recognize known 12-byte records and leave unfamiliar formats to Windows.
    // https://github.com/electron/electron/blob/main/shell/browser/browser_win.cc
    // https://github.com/unslothai/unsloth/blob/main/studio/src-tauri/src/main.rs
    match bytes {
        None | Some([0 | 2 | 6, 0, 0, 0, _, _, _, _, _, _, _, _]) => Approval::Allowed,
        Some([3 | 7, 0, 0, 0, _, _, _, _, _, _, _, _]) => Approval::Blocked,
        Some(_) => Approval::Unknown,
    }
}

fn inspect(snapshot: Snapshot) -> State {
    let Snapshot { run, approval, executable } = snapshot;
    match run {
        RunEntry::Absent => State::Disabled,
        RunEntry::Other => State::NeedsAttention(
            "Запись автозапуска Windows указывает на другой исполняемый файл или команду. Включите автозапуск, чтобы обновить запись для этой установки.".to_owned(),
        ),
        RunEntry::Current => {
            if let Err(error) = executable {
                return State::NeedsAttention(error);
            }
            match approval {
                Approval::Allowed => State::Enabled,
                Approval::Blocked => State::NeedsAttention(
                    "Windows отключила автозапуск Agent Hub. Включите Agent Hub в разделе «Параметры → Приложения → Автозагрузка» или в диспетчере задач, на вкладке автозагрузки.".to_owned(),
                ),
                Approval::Unknown => State::NeedsAttention(
                    "Не удалось распознать разрешение Windows на автозапуск. Проверьте Agent Hub в разделе «Параметры → Приложения → Автозагрузка» или в диспетчере задач, на вкладке автозагрузки.".to_owned(),
                ),
            }
        }
    }
}

fn effect(action: Action, snapshot: &Snapshot) -> Result<Effect, String> {
    match action {
        Action::Inspect => Ok(Effect::None),
        Action::Enable => {
            snapshot.executable.clone()?;
            match snapshot.run {
                RunEntry::Absent | RunEntry::Other => Ok(Effect::Write),
                RunEntry::Current => Ok(Effect::None),
            }
        }
        Action::Disable => match snapshot.run {
            RunEntry::Absent => Ok(Effect::None),
            RunEntry::Current | RunEntry::Other => Ok(Effect::Delete),
        },
    }
}

#[cfg(windows)]
pub(super) fn registration(
    program: &std::path::Path,
) -> Result<std::sync::Arc<dyn super::Registration>, String> {
    native::registration(program)
}

#[cfg(windows)]
mod native {
    use std::{
        io::ErrorKind,
        os::windows::ffi::OsStrExt,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use winreg::{
        RegKey, RegValue,
        enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_SZ},
    };

    use super::super::Registration;
    use super::{
        Action, Approval, Effect, RunEntry, Snapshot, State, approval, command_line, effect,
        inspect,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const APPROVAL_KEY: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const VALUE_NAME: &str = "Agent Hub";

    struct WindowsRegistration {
        program: PathBuf,
        command: Vec<u8>,
    }

    pub(super) fn registration(program: &Path) -> Result<Arc<dyn Registration>, String> {
        if !program.is_absolute() {
            return Err(
                "Для автозапуска Windows нужен абсолютный путь к исполняемому файлу.".to_owned()
            );
        }
        let program_units: Vec<_> = program.as_os_str().encode_wide().collect();
        let command = command_line(&program_units)?
            .into_iter()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        Ok(Arc::new(WindowsRegistration { program: program.to_owned(), command }))
    }

    impl Registration for WindowsRegistration {
        fn apply(&self, action: Action) -> Result<State, String> {
            let snapshot = self.snapshot(action)?;
            match effect(action, &snapshot)? {
                Effect::None => return Ok(inspect(snapshot)),
                Effect::Write => {
                    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
                        .create_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
                        .map_err(|error| {
                            format!("Не удалось открыть раздел автозапуска Windows: {error}")
                        })?;
                    key.set_raw_value(
                        VALUE_NAME,
                        &RegValue { vtype: REG_SZ, bytes: self.command.as_slice().into() },
                    )
                    .map_err(|error| {
                        format!("Не удалось добавить Agent Hub в автозапуск Windows: {error}")
                    })?;
                }
                Effect::Delete => remove_run_value()?,
            }
            // Re-read authoritative state, including a concurrent Windows approval change.
            // Never change StartupApproved, even after an explicit Enable request.
            let state = inspect(self.snapshot(Action::Inspect)?);
            if matches!(action, Action::Disable) && !matches!(state, State::Disabled) {
                return Err(
                    "Запись автозапуска Windows изменилась во время отключения. Обновите состояние и повторите попытку."
                        .to_owned(),
                );
            }
            if matches!(action, Action::Enable) && matches!(state, State::Disabled) {
                return Err("Запись автозапуска Windows исчезла во время включения. Обновите состояние и повторите попытку.".to_owned());
            }
            Ok(state)
        }
    }

    impl WindowsRegistration {
        fn snapshot(&self, action: Action) -> Result<Snapshot, String> {
            let run = match read_value(RUN_KEY)? {
                None => RunEntry::Absent,
                Some(value) if value.vtype == REG_SZ && value.bytes == self.command => {
                    RunEntry::Current
                }
                Some(_) => RunEntry::Other,
            };
            // Disabling remains possible if the binary moved or approval cannot be read.
            if matches!(action, Action::Disable)
                || (matches!(action, Action::Inspect) && run == RunEntry::Absent)
            {
                return Ok(Snapshot { run, approval: Approval::Allowed, executable: Ok(()) });
            }
            let approval = match read_value(APPROVAL_KEY)? {
                None => approval(None),
                Some(value) if value.vtype == REG_BINARY => approval(Some(&value.bytes)),
                Some(_) => Approval::Unknown,
            };
            let executable = std::fs::metadata(&self.program)
                .map_err(|error| format!("Исполняемый файл автозапуска {} недоступен: {error}. Восстановите файл или отключите автозапуск.", self.program.display()))
                .and_then(|metadata| {
                    if metadata.is_file() {
                        Ok(())
                    } else {
                        Err(format!("Путь автозапуска {} не указывает на файл. Восстановите исполняемый файл или отключите автозапуск.", self.program.display()))
                    }
                });
            Ok(Snapshot { run, approval, executable })
        }
    }

    fn read_value(path: &str) -> Result<Option<RegValue<'static>>, String> {
        let key =
            match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(path, KEY_QUERY_VALUE) {
                Ok(key) => key,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(format!(
                        "Не удалось прочитать раздел автозапуска Windows {path}: {error}"
                    ));
                }
            };
        match key.get_raw_value(VALUE_NAME) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "Не удалось прочитать запись Agent Hub в разделе автозапуска Windows {path}: {error}"
            )),
        }
    }

    fn remove_run_value() -> Result<(), String> {
        let key = match RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
        {
            Ok(key) => key,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!("Не удалось открыть раздел автозапуска Windows: {error}"));
            }
        };
        match key.delete_value(VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(format!("Не удалось удалить Agent Hub из автозапуска Windows: {error}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Action, Approval, Effect, RunEntry, Snapshot, State, approval, command_line, effect,
        inspect,
    };

    #[test]
    fn quotes_unicode_executable_with_spaces() -> Result<(), String> {
        let program: Vec<_> = r"C:\Users\Алиса\Agent Hub\agent-hub.exe".encode_utf16().collect();
        assert_eq!(
            command_line(&program)?,
            r#""C:\Users\Алиса\Agent Hub\agent-hub.exe" --autostart"#.encode_utf16().collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn rejects_command_injection_and_overlong_commands() {
        for program in [
            r#"C:\Hub\bad"name.exe"#.to_owned(),
            "C:\\Hub\\bad\0name.exe".to_owned(),
            "a".repeat(249),
        ] {
            assert!(command_line(&program.encode_utf16().collect::<Vec<_>>()).is_err());
        }
    }

    #[test]
    fn approval_decodes_windows_disables_and_reenables() {
        let cases = [
            (0, Approval::Allowed),
            (2, Approval::Allowed),
            (3, Approval::Blocked),
            (6, Approval::Allowed),
            (7, Approval::Blocked),
            (99, Approval::Unknown),
        ];
        for (state, expected) in cases {
            // A re-enabled entry may retain its previous disable timestamp.
            let bytes = [state, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
            assert_eq!(approval(Some(&bytes)), expected);
        }
    }

    #[test]
    fn unknown_approval_is_not_assumed_enabled() {
        assert!(matches!(approval(Some(&[2])), Approval::Unknown));
        assert!(matches!(approval(Some(&[2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])), Approval::Unknown));
        assert!(matches!(approval(None), Approval::Allowed));
    }

    #[test]
    fn absent_registration_defaults_to_disabled_without_a_binary() {
        assert!(matches!(
            inspect(Snapshot {
                run: RunEntry::Absent,
                approval: Approval::Blocked,
                executable: Err("missing executable".to_owned()),
            }),
            State::Disabled
        ));
    }

    #[test]
    fn inspection_reports_os_disables_stale_entries_and_missing_binaries() {
        let cases = [
            (RunEntry::Current, Approval::Blocked, Ok(())),
            (RunEntry::Current, Approval::Unknown, Ok(())),
            (RunEntry::Other, Approval::Allowed, Ok(())),
            (RunEntry::Current, Approval::Allowed, Err("missing executable".to_owned())),
        ];
        for (run, approval, executable) in cases {
            assert!(matches!(
                inspect(Snapshot { run, approval, executable }),
                State::NeedsAttention(_)
            ));
        }
    }

    #[test]
    fn current_registration_with_approval_is_enabled() {
        assert!(matches!(
            inspect(Snapshot {
                run: RunEntry::Current,
                approval: Approval::Allowed,
                executable: Ok(())
            }),
            State::Enabled
        ));
    }

    #[test]
    fn enable_and_disable_are_idempotent() -> Result<(), String> {
        let cases = [
            (Action::Enable, RunEntry::Absent, Effect::Write),
            (Action::Enable, RunEntry::Other, Effect::Write),
            (Action::Enable, RunEntry::Current, Effect::None),
            (Action::Disable, RunEntry::Current, Effect::Delete),
            (Action::Disable, RunEntry::Other, Effect::Delete),
            (Action::Disable, RunEntry::Absent, Effect::None),
            (Action::Inspect, RunEntry::Other, Effect::None),
        ];
        for (action, run, expected) in cases {
            assert_eq!(
                effect(action, &Snapshot { run, approval: Approval::Allowed, executable: Ok(()) })?,
                expected
            );
        }
        Ok(())
    }

    #[test]
    fn explicit_enable_keeps_windows_approval_authoritative() -> Result<(), String> {
        let snapshot =
            Snapshot { run: RunEntry::Current, approval: Approval::Blocked, executable: Ok(()) };
        assert_eq!(effect(Action::Enable, &snapshot)?, Effect::None);
        assert!(matches!(inspect(snapshot), State::NeedsAttention(_)));
        Ok(())
    }

    #[test]
    fn missing_binary_blocks_enable_but_not_disable() {
        let snapshot = Snapshot {
            run: RunEntry::Current,
            approval: Approval::Allowed,
            executable: Err("missing executable".to_owned()),
        };
        assert!(effect(Action::Enable, &snapshot).is_err());
        assert!(matches!(effect(Action::Disable, &snapshot), Ok(Effect::Delete)));
    }
}
