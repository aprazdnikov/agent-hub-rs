//! What the hub hands a `Pi` child: the bridge extension, and the approvals it produces.
//!
//! Pi has no permission protocol of its own — tools run silently. The bridge extension is what
//! gives the hub both of its integrations: it registers the session's MCP server so the agent
//! can reach `send_file` and `ask_user`, and it intercepts the writing tools so a human can say
//! no. The extension reads its secrets from the child's environment, never from argv or the file
//! itself, so the same checked-in text is safe for every conversation.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hub_agent::channel::UserChannel;
use hub_core::domain::{Decision, ToolRequest};
use hub_core::settings::PiApproval;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::protocol;

/// How long a human may take before the hub answers for them. Pi's own dialog expires too;
/// answering first keeps the outcome the hub already reported to the user.
const HUMAN_TIMEOUT: Duration = Duration::from_mins(5);

/// The bridge loaded with `--extension`. Plain ESM with no imports: the file is written into a
/// private directory and must load before any project trust is decided.
///
/// `exposure: "direct"` is not cosmetic. Without it Pi hides MCP servers behind a code-mode
/// wrapper and the model cannot call `send_file` or `ask_user` as tools at all; that was
/// measured against the installed 1.6.0 build.
pub const BRIDGE: &str = r#"export default function (pi) {
  const url = process.env.AGENT_HUB_PI_MCP_URL;
  const token = process.env.AGENT_HUB_PI_MCP_TOKEN;
  if (url && token) {
    pi.registerMcpServer("agent-hub", {
      url,
      headers: { Authorization: `Bearer ${token}` },
      exposure: "direct",
    });
  }

  const memory = process.env.AGENT_HUB_PI_MEMORY;
  if (memory) {
    const server = JSON.parse(memory);
    pi.registerMcpServer(server.name, {
      command: server.command,
      args: server.args,
      env: server.env ?? {},
      exposure: "direct",
    });
  }

  const gate = (process.env.AGENT_HUB_PI_GATE ?? "").split(",").filter(Boolean);
  if (gate.length === 0) return;

  pi.on("tool_call", async (event, ctx) => {
    if (!gate.includes(event.toolName)) return;
    const detail = event.input?.command ?? event.input?.path ?? "";
    const allowed = await ctx.ui.confirm("agent-hub", `Allow ${event.toolName}: ${detail}`);
    if (!allowed) return { block: true, reason: "Rejected in agent-hub" };
  });
}
"#;

/// Pi shows a confirmation as `Allow <tool>: <detail>` — the bridge above writes it in exactly
/// that shape. The hub names the tool it gates, so every backend asks the same way.
fn allow_message(message: &str) -> (String, String) {
    match message.strip_prefix("Allow ").and_then(|rest| rest.split_once(": ")) {
        Some((tool, detail)) => (tool.to_owned(), detail.to_owned()),
        None => ("pi".to_owned(), message.to_owned()),
    }
}

/// Answers the approval dialogs one `Pi` child opens.
#[derive(Clone)]
pub struct Gate {
    channel: Arc<dyn UserChannel>,
    cancel: CancellationToken,
    waiting: Arc<Mutex<HashSet<String>>>,
}

impl Gate {
    #[must_use]
    pub fn new(channel: Arc<dyn UserChannel>, cancel: CancellationToken) -> Self {
        Self { channel, cancel, waiting: Arc::new(Mutex::new(HashSet::new())) }
    }

    /// Asks the user about one dialog and writes the answer to the child's input.
    ///
    /// A dialog already being answered is skipped: `Pi` can send the same request twice while the
    /// first is still open, and a second question would take a second decision for nothing. Once
    /// answered, the id is released, so a retry after an expiry asks again.
    pub async fn answer(&self, input: &mpsc::Sender<String>, id: &str, summary: &str) {
        if self.cancel.is_cancelled() || !self.reserve(id) {
            return;
        }
        let (tool, detail) = allow_message(summary);
        let request = ToolRequest { tool, summary: detail };
        let outcome = tokio::select! {
            biased;
            () = self.cancel.cancelled() => None,
            outcome = tokio::time::timeout(HUMAN_TIMEOUT, self.channel.request(request)) =>
                outcome.ok(),
        };
        self.release(id);
        let allowed = match outcome {
            Some(Decision::Allowed) => true,
            Some(Decision::Denied(denied)) => {
                tracing::debug!(reason = %denied.reason, "Pi approval denied");
                false
            }
            // Either the hub stopped first or the human never answered: neither is permission.
            None => {
                tracing::warn!("Pi approval was not answered; rejecting once");
                false
            }
        };
        let line = protocol::Wire::approval_answer(id, allowed);
        if input.send(line).await.is_err() {
            tracing::warn!("Pi approval answer could not be written");
        }
    }

    fn reserve(&self, id: &str) -> bool {
        lock(&self.waiting).insert(id.to_owned())
    }

    fn release(&self, id: &str) {
        lock(&self.waiting).remove(id);
    }
}

fn lock(mutex: &Mutex<HashSet<String>>) -> MutexGuard<'_, HashSet<String>> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The gate list `Pi` is started with: empty means the bridge registers no hook at all.
#[must_use]
pub const fn gate_tools(approval: PiApproval) -> &'static str {
    match approval {
        PiApproval::Default => protocol::GATE_TOOLS,
        PiApproval::Yolo => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use hub_core::domain::{Denied, FileDelivery, OutgoingFile, Question, QuestionsOutcome};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Channel {
        decision: Decision,
        calls: AtomicUsize,
    }

    impl UserChannel for Channel {
        fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(tool.tool, "bash");
            assert_eq!(tool.summary, "ls");
            Box::pin(async move { self.decision.clone() })
        }

        fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            Box::pin(
                async move { QuestionsOutcome::Denied(Denied { reason: "unused".to_owned() }) },
            )
        }

        fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            Box::pin(async move { FileDelivery::Delivered })
        }
    }

    fn human(decision: Decision) -> Arc<Channel> {
        Arc::new(Channel { decision, calls: AtomicUsize::new(0) })
    }

    async fn run(decision: Decision, id: &str) -> Vec<Value> {
        let (input, mut lines) = mpsc::channel::<String>(8);
        let gate = Gate::new(human(decision), CancellationToken::new());
        let collector = tokio::spawn(async move {
            let mut seen = Vec::new();
            while let Some(line) = lines.recv().await {
                seen.push(serde_json::from_str(&line).unwrap());
            }
            seen
        });
        gate.answer(&input, id, "Allow bash: ls").await;
        drop(input);
        collector.await.unwrap_or_default()
    }

    #[rstest::rstest]
    #[case("Allow bash: ls -la", "bash", "ls -la")]
    #[case("Allow edit: src/main.rs", "edit", "src/main.rs")]
    #[case("Allow write: /tmp/a.txt", "write", "/tmp/a.txt")]
    /// Без префикса моста имя остаётся общим, а текст — как его показал Pi.
    #[case("Разрешить перезапись?", "pi", "Разрешить перезапись?")]
    #[case("Allowed: yes", "pi", "Allowed: yes")]
    fn approval_names_the_gated_tool(
        #[case] message: &str,
        #[case] tool: &str,
        #[case] detail: &str,
    ) {
        assert_eq!(allow_message(message), (tool.to_owned(), detail.to_owned()));
    }

    #[tokio::test]
    async fn an_allowed_approval_confirms_the_dialog() {
        let lines = run(Decision::Allowed, "u1").await;
        assert_eq!(
            lines,
            vec![json!({"type": "extension_ui_response", "id": "u1", "confirmed": true})]
        );
    }

    #[tokio::test]
    async fn a_denied_approval_blocks_the_dialog() {
        let lines = run(Decision::Denied(Denied { reason: "нет".to_owned() }), "u2").await;
        assert_eq!(
            lines,
            vec![json!({"type": "extension_ui_response", "id": "u2", "confirmed": false})]
        );
    }

    #[tokio::test]
    async fn a_cancelled_gate_stops_asking() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let gate = Gate::new(human(Decision::Allowed), cancel);
        let (input, mut lines) = mpsc::channel::<String>(4);
        gate.answer(&input, "u3", "Allow bash: ls").await;
        drop(input);
        assert!(lines.recv().await.is_none());
    }

    /// A human that is asked but holds the answer until the test lets it through: the whole
    /// point of the de-duplication is what happens while a dialog is still open.
    struct Slow {
        calls: AtomicUsize,
        /// One permit added per question, taken by the test to know a question is in flight.
        asked: tokio::sync::Semaphore,
        /// No permits: the question is answered only when the test opens this.
        release: tokio::sync::Semaphore,
    }

    impl Default for Slow {
        fn default() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                asked: tokio::sync::Semaphore::new(0),
                release: tokio::sync::Semaphore::new(0),
            }
        }
    }

    impl UserChannel for Slow {
        fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.asked.add_permits(1);
            let release = &self.release;
            Box::pin(async move {
                let _permit = release.acquire().await.unwrap();
                Decision::Allowed
            })
        }

        fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            Box::pin(async { QuestionsOutcome::Denied(Denied { reason: "unused".to_owned() }) })
        }

        fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            Box::pin(async { FileDelivery::Delivered })
        }
    }

    #[tokio::test]
    async fn one_dialog_is_answered_once_while_it_is_open() {
        let (input, mut lines) = mpsc::channel::<String>(8);
        let human = Arc::new(Slow::default());
        let gate = Gate::new(Arc::clone(&human) as Arc<dyn UserChannel>, CancellationToken::new());
        let first = {
            let gate = gate.clone();
            let input = input.clone();
            tokio::spawn(async move { gate.answer(&input, "u4", "Allow bash: ls").await })
        };
        // Permit taken: a question is with the human now, and its dialog is still open.
        drop(human.asked.acquire().await.unwrap());
        gate.answer(&input, "u4", "Allow bash: ls").await;
        assert_eq!(human.calls.load(Ordering::SeqCst), 1, "the open dialog was asked twice");
        assert!(lines.try_recv().is_err(), "an answer was written for a dialog still open");
        human.release.add_permits(1);
        first.await.unwrap();
        drop(input);
        let mut answered: Vec<Value> = Vec::new();
        while let Some(line) = lines.recv().await {
            answered.push(serde_json::from_str(&line).unwrap());
        }
        assert_eq!(
            answered,
            vec![json!({"type": "extension_ui_response", "id": "u4", "confirmed": true})]
        );
    }

    #[tokio::test]
    async fn an_answered_dialog_can_be_asked_again() {
        let (input, mut lines) = mpsc::channel::<String>(8);
        let gate = Gate::new(human(Decision::Allowed), CancellationToken::new());
        gate.answer(&input, "u5", "Allow bash: ls").await;
        gate.answer(&input, "u5", "Allow bash: ls").await;
        drop(input);
        let mut answered = 0;
        while lines.recv().await.is_some() {
            answered += 1;
        }
        assert_eq!(answered, 2, "a retry after an answer was swallowed");
    }

    #[test]
    fn the_bridge_carries_no_secret() {
        assert!(
            BRIDGE.contains("process.env.AGENT_HUB_PI_MCP_TOKEN"),
            "the bridge reads its token"
        );
        for needle in ["Bearer null", "sk-", " eyJ"] {
            assert!(
                !BRIDGE.to_lowercase().contains(&needle.to_lowercase()),
                "bridge embeds {needle}"
            );
        }
    }

    #[rstest::rstest]
    #[case::guarded(PiApproval::Default, "bash,edit,write")]
    #[case::yolo(PiApproval::Yolo, "")]
    fn only_the_guarded_mode_intercepts(#[case] approval: PiApproval, #[case] tools: &str) {
        assert_eq!(gate_tools(approval), tools);
    }
}
