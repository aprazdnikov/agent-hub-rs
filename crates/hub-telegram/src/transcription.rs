//! Speech recognition boundary; adapters never submit tasks themselves.

use std::time::Duration;

use futures::future::BoxFuture;
use hub_core::speech::SpeechSettings;
use tokio_util::sync::CancellationToken;

pub const MAX_AUDIO_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_DURATION: Duration = Duration::from_mins(3);
pub const TIMEOUT: Duration = Duration::from_mins(3);
/// UTF-16 units, leaving room for the preview's explanation in one Telegram message.
pub const MAX_TEXT_UNITS: usize = 3000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpeechError {
    #[error("Распознавание выключено. Выберите способ в настройках на ПК или отправьте текст.")]
    Disabled,
    #[error("Распознавание отменено.")]
    Cancelled,
    #[error("Распознавание заняло больше 3 минут. Попробуйте более короткое сообщение или текст.")]
    Timeout,
    #[error("{0}")]
    Failed(String),
}

pub trait Transcriber: Send + Sync + 'static {
    fn transcribe(
        &self,
        settings: SpeechSettings,
        audio: Vec<u8>,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<String, SpeechError>>;
}
