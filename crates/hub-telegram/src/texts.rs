//! What the bot says, word for word as in the Python version.

use std::fmt::Display;
use std::path::Path;
use std::time::Duration;

use hub_core::attachments::UPLOADS_DIR;
use hub_core::domain::{AbsolutePath, BackendKind, Decision, ToolRequest, ToolUse, TopicSession};
use hub_core::escape::escape;
use hub_core::render::truncate;

pub const CREATE_TOPIC: &str = "Создайте тему в группе — каждая тема это отдельная сессия.";
pub const ALREADY_RUNNING: &str = "⏳ В этой теме уже выполняется задача. /stop — прервать.";
pub const NOTHING_TO_STOP: &str = "Нечего останавливать";
pub const NO_SESSION: &str = "Сессии нет — напишите задачу или /new";
pub const RUNNING: &str = "⏳ выполняется";
pub const WAITING: &str = "💤 ожидает";
pub const VOICE_UNSUPPORTED: &str =
    "🎤 Голосовые сообщения пока не поддерживаются — напишите текстом.";
pub const CWD_USAGE: &str = "Укажите путь: /cwd <путь>";
pub const STOPPED: &str = "⏹ Остановлено";
pub const INTERNAL_ERROR: &str = "💥 Внутренняя ошибка agent-hub, подробности в логе приложения";
pub const NEW_SESSION: &str = "🆕 Новая сессия";
pub const CWD_CHANGED: &str = "📁 Директория изменена";
pub const CONTEXT_RESET: &str = "🔄 Контекст сброшен";
pub const APPROVAL_STALE: &str = "Запрос уже неактуален";
pub const QUESTION_STALE: &str = "Вопрос уже неактуален";
pub const NOTHING_SELECTED: &str = "Отметьте хотя бы один вариант";
pub const QUEUE_FULL: &str = "⚠️ Слишком много сообщений в очереди — дождитесь ответа агента.";
pub const APPROVAL_UNSENT: &str = "Не удалось отправить запрос подтверждения в Telegram";
pub const QUESTION_UNSENT: &str = "Не удалось отправить вопрос в Telegram";
pub const CURRENT_SESSION: &str = "ℹ️ Текущая сессия";
pub const BACKEND_ALREADY: &str = "ℹ️ Бэкенд уже выбран";
pub const BACKEND_SWITCHED: &str = "🔀 Бэкенд изменён";
const APPROVAL_TEXT_LIMIT: usize = 3500;
const TOOL_CALL_TEXT_LIMIT: usize = 900;
const FAILURE_TEXT_LIMIT: usize = 3500;

#[must_use]
pub fn help(root: &Path) -> String {
    let uploads = UPLOADS_DIR.join("/");
    let backends = backend_names();
    format!(
        "Каждая тема этой группы — отдельная сессия агента.\n\n\
         Просто пишите задачу в теме. Команды:\n\
         /new [claude|codex|qwen] [путь] — новая сессия в этой теме (сброс контекста)\n\
         /backend [claude|codex|qwen] — сменить агента в этой теме (сброс контекста)\n\
         /cwd <путь> — сменить рабочую директорию (сброс контекста)\n\
         /reset — начать разговор заново в той же директории\n\
         /stop — прервать текущую задачу\n\
         /status — состояние сессии\n\
         /help — эта справка\n\n\
         Можно прикладывать фото (агент их видит) и файлы (сохраняются в {uploads} в\n\
         рабочей директории). Голосовые сообщения не поддерживаются.\n\n\
         Пути абсолютные или относительно корня: {}\n\
         Бэкенды: {backends}",
        root.display()
    )
}

#[must_use]
pub fn unknown_backend(name: &str) -> String {
    format!("⚠️ Неизвестный бэкенд {name}. Доступны: {}", backend_names())
}

fn backend_names() -> String {
    BackendKind::ALL.iter().map(|kind| kind.name()).collect::<Vec<_>>().join(", ")
}

#[must_use]
pub fn describe(title: &str, session: &TopicSession) -> String {
    format!(
        "{title}\nbackend: {}\ncwd: {}\nsession: {}",
        session.backend.name(),
        session.cwd.as_path().display(),
        session.session.as_ref().map_or("—", |id| id.as_str())
    )
}

#[must_use]
pub fn approval_html(tool: &ToolRequest) -> String {
    let summary = escape(&truncate(&tool.summary, APPROVAL_TEXT_LIMIT));
    format!("🔐 <b>{}</b>\n<pre>{summary}</pre>", escape(&tool.tool))
}

#[must_use]
pub fn tool_call_html(call: &ToolUse) -> String {
    let line = escape(&truncate(&call.summary, TOOL_CALL_TEXT_LIMIT));
    format!("🔧 <b>{}</b> <code>{line}</code>", escape(&call.tool))
}

#[must_use]
pub fn failure(reason: &str) -> String {
    truncate(&format!("❌ {reason}"), FAILURE_TEXT_LIMIT)
}

#[must_use]
pub fn warning(error: &dyn Display) -> String {
    format!("⚠️ {error}")
}

#[must_use]
pub fn verdict(decision: &Decision) -> &'static str {
    match decision {
        Decision::Allowed => "✅ Разрешено",
        Decision::Denied(_) => "❌ Запрещено",
    }
}

#[must_use]
pub fn no_answer(timeout: Duration) -> String {
    format!("Нет ответа пользователя за {} с", timeout.as_secs())
}

#[must_use]
pub fn outside_root(cwd: &AbsolutePath, root: &AbsolutePath) -> String {
    format!(
        "⚠️ Рабочая директория {} вне корня {}. Смените её: /cwd <путь>",
        cwd.as_path().display(),
        root.as_path().display()
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use hub_core::domain::{BackendKind, Denied, SessionId};

    use super::*;

    fn absolute(name: &str) -> AbsolutePath {
        let base = PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" });
        AbsolutePath::new(base.join(name)).unwrap()
    }

    #[test]
    fn unknown_backend_lists_the_known_ones() {
        assert_eq!(
            unknown_backend("gpt"),
            "⚠️ Неизвестный бэкенд gpt. Доступны: claude, codex, qwen"
        );
    }

    #[test]
    fn help_names_root_uploads_and_backends() {
        let root = absolute("");
        let text = help(root.as_path());
        assert!(text.starts_with("Каждая тема этой группы — отдельная сессия агента."));
        assert!(text.contains(".agent-hub/uploads"));
        assert!(text.contains(&root.as_path().display().to_string()));
        assert!(text.ends_with("Бэкенды: claude, codex, qwen"));
        assert!(text.contains(
            "/new [claude|codex|qwen] [путь] — новая сессия в этой теме (сброс контекста)"
        ));
        assert!(text.contains(
            "/backend [claude|codex|qwen] — сменить агента в этой теме (сброс контекста)"
        ));
    }

    #[test]
    fn describe_lists_backend_cwd_and_session() {
        let session = TopicSession::fresh(BackendKind::Claude, absolute("p"));
        assert_eq!(
            describe(NEW_SESSION, &session),
            format!(
                "🆕 Новая сессия\nbackend: claude\ncwd: {}\nsession: —",
                absolute("p").as_path().display()
            )
        );
        let resumed = session.with_session(SessionId::parse("abc"));
        assert!(describe(WAITING, &resumed).ends_with("session: abc"));
    }

    #[test]
    fn approval_escapes_and_truncates() {
        let tool =
            ToolRequest { tool: "Bash".to_owned(), summary: format!("a<b {}", "x".repeat(4000)) };
        let html = approval_html(&tool);
        assert!(html.starts_with("🔐 <b>Bash</b>\n<pre>a&lt;b "));
        assert!(html.ends_with("…</pre>"));
    }

    #[test]
    fn tool_call_is_one_line() {
        let call = ToolUse { tool: "Read".to_owned(), summary: "src/<main>.rs".to_owned() };
        assert_eq!(tool_call_html(&call), "🔧 <b>Read</b> <code>src/&lt;main&gt;.rs</code>");
    }

    #[test]
    fn failure_is_marked_and_truncated() {
        assert_eq!(failure("boom"), "❌ boom");
        assert_eq!(failure(&"x".repeat(5000)).chars().count(), 3500);
    }

    #[test]
    fn verdicts_and_timeouts() {
        assert_eq!(verdict(&Decision::Allowed), "✅ Разрешено");
        assert_eq!(verdict(&Decision::Denied(Denied::new("нет"))), "❌ Запрещено");
        assert_eq!(no_answer(Duration::from_mins(10)), "Нет ответа пользователя за 600 с");
        assert_eq!(warning(&"плохо"), "⚠️ плохо");
    }

    #[test]
    fn outside_root_suggests_cwd() {
        assert!(
            outside_root(&absolute("old"), &absolute("new")).ends_with("Смените её: /cwd <путь>")
        );
    }
}
