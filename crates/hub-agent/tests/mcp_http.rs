//! The hub's MCP server on a real loopback socket, driven with raw HTTP/1.1.

#[cfg(test)]
mod tests {

    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::channel::UserChannel;
    use hub_agent::mcp_http::McpHttpServer;
    use hub_core::domain::{
        Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome, ToolRequest,
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[derive(Default)]
    struct Recording {
        sent: Mutex<Vec<OutgoingFile>>,
        asked: Mutex<Vec<Question>>,
        denial: Option<String>,
    }

    impl UserChannel for Recording {
        fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
            Box::pin(async { Decision::Allowed })
        }
        fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            self.asked.lock().unwrap().extend(questions);
            Box::pin(async {
                if let Some(reason) = &self.denial {
                    return QuestionsOutcome::Denied(hub_core::domain::Denied::new(reason.clone()));
                }
                QuestionsOutcome::Answered(vec![hub_core::domain::QuestionAnswer {
                    question: "Формат?".to_owned(),
                    answer: "CSV".to_owned(),
                }])
            })
        }
        fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            self.sent.lock().unwrap().push(file);
            Box::pin(async { FileDelivery::Delivered })
        }
    }

    struct Reply {
        status: u16,
        head: String,
        body: String,
    }

    fn address(server: &McpHttpServer) -> String {
        let rest = server.url().strip_prefix("http://").unwrap();
        rest.split_once('/').unwrap().0.to_owned()
    }

    async fn send(address: &str, request: String) -> Reply {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
            .await
            .unwrap()
            .unwrap();
        let text = String::from_utf8(raw).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        Reply { status, head: head.to_owned(), body: body.to_owned() }
    }

    fn request(address: &str, method: &str, path: &str, token: Option<&str>, body: &str) -> String {
        let auth =
            token.map(|token| format!("Authorization: Bearer {token}\r\n")).unwrap_or_default();
        format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{auth}Content-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn start() -> (Arc<Recording>, McpHttpServer) {
        let channel = Arc::new(Recording::default());
        let server =
            McpHttpServer::start(Arc::clone(&channel) as Arc<dyn UserChannel>).await.unwrap();
        (channel, server)
    }

    #[tokio::test]
    async fn opted_in_questions_reach_the_user() {
        let channel = Arc::new(Recording::default());
        let server =
            McpHttpServer::start_with_questions(Arc::clone(&channel) as Arc<dyn UserChannel>)
                .await
                .unwrap();
        let call = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {
            "name": "ask_user", "arguments": {"questions": [{"question": "Формат?"}]}
        }});
        let reply = send(
            &address(&server),
            request(&address(&server), "POST", "/mcp", Some(server.token()), &call.to_string()),
        )
        .await;
        let body: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(body.pointer("/result/isError"), Some(&json!(false)));
        assert!(
            body.pointer("/result/content/0/text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains("CSV"))
        );
        assert_eq!(channel.asked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn questions_are_opt_in_and_authenticated() {
        let (channel, files) = start().await;
        let server =
            McpHttpServer::start_with_questions(Arc::clone(&channel) as Arc<dyn UserChannel>)
                .await
                .unwrap();
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "ask_user", "arguments": {"questions": [{"question": "Формат?"}]}
        }})
        .to_string();
        let disabled = send(
            &address(&files),
            request(&address(&files), "POST", "/mcp", Some(files.token()), &call),
        )
        .await;
        let unauthenticated =
            send(&address(&server), request(&address(&server), "POST", "/mcp", None, &call)).await;
        let disabled_body: Value = serde_json::from_str(&disabled.body).unwrap();
        assert_eq!(disabled_body.pointer("/error/code"), Some(&json!(-32602)));
        assert_eq!(unauthenticated.status, 401);
        assert!(channel.asked.lock().unwrap().is_empty());
        let listed = send(
            &address(&server),
            request(
                &address(&server),
                "POST",
                "/mcp",
                Some(server.token()),
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            ),
        )
        .await;
        let listed_body: Value = serde_json::from_str(&listed.body).unwrap();
        assert_eq!(listed_body.pointer("/result/tools/1/name"), Some(&json!("ask_user")));
        assert_eq!(listed_body.pointer("/result/tools/2"), None);
    }

    #[tokio::test]
    async fn malformed_or_excessive_questions_never_reach_the_user() {
        let channel = Arc::new(Recording::default());
        let server =
            McpHttpServer::start_with_questions(Arc::clone(&channel) as Arc<dyn UserChannel>)
                .await
                .unwrap();
        for arguments in [
            json!({}),
            json!({"questions": []}),
            json!({"questions": vec![json!({"question":"q"}); 6]}),
            json!({"questions": [{"question":"q", "options": vec![json!({"label":"a"}); 5]}]}),
        ] {
            let call = json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{"name":"ask_user", "arguments":arguments}});
            let reply = send(
                &address(&server),
                request(&address(&server), "POST", "/mcp", Some(server.token()), &call.to_string()),
            )
            .await;
            let body: Value = serde_json::from_str(&reply.body).unwrap();
            assert_eq!(body.pointer("/result/isError"), Some(&json!(true)));
        }
        assert!(channel.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn denied_questions_return_a_tool_error() {
        let channel = Arc::new(Recording {
            denial: Some("Отклонено".to_owned()),
            ..Recording::default()
        });
        let server = McpHttpServer::start_with_questions(channel).await.unwrap();
        let call = json!({"jsonrpc":"2.0", "id":4, "method":"tools/call", "params":{
            "name":"ask_user", "arguments":{"questions":[{"question":"Формат?"}]}
        }});
        let reply = send(
            &address(&server),
            request(&address(&server), "POST", "/mcp", Some(server.token()), &call.to_string()),
        )
        .await;
        let body: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(
            body.get("result"),
            Some(&json!({"content":[{"type":"text","text":"Отклонено"}],"isError":true}))
        );
    }

    #[tokio::test]
    async fn listens_on_loopback_only() {
        let (_channel, server) = start().await;
        assert!(server.url().starts_with("http://127.0.0.1:") && server.url().ends_with("/mcp"));
    }

    #[tokio::test]
    async fn requests_without_the_session_token_are_refused() {
        let (channel, server) = start().await;
        let call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_file","arguments":{"path":"a.txt"}}}"#;
        let missing =
            send(&address(&server), request(&address(&server), "POST", "/mcp", None, call)).await;
        let wrong = send(
            &address(&server),
            request(&address(&server), "POST", "/mcp", Some("guess"), call),
        )
        .await;
        assert_eq!((missing.status, wrong.status), (401, 401));
        assert!(channel.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn handshake_and_listing_work() {
        let (_channel, server) = start().await;
        let token = Some(server.token());
        let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}"#;
        let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let init =
            send(&address(&server), request(&address(&server), "POST", "/mcp", token, init)).await;
        let notified =
            send(&address(&server), request(&address(&server), "POST", "/mcp", token, initialized))
                .await;
        let listed =
            send(&address(&server), request(&address(&server), "POST", "/mcp", token, list)).await;
        let init_body: Value = serde_json::from_str(&init.body).unwrap();
        let listed_body: Value = serde_json::from_str(&listed.body).unwrap();
        assert_eq!(
            (
                init.status,
                init_body.pointer("/result/serverInfo/name").cloned(),
                notified.status,
                notified.body.as_str(),
                listed_body.pointer("/result/tools/0/name").cloned()
            ),
            (200, Some(json!("agent-hub")), 202, "", Some(json!("send_file")))
        );
        assert!(init.head.to_ascii_lowercase().contains("content-type: application/json"));
    }

    #[tokio::test]
    async fn send_file_reaches_the_user() {
        let (channel, server) = start().await;
        let call = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"send_file","arguments":{"path":"out/a.txt","caption":"отчёт"}}}"#;
        let reply = send(
            &address(&server),
            request(&address(&server), "POST", "/mcp", Some(server.token()), call),
        )
        .await;
        let body: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(
            body,
            json!({"jsonrpc": "2.0", "id": 7, "result": {
                "content": [{"type": "text", "text": "Файл out/a.txt отправлен пользователю"}],
                "isError": false
            }})
        );
        assert_eq!(
            *channel.sent.lock().unwrap(),
            [OutgoingFile { path: "out/a.txt".to_owned(), caption: "отчёт".to_owned() }]
        );
    }

    #[tokio::test]
    async fn browser_and_foreign_host_requests_are_forbidden() {
        let (channel, server) = start().await;
        let call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_file","arguments":{"path":"a.txt"}}}"#;
        let address = address(&server);
        let with_origin = request(&address, "POST", "/mcp", Some(server.token()), call)
            .replace("Content-Type", "Origin: http://evil.example\r\nContent-Type");
        let rebound = request("evil.example", "POST", "/mcp", Some(server.token()), call);
        let origin = send(&address, with_origin).await;
        let host = send(&address, rebound).await;
        assert_eq!((origin.status, origin.body.as_str()), (403, ""));
        assert_eq!((host.status, host.body.as_str()), (403, ""));
        assert!(channel.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn wrong_method_path_and_size_are_rejected() {
        let (_channel, server) = start().await;
        let token = Some(server.token());
        let get =
            send(&address(&server), request(&address(&server), "GET", "/mcp", token, "")).await;
        let other =
            send(&address(&server), request(&address(&server), "POST", "/other", token, "{}"))
                .await;
        let huge = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: 2097152\r\nConnection: close\r\n\r\n",
            address(&server),
            server.token()
        );
        let huge = send(&address(&server), huge).await;
        let garbage =
            send(&address(&server), request(&address(&server), "POST", "/mcp", token, "не json"))
                .await;
        assert_eq!((get.status, other.status, huge.status, garbage.status), (405, 404, 413, 400));
        assert!(get.head.to_ascii_lowercase().contains("allow: post"));
        assert_eq!(
            serde_json::from_str::<Value>(&garbage.body).unwrap().pointer("/error/code"),
            Some(&json!(-32700))
        );
    }

    #[tokio::test]
    async fn dropping_the_server_stops_listening() {
        let (_channel, server) = start().await;
        let address = address(&server);
        drop(server);
        for _ in 0..200 {
            if TcpStream::connect(&address).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the server still accepts connections after drop");
    }
}
