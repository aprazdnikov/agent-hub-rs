#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hub_core::commands::Command;
use hub_core::settings::{Draft, TelegramSettings};
use hub_telegram::telegram::{MenuError, synchronize_commands};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rstest::rstest;
use serde_json::{Value, json};
use teloxide::Bot;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

#[derive(Clone, Copy)]
enum Policy {
    Replace,
    Rejected,
    False,
    ReadRejected,
    Mismatch,
    Stall,
}

#[derive(Default)]
struct State {
    menus: BTreeMap<String, Value>,
    requests: Vec<(String, Value)>,
}

struct Api {
    bot: Bot,
    state: Arc<Mutex<State>>,
    entered: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn old_menu() -> Value {
    json!([{"command": "obsolete", "description": "Old BotFather entry"},
        {"command": "help", "description": "Old help text"}])
}

fn menu_key(scope: &Value, language: &str) -> String {
    format!("{scope}:{language}")
}

async fn api(policy: Policy) -> Api {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = teloxide::net::default_reqwest_settings()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();
    let bot = Bot::with_client("1:test", client)
        .set_api_url(format!("http://{address}/").parse().unwrap());
    let state = Arc::new(Mutex::new(State::default()));
    state.lock().unwrap().menus.insert("unrelated-global-menu".to_owned(), old_menu());
    for key in managed_keys() {
        state.lock().unwrap().menus.insert(key, old_menu());
    }
    let serving = Arc::clone(&state);
    let entered = Arc::new(Notify::new());
    let signal = Arc::clone(&entered);
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let state = Arc::clone(&serving);
            let signal = Arc::clone(&signal);
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<Incoming>| {
                    let state = Arc::clone(&state);
                    let signal = Arc::clone(&signal);
                    async move {
                        let method =
                            request.uri().path().rsplit('/').next().unwrap().to_ascii_lowercase();
                        let bytes = request.into_body().collect().await.unwrap().to_bytes();
                        let params: Value = serde_json::from_slice(&bytes).unwrap();
                        state.lock().unwrap().requests.push((method.clone(), params.clone()));
                        signal.notify_one();
                        if matches!(policy, Policy::Stall) {
                            std::future::pending::<()>().await;
                        }
                        let key = menu_key(
                            params.get("scope").unwrap(),
                            params.get("language_code").and_then(Value::as_str).unwrap(),
                        );
                        let result = match method.as_str() {
                            "setmycommands" => match policy {
                                Policy::Replace
                                | Policy::Mismatch
                                | Policy::Stall
                                | Policy::ReadRejected => {
                                    state
                                        .lock()
                                        .unwrap()
                                        .menus
                                        .insert(key, params.get("commands").unwrap().clone());
                                    json!({"ok": true, "result": true})
                                }
                                Policy::Rejected => {
                                    json!({"ok": false, "error_code": 400, "description": "fixture secret should never enter MenuError"})
                                }
                                Policy::False => json!({"ok": true, "result": false}),
                            },
                            "getmycommands" => {
                                let menu = if matches!(policy, Policy::Mismatch) {
                                    old_menu()
                                } else {
                                    state
                                        .lock()
                                        .unwrap()
                                        .menus
                                        .get(&key)
                                        .cloned()
                                        .unwrap_or_else(old_menu)
                                };
                                if matches!(policy, Policy::ReadRejected) {
                                    json!({"ok": false, "error_code": 400, "description": "fixture read rejected"})
                                } else {
                                    json!({"ok": true, "result": menu})
                                }
                            }
                            other => unexpected_method_reply(other),
                        };
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(
                            result.to_string(),
                        ))))
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    Api { bot, state, entered, task }
}

fn unexpected_method_reply(method: &str) -> Value {
    // A failed fixture response makes the client return an error instead of hanging.
    json!({"ok": false, "error_code": 404, "description": format!("unexpected fixture method: {method}")})
}

fn settings() -> TelegramSettings {
    let root = std::env::temp_dir();
    Draft {
        token: "1:test".to_owned(),
        chat: "-100123".to_owned(),
        users: "7,8".to_owned(),
        workspace_root: root.display().to_string(),
        ..Draft::default()
    }
    .parse(&root)
    .unwrap()
    .telegram
}

fn expected_menu() -> Value {
    json!(
        Command::ALL
            .into_iter()
            .map(|command| json!({
                "command": command.name(), "description": command.description()
            }))
            .collect::<Vec<_>>()
    )
}

fn managed_keys() -> Vec<String> {
    let mut expected_keys = Vec::new();
    for scope in [
        json!({"type":"chat", "chat_id":-100_123}),
        json!({"type":"chat_administrators", "chat_id":-100_123}),
        json!({"type":"chat_member", "chat_id":-100_123,"user_id":7}),
        json!({"type":"chat_member", "chat_id":-100_123,"user_id":8}),
    ] {
        for language in ["", "ru", "en"] {
            expected_keys.push(menu_key(&scope, language));
        }
    }
    expected_keys.sort();
    expected_keys
}

#[tokio::test]
async fn source_registry_replaces_old_menus_in_exact_scopes_and_languages() {
    let api = api(Policy::Replace).await;
    assert!(api.state.lock().unwrap().menus.values().all(|menu| *menu == old_menu()));
    let settings = settings();
    synchronize_commands(&api.bot, &settings).await.unwrap();
    let state = api.state.lock().unwrap();
    let written_keys: Vec<_> =
        state.menus.keys().filter(|key| *key != "unrelated-global-menu").cloned().collect();
    assert_eq!(written_keys, managed_keys(), "every managed scope must be replaced, not appended");
    for key in &written_keys {
        assert_eq!(state.menus.get(key), Some(&expected_menu()));
    }
    assert_eq!(state.menus.get("unrelated-global-menu"), Some(&old_menu()));
    for pair in state.requests.chunks(2) {
        let [(write, written), (read, readback)] = pair else {
            assert_eq!(pair.len(), 2, "write without readback");
            return;
        };
        assert_eq!((write.as_str(), read.as_str()), ("setmycommands", "getmycommands"));
        assert_eq!(
            (written.get("scope"), written.get("language_code")),
            (readback.get("scope"), readback.get("language_code"))
        );
        assert_eq!(written.get("commands"), Some(&expected_menu()));
    }
}

#[tokio::test]
async fn repeated_registration_is_idempotent() {
    let api = api(Policy::Replace).await;
    let settings = settings();
    synchronize_commands(&api.bot, &settings).await.unwrap();
    let first = api.state.lock().unwrap().menus.clone();
    synchronize_commands(&api.bot, &settings).await.unwrap();
    assert_eq!(api.state.lock().unwrap().menus, first);
}

#[rstest]
#[case(Policy::Rejected)]
#[case(Policy::False)]
#[tokio::test]
async fn failed_write_is_reported_without_leaking_api_details(#[case] policy: Policy) {
    let api = api(policy).await;
    let error = synchronize_commands(&api.bot, &settings()).await.unwrap_err();
    assert!(matches!(error, MenuError::Request { operation: "setMyCommands" }));
    assert!(!error.to_string().contains("secret"));
    assert_eq!(api.state.lock().unwrap().requests.len(), 1);
}

#[tokio::test]
async fn failed_readback_is_not_success() {
    let api = api(Policy::ReadRejected).await;
    assert!(matches!(
        synchronize_commands(&api.bot, &settings()).await,
        Err(MenuError::Request { operation: "getMyCommands" })
    ));
    assert_eq!(api.state.lock().unwrap().requests.len(), 2);
}

#[tokio::test]
async fn a_successful_write_with_wrong_readback_is_not_success() {
    let api = api(Policy::Mismatch).await;
    assert!(matches!(synchronize_commands(&api.bot, &settings()).await, Err(MenuError::Mismatch)));
}

#[tokio::test]
async fn stalled_api_cannot_delay_bot_start_forever() {
    let api = api(Policy::Stall).await;
    let bot = api.bot.clone();
    let settings = settings();
    let sync = tokio::spawn(async move { synchronize_commands(&bot, &settings).await });
    tokio::time::timeout(Duration::from_secs(2), api.entered.notified()).await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(matches!(sync.await.unwrap(), Err(MenuError::Timeout)));
}
