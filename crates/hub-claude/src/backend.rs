//! The agent CLI as a child process for one conversation.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use futures::StreamExt;
use hub_core::domain::{AgentEvent, Prompt, TopicSession};
use hub_core::settings::ClaudeSettings;
use tokio::io::AsyncRead;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::outgoing::{ENTRYPOINT, cli_args};
use hub_agent::cli::hide_window;
use hub_agent::conversation::Conversation;
use crate::session::converse;

// After stdin closes the CLI finishes on its own; this bounds the wait before killing it.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STDERR_LINE: usize = 64 * 1024;

pub struct ClaudeBackend {
    cli: PathBuf,
    settings: ClaudeSettings,
}

impl ClaudeBackend {
    #[must_use]
    pub fn new(cli: PathBuf, settings: ClaudeSettings) -> Self {
        Self { cli, settings }
    }

    /// Runs one conversation and returns the inbox with the prompts it did not take.
    pub async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> mpsc::Receiver<Prompt> {
        let mut command = Command::new(&self.cli);
        command
            .args(cli_args(&self.settings, session.session.as_ref()))
            .current_dir(session.cwd.as_path())
            .env(ENTRYPOINT.0, ENTRYPOINT.1)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_window(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                fail(&conversation, format!("Не удалось запустить Claude: {error}")).await;
                return inbox;
            }
        };
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            fail(&conversation, "Claude запущен без стандартных потоков".to_owned()).await;
            return inbox;
        };
        tokio::spawn(log_stderr(stderr));
        let inbox = converse(stdout, stdin, prompt, inbox, conversation).await;
        if tokio::time::timeout(EXIT_TIMEOUT, child.wait()).await.is_err()
            && let Err(error) = child.kill().await
        {
            tracing::warn!(%error, "agent cli could not be killed");
        }
        inbox
    }
}

async fn fail(conversation: &Conversation, reason: String) {
    // Nobody listening means the topic is gone; there is no one left to tell.
    let _ = conversation.events.send(AgentEvent::Failed(reason)).await;
}

async fn log_stderr<R: AsyncRead + Unpin>(stderr: R) {
    let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(MAX_STDERR_LINE));
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => tracing::warn!(%line, "agent cli stderr"),
            Err(error) => {
                tracing::debug!(%error, "agent cli stderr unreadable");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_core::domain::{
        AbsolutePath, BackendKind, Decision, FileDelivery, OutgoingFile, Question,
        QuestionsOutcome, ToolRequest,
    };
    use hub_core::settings::PermissionMode;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::Limits;

    struct Allowing;

    impl UserChannel for Allowing {
        fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
            Box::pin(async { Decision::Allowed })
        }
        fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
        }
        fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            Box::pin(async { FileDelivery::Delivered })
        }
    }

    fn settings() -> ClaudeSettings {
        ClaudeSettings {
            cli: None,
            model: None,
            permission_mode: PermissionMode::Default,
            budget: None,
        }
    }

    fn prompt(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    #[tokio::test]
    async fn missing_cli_fails_the_conversation_and_keeps_the_inbox() {
        let backend =
            ClaudeBackend::new(std::env::temp_dir().join("definitely-not-claude"), settings());
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let session = TopicSession::fresh(BackendKind::Claude, cwd);
        let (events_out, mut events) = mpsc::channel(4);
        let (inbox, inbox_in) = mpsc::channel(1);
        inbox.send(prompt("later")).await.unwrap();
        let conversation = Conversation {
            channel: Arc::new(Allowing),
            events: events_out,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        };

        let mut leftover = backend.run(&session, prompt("hi"), inbox_in, conversation).await;

        assert!(matches!(
            events.recv().await,
            Some(AgentEvent::Failed(reason)) if reason.starts_with("Не удалось запустить Claude")
        ));
        assert!(leftover.try_recv().is_ok());
    }
}
