// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for zellij detection and layout generation.
//!
//! These tests verify the zellij integration without requiring a running
//! zellij session. Pane-spawning and session-launch commands are not tested
//! here because they require a live environment; see the human test plan for
//! manual verification of those paths.

use askama::Template as _;
use pattern_cli::tui::zellij::detect::{ZellijState, detect, session_name_for_project};
use pattern_cli::tui::zellij::layout::PatternLayout;

// ---------------------------------------------------------------------------
// Detection tests
// ---------------------------------------------------------------------------

#[test]
fn detect_returns_in_session_when_env_var_is_set() {
    // SAFETY: nextest runs each test in its own process, so env mutation is safe.
    unsafe { std::env::set_var("ZELLIJ_SESSION_NAME", "integration-test-session") };
    let state = detect();
    unsafe { std::env::remove_var("ZELLIJ_SESSION_NAME") };

    assert_eq!(
        state,
        ZellijState::InSession {
            session_name: "integration-test-session".into()
        }
    );
}

#[test]
fn detect_returns_available_or_not_available_without_env_var() {
    unsafe { std::env::remove_var("ZELLIJ_SESSION_NAME") };
    let state = detect();
    assert!(
        matches!(state, ZellijState::Available | ZellijState::NotAvailable),
        "expected Available or NotAvailable, got {state:?}"
    );
}

#[test]
fn detect_ignores_empty_env_var() {
    unsafe { std::env::set_var("ZELLIJ_SESSION_NAME", "") };
    let state = detect();
    unsafe { std::env::remove_var("ZELLIJ_SESSION_NAME") };

    // Empty string must NOT be treated as InSession.
    assert!(
        matches!(state, ZellijState::Available | ZellijState::NotAvailable),
        "empty ZELLIJ_SESSION_NAME should not be InSession, got {state:?}"
    );
}

#[test]
fn session_name_starts_with_pattern_prefix() {
    let name = session_name_for_project(None);
    assert!(
        name.starts_with("pattern-"),
        "session name must start with 'pattern-', got: {name}"
    );
}

#[test]
fn session_name_is_non_empty_after_prefix() {
    let name = session_name_for_project(None);
    assert!(
        name.len() > "pattern-".len(),
        "session name must have content after prefix, got: {name}"
    );
}

// ---------------------------------------------------------------------------
// Layout generation tests
// ---------------------------------------------------------------------------

const TEST_BIN: &str = "/abs/path/to/pattern";

#[test]
fn single_layout_renders_valid_kdl() {
    let layout = PatternLayout::single(TEST_BIN, None);
    let rendered = layout.render().expect("render must succeed");
    rendered
        .parse::<kdl::KdlDocument>()
        .unwrap_or_else(|e| panic!("layout KDL is not valid: {e}\n---\n{rendered}"));
}

#[test]
fn single_layout_with_agent_renders_valid_kdl() {
    let layout = PatternLayout::single(TEST_BIN, Some("supervisor"));
    let rendered = layout.render().expect("render must succeed");
    rendered
        .parse::<kdl::KdlDocument>()
        .unwrap_or_else(|e| panic!("layout KDL is not valid: {e}\n---\n{rendered}"));
}

#[test]
fn constellation_layout_renders_valid_kdl() {
    let layout =
        PatternLayout::constellation(TEST_BIN, &["alpha".into(), "beta".into(), "gamma".into()]);
    let rendered = layout.render().expect("render must succeed");
    rendered
        .parse::<kdl::KdlDocument>()
        .unwrap_or_else(|e| panic!("constellation KDL is not valid: {e}\n---\n{rendered}"));
}

#[test]
fn single_layout_includes_no_auto_launch_flag() {
    let layout = PatternLayout::single(TEST_BIN, None);
    let rendered = layout.render().expect("render must succeed");
    assert!(
        rendered.contains("--no-auto-launch-zj"),
        "layout must include --no-auto-launch-zj flag, got:\n{rendered}"
    );
}

#[test]
fn constellation_layout_has_correct_pane_count() {
    let agents = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
    let layout = PatternLayout::constellation(TEST_BIN, &agents);
    let rendered = layout.render().expect("render must succeed");
    let pane_count = rendered
        .matches(&format!(r#"command "{TEST_BIN}""#))
        .count();
    assert_eq!(
        pane_count, 3,
        "expected 3 pane blocks for 3 agents, got {pane_count}:\n{rendered}"
    );
}

#[test]
fn constellation_layout_includes_agent_args() {
    let agents = vec!["alpha".to_string(), "beta".to_string()];
    let layout = PatternLayout::constellation(TEST_BIN, &agents);
    let rendered = layout.render().expect("render must succeed");
    assert!(
        rendered.contains(r#""@alpha""#),
        "missing @alpha arg:\n{rendered}"
    );
    assert!(
        rendered.contains(r#""@beta""#),
        "missing @beta arg:\n{rendered}"
    );
}

// ---------------------------------------------------------------------------
// Pane command outside zellij tests
// ---------------------------------------------------------------------------

#[test]
fn pane_command_outside_zellij_shows_system_error() {
    use pattern_cli::tui::app::App;

    let mut app = App::new();
    app.set_zellij_state(ZellijState::NotAvailable);

    // No messages before dispatch.
    assert_eq!(
        app.conversation_batch_count(),
        0,
        "expected no messages before dispatch"
    );

    app.dispatch_slash_command("/pane @supervisor");

    // A system error message must have been pushed — the conversation should
    // now have exactly one batch.
    assert_eq!(
        app.conversation_batch_count(),
        1,
        "expected a system error message after /pane outside zellij"
    );

    // The error message must be about zellij, not a generic "unknown command"
    // or similar. This ensures the guard path is exercised, not some fallback.
    let msg = app
        .last_conversation_message()
        .expect("expected a message in the batch");
    assert!(
        msg.to_lowercase().contains("zellij"),
        "error message must mention zellij; got: {msg:?}"
    );
}

// ---------------------------------------------------------------------------
// Constellation command tests
// ---------------------------------------------------------------------------

#[test]
fn constellation_command_requires_agents() {
    use pattern_cli::commands::constellation::run_constellation;

    // Use NotAvailable so this test only exercises the empty-agents guard,
    // not any zellij-availability check.
    let result = run_constellation(vec![], &ZellijState::NotAvailable);
    assert!(result.is_err(), "expected error for empty agent list");
    let msg = format!("{:?}", result.unwrap_err());
    assert!(
        msg.contains("at least one"),
        "expected 'at least one agent' error, got: {msg}"
    );
}

#[test]
fn constellation_command_rejects_nested_session() {
    use pattern_cli::commands::constellation::run_constellation;

    let state = ZellijState::InSession {
        session_name: "outer".into(),
    };
    let result = run_constellation(vec!["alpha".into()], &state);
    assert!(result.is_err());
    let msg = format!("{:?}", result.unwrap_err());
    assert!(msg.contains("already inside"), "got: {msg}");
}

#[test]
fn constellation_command_rejects_missing_zellij() {
    use pattern_cli::commands::constellation::run_constellation;

    let result = run_constellation(vec!["alpha".into()], &ZellijState::NotAvailable);
    assert!(result.is_err());
    let msg = format!("{:?}", result.unwrap_err());
    assert!(msg.contains("not available"), "got: {msg}");
}
