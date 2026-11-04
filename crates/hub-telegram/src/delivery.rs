//! Best-effort chat output must not stop the collector from journaling a finished answer.

use crate::{
    messenger::{Outgoing, Target},
    sender::Sender,
};
use hub_core::{
    domain::TopicKey,
    render::{TELEGRAM_TEXT_LIMIT, split_message},
    results::MAX_TEXT_BYTES,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
pub enum Style {
    Plain,
    Markdown,
    Html,
}
struct Item {
    style: Style,
    text: String,
}
pub struct Delivery {
    queue: mpsc::Sender<Item>,
    dropped: Arc<AtomicUsize>,
}
pub struct Receiver {
    queue: mpsc::Receiver<Item>,
    dropped: Arc<AtomicUsize>,
}

pub fn channel() -> (Delivery, Receiver) {
    let (queue, receiver) = mpsc::channel(32);
    let dropped = Arc::new(AtomicUsize::new(0));
    (Delivery { queue, dropped: dropped.clone() }, Receiver { queue: receiver, dropped })
}
impl Delivery {
    pub fn send(&self, style: Style, text: String) {
        if text.len() <= MAX_TEXT_BYTES {
            self.one(style, text);
        } else {
            for chunk in split_message(&text, TELEGRAM_TEXT_LIMIT) {
                self.one(style, chunk);
            }
        }
    }
    // Oversized groups cannot be archived: preserve their ordinary output with backpressure.
    pub async fn send_all(&self, style: Style, text: String) {
        let (style, chunks) = match style {
            Style::Markdown => (
                Style::Html,
                hub_core::markdown::markdown_to_html_chunks(&text, TELEGRAM_TEXT_LIMIT),
            ),
            Style::Plain | Style::Html => (style, split_message(&text, TELEGRAM_TEXT_LIMIT)),
        };
        for chunk in chunks {
            if self.queue.send(Item { style, text: chunk }).await.is_err() {
                return;
            }
        }
    }
    pub fn final_message(&self, style: Style, text: String) {
        if self.queue.try_send(Item { style, text }).is_err() {
            self.missed();
        }
    }
    fn missed(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            "session delivery queue full or closed; last successful answer remains available via /last"
        );
    }
    fn one(&self, style: Style, text: String) {
        // Leave room for a save warning, final answer and completion marker after a burst of tools.
        if self.queue.capacity() <= 3 || self.queue.try_send(Item { style, text }).is_err() {
            self.missed();
        }
    }
}
impl Receiver {
    pub async fn run(mut self, sender: &Sender, key: TopicKey, cancel: &CancellationToken) {
        let forward = async {
            while let Some(item) = self.queue.recv().await {
                if self.dropped.swap(0, Ordering::Relaxed) > 0 {
                    let _ = tokio::time::timeout(Duration::from_secs(65), sender.text(Target::Topic(key), "⚠️ Часть сообщений не отправлена из-за переполнения очереди. Последний сохранённый ответ: /last.")).await;
                }
                let send = async {
                    match item.style {
                        Style::Plain => sender.text(Target::Topic(key), &item.text).await,
                        Style::Markdown => sender.markdown(key, &item.text).await,
                        Style::Html => {
                            sender.one(Target::Topic(key), Outgoing::html(item.text)).await;
                        }
                    }
                };
                if tokio::time::timeout(Duration::from_secs(65), send).await.is_err() {
                    tracing::warn!("session delivery timed out; use /last");
                }
            }
        };
        tokio::select! { () = cancel.cancelled() => {}, () = forward => {} }
    }
}
