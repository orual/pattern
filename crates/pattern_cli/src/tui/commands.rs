//! Slash command registry and parser.
//!
//! Defines the built-in slash commands available in the TUI, their metadata
//! (target, argument hints), and a parser that splits `/command arg1 arg2`
//! input into structured parts for dispatch.

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
    pub arg_hint: ArgHint,
}

/// All built-in commands.
pub fn builtin_commands() -> &'static [CommandDef] {
    &[
        CommandDef {
            name: "clear",
            description: "Clear conversation view",
            target: CommandTarget::Local,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: "quit",
            description: "Exit the TUI",
            target: CommandTarget::Local,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: "panel",
            description: "Toggle side panel",
            target: CommandTarget::Local,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: "expand",
            description: "Expand focused section",
            target: CommandTarget::Local,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: "front",
            description: "Switch fronting persona",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::AgentName,
        },
        CommandDef {
            name: "agents",
            description: "List active agents",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: "status",
            description: "Show runtime status",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: "context",
            description: "Show context/memory info",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::None,
        },
        CommandDef {
            name: "shutdown",
            description: "Stop the daemon",
            target: CommandTarget::Runtime,
            arg_hint: ArgHint::None,
        },
    ]
}

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

/// Look up a command by name.
///
/// Supports only built-in commands. Plugin-namespaced commands (e.g.,
/// `plugin:cmd`) are forwarded to the daemon without registry lookup.
pub fn lookup_command(name: &str) -> Option<&'static CommandDef> {
    builtin_commands().iter().find(|c| c.name == name)
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
    fn lookup_command_found() {
        let cmd = lookup_command("clear");
        assert!(cmd.is_some());
        let cmd = cmd.unwrap();
        assert_eq!(cmd.name, "clear");
        assert_eq!(cmd.target, CommandTarget::Local);
    }

    #[test]
    fn lookup_command_not_found() {
        let cmd = lookup_command("nonexistent");
        assert!(cmd.is_none());
    }
}
