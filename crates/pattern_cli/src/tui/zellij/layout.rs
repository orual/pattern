// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! KDL layout generation for zellij sessions via Askama templates.
//!
//! [`PatternLayout`] renders a `layout { ... }` KDL document that zellij
//! reads on startup. Two factory methods cover the common cases:
//! [`PatternLayout::single`] (one agent pane) and
//! [`PatternLayout::constellation`] (one pane per agent).
//!
//! Every user-supplied string (binary paths, agent names, command args) is
//! escaped via [`kdl_escape`] before reaching the template. The template
//! itself uses `escape = "none"` because KDL's quoting rules differ from
//! HTML's and Askama has no built-in KDL escape.

use askama::Template;

/// Escape a string for inclusion inside a KDL double-quoted string literal.
///
/// Quotes, backslashes, ASCII C0 control characters (0x00–0x1F), and DEL
/// (0x7F) are replaced with their standard escape sequences. Non-control
/// codepoints, including all printable ASCII and the full non-ASCII range,
/// are passed through verbatim — KDL strings are UTF-8 and accept any
/// non-control codepoint.
///
/// Without this, a path or agent name containing a literal `"` or `\` would
/// produce invalid KDL and zellij would fail to parse the layout at startup.
fn kdl_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\x7f' => {
                // Other control characters — use \u{hex} form.
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Definition of a single zellij pane in the layout.
///
/// Fields are `pub(crate)` so the KDL-escape invariant (all string fields
/// are pre-escaped via [`kdl_escape`]) can only be established by the
/// factory methods on [`PatternLayout`]. External callers construct a
/// layout via `PatternLayout::single` / `::constellation` / `::with_daemon`.
#[derive(Debug, Clone)]
pub struct PaneDef {
    /// Optional pane label shown in the zellij tab bar.
    pub(crate) name: Option<String>,
    /// The command to run inside the pane (typically `"pattern"`).
    pub(crate) command: String,
    /// Arguments passed to the command.
    pub(crate) args: Vec<String>,
    /// Optional size as a percentage of the containing split.
    pub(crate) size_pct: Option<u16>,
}

/// Askama template that renders a zellij KDL layout document.
#[derive(Template)]
#[template(path = "zellij_layout.kdl", escape = "none")]
pub struct PatternLayout {
    /// Ordered list of panes to include in the `Pattern` tab.
    pub(crate) chat_panes: Vec<PaneDef>,
    /// Optional daemon tab. When `Some`, zellij creates a second tab named
    /// `"pattern-daemon"` running the server. The tab is NOT marked focused,
    /// so the Pattern chat tab stays in front on session startup (AC6.8).
    pub(crate) daemon: Option<PaneDef>,
}

impl PatternLayout {
    /// Single-pane layout for `pattern chat [--no-auto-launch-zj] [@agent]`.
    ///
    /// `pattern_bin` is the path (or name) of the pattern binary zellij should
    /// launch inside the pane. Callers in production use
    /// [`super::locate_pattern_binary`] so spawned panes invoke the same build
    /// as the caller (important for development when `pattern` is not on
    /// `PATH`).
    ///
    /// The rendered pane always includes `--no-auto-launch-zj` so that a pane
    /// spawned inside an existing zellij session doesn't try to re-launch zellij.
    ///
    /// The returned layout has no daemon tab — call [`PatternLayout::with_daemon`]
    /// to add one for AC6.8.
    pub fn single(pattern_bin: &str, agent: Option<&str>) -> Self {
        let mut args = vec!["chat".to_string(), "--no-auto-launch-zj".to_string()];
        if let Some(agent) = agent {
            args.push(kdl_escape(&format!("@{agent}")));
        }
        Self {
            chat_panes: vec![PaneDef {
                name: agent.map(|a| kdl_escape(&format!("@{a}"))),
                command: kdl_escape(pattern_bin),
                args,
                size_pct: None,
            }],
            daemon: None,
        }
    }

    /// Multi-agent layout — one pane per agent, sized equally.
    ///
    /// See [`PatternLayout::single`] for the `pattern_bin` contract.
    pub fn constellation(pattern_bin: &str, agents: &[String]) -> Self {
        let chat_panes = agents
            .iter()
            .map(|name| PaneDef {
                name: Some(kdl_escape(&format!("@{name}"))),
                command: kdl_escape(pattern_bin),
                args: vec![
                    "chat".to_string(),
                    kdl_escape(&format!("@{name}")),
                    "--no-auto-launch-zj".to_string(),
                ],
                size_pct: None,
            })
            .collect();
        Self {
            chat_panes,
            daemon: None,
        }
    }

    /// Attach a `pattern-daemon` tab to this layout.
    ///
    /// The daemon itself runs as a detached background process (managed by
    /// `ensure_daemon_running`); this tab is a log viewer that `tail -F`s the
    /// daemon's log file. Using `tail -F` means the pane keeps following the
    /// file even across rotations or pre-creation, and — because the daemon
    /// is not a child of zellij — the daemon survives zellij session exit.
    ///
    /// The tab is NOT marked focused, so the Pattern chat tab stays in front
    /// on session startup (AC6.8).
    pub fn with_daemon(mut self, log_path: &str) -> Self {
        self.daemon = Some(PaneDef {
            name: Some("pattern-daemon".to_string()),
            command: "tail".to_string(),
            args: vec!["-F".to_string(), kdl_escape(log_path)],
            size_pct: None,
        });
        self
    }

    /// Render the layout to `<daemon_state_dir>/layout.kdl` and return
    /// the path.
    ///
    /// Writing to a deterministic path avoids races: zellij may read
    /// the file asynchronously after the launch command returns, so a
    /// tempfile that gets dropped immediately is unreliable. The path
    /// matches `DaemonState::state_dir()` so layout, state, cert, and
    /// log all live together.
    pub fn write_layout(&self) -> std::io::Result<std::path::PathBuf> {
        let rendered = self.render().map_err(std::io::Error::other)?;
        let dir = pattern_server::state::DaemonState::state_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("layout.kdl");
        std::fs::write(&path, rendered.as_bytes())?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_BIN: &str = "/abs/path/to/pattern";

    #[test]
    fn single_layout_contains_pattern_command_and_no_auto_launch() {
        let layout = PatternLayout::single(TEST_BIN, None);
        let rendered = layout.render().expect("render must succeed");
        assert!(rendered.contains(&format!(r#"command "{TEST_BIN}""#)));
        assert!(rendered.contains(r#""--no-auto-launch-zj""#));
    }

    #[test]
    fn single_layout_with_agent_includes_at_prefix_and_no_auto_launch() {
        let layout = PatternLayout::single(TEST_BIN, Some("supervisor"));
        let rendered = layout.render().expect("render must succeed");
        assert!(rendered.contains(r#""@supervisor""#));
        assert!(rendered.contains(r#""--no-auto-launch-zj""#));
    }

    #[test]
    fn constellation_layout_has_one_block_per_agent() {
        let agents = vec!["alpha".into(), "beta".into(), "gamma".into()];
        let layout = PatternLayout::constellation(TEST_BIN, &agents);
        let rendered = layout.render().expect("render must succeed");
        assert_eq!(
            rendered
                .matches(&format!(r#"command "{TEST_BIN}""#))
                .count(),
            3,
            "expected 3 pane blocks, got:\n{rendered}"
        );
        assert!(rendered.contains(r#""@alpha""#));
        assert!(rendered.contains(r#""@beta""#));
        assert!(rendered.contains(r#""@gamma""#));
    }

    #[test]
    fn generated_kdl_is_syntactically_valid() {
        let layout = PatternLayout::single(TEST_BIN, Some("supervisor"));
        let rendered = layout.render().expect("render must succeed");
        rendered
            .parse::<kdl::KdlDocument>()
            .unwrap_or_else(|e| panic!("rendered KDL failed to parse: {e}\n---\n{rendered}"));
    }

    #[test]
    fn constellation_kdl_is_syntactically_valid() {
        let layout = PatternLayout::constellation(
            TEST_BIN,
            &["alpha".into(), "beta".into(), "gamma".into()],
        );
        let rendered = layout.render().expect("render must succeed");
        rendered
            .parse::<kdl::KdlDocument>()
            .unwrap_or_else(|e| panic!("constellation KDL failed to parse: {e}\n---\n{rendered}"));
    }

    const LOG_PATH: &str = "/abs/path/to/daemon.log";

    #[test]
    fn default_layout_has_no_daemon_tab() {
        let layout = PatternLayout::single(TEST_BIN, None);
        let rendered = layout.render().expect("render must succeed");
        assert!(
            !rendered.contains("pattern-daemon"),
            "layout without with_daemon() must not include a daemon tab, got:\n{rendered}"
        );
    }

    #[test]
    fn with_daemon_adds_second_unfocused_tab() {
        let layout = PatternLayout::single(TEST_BIN, Some("supervisor")).with_daemon(LOG_PATH);
        let rendered = layout.render().expect("render must succeed");

        // Two tabs total — chat + daemon.
        assert_eq!(
            rendered.matches("tab name=").count(),
            2,
            "expected 2 tabs, got:\n{rendered}"
        );
        // Daemon tab is not marked focused.
        assert!(
            rendered.contains(r#"tab name="pattern-daemon""#),
            "expected pattern-daemon tab, got:\n{rendered}"
        );
        // The only focus=true is on the Pattern tab.
        assert_eq!(
            rendered.matches("focus=true").count(),
            1,
            "exactly one tab must have focus=true, got:\n{rendered}"
        );
        // Daemon tab tails the log file rather than running the server.
        assert!(
            rendered.contains(r#"command "tail""#),
            "daemon tab must invoke tail, got:\n{rendered}"
        );
        assert!(
            rendered.contains(r#"args "-F" "/abs/path/to/daemon.log""#),
            "daemon tab must pass -F + log path to tail, got:\n{rendered}"
        );
    }

    #[test]
    fn with_daemon_kdl_is_syntactically_valid() {
        let layout = PatternLayout::single(TEST_BIN, Some("supervisor")).with_daemon(LOG_PATH);
        let rendered = layout.render().expect("render must succeed");
        rendered
            .parse::<kdl::KdlDocument>()
            .unwrap_or_else(|e| panic!("with_daemon KDL failed to parse: {e}\n---\n{rendered}"));
    }

    #[test]
    fn constellation_with_daemon_has_daemon_tab() {
        let layout = PatternLayout::constellation(TEST_BIN, &["alpha".into(), "beta".into()])
            .with_daemon(LOG_PATH);
        let rendered = layout.render().expect("render must succeed");
        assert!(
            rendered.contains(r#"tab name="pattern-daemon""#),
            "expected pattern-daemon tab in constellation layout, got:\n{rendered}"
        );
    }

    /// Paths and agent names containing KDL-special characters (`"`, `\`,
    /// control bytes) must be escaped so the rendered layout stays valid
    /// KDL. Without this, zellij would refuse to parse the layout and the
    /// session would fail to launch with no diagnostic.
    #[test]
    fn special_characters_are_escaped_in_rendered_kdl() {
        // Path with an embedded quote and a backslash — both legal on UNIX
        // filesystems, both KDL-special.
        let tricky_bin = "/weird/path\"with\\quote";
        let layout = PatternLayout::single(tricky_bin, Some("agent\"name"));
        let rendered = layout.render().expect("render must succeed");

        // Rendered KDL must parse.
        rendered
            .parse::<kdl::KdlDocument>()
            .unwrap_or_else(|e| panic!("escaped KDL failed to parse: {e}\n---\n{rendered}"));

        // The escaped forms must be present (and the raw unescaped quote
        // must NOT appear adjacent to the path, which would break quoting).
        assert!(
            rendered.contains(r#"/weird/path\"with\\quote"#),
            "expected escaped binary path in output, got:\n{rendered}"
        );
        assert!(
            rendered.contains(r#"@agent\"name"#),
            "expected escaped agent name in output, got:\n{rendered}"
        );
    }
}
