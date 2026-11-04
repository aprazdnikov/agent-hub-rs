use super::{Action, Registration, State};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

const FILE: &str = "io.github.aprazdnikov.agent-hub.desktop";

pub(super) fn registration(program: &Path) -> Result<Arc<dyn Registration>, String> {
    let base = directories::BaseDirs::new().ok_or("Не найден домашний каталог")?;
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| base.home_dir().join(".config"));
    let system = std::env::var_os("XDG_CONFIG_DIRS").filter(|v| !v.is_empty()).map_or_else(
        || vec![PathBuf::from("/etc/xdg")],
        |v| std::env::split_paths(&v).filter(|p| p.is_absolute()).collect(),
    );
    Ok(Arc::new(Desktop {
        program: program.to_owned(),
        user: user.join("autostart").join(FILE),
        system: system.into_iter().map(|p| p.join("autostart").join(FILE)).collect(),
    }))
}

struct Desktop {
    program: PathBuf,
    user: PathBuf,
    system: Vec<PathBuf>,
}
impl Registration for Desktop {
    fn apply(&self, action: Action) -> Result<State, String> {
        match action {
            Action::Inspect => {}
            Action::Enable | Action::Disable => {
                // Inspect the destination even when replacing it: never overwrite a symlink,
                // directory or an unexpectedly large unrelated file.
                if let Some(previous) = read(&self.user)? {
                    let keys = entry(&previous)?;
                    if keys.get("X-Agent-Hub-Autostart") != Some(&"1")
                        || keys.get("Type") != Some(&"Application")
                    {
                        return Err(format!(
                            "{} не является регистрацией agent-hub. Сохраните или переименуйте этот файл вручную перед изменением автозапуска.",
                            self.user.display()
                        ));
                    }
                }
                let content = match action {
                    Action::Enable => {
                        executable(Path::new("/bin/sh"))?;
                        executable(&self.program)?;
                        format!(
                            "[Desktop Entry]\nType=Application\nName=agent-hub\nX-Agent-Hub-Autostart=1\nExec={}\nTerminal=false\nHidden=false\n",
                            exec(&self.program)?
                        )
                    }
                    Action::Disable => {
                        "[Desktop Entry]\nType=Application\nName=agent-hub\nX-Agent-Hub-Autostart=1\nHidden=true\n"
                            .to_owned()
                    }
                    Action::Inspect => return self.inspect(),
                };
                crate::atomic::write_atomic(&self.user, content.as_bytes())
                    .map_err(|e| format!("Не удалось изменить {}: {e}", self.user.display()))?;
            }
        }
        self.inspect()
    }
}

impl Desktop {
    fn inspect(&self) -> Result<State, String> {
        for path in std::iter::once(&self.user).chain(&self.system) {
            let Some(content) = read(path)? else { continue };
            let keys = match entry(&content) {
                Ok(keys) => keys,
                Err(reason) => return Ok(State::NeedsAttention(reason)),
            };
            if keys.get("Hidden") == Some(&"true")
                || keys.get("X-GNOME-Autostart-enabled") == Some(&"false")
            {
                return Ok(State::Disabled);
            }
            if keys.keys().any(|key| {
                !matches!(
                    *key,
                    "Type"
                        | "Name"
                        | "Comment"
                        | "Icon"
                        | "Version"
                        | "Exec"
                        | "Terminal"
                        | "Hidden"
                        | "X-GNOME-Autostart-enabled"
                        | "X-Agent-Hub-Autostart"
                )
            }) || keys.get("Type") != Some(&"Application")
                || keys.get("Terminal").is_some_and(|v| *v != "false")
                || keys.get("Hidden").is_some_and(|v| *v != "false")
                || keys.get("X-GNOME-Autostart-enabled").is_some_and(|v| *v != "true")
            {
                return Ok(State::NeedsAttention("Запись автозапуска содержит изменённые условия. Включите заново для обновления регистрации.".into()));
            }
            let expected = exec(&self.program)?;
            if keys.get("Exec") != Some(&expected.as_str()) {
                return Ok(State::NeedsAttention("В автозапуске указан другой путь или команда. Включите заново для обновления регистрации.".into()));
            }
            return Ok(
                match executable(&self.program).and_then(|()| executable(Path::new("/bin/sh"))) {
                    Ok(()) => State::Enabled,
                    Err(reason) => State::NeedsAttention(reason),
                },
            );
        }
        Ok(State::Disabled)
    }
}

const MAX_FILE: u64 = 16 * 1024;
fn read(path: &Path) -> Result<Option<String>, String> {
    let failure = |e: io::Error| format!("Не удалось прочитать {}: {e}", path.display());
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(failure(e)),
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE {
        return Err(format!("{}: ожидался обычный файл до 16 КиБ", path.display()));
    }
    let mut text = String::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(failure)?
        .take(MAX_FILE + 1)
        .read_to_string(&mut text)
        .map_err(failure)?;
    if text.len() as u64 > MAX_FILE {
        return Err("Запись автозапуска превышает 16 КиБ".into());
    }
    Ok(Some(text))
}

fn entry(text: &str) -> Result<HashMap<&str, &str>, String> {
    let mut keys = HashMap::new();
    let mut inside = false;
    let mut found = false;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if line.starts_with('[') {
            inside = line == "[Desktop Entry]";
            if inside && found {
                return Err("Повторный раздел Desktop Entry".into());
            }
            found |= inside;
        } else if inside {
            let Some((key, value)) = line.split_once('=') else {
                return Err("Повреждённая запись автозапуска".into());
            };
            if keys.insert(key.trim(), value.trim()).is_some() {
                return Err("Повторный параметр автозапуска".into());
            }
        }
    }
    if !found {
        return Err("В записи автозапуска отсутствует Desktop Entry".into());
    }
    Ok(keys)
}

fn executable(path: &Path) -> Result<(), String> {
    let metadata =
        path.metadata().map_err(|e| format!("Программа {} недоступна: {e}", path.display()))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err("Путь программы не является исполняемым файлом".into());
    }
    Ok(())
}

fn exec(path: &Path) -> Result<String, String> {
    let raw = path.to_str().ok_or("Путь программы должен быть UTF-8")?;
    if !path.is_absolute() || raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err("Для автозапуска нужен абсолютный путь без управляющих символов".into());
    }
    // A fixed script execs literal argv; the path is never inserted into shell source.
    // Direct Exec cannot represent '=' and GLib checks '%' before field-code expansion.
    Ok(format!("/bin/sh -c {} agent-hub {} --autostart", argument("exec \"$@\""), argument(raw)))
}

fn argument(raw: &str) -> String {
    // Exec has two escape layers: argument quoting, then desktop-entry string escaping.
    let mut quoted = String::new();
    for c in raw.chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            quoted.push('\\');
        }
        if c == '%' {
            quoted.push('%');
        }
        quoted.push(c);
    }
    format!("\"{}\"", quoted.replace('\\', "\\\\"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture(root: &Path) -> Desktop {
        let program = root.join("программа с пробелами");
        fs::write(&program, b"executable fixture").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        Desktop {
            program,
            user: root.join("user/autostart").join(FILE),
            system: vec![root.join("system/autostart").join(FILE)],
        }
    }

    #[test]
    fn enable_disable_reinspect_and_update_keep_os_state_authoritative() {
        let temp = tempfile::tempdir().unwrap();
        let backend = fixture(temp.path());
        assert_eq!(backend.apply(Action::Inspect), Ok(State::Disabled));
        assert!(!backend.user.exists());
        assert_eq!(backend.apply(Action::Enable), Ok(State::Enabled));
        let registered = fs::read(&backend.user).unwrap();
        fs::write(&backend.program, b"updated executable").unwrap();
        assert_eq!(backend.apply(Action::Inspect), Ok(State::Enabled));
        assert_eq!(fs::read(&backend.user).unwrap(), registered);
        assert_eq!(backend.apply(Action::Enable), Ok(State::Enabled));
        assert_eq!(backend.apply(Action::Disable), Ok(State::Disabled));
        assert_eq!(backend.apply(Action::Disable), Ok(State::Disabled));
        fs::remove_file(&backend.user).unwrap();
        assert_eq!(backend.apply(Action::Inspect), Ok(State::Disabled));
    }

    #[test]
    fn user_hidden_entry_masks_system_autostart() {
        let temp = tempfile::tempdir().unwrap();
        let backend = fixture(temp.path());
        backend.apply(Action::Enable).unwrap();
        fs::create_dir_all(backend.system[0].parent().unwrap()).unwrap();
        fs::rename(&backend.user, &backend.system[0]).unwrap();
        assert_eq!(backend.apply(Action::Inspect), Ok(State::Enabled));
        assert_eq!(backend.apply(Action::Disable), Ok(State::Disabled));
        assert!(backend.system[0].exists());
        assert!(fs::read_to_string(&backend.user).unwrap().contains("Hidden=true"));
    }

    #[test]
    fn external_changes_and_old_paths_are_not_reported_as_enabled() {
        let temp = tempfile::tempdir().unwrap();
        let mut backend = fixture(temp.path());
        backend.apply(Action::Enable).unwrap();
        let original = fs::read_to_string(&backend.user).unwrap();
        fs::write(&backend.user, format!("{original}X-GNOME-Autostart-enabled=false\n")).unwrap();
        assert_eq!(backend.apply(Action::Inspect), Ok(State::Disabled));
        fs::write(&backend.user, format!("{original}OnlyShowIn=GNOME;\n")).unwrap();
        assert!(matches!(backend.apply(Action::Inspect), Ok(State::NeedsAttention(_))));
        fs::write(&backend.user, &original).unwrap();
        let moved = temp.path().join("new program");
        fs::rename(&backend.program, &moved).unwrap();
        assert!(matches!(backend.apply(Action::Inspect), Ok(State::NeedsAttention(_))));
        backend.program = moved;
        assert!(matches!(backend.apply(Action::Inspect), Ok(State::NeedsAttention(_))));
        assert_eq!(backend.apply(Action::Enable), Ok(State::Enabled));
    }

    #[test]
    fn symlinks_oversized_and_non_files_are_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let backend = fixture(temp.path());
        fs::create_dir_all(backend.user.parent().unwrap()).unwrap();
        symlink(&backend.program, &backend.user).unwrap();
        assert!(backend.apply(Action::Enable).is_err());
        assert!(backend.apply(Action::Disable).is_err());
        assert_eq!(fs::read(&backend.program).unwrap(), b"executable fixture");
        fs::remove_file(&backend.user).unwrap();
        fs::create_dir(&backend.user).unwrap();
        assert!(backend.apply(Action::Inspect).is_err());
        fs::remove_dir(&backend.user).unwrap();
        fs::write(&backend.user, vec![b'x'; 20000]).unwrap();
        assert!(backend.apply(Action::Inspect).is_err());
        assert!(backend.apply(Action::Enable).is_err());
    }

    #[test]
    fn missing_or_non_executable_program_cannot_enable_but_can_disable() {
        let temp = tempfile::tempdir().unwrap();
        let backend = fixture(temp.path());
        fs::set_permissions(&backend.program, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(backend.apply(Action::Enable).is_err());
        fs::remove_file(&backend.program).unwrap();
        assert!(backend.apply(Action::Enable).is_err());
        assert_eq!(backend.apply(Action::Disable), Ok(State::Disabled));
    }

    #[test]
    fn foreign_and_malformed_files_are_preserved_on_enable_and_disable() {
        let temp = tempfile::tempdir().unwrap();
        let backend = fixture(temp.path());
        fs::create_dir_all(backend.user.parent().unwrap()).unwrap();
        for content in [
            "user-owned data",
            "[Desktop Entry]\nX-Agent-Hub-Autostart=1\nHidden=true\nHidden=false\n",
        ] {
            fs::write(&backend.user, content).unwrap();
            for action in [Action::Enable, Action::Disable] {
                assert!(backend.apply(action).is_err());
                assert_eq!(fs::read_to_string(&backend.user).unwrap(), content);
            }
        }
    }

    #[test]
    fn exec_escapes_both_desktop_layers_and_rejects_unrepresentable_paths() {
        assert_eq!(
            argument("/tmp/Проект $x \"q\" `id` \\ %.exe"),
            r#""/tmp/Проект \\$x \\"q\\" \\`id\\` \\\\ %%.exe""#
        );
        for path in ["relative", "/tmp/bad\npath"] {
            assert!(exec(Path::new(path)).is_err());
        }
        assert!(entry("[Desktop Entry]\nHidden=true\nHidden=false\n").is_err());
    }

    #[test]
    #[ignore = "requires desktop-file-validate and gio; launches only a temporary fixture"]
    fn desktop_entry_launches_literal_path_through_gio() {
        let temp = tempfile::tempdir().unwrap();
        let mut backend = fixture(temp.path());
        backend.program =
            temp.path().join("Программа = $x `id` 'q' \"q\" \\ %name $(touch injected)");
        fs::write(
            &backend.program,
            "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\" > \"$HUB_AUTOSTART_OUTPUT\"\n",
        )
        .unwrap();
        fs::set_permissions(&backend.program, fs::Permissions::from_mode(0o700)).unwrap();
        backend.apply(Action::Enable).unwrap();
        assert!(
            std::process::Command::new("desktop-file-validate")
                .arg(&backend.user)
                .status()
                .unwrap()
                .success()
        );
        let output = temp.path().join("launched");
        assert!(
            std::process::Command::new("gio")
                .arg("launch")
                .arg(&backend.user)
                .env("HUB_AUTOSTART_OUTPUT", &output)
                .current_dir(temp.path())
                .status()
                .unwrap()
                .success()
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !output.exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            fs::read_to_string(output).unwrap(),
            format!("{}\n--autostart\n", backend.program.display())
        );
        assert!(!temp.path().join("injected").exists());
    }
}
