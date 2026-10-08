//! Round trips through the real `qwen --acp`. They spend tokens and need a configured qwen
//! (its own setup), so they are opt-in:
//! `AGENT_HUB_LIVE_QWEN=1 cargo test -p hub-qwen --test live -- --ignored --test-threads=1`.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::{Conversation, Limits};
    use hub_core::domain::{
        AbsolutePath, AgentEvent, BackendKind, Decision, Denied, FileDelivery, OutgoingFile,
        Prompt, Question, QuestionAnswer, QuestionsOutcome, ToolRequest, TopicSession, Usage,
    };
    use hub_core::settings::{Draft, QwenApproval, QwenDraft, QwenSettings};
    use hub_qwen::backend::QwenBackend;
    use hub_qwen::version::{check, locate};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    /// Denies every tool, answers every question with its first option, records what it saw.
    #[derive(Default)]
    struct Recording {
        requests: Mutex<Vec<ToolRequest>>,
        questions: Mutex<Vec<Question>>,
    }

    impl UserChannel for Recording {
        fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
            self.requests.lock().unwrap().push(tool);
            Box::pin(async { Decision::Denied(Denied::new("live test")) })
        }
        fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            let answers = questions
                .iter()
                .map(|q| QuestionAnswer {
                    question: q.text().to_owned(),
                    answer: q.options().first().map(|o| o.label.clone()).unwrap_or_default(),
                })
                .collect();
            self.questions.lock().unwrap().extend(questions);
            Box::pin(async move { QuestionsOutcome::Answered(answers) })
        }
        fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            Box::pin(async { FileDelivery::Delivered })
        }
    }

    fn settings(approval: QwenApproval) -> QwenSettings {
        let root = std::env::temp_dir();
        Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            qwen: QwenDraft { approval, ..QwenDraft::default() },
            ..Draft::default()
        }
        .parse(&root)
        .unwrap()
        .qwen
    }

    async fn live(text: &str, approval: QwenApproval) -> Option<(Vec<AgentEvent>, Arc<Recording>)> {
        std::env::var_os("AGENT_HUB_LIVE_QWEN")?;
        let cli = locate(None).unwrap();
        check(&cli).await.unwrap();
        let channel = Arc::new(Recording::default());
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let session = TopicSession::fresh(BackendKind::Qwen, cwd);
        let (events, mut received) = mpsc::channel(256);
        let conversation = Conversation {
            channel: Arc::clone(&channel) as Arc<dyn UserChannel>,
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_mins(1)),
        };
        let (_inbox_in, inbox) = mpsc::channel(1);
        let prompt = Prompt::new(text.to_owned(), Vec::new()).unwrap();
        let backend = QwenBackend::new(cli, settings(approval));
        let run = backend.run(&session, prompt, inbox, conversation);
        let _inbox = tokio::time::timeout(Duration::from_mins(5), run).await.unwrap();
        Some((std::iter::from_fn(|| received.try_recv().ok()).collect(), channel))
    }

    #[tokio::test]
    #[ignore = "needs a configured qwen and spends tokens"]
    async fn qwen_answers_a_prompt() {
        let Some((seen, _channel)) = live("Ответь одним словом: да", QwenApproval::Plan).await
        else {
            return;
        };
        assert!(matches!(seen.first(), Some(AgentEvent::SessionStarted(_))), "{seen:?}");
        assert!(
            matches!(seen.last(), Some(AgentEvent::Finished(f)) if matches!(f.usage, Usage::Qwen { .. })),
            "{seen:?}"
        );
    }

    #[tokio::test]
    #[ignore = "needs a configured qwen and spends tokens"]
    async fn qwen_asks_through_the_hub_and_gets_the_answer() {
        let text = "Вызови инструмент ask_user_question с одним вопросом «Какой цвет?» и \
                    вариантами «синий» и «красный», затем ответь одним словом — выбранным цветом.";
        let Some((seen, channel)) = live(text, QwenApproval::Default).await else { return };
        assert!(!channel.questions.lock().unwrap().is_empty(), "{seen:?}");
        let said: String = seen
            .iter()
            .filter_map(|e| {
                if let AgentEvent::AssistantText(text) = e { Some(text.as_str()) } else { None }
            })
            .collect();
        assert!(said.to_lowercase().contains("син"), "answers did not reach qwen: {seen:?}");
    }

    #[tokio::test]
    #[ignore = "needs a configured qwen and spends tokens"]
    async fn qwen_asks_before_using_send_file() {
        let text = "Отправь мне файл README.md инструментом send_file сервера agent-hub.";
        let Some((seen, channel)) = live(text, QwenApproval::Default).await else { return };
        let requests = channel.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|r| r.summary.contains("README") || r.summary.contains("send_file")),
            "no approval was asked for send_file: {requests:?} {seen:?}"
        );
    }
}
