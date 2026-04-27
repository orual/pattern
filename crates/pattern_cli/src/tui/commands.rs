//! Slash command registry and parser.
//!
//! Defines the built-in slash commands available in the TUI, their metadata
//! (target, argument hints), and a parser that splits `/command arg1 arg2`
//! input into structured parts for dispatch.
//!
//! [`CommandRegistry`] is the central lookup table. It starts populated with
//! all built-in commands and can be augmented at runtime with plugin commands
//! fetched from the daemon on session init.

/// Where the command is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTarget {
    /// Handled locally by the TUI (no daemon call).
    Local,
    /// Forwarded to the daemon's runtime.
    Runtime,
}

/// What kind of argument a command expects (for autocomplete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgHint {
    /// No arguments.
    None,
    /// Agent name (completable from daemon's agent list).
    AgentName,
    /// Free-form text.
    #[allow(dead_code)] // used by autocomplete argument completion (future)
    FreeText,
}

/// Definition of a slash command.
#[derive(Debug, Clone)]
pub struct CommandDef {
    /// The command name (without leading `/`).
    pub name: &'static str,
    /// Human-readable description for autocomplete display.
    pub description: &'static str,
    /// Where this command is dispatched.
    pub target: CommandTarget,
    /// What kind of argument the command expects.
    #[allow(dead_code)] // used by autocomplete argument completion (future)
    pub arg_hint: ArgHint,
}

// Local command names.
pub const CMD_CLEAR: &str = "clear";
pub const CMD_QUIT: &str = "quit";
pub const CMD_PANEL: &str = "panel";
pub const CMD_PANE: &str = "pane";
pub const CMD_FLOAT: &str = "float";

// Runtime command names.
pub const CMD_FRONT: &str = "front";
/// Phase 6 T8: one-shot direct-recipient override for the next outbound message.
pub const CMD_AGENT: &str = "agent";
pub const CMD_AGENTS: &str = "agents";
pub const CMD_STATUS: &str = "status";
pub const CMD_SHUTDOWN: &str = "shutdown";
pub const CMD_CANCEL: &str = "cancel";

/// All built-in commands.
pub fn builtin_commands() -> &'static [CommandDef] {
    &[
        CommandDef {
            name: CMD_CLEAR,
            description: "Clear conversation view",
            target: CommandTarget::Local,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: CMD_QUIT,
            description: "Exit the TUI",
            target: CommandTarget::Local,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: CMD_PANEL,
            description: "Toggle side panel",
            target: CommandTarget::Local,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: CMD_FRONT,
            description: "Set or clear the route-lock for outbound messages",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::AgentName,
        },
        CommandDef {
            name: CMD_AGENT,
            description: "One-shot direct override for the next outbound message",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::AgentName,
        },
        CommandDef {
            name: CMD_AGENTS,
            description: "List active agents",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: CMD_STATUS,
            description: "Show runtime status",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::None,
        },
        // Note: /context is not registered here. Context/memory display is
        // deferred; the status bar already shows token usage and dedicated
        // memory inspection is a larger design question.
        CommandDef {
            name: CMD_SHUTDOWN,
            description: "Stop the daemon",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: CMD_CANCEL,
            description: "Cancel the current response",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: CMD_PANE,
            description: "Open agent in new tiled pane (zellij)",
            target: CommandTarget::Local,
            arg_hint: ArgHint::AgentName,
        },
        CommandDef {
            name: CMD_FLOAT,
            description: "Open agent in floating pane (zellij)",
            target: CommandTarget::Local,
            arg_hint: ArgHint::AgentName,
        },
    ]
}

// ---------------------------------------------------------------------------
// CommandRegistry
// ---------------------------------------------------------------------------

/// A registered command entry. Unlike [`CommandDef`] (which uses `&'static str`
/// for built-ins), registry entries own their strings so that daemon-provided
/// plugin commands — whose names are not known at compile time — can be stored
/// alongside built-ins.
#[derive(Debug, Clone)]
pub struct RegistryEntry {
    /// Command name (without leading `/`).
    pub name: String,
    /// Human-readable description for autocomplete display.
    pub description: String,
    /// Where this command is dispatched.
    pub target: CommandTarget,
}

/// Mutable command registry that merges built-in TUI commands with any
/// additional commands fetched from the daemon on session init.
///
/// Built-in commands are loaded at construction; daemon-provided commands are
/// added via [`CommandRegistry::register_daemon_commands`]. Built-ins always
/// take precedence: if a daemon command has the same name as a built-in it is
/// silently ignored.
#[derive(Debug)]
pub struct CommandRegistry {
    entries: Vec<RegistryEntry>,
    /// Cached `(value, description)` pairs for autocomplete; rebuilt whenever
    /// entries change.
    candidates: Vec<(String, String)>,
}

impl CommandRegistry {
    /// Construct a registry pre-populated with all built-in commands.
    pub fn new() -> Self {
        let entries: Vec<RegistryEntry> = builtin_commands()
            .iter()
            .map(|cmd| RegistryEntry {
                name: cmd.name.to_string(),
                description: cmd.description.to_string(),
                target: cmd.target,
            })
            .collect();
        let candidates = Self::build_candidates(&entries);
        Self {
            entries,
            candidates,
        }
    }

    /// Add commands fetched from the daemon.
    ///
    /// Each item is a `(name, description)` pair. Commands with names that
    /// already exist in the registry (built-ins) are skipped. Daemon commands
    /// always get `CommandTarget::Runtime` since they require a daemon
    /// connection to execute.
    pub fn register_daemon_commands(&mut self, commands: Vec<(String, String)>) {
        let mut changed = false;
        for (name, description) in commands {
            if !self.entries.iter().any(|e| e.name == name) {
                self.entries.push(RegistryEntry {
                    name,
                    description,
                    target: CommandTarget::Runtime,
                });
                changed = true;
            }
        }
        if changed {
            self.candidates = Self::build_candidates(&self.entries);
        }
    }

    /// Look up a command by exact name.
    ///
    /// Plugin-namespaced commands (e.g. `plugin:cmd`) are forwarded to the
    /// daemon without registry lookup; callers should check for `:` before
    /// calling this.
    pub fn lookup(&self, name: &str) -> Option<&RegistryEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Return `(value, description)` pairs suitable for fuzzy autocomplete.
    pub fn candidates(&self) -> &[(String, String)] {
        &self.candidates
    }

    fn build_candidates(entries: &[RegistryEntry]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|e| (e.name.clone(), e.description.clone()))
            .collect()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a slash command string into (command_name, args).
///
/// Returns `None` if the string doesn't start with `/`.
pub fn parse_slash_command(input: &str) -> Option<(&str, Vec<&str>)> {
    let input = input.trim();
    let without_slash = input.strip_prefix('/')?;
    let mut parts = without_slash.split_whitespace();
    let command = parts.next()?;
    let args: Vec<&str> = parts.collect();
    Some((command, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_slash_command_basic() {
        let result = parse_slash_command("/quit");
        assert_eq!(result, Some(("quit", vec![])));
    }

    #[test]
    fn parse_slash_command_with_args() {
        let result = parse_slash_command("/front @supervisor");
        assert_eq!(result, Some(("front", vec!["@supervisor"])));
    }

    #[test]
    fn parse_slash_command_not_slash() {
        let result = parse_slash_command("hello");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_slash_command_namespaced() {
        let result = parse_slash_command("/plugin:cmd arg");
        assert_eq!(result, Some(("plugin:cmd", vec!["arg"])));
    }

    #[test]
    fn registry_lookup_builtin_found() {
        let reg = CommandRegistry::new();
        let entry = reg.lookup("clear");
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.name, "clear");
        assert_eq!(entry.target, CommandTarget::Local);
    }

    #[test]
    fn registry_lookup_not_found() {
        let reg = CommandRegistry::new();
        assert!(reg.lookup("nonexistent").is_none());
    }

    #[test]
    fn registry_daemon_commands_augment_candidates() {
        let mut reg = CommandRegistry::new();
        let initial_count = reg.candidates().len();

        reg.register_daemon_commands(vec![(
            "plugin:summarize".into(),
            "Summarise conversation".into(),
        )]);

        assert_eq!(reg.candidates().len(), initial_count + 1);
        let entry = reg.lookup("plugin:summarize").unwrap();
        assert_eq!(entry.target, CommandTarget::Runtime);
    }

    #[test]
    fn registry_daemon_commands_do_not_override_builtins() {
        let mut reg = CommandRegistry::new();
        let initial_count = reg.candidates().len();

        // "clear" is already a built-in local command.
        reg.register_daemon_commands(vec![("clear".into(), "Daemon version of clear".into())]);

        // Count must not increase; the built-in entry must still be Local.
        assert_eq!(reg.candidates().len(), initial_count);
        let entry = reg.lookup("clear").unwrap();
        assert_eq!(entry.target, CommandTarget::Local);
    }
}
