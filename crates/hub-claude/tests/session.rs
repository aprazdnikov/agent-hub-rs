//! The conversation loop against a scripted CLI on an in-memory pipe.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::{Conversation, Limits};
    use hub_claude::session::converse;
    use hub_core::domain::{
        AgentEvent, Decision, Denied, FileDelivery, Finished, OutgoingFile, Prompt, Question,
        QuestionAnswer, QuestionsOutcome, SessionId, ToolRequest, Usage,
    };
    use rust_decimal::Decimal;
    use serde_json::{Value, json};
    use tokio::io::{
        AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
    };
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;

    const WAIT: Duration = Duration::from_secs(5);

    enum Approval {
        Answer(Decision),
        Never,
    }

    #[derive(Default)]
    struct Calls {
        requests: Vec<ToolRequest>,
        asked: Vec<Vec<Question>>,
        sent: Vec<OutgoingFile>,
    }

    struct FakeChannel {
        approval: Approval,
        outcome: QuestionsOutcome,
        delivery: FileDelivery,
        calls: Mutex<Calls>,
    }

    impl FakeChannel {
        fn allowing() -> Self {
            Self {
                approval: Approval::Answer(Decision::Allowed),
                outcome: QuestionsOutcome::Answered(Vec::new()),
                delivery: FileDelivery::Delivered,
                calls: Mutex::default(),
            }
        }
    }

    impl UserChannel for FakeChannel {
        fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
            self.calls.lock().unwrap().requests.push(tool);
            match &self.approval {
                Approval::Answer(decision) => {
                    let decision = decision.clone();
                    Box::pin(async move { decision })
                }
                Approval::Never => Box::pin(std::future::pending()),
            }
        }

        fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            self.calls.lock().unwrap().asked.push(questions);
            let outcome = self.outcome.clone();
            Box::pin(async move { outcome })
        }

        fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            self.calls.lock().unwrap().sent.push(file);
            let delivery = self.delivery.clone();
            Box::pin(async move { delivery })
        }
    }

    struct FakeCli {
        input: Lines<BufReader<ReadHalf<DuplexStream>>>,
        output: WriteHalf<DuplexStream>,
    }

    impl FakeCli {
        async fn recv(&mut self) -> Option<Value> {
            let line =
                tokio::time::timeout(WAIT, self.input.next_line()).await.unwrap().unwrap()?;
            Some(serde_json::from_str(&line).unwrap())
        }

        async fn expect(&mut self) -> Value {
            self.recv().await.expect("the hub closed stdin")
        }

        async fn send(&mut self, message: &Value) {
            self.send_raw(format!("{message}\n").as_bytes()).await;
        }

        async fn send_raw(&mut self, bytes: &[u8]) {
            self.output.write_all(bytes).await.unwrap();
        }

        async fn close(&mut self) {
            self.output.shutdown().await.unwrap();
        }

        /// Answers `initialize` and takes the first prompt in, as the real CLI does.
        async fn handshake(&mut self) {
            let initialize = self.expect().await;
            assert_eq!(initialize["request"]["subtype"], "initialize");
            self.send(&json!({"type": "control_response", "response": {
                "subtype": "success", "request_id": initialize["request_id"], "response": {}}}))
                .await;
            let prompt = self.expect().await;
            assert_eq!(prompt["type"], "user");
            self.accept(&prompt).await;
        }

        async fn accept(&mut self, prompt: &Value) {
            self.send(&json!({"type": "user", "uuid": prompt["uuid"], "isReplay": true,
                              "message": {"role": "user", "content": "…"}}))
                .await;
        }
    }

    struct Harness {
        cli: FakeCli,
        events: mpsc::Receiver<AgentEvent>,
        inbox: mpsc::Sender<Prompt>,
        cancel: CancellationToken,
        run: JoinHandle<mpsc::Receiver<Prompt>>,
        channel: Arc<FakeChannel>,
    }

    impl Harness {
        fn start(channel: FakeChannel, limits: Limits) -> Self {
            Self::start_with(channel, limits, 64)
        }

        fn start_with(channel: FakeChannel, limits: Limits, backlog: usize) -> Self {
            let (hub, cli) = tokio::io::duplex(1 << 20);
            let (hub_read, hub_write) = tokio::io::split(hub);
            let (cli_read, cli_write) = tokio::io::split(cli);
            let (events_out, events) = mpsc::channel(backlog);
            let (inbox, inbox_in) = mpsc::channel(8);
            let cancel = CancellationToken::new();
            let channel = Arc::new(channel);
            let conversation = Conversation {
                channel: Arc::clone(&channel) as Arc<dyn UserChannel>,
                events: events_out,
                cancel: cancel.clone(),
                limits,
            };
            let run = tokio::spawn(converse(
                hub_read,
                hub_write,
                prompt("first"),
                inbox_in,
                conversation,
            ));
            Self {
                cli: FakeCli { input: BufReader::new(cli_read).lines(), output: cli_write },
                events,
                inbox,
                cancel,
                run,
                channel,
            }
        }

        async fn finish(mut self) -> (Vec<AgentEvent>, mpsc::Receiver<Prompt>, FakeCli) {
            let leftover = tokio::time::timeout(WAIT, self.run).await.unwrap().unwrap();
            let mut events = Vec::new();
            while let Some(event) = self.events.recv().await {
                events.push(event);
            }
            (events, leftover, self.cli)
        }
    }

    fn limits() -> Limits {
        Limits::new(Duration::from_mins(1))
    }

    fn prompt(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn session() -> SessionId {
        SessionId::parse("s").unwrap()
    }

    fn result(origin: Option<&str>) -> Value {
        json!({"type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
               "session_id": "s", "total_cost_usd": 0.5,
               "origin": origin.map(|kind| json!({"kind": kind}))})
    }

    fn text(text: &str) -> Value {
        json!({"type": "assistant", "session_id": "s",
               "message": {"content": [{"type": "text", "text": text}]}})
    }

    fn started(id: &str, description: &str) -> Value {
        json!({"type": "system", "subtype": "task_started", "task_id": id,
               "description": description, "is_backgrounded": true})
    }

    fn notified(id: &str) -> Value {
        json!({"type": "system", "subtype": "task_notification", "task_id": id})
    }

    fn can_use_tool(id: &str, tool: &str, input: &Value) -> Value {
        json!({"type": "control_request", "request_id": id,
               "request": {"subtype": "can_use_tool", "tool_name": tool, "input": input}})
    }

    fn finished(background: usize) -> AgentEvent {
        AgentEvent::Finished(Finished {
            session: session(),
            usage: Usage::Claude { turns: 1, cost: Some(Decimal::new(5, 1)) },
            background,
        })
    }

    #[tokio::test]
    async fn plain_turn_relays_text_and_finishes() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        harness.cli.send(&text("Привет")).await;
        harness.cli.send(&result(None)).await;

        let (events, mut leftover, _cli) = harness.finish().await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(session()),
                AgentEvent::AssistantText("Привет".to_owned()),
                finished(0)
            ]
        );
        assert!(leftover.try_recv().is_err());
    }

    #[tokio::test]
    async fn approval_is_asked_and_answered() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        harness.cli.send(&can_use_tool("r1", "Bash", &json!({"command": "ls"}))).await;

        let reply = harness.cli.expect().await;
        assert_eq!(reply["response"]["request_id"], "r1");
        assert_eq!(
            reply["response"]["response"],
            json!({"behavior": "allow", "updatedInput": {"command": "ls"}})
        );
        assert_eq!(
            harness.channel.calls.lock().unwrap().requests,
            [ToolRequest { tool: "Bash".to_owned(), summary: "ls".to_owned() }]
        );
        harness.cli.send(&result(None)).await;
        harness.finish().await;
    }

    #[tokio::test]
    async fn denied_approval_returns_its_reason() {
        let channel = FakeChannel {
            approval: Approval::Answer(Decision::Denied(Denied::new("нет"))),
            ..FakeChannel::allowing()
        };
        let mut harness = Harness::start(channel, limits());
        harness.cli.handshake().await;
        harness.cli.send(&can_use_tool("r1", "Bash", &json!({"command": "rm -rf /"}))).await;

        let reply = harness.cli.expect().await;
        assert_eq!(reply["response"]["response"], json!({"behavior": "deny", "message": "нет"}));
        harness.cli.send(&result(None)).await;
        harness.finish().await;
    }

    #[tokio::test]
    async fn question_answers_go_into_the_tool_input() {
        let answers = vec![QuestionAnswer {
            question: "Цвет?".to_owned(),
            answer: "Синий".to_owned(),
        }];
        let channel =
            FakeChannel { outcome: QuestionsOutcome::Answered(answers), ..FakeChannel::allowing() };
        let mut harness = Harness::start(channel, limits());
        harness.cli.handshake().await;
        let input = json!({"questions": [{"question": "Цвет?", "header": "",
                                          "options": [{"label": "Синий"}]}]});
        harness.cli.send(&can_use_tool("r1", "AskUserQuestion", &input)).await;

        let reply = harness.cli.expect().await;
        assert_eq!(
            reply["response"]["response"]["updatedInput"]["answers"],
            json!({"Цвет?": "Синий"})
        );
        assert_eq!(harness.channel.calls.lock().unwrap().asked.len(), 1);
        harness.cli.send(&result(None)).await;
        harness.finish().await;
    }

    #[tokio::test]
    async fn send_file_goes_through_mcp() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        harness
            .cli
            .send(&json!({"type": "control_request", "request_id": "m1", "request": {
                "subtype": "mcp_message", "server_name": "agent-hub", "message": {
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"name": "send_file", "arguments": {"path": "out/r.pdf"}}}}}))
            .await;

        let reply = harness.cli.expect().await;
        assert_eq!(reply["response"]["request_id"], "m1");
        assert_eq!(
            reply["response"]["response"]["mcp_response"]["result"]["content"][0]["text"],
            "Файл out/r.pdf отправлен пользователю"
        );
        assert_eq!(
            harness.channel.calls.lock().unwrap().sent,
            [OutgoingFile { path: "out/r.pdf".to_owned(), caption: String::new() }]
        );
        harness.cli.send(&result(None)).await;
        harness.finish().await;
    }

    #[tokio::test]
    async fn background_task_keeps_the_conversation_open_for_its_report() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        for message in [
            started("b", "sleep"),
            result(None),
            notified("b"),
            text("BG-DONE"),
            result(Some("task-notification")),
        ] {
            harness.cli.send(&message).await;
        }

        let (events, _, _) = harness.finish().await;
        let backgrounds: Vec<usize> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Finished(finished) => Some(finished.background),
                _ => None,
            })
            .collect();
        assert_eq!(backgrounds, [1, 0]);
        assert!(events.contains(&AgentEvent::AssistantText("BG-DONE".to_owned())));
    }

    #[tokio::test]
    async fn prompt_during_background_goes_to_the_same_conversation() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        harness.cli.send(&started("b", "sleep")).await;
        harness.cli.send(&result(None)).await;

        harness.inbox.send(prompt("ещё вопрос")).await.unwrap();
        let next = harness.cli.expect().await;
        assert_eq!(next["message"]["content"][0]["text"], "ещё вопрос");
        harness.cli.accept(&next).await;
        for message in
            [result(None), notified("b"), text("BG-DONE"), result(Some("task-notification"))]
        {
            harness.cli.send(&message).await;
        }
        harness.finish().await;
    }

    #[tokio::test]
    async fn background_outliving_the_budget_is_abandoned() {
        let mut harness =
            Harness::start(FakeChannel::allowing(), Limits::new(Duration::from_millis(20)));
        harness.cli.handshake().await;
        harness.cli.send(&started("b", "sleep 600")).await;
        harness.cli.send(&result(None)).await;

        let (events, _, _) = harness.finish().await;
        assert_eq!(
            events.last(),
            Some(&AgentEvent::BackgroundAbandoned(vec!["sleep 600".to_owned()]))
        );
    }

    #[tokio::test]
    async fn cli_exit_without_result_fails() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        harness.cli.close().await;

        let (events, _, _) = harness.finish().await;
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Failed("Claude завершился без результата".to_owned()))
        );
    }

    #[tokio::test]
    async fn cli_exit_after_result_is_not_a_failure() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        harness.cli.send(&started("b", "sleep")).await;
        harness.cli.send(&result(None)).await;
        harness.cli.close().await;

        let (events, _, _) = harness.finish().await;
        assert_eq!(events.last(), Some(&finished(1)));
    }

    #[tokio::test]
    async fn cancel_sends_interrupt_and_ends() {
        let channel = FakeChannel { approval: Approval::Never, ..FakeChannel::allowing() };
        let mut harness = Harness::start(channel, limits());
        harness.cli.handshake().await;
        harness.cli.send(&can_use_tool("r1", "Bash", &json!({"command": "ls"}))).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        harness.cancel.cancel();

        let interrupt = harness.cli.expect().await;
        assert_eq!(interrupt["request"]["subtype"], "interrupt");
        let (_, _, mut cli) = harness.finish().await;
        assert_eq!(cli.recv().await, None, "no reply to the abandoned approval");
    }

    #[tokio::test]
    async fn stop_is_seen_while_events_wait_for_a_slow_reader() {
        let mut harness = Harness::start_with(FakeChannel::allowing(), limits(), 1);
        harness.cli.handshake().await;
        for reply in ["one", "two", "three"] {
            harness.cli.send(&text(reply)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        harness.cancel.cancel();

        let interrupt = harness.cli.expect().await;
        assert_eq!(interrupt["request"]["subtype"], "interrupt");
        let leftover = tokio::time::timeout(WAIT, harness.run).await;
        assert!(leftover.is_ok(), "the conversation must end on /stop");
    }

    #[tokio::test]
    async fn control_cancel_request_abandons_pending_approval() {
        let channel = FakeChannel { approval: Approval::Never, ..FakeChannel::allowing() };
        let mut harness = Harness::start(channel, limits());
        harness.cli.handshake().await;
        harness.cli.send(&can_use_tool("r1", "Bash", &json!({"command": "ls"}))).await;
        harness.cli.send(&json!({"type": "control_cancel_request", "request_id": "r1"})).await;
        harness.cli.send(&result(None)).await;

        let (events, _, mut cli) = harness.finish().await;
        assert_eq!(events.last(), Some(&finished(0)));
        assert_eq!(cli.recv().await, None);
    }

    #[tokio::test]
    async fn initialize_error_fails() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        let initialize = harness.cli.expect().await;
        harness
            .cli
            .send(&json!({"type": "control_response", "response": {
                "subtype": "error", "request_id": initialize["request_id"], "error": "bad flags"}}))
            .await;

        let (events, _, _) = harness.finish().await;
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Failed("Claude отклонил инициализацию: bad flags".to_owned()))
        );
    }

    #[tokio::test]
    async fn unparsable_line_is_skipped() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        harness.cli.send_raw(b"not json\n").await;
        harness.cli.send(&result(None)).await;

        let (events, _, _) = harness.finish().await;
        assert_eq!(events.last(), Some(&finished(0)));
    }

    #[tokio::test]
    async fn invalid_utf8_fails_the_conversation() {
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        harness.cli.send_raw(&[0xff, 0xfe, b'\n']).await;

        let (events, _, _) = harness.finish().await;
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Failed(reason)) if reason.starts_with("Не удалось прочитать вывод Claude")
        ));
    }

    #[tokio::test]
    async fn leftover_prompt_is_returned() {
        let channel = FakeChannel { approval: Approval::Never, ..FakeChannel::allowing() };
        let mut harness = Harness::start(channel, limits());
        harness.cli.handshake().await;
        harness.cancel.cancel();
        harness.inbox.send(prompt("после остановки")).await.unwrap();

        let (_, mut leftover, _) = harness.finish().await;
        let pending = leftover.try_recv();
        assert!(pending.is_ok_and(|prompt| prompt.text() == "после остановки"));
    }
}
