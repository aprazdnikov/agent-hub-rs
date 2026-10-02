//! Logs go to a daily file and to a ring buffer the window shows.

use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{self, Rotation};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, fmt as layer_fmt};

pub const CAPACITY: usize = 500;
const KEPT_FILES: usize = 14;
const REJECTED: &str = "rejected update";

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejected {
    pub chat: Option<i64>,
    pub user: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub time: String,
    pub level: Level,
    pub text: String,
    pub rejected: Option<Rejected>,
}

#[derive(Clone, Default)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<LogLine>>>,
    repaint: Arc<OnceLock<Repaint>>,
}

impl LogBuffer {
    #[must_use]
    pub fn lines(&self) -> Vec<LogLine> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect()
    }

    /// The window exists only after logging starts, so it subscribes later.
    pub fn on_change(&self, repaint: Repaint) {
        // A second subscriber is never registered; ignoring it keeps the first.
        let _ = self.repaint.set(repaint);
    }

    fn push(&self, line: LogLine) {
        {
            let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
            if lines.len() == CAPACITY {
                lines.pop_front();
            }
            lines.push_back(line);
        }
        if let Some(repaint) = self.repaint.get() {
            repaint();
        }
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
    chat: Option<i64>,
    user: Option<u64>,
}

impl Fields {
    fn extra(&mut self, field: &Field, value: &dyn fmt::Display) {
        // Records bridged from the `log` crate carry their origin as `log.*` fields.
        if field.name().starts_with("log.") {
            return;
        }
        // Writing into a String cannot fail.
        let _ = write!(self.rest, " {}={value}", field.name());
    }
}

impl Visit for Fields {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "chat_id" {
            self.chat = Some(value);
        }
        self.extra(field, &value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "user_id" => self.user = Some(value),
            "chat_id" => self.chat = i64::try_from(value).ok(),
            _other => {}
        }
        self.extra(field, &value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            self.extra(field, &value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.extra(field, &format_args!("{value:?}"));
        }
    }
}

impl<S: Subscriber> Layer<S> for LogBuffer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut time = String::new();
        // Formatting the clock into a String cannot fail.
        let _ = SystemTime.format_time(&mut Writer::new(&mut time));
        let rejected = (fields.message == REJECTED)
            .then_some(Rejected { chat: fields.chat, user: fields.user });
        self.push(LogLine {
            time,
            level: *event.metadata().level(),
            text: format!("{}{}", fields.message, fields.rest),
            rejected,
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("не удалось открыть каталог логов: {0}")]
    Appender(#[from] rolling::InitError),
    #[error("логирование уже настроено: {0}")]
    Subscriber(#[from] tracing_subscriber::util::TryInitError),
}

fn filter() -> Targets {
    // HTTP internals log request URLs, and Telegram's carry the bot token.
    Targets::new()
        .with_default(Level::INFO)
        .with_target("reqwest", Level::WARN)
        .with_target("hyper", Level::WARN)
        .with_target("hyper_util", Level::WARN)
}

pub fn init(logs: &Path, buffer: LogBuffer) -> Result<WorkerGuard, LogError> {
    let appender = rolling::Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("agent-hub")
        .filename_suffix("log")
        .max_log_files(KEPT_FILES)
        .build(logs)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::registry()
        .with(layer_fmt::layer().with_writer(writer).with_ansi(false).with_filter(filter()))
        .with(buffer.with_filter(filter()))
        .try_init()?;
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    fn capture(emit: impl FnOnce()) -> Vec<LogLine> {
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::registry().with(buffer.clone());
        tracing::subscriber::with_default(subscriber, emit);
        buffer.lines()
    }

    #[test]
    fn message_and_fields_form_the_text() {
        let lines = capture(|| tracing::info!(turns = 3, "turn finished"));
        let [line] = lines.as_slice() else { panic!("one line expected: {lines:?}") };
        assert_eq!(line.level, Level::INFO);
        assert_eq!(line.text, "turn finished turns=3");
        assert_eq!(line.rejected, None);
    }

    #[test]
    fn rejected_update_carries_its_ids() {
        let lines = capture(|| {
            tracing::warn!(chat_id = -100_i64, user_id = Some(5_u64), "rejected update");
            tracing::warn!(chat_id = Some(-7_i64), user_id = 9_u64, "rejected update");
        });
        let rejected: Vec<_> = lines.iter().map(|line| line.rejected).collect();
        assert_eq!(
            rejected,
            [
                Some(Rejected { chat: Some(-100), user: Some(5) }),
                Some(Rejected { chat: Some(-7), user: Some(9) }),
            ]
        );
    }

    #[test]
    fn only_the_last_lines_are_kept() {
        let lines = capture(|| {
            for index in 0..CAPACITY + 3 {
                tracing::info!(index, "tick");
            }
        });
        assert_eq!(lines.len(), CAPACITY);
        assert_eq!(lines.first().map(|line| line.text.as_str()), Some("tick index=3"));
    }

    #[test]
    fn records_of_the_log_crate_reach_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let buffer = LogBuffer::default();
        let _guard = init(dir.path(), buffer.clone()).unwrap();
        log::error!(target: "teloxide::update_listeners", "getUpdates conflict");
        // The subscriber is global, so events of tests on other threads land here too.
        let bridged = buffer.lines().into_iter().find(|line| line.text == "getUpdates conflict");
        assert_eq!(bridged.map(|line| line.level), Some(Level::ERROR));
    }

    #[test]
    fn change_wakes_the_window() {
        let buffer = LogBuffer::default();
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        buffer.on_change(Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let subscriber = tracing_subscriber::registry().with(buffer.clone());
        tracing::subscriber::with_default(subscriber, || tracing::info!("one"));
        assert_eq!(woken.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
