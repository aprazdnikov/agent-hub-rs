//! Lines this hub writes to the agent CLI's stdin, and the CLI's command line.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hub_core::domain::{Prompt, SessionId};
use hub_core::settings::ClaudeSettings;
use serde_json::{Map, Value, json};

pub const MCP_SERVER: &str = "agent-hub";
const MCP_CONFIG: &str = r#"{"mcpServers":{"agent-hub":{"type":"sdk","name":"agent-hub"}}}"#;
// Lets the CLI tell SDK hosts apart.
pub const ENTRYPOINT: (&str, &str) = ("CLAUDE_CODE_ENTRYPOINT", "sdk-rs");

/// Stream-json user message with images first, as the Messages API recommends.
#[must_use]
pub fn user_message(prompt: &Prompt, uuid: &str) -> Value {
    let images = prompt.images().iter().map(|image| {
        json!({"type": "image", "source": {
            "type": "base64",
            "media_type": image.media.mime(),
            "data": STANDARD.encode(&image.data),
        }})
    });
    let text =
        (!prompt.text().trim().is_empty()).then(|| json!({"type": "text", "text": prompt.text()}));
    let content: Vec<Value> = images.chain(text).collect();
    json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
        "uuid": uuid,
    })
}

#[must_use]
pub fn control_request(request_id: &str, request: Value) -> Value {
    object([
        ("type", Value::from("control_request")),
        ("request_id", Value::from(request_id)),
        ("request", request),
    ])
}

#[must_use]
pub fn initialize() -> Value {
    json!({"subtype": "initialize", "hooks": null})
}

#[must_use]
pub fn interrupt() -> Value {
    json!({"subtype": "interrupt"})
}

#[must_use]
pub fn control_success(request_id: &str, response: Value) -> Value {
    let reply = object([
        ("subtype", Value::from("success")),
        ("request_id", Value::from(request_id)),
        ("response", response),
    ]);
    object([("type", Value::from("control_response")), ("response", reply)])
}

#[must_use]
pub fn control_error(request_id: &str, error: &str) -> Value {
    json!({"type": "control_response", "response": {
        "subtype": "error", "request_id": request_id, "error": error,
    }})
}

/// A JSON object that takes ownership of its values instead of serializing copies.
#[must_use]
pub fn object<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Object(
        fields.into_iter().map(|(key, value)| (key.to_owned(), value)).collect::<Map<_, _>>(),
    )
}

/// The operator's own setup (CLAUDE.md, permission allowlists, skills, MCP servers) is loaded,
/// so a topic behaves like the CLI started in the same directory.
#[must_use]
pub fn cli_args(settings: &ClaudeSettings, session: Option<&SessionId>) -> Vec<String> {
    let mut args: Vec<String> = [
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        settings.permission_mode.wire(),
    ]
    .map(str::to_owned)
    .into();
    if let Some(model) = &settings.model {
        args.extend(["--model".to_owned(), model.clone()]);
    }
    if let Some(budget) = settings.budget {
        args.extend(["--max-budget-usd".to_owned(), budget.amount().to_string()]);
    }
    if let Some(session) = session {
        args.push(format!("--resume={}", session.as_str()));
    }
    // Replay echoes each prompt when a turn takes it in: prompts sent mid-turn are merged into
    // that turn, so counting results cannot tell when all prompts are answered.
    args.extend(
        [
            "--setting-sources=user,project,local",
            "--replay-user-messages",
            "--mcp-config",
            MCP_CONFIG,
        ]
        .map(str::to_owned),
    );
    args
}

#[cfg(test)]
mod tests {
    use hub_core::domain::{Image, ImageMediaType};
    use hub_core::settings::{Draft, PermissionMode};
    use serde_json::json;

    use super::*;

    fn settings(draft: Draft) -> ClaudeSettings {
        let home = if cfg!(windows) { r"C:\Users\me" } else { "/home/me" };
        Draft {
            token: "1:a".to_owned(),
            chat: "-1".to_owned(),
            users: "1".to_owned(),
            workspace_root: "~/p".to_owned(),
            ..draft
        }
        .parse(std::path::Path::new(home))
        .unwrap()
        .claude
    }

    #[test]
    fn user_message_puts_images_before_text() {
        let image = Image { media: ImageMediaType::Jpeg, data: vec![0xff, 0xd8] };
        let prompt = Prompt::new("что на фото?".to_owned(), vec![image]).unwrap();
        assert_eq!(
            user_message(&prompt, "p1"),
            json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "/9g="}},
                    {"type": "text", "text": "что на фото?"}
                ]},
                "parent_tool_use_id": null,
                "uuid": "p1"
            })
        );
    }

    #[test]
    fn user_message_without_text_has_only_images() {
        let image = Image { media: ImageMediaType::Png, data: vec![1] };
        let prompt = Prompt::new(String::new(), vec![image]).unwrap();
        let content = user_message(&prompt, "p").pointer("/message/content").cloned().unwrap();
        assert_eq!(content.as_array().map(Vec::len), Some(1));
        assert_eq!(content.pointer("/0/type"), Some(&json!("image")));
    }

    #[test]
    fn control_lines_have_the_cli_shape() {
        assert_eq!(
            control_request("r", initialize()),
            json!({"type": "control_request", "request_id": "r",
                   "request": {"subtype": "initialize", "hooks": null}})
        );
        assert_eq!(
            control_success("r", json!({"x": 1})),
            json!({"type": "control_response",
                   "response": {"subtype": "success", "request_id": "r", "response": {"x": 1}}})
        );
        assert_eq!(
            control_error("r", "no"),
            json!({"type": "control_response",
                   "response": {"subtype": "error", "request_id": "r", "error": "no"}})
        );
        assert_eq!(interrupt(), json!({"subtype": "interrupt"}));
    }

    #[test]
    fn minimal_cli_args() {
        assert_eq!(
            cli_args(&settings(Draft::default()), None),
            [
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
                "--permission-prompt-tool",
                "stdio",
                "--permission-mode",
                "default",
                "--setting-sources=user,project,local",
                "--replay-user-messages",
                "--mcp-config",
                r#"{"mcpServers":{"agent-hub":{"type":"sdk","name":"agent-hub"}}}"#,
            ]
        );
    }

    #[test]
    fn optional_cli_args() {
        let draft = Draft {
            model: "claude-opus-5-5".to_owned(),
            budget: "2.50".to_owned(),
            permission_mode: PermissionMode::AcceptEdits,
            ..Draft::default()
        };
        let args = cli_args(&settings(draft), SessionId::parse("abc").as_ref());
        let joined = args.join(" ");
        assert!(joined.contains("--permission-mode acceptEdits"));
        assert!(joined.contains("--model claude-opus-5-5"));
        assert!(joined.contains("--max-budget-usd 2.50"));
        assert!(args.contains(&"--resume=abc".to_owned()));
    }
}
