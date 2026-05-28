// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Phase 6 T8: constellation panel data model + renderer.
//!
//! [`ConstellationView`] holds the cached registry snapshot the panel renders.
//! The TUI populates it on session init (via three RPCs in parallel —
//! `list_personas` + `list_groups` plus the `fronting_snapshot` from
//! `SessionInfo`), and re-fetches on every `WireTurnEvent::ConstellationChanged`
//! event.
//!
//! [`render_constellation_panel`] is the render entry point — pure function
//! over `(ConstellationView, fronting_state, route_lock)`, returns a
//! `ratatui::text::Text` ready to paint into the panel's content area.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use smol_str::SmolStr;

use pattern_server::protocol::{WireGroupSummary, WirePersonaSummary, WireRoutingRule};

// ── Data model ────────────────────────────────────────────────────────────────

/// Cached constellation registry state for the panel.
///
/// Populated on session init via the daemon's `ListPersonas` + `ListGroups`
/// RPCs and refreshed on every `ConstellationChanged` notification.
#[derive(Debug, Default, Clone)]
pub struct ConstellationView {
    pub personas: Vec<WirePersonaSummary>,
    pub groups: Vec<WireGroupSummary>,
    /// Tracks whether the initial fetch has happened. The panel renders an
    /// "loading…" placeholder until this is true.
    pub loaded: bool,
}

impl ConstellationView {
    /// Look up a persona by id or display name (case-insensitive on name).
    /// Used by `/promote`, `/relate`, `/agent` to resolve `@name` or `@id`
    /// inputs to a canonical persona id.
    ///
    /// Returns `Err(NotFound)` if neither matches; `Err(Ambiguous)` if
    /// multiple personas share the same display name.
    pub fn resolve_handle(&self, input: &str) -> Result<SmolStr, ResolveError> {
        // Trim whitespace before stripping the optional `@` prefix so callers
        // do not have to normalize the input string themselves.
        let trimmed = input.trim().trim_start_matches('@');
        if trimmed.is_empty() {
            return Err(ResolveError::Empty);
        }

        // 1. Exact id match wins (deterministic; ids are unique).
        if let Some(p) = self.personas.iter().find(|p| p.id == trimmed) {
            return Ok(SmolStr::from(p.id.as_str()));
        }

        // 2. Case-insensitive display name match.
        // Uses `unicase::eq` for Unicode-aware case folding so names with
        // non-ASCII characters (e.g. accented letters) are matched correctly.
        // This is consistent with `slug_from_name` in spawn/sibling.rs which
        // uses Rust's standard Unicode `to_lowercase`.
        let name_matches: Vec<&WirePersonaSummary> = self
            .personas
            .iter()
            .filter(|p| unicase::eq(p.name.as_str(), trimmed))
            .collect();
        match name_matches.as_slice() {
            [one] => Ok(SmolStr::from(one.id.as_str())),
            [] => Err(ResolveError::NotFound(trimmed.to_string())),
            many => Err(ResolveError::Ambiguous {
                input: trimmed.to_string(),
                candidates: many.iter().map(|p| p.id.clone()).collect(),
            }),
        }
    }
}

/// Errors from [`ConstellationView::resolve_handle`].
#[derive(Debug, Clone)]
pub enum ResolveError {
    /// The input was empty or contained only whitespace (after stripping `@`).
    Empty,
    NotFound(String),
    Ambiguous {
        input: String,
        candidates: Vec<String>,
    },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "persona handle must not be empty"),
            Self::NotFound(s) => write!(f, "no persona matching {s:?}"),
            Self::Ambiguous { input, candidates } => write!(
                f,
                "ambiguous persona {input:?}; matches: {}",
                candidates.join(", ")
            ),
        }
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Status glyph + color for a persona.
fn status_glyph(status: &str, is_fronting: bool) -> (&'static str, Color) {
    if is_fronting {
        return ("★", Color::Cyan);
    }
    match status {
        "active" => ("●", Color::Green),
        "draft" => ("○", Color::Yellow),
        "inactive" => ("◌", Color::DarkGray),
        _ => ("?", Color::DarkGray),
    }
}

/// Snake-case relationship kind to human-readable prose.
fn humanize_kind(kind: &str) -> String {
    kind.replace('_', " ")
}

/// Pattern-type to a compact prefix-string for routing rules.
fn rule_prefix(pattern_type: &str) -> &'static str {
    match pattern_type {
        "Prefix" => "prefix",
        "Contains" => "contains",
        "TopicTag" => "tag",
        "Regex" => "regex",
        _ => "rule",
    }
}

/// Render a routing rule as `prefix "!math" → Math Specialist`.
fn render_rule_line(rule: &WireRoutingRule, target_name: Option<&str>) -> Line<'static> {
    let rule_prefix = rule_prefix(&rule.pattern_type);
    let target = target_name.unwrap_or(rule.target.as_str()).to_string();
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            rule_prefix.to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(
            format!("\"{}\"", rule.pattern_value),
            Style::default().fg(Color::White),
        ),
        Span::styled(" → ", Style::default().fg(Color::DarkGray)),
        Span::styled(target, Style::default().add_modifier(Modifier::BOLD)),
    ])
}

/// Render the full constellation panel.
///
/// Sections:
/// 1. Fronting summary (active, fallback, rules)
/// 2. Personas (one line each: glyph + display name + dim id)
/// 3. Relationships (`Alice → Bob   supervisor of`)
/// 4. Groups
pub fn render_constellation_panel(
    view: &ConstellationView,
    fronting_active: &[SmolStr],
    fronting_fallback: Option<&SmolStr>,
    routing_rules: &[WireRoutingRule],
    route_lock: Option<&SmolStr>,
) -> Text<'static> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // ── Fronting section ─────────────────────────────────────────────────────
    let header = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    lines.push(Line::from(Span::styled("Fronting", header)));

    // Display-name resolver for fronting ids (falls back to id when not found).
    let display_for = |id: &str| -> String {
        view.personas
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string())
    };

    if let Some(locked) = route_lock {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("→ ", Style::default().fg(Color::Yellow)),
            Span::raw(display_for(locked.as_str())),
            Span::styled(
                "  (route lock)".to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    if fronting_active.is_empty() {
        if let Some(fb) = fronting_fallback {
            lines.push(Line::from(vec![
                Span::raw("  fallback: "),
                Span::styled(
                    display_for(fb.as_str()),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));
        } else if route_lock.is_none() {
            lines.push(Line::from(Span::styled(
                "  no fronting configured",
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else {
        let active_names: Vec<String> = fronting_active
            .iter()
            .map(|id| display_for(id.as_str()))
            .collect();
        lines.push(Line::from(vec![
            Span::raw("  active: "),
            Span::styled(
                active_names.join(", "),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        if let Some(fb) = fronting_fallback
            && !fronting_active.iter().any(|a| a == fb)
        {
            lines.push(Line::from(vec![
                Span::raw("  fallback: "),
                Span::raw(display_for(fb.as_str())),
            ]));
        }
    }
    if !routing_rules.is_empty() {
        lines.push(Line::from("  rules:"));
        for rule in routing_rules {
            let target_name = view
                .personas
                .iter()
                .find(|p| p.id == rule.target)
                .map(|p| p.name.as_str());
            lines.push(render_rule_line(rule, target_name));
        }
    }

    lines.push(Line::from(""));

    // ── Personas section ─────────────────────────────────────────────────────
    if !view.loaded {
        lines.push(Line::from(Span::styled("Personas (loading…)", header)));
    } else {
        lines.push(Line::from(Span::styled(
            format!("Personas ({})", view.personas.len()),
            header,
        )));
        for p in &view.personas {
            let is_fronting = fronting_active.iter().any(|a| a.as_str() == p.id);
            let (glyph, color) = status_glyph(&p.status, is_fronting);
            let name_style = if is_fronting {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if p.status == "draft" {
                Style::default().fg(Color::Yellow)
            } else if p.status == "inactive" {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(glyph.to_string(), Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(p.name.clone(), name_style),
                Span::styled(
                    format!("  ({})", p.id),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));

    // ── Relationships section ────────────────────────────────────────────────
    // Walk every persona's outgoing edges. Display name lookup falls back
    // to id when the other endpoint is missing from the cache (rare —
    // would mean the cache is stale). Stable order: by (from-name, to-name).
    let mut edges: Vec<(String, String, String)> = Vec::new();
    for p in &view.personas {
        for (other_id, kind) in &p.outgoing_relationships {
            let from_name = p.name.clone();
            let to_name = display_for(other_id.as_str());
            edges.push((from_name, to_name, humanize_kind(kind)));
        }
    }
    edges.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    if !edges.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("Relationships ({})", edges.len()),
            header,
        )));
        // Compute alignment width.
        let max_from_to = edges
            .iter()
            .map(|(f, t, _)| format!("{f} → {t}").len())
            .max()
            .unwrap_or(0);
        for (from, to, kind) in &edges {
            let pair = format!("{from} → {to}");
            let pad = " ".repeat(max_from_to.saturating_sub(pair.len()) + 2);
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::raw(pair),
                Span::raw(pad),
                Span::styled(kind.clone(), Style::default().fg(Color::DarkGray)),
            ]));
        }
        lines.push(Line::from(""));
    }

    // ── Groups section ───────────────────────────────────────────────────────
    if !view.groups.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("Groups ({})", view.groups.len()),
            header,
        )));
        for g in &view.groups {
            let members: Vec<String> = g
                .members
                .iter()
                .map(|id| display_for(id.as_str()))
                .collect();
            let scope = g.project_id.as_deref().unwrap_or("global");
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    g.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  [{scope}]  "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(members.join(", ")),
            ]));
        }
    }

    Text::from(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona(id: &str, name: &str, status: &str) -> WirePersonaSummary {
        WirePersonaSummary {
            id: id.to_string(),
            name: name.to_string(),
            status: status.to_string(),
            config_path: None,
            project_attachments: vec![],
            outgoing_relationships: vec![],
        }
    }

    #[test]
    fn resolve_handle_exact_id_wins() {
        let view = ConstellationView {
            personas: vec![
                persona("alice", "Alice", "active"),
                persona("bob", "alice", "active"), // adversarial: name matches another's id
            ],
            ..Default::default()
        };
        let resolved = view.resolve_handle("alice").unwrap();
        assert_eq!(resolved.as_str(), "alice", "exact id match must win");
    }

    #[test]
    fn resolve_handle_strips_at_prefix() {
        let view = ConstellationView {
            personas: vec![persona("alice", "Alice", "active")],
            ..Default::default()
        };
        assert_eq!(view.resolve_handle("@alice").unwrap().as_str(), "alice");
    }

    #[test]
    fn resolve_handle_falls_back_to_display_name() {
        let view = ConstellationView {
            personas: vec![persona("a-1", "Alice", "active")],
            ..Default::default()
        };
        assert_eq!(view.resolve_handle("Alice").unwrap().as_str(), "a-1");
        // Case-insensitive.
        assert_eq!(
            view.resolve_handle("alice").unwrap().as_str(),
            "a-1",
            "name match should be case-insensitive"
        );
    }

    #[test]
    fn resolve_handle_ambiguous_name_errors() {
        let view = ConstellationView {
            personas: vec![
                persona("a-1", "Alice", "active"),
                persona("a-2", "Alice", "draft"),
            ],
            ..Default::default()
        };
        let err = view.resolve_handle("Alice").unwrap_err();
        match err {
            ResolveError::Ambiguous { candidates, .. } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn resolve_handle_not_found() {
        let view = ConstellationView::default();
        let err = view.resolve_handle("nobody").unwrap_err();
        assert!(matches!(err, ResolveError::NotFound(_)));
    }

    /// `resolve_handle` must return `Err(ResolveError::Empty)` for inputs that
    /// are empty, whitespace-only, or reduce to empty after stripping the `@`
    /// prefix. This prevents callers from accidentally resolving the empty
    /// string to some persona via the `NotFound` path.
    #[test]
    fn resolve_handle_rejects_empty_or_whitespace() {
        let view = ConstellationView {
            personas: vec![persona("alice", "Alice", "active")],
            ..Default::default()
        };

        // Empty string.
        assert!(
            matches!(view.resolve_handle(""), Err(ResolveError::Empty)),
            "empty string must be Empty"
        );

        // Whitespace-only.
        assert!(
            matches!(view.resolve_handle("   "), Err(ResolveError::Empty)),
            "whitespace-only must be Empty"
        );

        // Just the @ prefix with nothing after (reduces to empty after strip).
        assert!(
            matches!(view.resolve_handle("@"), Err(ResolveError::Empty)),
            "bare @ must be Empty"
        );
    }
}
