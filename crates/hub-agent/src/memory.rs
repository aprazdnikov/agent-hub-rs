//! The hub-owned project memory server, launched by a harness over stdio.

use std::path::Path;

use hub_core::domain::{AbsolutePath, BackendKind};
use serde_json::{Value, json};

/// Reserved for this integration; independent of the session-scoped chat tools.
pub const SERVER_NAME: &str = "agent-hub-memory";

#[derive(Clone, Debug)]
pub struct MemoryMcp {
    command: String,
    args: Vec<String>,
}

impl MemoryMcp {
    /// The application supplies its executable; protocol code never guesses a binary.
    pub fn new(program: &Path, project: &AbsolutePath, actor: BackendKind) -> Result<Self, String> {
        if !program.is_absolute() {
            return Err("Путь к серверу памяти должен быть абсолютным".to_owned());
        }
        let command = program.to_str().ok_or("Путь к серверу памяти не в UTF-8")?.to_owned();
        let project = project.as_path().to_str().ok_or("Каталог проекта не в UTF-8")?;
        Ok(Self {
            command,
            args: vec![
                "--memory-mcp".to_owned(),
                "--project-dir".to_owned(),
                project.to_owned(),
                "--actor".to_owned(),
                actor.name().to_owned(),
            ],
        })
    }

    /// ACP represents stdio servers without a transport discriminator.
    #[must_use]
    pub fn acp_server(&self) -> Value {
        json!({"name": SERVER_NAME, "command": self.command, "args": self.args, "env": []})
    }

    /// Claude Code accepts this stdio definition inside its per-launch `--mcp-config`.
    #[must_use]
    pub fn claude_server(&self) -> Value {
        json!({"type": "stdio", "command": self.command, "args": self.args, "env": {}})
    }
}

/// Dotted overrides touch only the reserved server, preserving every other MCP definition.
/// Explicit disabling also revokes this server when resuming a prior memory-enabled thread.
#[must_use]
pub fn codex_config(memory: Option<&MemoryMcp>) -> Value {
    match memory {
        Some(memory) => json!({"mcp_servers.agent-hub-memory": {
            "command": memory.command,
            "args": memory.args,
            "enabled": true,
            "startup_timeout_sec": 10,
            "tool_timeout_sec": 30,
        }}),
        // Codex validates a transport even for disabled servers. A deliberately nonexistent
        // command makes this a valid stdio definition that cannot accidentally launch a GUI.
        None => json!({"mcp_servers.agent-hub-memory": {
            "command": "__agent_hub_memory_disabled__", "args": [], "enabled": false,
        }}),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn stdio_arguments_preserve_spaces_and_metacharacters_without_a_shell() {
        let root = PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" });
        let program = root.join("hub app");
        let project = AbsolutePath::new(root.join("project $(echo surprise)")).unwrap();
        let memory = MemoryMcp::new(&program, &project, BackendKind::Qwen).unwrap();
        assert_eq!(
            memory.acp_server(),
            json!({
                "name": SERVER_NAME, "command": program.to_str().unwrap(),
                "args": ["--memory-mcp", "--project-dir", project.as_path().to_str().unwrap(), "--actor", "qwen"],
                "env": [],
            })
        );
    }

    #[test]
    fn codex_overrides_never_replace_the_mcp_servers_table() {
        let project = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let memory =
            MemoryMcp::new(&project.as_path().join("agent-hub"), &project, BackendKind::Codex)
                .unwrap();
        for config in [codex_config(Some(&memory)), codex_config(None)] {
            let keys: Vec<_> = config.as_object().unwrap().keys().map(String::as_str).collect();
            assert!(keys.iter().all(|key| key.starts_with("mcp_servers.agent-hub-memory")));
            assert!(!config.as_object().unwrap().contains_key("mcp_servers"));
        }
    }

    #[test]
    fn relative_program_is_rejected() {
        let project = AbsolutePath::new(std::env::temp_dir()).unwrap();
        assert!(MemoryMcp::new(Path::new("agent-hub"), &project, BackendKind::Codex).is_err());
    }
}
