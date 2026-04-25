//! Bookmark-name construction for persistent forks.
//!
//! Persistent forks (Phase 3 Tasks 4-6) live in dedicated jj workspaces and
//! are tracked by a namespaced bookmark of the form `<agent>/<task>`. This
//! module owns the sanitization rules so the same canonicalization is
//! applied wherever the name is constructed (handler dispatch, conflict
//! pre-checks, cleanup paths).
//!
//! The output is constrained to ASCII alphanumerics, `-`, and a single `/`
//! separator. Anything else (spaces, capitals, punctuation, leading/trailing
//! dashes) is normalised to lowercase dashes and trimmed. An empty task slug
//! falls back to `anon-<short>`, where `<short>` is the first 8 characters
//! of a fresh `new_id()` (32-char unhyphenated UUID).
//!
//! # Examples
//!
//! ```
//! use pattern_memory::jj::fork_bookmark::fork_bookmark_name;
//! use pattern_core::BlockRef;
//!
//! let task = BlockRef::new("Refactor Foo!", "blk-1");
//! let name = fork_bookmark_name("agent-orual", Some(&task));
//! assert_eq!(name, "agent-orual/refactor-foo");
//! ```
use pattern_core::BlockRef;
use pattern_core::types::ids::new_id;

/// Build the bookmark name `<agent>/<task-slug>` for a persistent fork.
///
/// `agent` is sanitised via [`sanitize_slug`]; the task slug is taken from
/// `task.label` when present, else falls back to `anon-<short-uuid>`.
pub fn fork_bookmark_name(agent: &str, task: Option<&BlockRef>) -> String {
    let task_slug = task
        .map(|t| sanitize_slug(&t.label))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(anon_slug);
    let agent_slug = {
        let s = sanitize_slug(agent);
        if s.is_empty() { anon_slug() } else { s }
    };
    format!("{}/{}", agent_slug, task_slug)
}

/// Lowercase ASCII-alphanumeric + `-` slug. Non-matching characters collapse
/// to `-`; runs of dashes are NOT collapsed (jj accepts them) but leading and
/// trailing dashes are trimmed.
pub fn sanitize_slug(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    mapped.trim_matches('-').to_string()
}

/// Fallback slug for tasks that produce an empty sanitised string.
fn anon_slug() -> String {
    let id = new_id();
    let short: String = id.chars().take(8).collect();
    format!("anon-{}", short)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitises_spaces_and_capitals() {
        assert_eq!(sanitize_slug("Refactor Foo"), "refactor-foo");
    }

    #[test]
    fn sanitises_punctuation_to_dashes() {
        assert_eq!(sanitize_slug("foo/bar!baz"), "foo-bar-baz");
    }

    #[test]
    fn trims_leading_and_trailing_dashes() {
        assert_eq!(sanitize_slug("---hello---"), "hello");
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(sanitize_slug(""), "");
        assert_eq!(sanitize_slug("---"), "");
        assert_eq!(sanitize_slug("!!!"), "");
    }

    #[test]
    fn anon_slug_has_expected_shape() {
        let s = anon_slug();
        assert!(s.starts_with("anon-"), "got: {s}");
        // "anon-" prefix (5) + 8 hex chars = 13.
        assert_eq!(s.len(), 13, "got: {s}");
    }

    #[test]
    fn fork_bookmark_name_with_task_uses_label() {
        let task = BlockRef::new("Refactor Foo!", "blk-1");
        let name = fork_bookmark_name("agent-orual", Some(&task));
        assert_eq!(name, "agent-orual/refactor-foo");
    }

    #[test]
    fn fork_bookmark_name_no_task_falls_back_to_anon() {
        let name = fork_bookmark_name("agent-orual", None);
        assert!(name.starts_with("agent-orual/anon-"), "got: {name}");
    }

    #[test]
    fn fork_bookmark_name_empty_task_label_falls_back_to_anon() {
        let task = BlockRef::new("", "blk-1");
        let name = fork_bookmark_name("agent-orual", Some(&task));
        assert!(name.starts_with("agent-orual/anon-"), "got: {name}");
    }

    #[test]
    fn fork_bookmark_name_empty_agent_falls_back_to_anon() {
        let task = BlockRef::new("hello", "blk-1");
        let name = fork_bookmark_name("", Some(&task));
        assert!(name.starts_with("anon-"), "got: {name}");
        assert!(name.contains("/hello"), "got: {name}");
    }

    #[test]
    fn fork_bookmark_name_lowercases_agent() {
        let task = BlockRef::new("hello", "blk-1");
        let name = fork_bookmark_name("Agent_NAME", Some(&task));
        assert_eq!(name, "agent-name/hello");
    }
}
