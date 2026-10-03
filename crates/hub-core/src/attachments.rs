//! Where user uploads land in the session directory and how the agent is told about them.

use std::path::{Path, PathBuf};

use crate::domain::MessageId;

// Relative to the session cwd, so the agent reads uploads without leaving its project.
pub const UPLOADS_DIR: [&str; 2] = [".agent-hub", "uploads"];
// Keeps uploads out of the project's git status without touching its own .gitignore.
pub const GITIGNORE: &str = "*\n";
// Bot API getFile refuses larger files.
pub const MAX_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;
// Bot API sendDocument refuses larger files.
pub const MAX_SEND_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_FILENAME_LENGTH: usize = 100;

#[must_use]
pub fn uploads_dir(cwd: &Path) -> PathBuf {
    UPLOADS_DIR.iter().fold(cwd.to_path_buf(), |path, part| path.join(part))
}

/// A single path component that cannot traverse, hide, or overflow.
#[must_use]
pub fn safe_filename(raw: Option<&str>, fallback: &str) -> String {
    let base = raw.unwrap_or_default().rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '_' })
        .collect();
    let name = cleaned.trim_start_matches('.');
    if name.trim_matches(['.', '_']).is_empty() {
        return fallback.to_owned();
    }
    if name.chars().count() <= MAX_FILENAME_LENGTH {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, suffix)) if suffix.chars().count() < MAX_FILENAME_LENGTH / 2 => {
            let keep = MAX_FILENAME_LENGTH - suffix.chars().count() - 1;
            format!("{}.{suffix}", stem.chars().take(keep).collect::<String>())
        }
        Some(_) | None => name.chars().take(MAX_FILENAME_LENGTH).collect(),
    }
}

/// Message ids are unique per chat, so uploads from different messages never collide.
#[must_use]
pub fn upload_path(cwd: &Path, message: MessageId, filename: Option<&str>) -> PathBuf {
    uploads_dir(cwd).join(format!("{}-{}", message.0, safe_filename(filename, "file")))
}

#[must_use]
pub fn prompt_text(text: &str, files: &[PathBuf]) -> String {
    if files.is_empty() {
        return text.to_owned();
    }
    let listing = std::iter::once("Приложенные файлы:".to_owned())
        .chain(files.iter().map(|path| format!("- {}", path.display())))
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() { listing } else { format!("{}\n\n{listing}", text.trim()) }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(Some("report.pdf"), "report.pdf")]
    #[case(Some("Отчёт за май.xlsx"), "Отчёт_за_май.xlsx")]
    #[case(Some("../../etc/passwd"), "passwd")]
    #[case(Some("..\\..\\win.ini"), "win.ini")]
    #[case(Some(".env"), "env")]
    #[case(Some("..."), "file")]
    #[case(Some(""), "file")]
    #[case(None, "file")]
    #[case(Some("a:b*c?.txt"), "a_b_c_.txt")]
    fn safe_filenames(#[case] raw: Option<&str>, #[case] expected: &str) {
        assert_eq!(safe_filename(raw, "file"), expected);
    }

    #[test]
    fn safe_filename_keeps_extension_when_truncating() {
        let name = safe_filename(Some(&format!("{}.tar.gz", "x".repeat(300))), "file");
        assert!(name.chars().count() <= MAX_FILENAME_LENGTH);
        assert_eq!(Path::new(&name).extension().and_then(|ext| ext.to_str()), Some("gz"));
    }

    #[test]
    fn safe_filename_truncates_long_suffix() {
        let name = safe_filename(Some(&format!("a.{}", "я".repeat(300))), "file");
        assert_eq!(name.chars().count(), MAX_FILENAME_LENGTH);
    }

    #[test]
    fn upload_path_stays_in_uploads_dir() {
        let cwd = PathBuf::from("project");
        assert_eq!(
            upload_path(&cwd, MessageId(42), Some("../../../x.txt")),
            cwd.join(".agent-hub").join("uploads").join("42-x.txt")
        );
    }

    #[test]
    fn prompt_text_lists_files_after_text() {
        let files = [PathBuf::from("u/1-a.pdf"), PathBuf::from("u/2-b.csv")];
        assert_eq!(
            prompt_text("сравни", &files),
            format!(
                "сравни\n\nПриложенные файлы:\n- {}\n- {}",
                files[0].display(),
                files[1].display()
            )
        );
    }

    #[test]
    fn prompt_text_without_files_is_unchanged() {
        assert_eq!(prompt_text("привет", &[]), "привет");
    }

    #[test]
    fn prompt_text_with_only_files() {
        let file = PathBuf::from("a");
        assert_eq!(prompt_text("  ", &[file]), "Приложенные файлы:\n- a");
    }
}
