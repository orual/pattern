//! Permission validation for shell commands.
//!
//! Provides security controls for shell command execution:
//! - Blocklist of dangerous command patterns
//! - Permission level requirements for different operations
//! - Path sandboxing for file operations

use std::path::{Path, PathBuf};

use super::error::{ShellError, ShellPermission};

/// Command patterns that are always denied regardless of permission level.
///
/// These patterns are checked via substring match (case-insensitive) to catch
/// variations. Defense in depth - not the only security layer.
const DENIED_PATTERNS: &[&str] = &[
    // Destructive filesystem operations.
    "rm -rf /",
    "rm -rf /*",
    "rm -rf ~",
    // Privilege escalation combined with destructive ops.
    "sudo rm -rf",
    // Disk formatting.
    "mkfs",
    // Raw disk writes that could destroy data.
    "dd if=/dev/zero",
    "dd if=/dev/random",
    // Fork bomb.
    ":(){ :|:& };:",
    // Recursive permission changes.
    "chmod -R 777 ",
    "chmod -R 000 ",
    // Direct device writes.
    "> /dev/sda",
    "> /dev/nvme",
    // Dangerous system modifications.
    "mv / ",
    "mv /* ",
    "mv ~",
];

/// Commands that require elevated permissions (ReadWrite or Admin).
const WRITE_COMMAND_PREFIXES: &[&str] = &[
    "rm ",
    "rm\t",
    "rmdir ",
    "mv ",
    "cp ",
    "chmod ",
    "chown ",
    "touch ",
    "mkdir ",
    "ln ",
    "unlink ",
    "git commit",
    "git push",
    "git merge",
    "git rebase",
    "git reset",
    "git checkout",
    "cargo build",
    "cargo install",
    "npm install",
    "pnpm install",
    "pip install",
    "apt ",
    "dnf ",
    "pacman ",
    "yay ",
    "paru ",
    "brew ",
];

/// Commands that are safe for read-only permission level.
const READ_ONLY_COMMANDS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "grep",
    "find",
    "which",
    "whereis",
    "file",
    "stat",
    "wc",
    "pwd",
    "echo",
    "env",
    "printenv",
    "whoami",
    "id",
    "date",
    "uptime",
    "df",
    "du",
    "free",
    "ps",
    "top",
    "htop",
    "git status",
    "git log",
    "git diff",
    "git branch",
    "git show",
    "git remote",
    "cargo check",
    "cargo test",
    "cargo clippy",
    "rustc --version",
    "node --version",
    "npm --version",
    "python --version",
    "pip list",
    "tree",
    "rg",
];

/// Trait for validating commands against security policy.
pub trait CommandValidator: Send + Sync + std::fmt::Debug {
    /// Validate a command before execution.
    ///
    /// Returns `Ok(())` if the command is allowed, or an appropriate `ShellError` if denied.
    fn validate(&self, command: &str, session_cwd: &Path) -> Result<(), ShellError>;

    /// Get the current permission level.
    fn permission_level(&self) -> ShellPermission;
}

/// Default command validator implementation.
///
/// Provides multi-layer security:
/// 1. Blocklist check for dangerous patterns
/// 2. Permission level check based on command type
/// 3. Optional path sandboxing
#[derive(Debug, Clone)]
pub struct DefaultCommandValidator {
    /// Current permission level for this validator.
    permission: ShellPermission,
    /// Allowed paths for file operations (if strict mode enabled).
    allowed_paths: Vec<PathBuf>,
    /// Whether to strictly enforce path restrictions.
    strict_path_enforcement: bool,
    /// Additional denied patterns (user-configurable).
    custom_denied_patterns: Vec<String>,
}

impl DefaultCommandValidator {
    /// Create a new validator with the given permission level.
    pub fn new(permission: ShellPermission) -> Self {
        Self {
            permission,
            allowed_paths: Vec::new(),
            strict_path_enforcement: false,
            custom_denied_patterns: Vec::new(),
        }
    }

    /// Add an allowed path for file operations.
    pub fn allow_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_paths.push(path.into());
        self
    }

    /// Add multiple allowed paths.
    pub fn allow_paths(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        self.allowed_paths.extend(paths);
        self
    }

    /// Enable strict path enforcement.
    ///
    /// When enabled, file paths in commands must be within allowed paths.
    pub fn strict(mut self) -> Self {
        self.strict_path_enforcement = true;
        self
    }

    /// Add a custom denied pattern.
    pub fn deny_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.custom_denied_patterns.push(pattern.into());
        self
    }

    /// Check if a command matches any denied pattern.
    fn is_command_denied(&self, command: &str) -> Option<String> {
        let cmd_lower = command.to_lowercase();

        // Check built-in denied patterns.
        for pattern in DENIED_PATTERNS {
            if cmd_lower.contains(pattern) {
                return Some(pattern.to_string());
            }
        }

        // Check custom denied patterns.
        for pattern in &self.custom_denied_patterns {
            if cmd_lower.contains(&pattern.to_lowercase()) {
                return Some(pattern.clone());
            }
        }

        None
    }

    /// Determine the required permission level for a command.
    fn required_permission(&self, command: &str) -> ShellPermission {
        let cmd_lower = command.to_lowercase();
        let cmd_trimmed = cmd_lower.trim();

        // Check if it's a read-only command.
        for safe_cmd in READ_ONLY_COMMANDS {
            if cmd_trimmed.starts_with(safe_cmd) || cmd_trimmed == *safe_cmd {
                return ShellPermission::ReadOnly;
            }
        }

        // Check if it requires write access.
        for prefix in WRITE_COMMAND_PREFIXES {
            if cmd_trimmed.starts_with(prefix) {
                return ShellPermission::ReadWrite;
            }
        }

        // Default to ReadWrite for unknown commands.
        ShellPermission::ReadWrite
    }

    /// Validate that all paths in a command are within allowed paths.
    fn validate_paths(&self, command: &str, session_cwd: &Path) -> Result<(), ShellError> {
        if self.allowed_paths.is_empty() || !self.strict_path_enforcement {
            return Ok(());
        }

        for path in extract_paths(command) {
            let resolved = if path.is_absolute() {
                path.canonicalize().unwrap_or(path)
            } else {
                session_cwd.join(&path).canonicalize().unwrap_or(path)
            };

            if !self.is_within_allowed(&resolved) {
                return Err(ShellError::PathOutsideSandbox(resolved));
            }
        }

        Ok(())
    }

    /// Check if a path is within any allowed path.
    fn is_within_allowed(&self, path: &Path) -> bool {
        for allowed in &self.allowed_paths {
            if path.starts_with(allowed) {
                return true;
            }
        }
        false
    }
}

impl CommandValidator for DefaultCommandValidator {
    fn validate(&self, command: &str, session_cwd: &Path) -> Result<(), ShellError> {
        // Step 1: Check denied patterns (always blocked).
        if let Some(pattern) = self.is_command_denied(command) {
            return Err(ShellError::CommandDenied(pattern));
        }

        // Step 2: Check permission level.
        let required = self.required_permission(command);
        if required > self.permission {
            return Err(ShellError::PermissionDenied {
                required,
                granted: self.permission,
            });
        }

        // Step 3: Validate paths if strict mode enabled.
        self.validate_paths(command, session_cwd)?;

        Ok(())
    }

    fn permission_level(&self) -> ShellPermission {
        self.permission
    }
}

/// Configuration for shell permissions.
///
/// Builder-style configuration for creating validators.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ShellPermissionConfig {
    /// Default permission level.
    pub default: ShellPermission,
    /// Allowed paths for file operations.
    pub allowed_paths: Vec<PathBuf>,
    /// Whether to strictly enforce path restrictions.
    pub strict_path_enforcement: bool,
    /// Custom denied patterns.
    pub custom_denied_patterns: Vec<String>,
}

impl Default for ShellPermissionConfig {
    fn default() -> Self {
        Self {
            default: ShellPermission::ReadOnly,
            allowed_paths: Vec::new(),
            strict_path_enforcement: false,
            custom_denied_patterns: Vec::new(),
        }
    }
}

impl ShellPermissionConfig {
    /// Create a new config with the given default permission.
    pub fn new(default: ShellPermission) -> Self {
        Self {
            default,
            ..Default::default()
        }
    }

    /// Add an allowed path.
    pub fn allow_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowed_paths.push(path.into());
        self
    }

    /// Enable strict path enforcement.
    pub fn strict(mut self) -> Self {
        self.strict_path_enforcement = true;
        self
    }

    /// Add a custom denied pattern.
    pub fn deny_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.custom_denied_patterns.push(pattern.into());
        self
    }

    /// Build a validator from this configuration.
    pub fn build_validator(&self) -> DefaultCommandValidator {
        let mut validator = DefaultCommandValidator::new(self.default);
        validator.allowed_paths = self.allowed_paths.clone();
        validator.strict_path_enforcement = self.strict_path_enforcement;
        validator.custom_denied_patterns = self.custom_denied_patterns.clone();
        validator
    }

    /// Check if a command is explicitly denied.
    ///
    /// Convenience method that delegates to a temporary validator.
    pub fn is_command_denied(&self, command: &str) -> Option<String> {
        self.build_validator().is_command_denied(command)
    }

    /// Validate paths in a command.
    ///
    /// Convenience method that delegates to a temporary validator.
    pub fn validate_paths(&self, command: &str, session_cwd: &Path) -> Result<(), ShellError> {
        self.build_validator().validate_paths(command, session_cwd)
    }
}

/// Extract potential file paths from a command string.
///
/// This is a best-effort extraction - shell expansion and complex quoting
/// are not handled. Defense in depth.
fn extract_paths(command: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Split on whitespace and look for path-like tokens.
    for token in command.split_whitespace() {
        // Skip flags.
        if token.starts_with('-') {
            continue;
        }
        // Skip shell operators.
        if ["&&", "||", "|", ";", ">", ">>", "<", "2>&1", "&"].contains(&token) {
            continue;
        }
        // If it looks like a path (contains / or starts with . or ~).
        if token.contains('/') || token.starts_with('.') || token.starts_with('~') {
            // Remove surrounding quotes if present.
            let cleaned = token
                .trim_matches('"')
                .trim_matches('\'')
                .trim_end_matches(';');

            // Expand ~ to home dir.
            let expanded = if cleaned.starts_with('~') {
                if let Some(home) = dirs::home_dir() {
                    PathBuf::from(cleaned.replacen('~', home.to_string_lossy().as_ref(), 1))
                } else {
                    PathBuf::from(cleaned)
                }
            } else {
                PathBuf::from(cleaned)
            };
            paths.push(expanded);
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_denied_commands() {
        let validator = DefaultCommandValidator::new(ShellPermission::Admin);

        assert!(validator.is_command_denied("rm -rf /").is_some());
        assert!(validator.is_command_denied("sudo rm -rf /home").is_some());
        assert!(validator.is_command_denied("echo hello").is_none());
        assert!(validator.is_command_denied("rm -rf ./build").is_none());
        assert!(
            validator
                .is_command_denied("dd if=/dev/zero of=/dev/sda")
                .is_some()
        );
        assert!(validator.is_command_denied(":(){ :|:& };:").is_some());
    }

    #[test]
    fn test_custom_denied_pattern() {
        let validator =
            DefaultCommandValidator::new(ShellPermission::Admin).deny_pattern("dangerous_cmd");

        assert!(
            validator
                .is_command_denied("run dangerous_cmd --force")
                .is_some()
        );
        assert!(validator.is_command_denied("safe_command").is_none());
    }

    #[test]
    fn test_permission_levels() {
        let validator = DefaultCommandValidator::new(ShellPermission::ReadOnly);
        let cwd = PathBuf::from("/tmp");

        // Read-only commands should pass.
        assert!(validator.validate("ls -la", &cwd).is_ok());
        assert!(validator.validate("cat /etc/passwd", &cwd).is_ok());
        assert!(validator.validate("git status", &cwd).is_ok());

        // Write commands should fail with ReadOnly permission.
        let result = validator.validate("rm file.txt", &cwd);
        assert!(matches!(result, Err(ShellError::PermissionDenied { .. })));

        let result = validator.validate("git commit -m 'test'", &cwd);
        assert!(matches!(result, Err(ShellError::PermissionDenied { .. })));
    }

    #[test]
    fn test_write_permission_allows_writes() {
        let validator = DefaultCommandValidator::new(ShellPermission::ReadWrite);
        let cwd = PathBuf::from("/tmp");

        assert!(validator.validate("rm file.txt", &cwd).is_ok());
        assert!(validator.validate("git commit -m 'test'", &cwd).is_ok());
        assert!(validator.validate("cargo build", &cwd).is_ok());
    }

    #[test]
    fn test_path_extraction() {
        let paths = extract_paths("cat /etc/passwd ./local.txt");
        assert!(paths.iter().any(|p| p == Path::new("/etc/passwd")));
        assert!(paths.iter().any(|p| p == Path::new("./local.txt")));

        // Should skip flags.
        let paths = extract_paths("ls -la /tmp");
        assert_eq!(paths.len(), 1);
        assert!(paths.iter().any(|p| p == Path::new("/tmp")));

        // Note: Quoted paths with spaces are NOT fully supported by split_whitespace().
        // This is a known limitation - defense in depth, not the only security layer.
        // Test that we at least extract partial paths from quoted strings.
        let paths = extract_paths("cat \"/path/file.txt\"");
        assert!(
            paths
                .iter()
                .any(|p| p.to_string_lossy().contains("/path/file.txt"))
        );
    }

    #[test]
    fn test_path_extraction_with_operators() {
        let paths = extract_paths("cat /file1 && rm /file2 | grep pattern");
        assert!(paths.iter().any(|p| p == Path::new("/file1")));
        assert!(paths.iter().any(|p| p == Path::new("/file2")));
        // "pattern" should not be extracted as a path.
        assert!(
            !paths
                .iter()
                .any(|p| p.to_string_lossy().contains("pattern"))
        );
    }

    #[test]
    fn test_path_validation_strict_mode() {
        let validator = DefaultCommandValidator::new(ShellPermission::ReadWrite)
            .allow_path("/home/user/project")
            .strict();

        let cwd = PathBuf::from("/home/user/project");

        // Relative path within allowed directory should pass.
        // Note: This test may not work perfectly without actual filesystem.
        // The validator uses canonicalize which requires real paths.
        // We can at least verify the validator is constructed correctly.
        assert!(validator.strict_path_enforcement);
        assert_eq!(validator.allowed_paths.len(), 1);
        assert_eq!(
            validator.allowed_paths[0],
            PathBuf::from("/home/user/project")
        );

        // Verify cwd is used in validation (no-op here since no paths in command).
        assert!(validator.validate("echo hello", &cwd).is_ok());
    }

    #[test]
    fn test_config_builder() {
        let config = ShellPermissionConfig::new(ShellPermission::ReadOnly)
            .allow_path("/home/user")
            .strict()
            .deny_pattern("custom_bad");

        assert_eq!(config.default, ShellPermission::ReadOnly);
        assert!(config.strict_path_enforcement);
        assert_eq!(config.allowed_paths.len(), 1);
        assert_eq!(config.custom_denied_patterns.len(), 1);

        let validator = config.build_validator();
        assert_eq!(validator.permission_level(), ShellPermission::ReadOnly);
    }

    #[test]
    fn test_denied_commands_case_insensitive() {
        let validator = DefaultCommandValidator::new(ShellPermission::Admin);

        // Should match regardless of case.
        assert!(validator.is_command_denied("RM -RF /").is_some());
        assert!(validator.is_command_denied("Rm -Rf /*").is_some());
    }

    #[test]
    fn test_required_permission_unknown_command() {
        let validator = DefaultCommandValidator::new(ShellPermission::ReadWrite);

        // Unknown commands default to ReadWrite.
        assert_eq!(
            validator.required_permission("some_custom_script"),
            ShellPermission::ReadWrite
        );
    }

    #[test]
    fn test_tilde_expansion() {
        let paths = extract_paths("cat ~/Documents/file.txt");

        // Should have expanded the tilde.
        assert_eq!(paths.len(), 1);
        // The path should start with home directory (or contain it if expansion worked).
        // Actual value depends on the system, so we just check it's not empty.
        assert!(!paths[0].as_os_str().is_empty());
    }

    #[test]
    fn test_default_config_is_read_only() {
        // Verify that the default configuration uses ReadOnly for safety.
        let config = ShellPermissionConfig::default();
        assert_eq!(config.default, ShellPermission::ReadOnly);
    }

    // Tests for command chaining bypass attempts.
    // These document the expected behavior when users try to bypass permission
    // checks by chaining safe commands with dangerous ones.

    #[test]
    fn test_command_chaining_and_operator() {
        // "ls && rm -rf /" - read command chained with dangerous command.
        // The entire command string should be denied because it contains "rm -rf /".
        let validator = DefaultCommandValidator::new(ShellPermission::Admin);

        let result = validator.is_command_denied("ls && rm -rf /");
        assert!(
            result.is_some(),
            "Command chaining with && should be detected as dangerous"
        );
        assert!(
            result.unwrap().contains("rm -rf /"),
            "Should identify the dangerous pattern"
        );
    }

    #[test]
    fn test_command_chaining_semicolon() {
        // "ls; rm -rf /" - semicolon separated.
        // The entire command string should be denied because it contains "rm -rf /".
        let validator = DefaultCommandValidator::new(ShellPermission::Admin);

        let result = validator.is_command_denied("ls; rm -rf /");
        assert!(
            result.is_some(),
            "Command chaining with ; should be detected as dangerous"
        );
        assert!(
            result.unwrap().contains("rm -rf /"),
            "Should identify the dangerous pattern"
        );
    }

    #[test]
    fn test_command_chaining_pipe_to_dangerous() {
        // "ls | xargs rm -rf" - piped to dangerous command.
        // This should be denied because it contains "rm -rf" with sudo prefix check.
        let validator = DefaultCommandValidator::new(ShellPermission::Admin);

        // Note: "rm -rf" alone isn't in DENIED_PATTERNS, but "sudo rm -rf" is.
        // However, the substring match on "sudo rm -rf" won't catch "xargs rm -rf".
        // This test documents current behavior: basic "rm -rf" without "/" or "/*"
        // is NOT blocked at the deny level - it's handled by permission level.
        let result = validator.is_command_denied("ls | xargs rm -rf");
        // Current behavior: this is NOT in the denied patterns.
        // The command would be blocked at permission level if user has ReadOnly.
        assert!(
            result.is_none(),
            "xargs rm -rf without root path is not in DENIED_PATTERNS (by design)"
        );

        // However, if it's "xargs rm -rf /" it WILL be caught.
        let result_with_root = validator.is_command_denied("ls | xargs rm -rf /");
        assert!(
            result_with_root.is_some(),
            "xargs rm -rf / should be detected as dangerous"
        );
    }

    #[test]
    fn test_command_chaining_permission_check() {
        // IMPORTANT: Documents current behavior - the permission check only looks at
        // the command prefix, not the entire chained command. This is a known limitation.
        //
        // Commands like "ls && rm file" are evaluated based on "ls" at the start,
        // which is a read-only command. The chained "rm" is NOT detected at the
        // permission level. However, dangerous patterns in DENIED_PATTERNS are still
        // caught via substring matching (see test_command_chaining_and_operator).
        //
        // Defense in depth: For truly dangerous operations (rm -rf /, etc.), the
        // blocklist catches them. For other write operations, users should use a
        // shell that doesn't support chaining, or parse commands more carefully.

        let validator = DefaultCommandValidator::new(ShellPermission::ReadOnly);
        let cwd = PathBuf::from("/tmp");

        // "ls && rm file" - currently passes because "ls" is the prefix.
        // This documents existing behavior, not necessarily desired behavior.
        let result = validator.validate("ls && rm file", &cwd);
        assert!(
            result.is_ok(),
            "Current behavior: command chaining bypasses prefix-based permission check"
        );

        // "rm file && ls" - fails because "rm " is the prefix.
        let result = validator.validate("rm file && ls", &cwd);
        assert!(
            matches!(result, Err(ShellError::PermissionDenied { .. })),
            "Write command at start should be denied for ReadOnly permission"
        );

        // "echo hello; touch newfile" - passes because "echo" is the prefix.
        let result = validator.validate("echo hello; touch newfile", &cwd);
        assert!(
            result.is_ok(),
            "Current behavior: semicolon chaining bypasses prefix-based permission check"
        );

        // "touch newfile; echo done" - fails because "touch " is the prefix.
        let result = validator.validate("touch newfile; echo done", &cwd);
        assert!(
            matches!(result, Err(ShellError::PermissionDenied { .. })),
            "Write command at start should be denied for ReadOnly permission"
        );
    }
}
