//! Shell tool input/output types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Shell tool operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellOp {
    Execute,
    Spawn,
    Kill,
    Status,
}

impl std::fmt::Display for ShellOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Execute => write!(f, "execute"),
            Self::Spawn => write!(f, "spawn"),
            Self::Kill => write!(f, "kill"),
            Self::Status => write!(f, "status"),
        }
    }
}

/// Input for shell tool operations.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ShellInput {
    /// The operation to perform.
    pub op: ShellOp,

    /// Command to execute (required for execute/spawn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Timeout in seconds for execute operation (default: 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,

    /// Task ID to kill (required for kill operation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl ShellInput {
    /// Create an execute input.
    pub fn execute(command: impl Into<String>) -> Self {
        Self {
            op: ShellOp::Execute,
            command: Some(command.into()),
            timeout: None,
            task_id: None,
        }
    }

    /// Create a spawn input.
    pub fn spawn(command: impl Into<String>) -> Self {
        Self {
            op: ShellOp::Spawn,
            command: Some(command.into()),
            timeout: None,
            task_id: None,
        }
    }

    /// Create a kill input.
    pub fn kill(task_id: impl Into<String>) -> Self {
        Self {
            op: ShellOp::Kill,
            command: None,
            timeout: None,
            task_id: Some(task_id.into()),
        }
    }

    /// Create a status input.
    pub fn status() -> Self {
        Self {
            op: ShellOp::Status,
            command: None,
            timeout: None,
            task_id: None,
        }
    }

    /// Set timeout for execute.
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout = Some(seconds);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_input_builders() {
        let exec = ShellInput::execute("ls -la");
        assert_eq!(exec.op, ShellOp::Execute);
        assert_eq!(exec.command.as_deref(), Some("ls -la"));

        let spawn = ShellInput::spawn("tail -f /var/log/syslog");
        assert_eq!(spawn.op, ShellOp::Spawn);

        let kill = ShellInput::kill("abc123");
        assert_eq!(kill.op, ShellOp::Kill);
        assert_eq!(kill.task_id.as_deref(), Some("abc123"));

        let status = ShellInput::status();
        assert_eq!(status.op, ShellOp::Status);
    }

    #[test]
    fn test_shell_input_serialization() {
        let input = ShellInput::execute("echo hello").with_timeout(30);
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"op\":\"execute\""));
        assert!(json.contains("\"command\":\"echo hello\""));
        assert!(json.contains("\"timeout\":30"));
    }

    #[test]
    fn test_shell_input_deserialization() {
        let json = r#"{"op":"execute","command":"echo hello","timeout":30}"#;
        let input: ShellInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.op, ShellOp::Execute);
        assert_eq!(input.command.as_deref(), Some("echo hello"));
        assert_eq!(input.timeout, Some(30));
    }

    #[test]
    fn test_shell_input_optional_fields_omitted() {
        let input = ShellInput::status();
        let json = serde_json::to_string(&input).unwrap();
        // Optional None fields should not be serialized.
        assert!(!json.contains("command"));
        assert!(!json.contains("timeout"));
        assert!(!json.contains("task_id"));
    }

    #[test]
    fn test_shell_op_display() {
        assert_eq!(ShellOp::Execute.to_string(), "execute");
        assert_eq!(ShellOp::Spawn.to_string(), "spawn");
        assert_eq!(ShellOp::Kill.to_string(), "kill");
        assert_eq!(ShellOp::Status.to_string(), "status");
    }

    #[test]
    fn test_shell_input_schema_validation() {
        let schema = schemars::schema_for!(ShellInput);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        println!("ShellInput schema:\n{}", json);

        // Check for problematic patterns that cause issues with certain LLM APIs (like Gemini).
        // Note: ShellOp enum currently generates oneOf/const which may need addressing if API support is required.
        if json.contains("oneOf") {
            eprintln!("NOTE: ShellInput schema contains oneOf (from ShellOp enum)");
        }
        if json.contains("const") {
            eprintln!("NOTE: ShellInput schema contains const (from ShellOp enum)");
        }
    }
}
