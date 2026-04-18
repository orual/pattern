//! Break-detection hashing — cheap per-turn snapshots of cache-bust-
//! sensitive components. Diffing two snapshots attributes an
//! unexpected cache invalidation to a specific subsystem (system
//! content, cache_control markers, tools, beta headers, model).
//!
//! # Why bother
//!
//! When `cache_read_input_tokens` drops unexpectedly between turns,
//! the cause is usually one of:
//!   - A segment's content actually changed (persona edit, prompt
//!     template tweak)
//!   - The cache_control markers moved (TTL or scope flip)
//!   - Tool definitions changed
//!   - Beta header set changed (breaking the cache key)
//!   - Model ID changed
//!
//! Without attribution, debugging a cache bust means inspecting every
//! dimension manually. The snapshot + diff surfaces which one changed
//! in a single `tracing::warn!` line.
//!
//! # Stability note
//!
//! Both `system_hash` and `cache_control_hash` use JSON serialization
//! rather than `Debug` formatting for stability across compiler versions.
//! `CacheControl` and `Tool` both implement `serde::Serialize`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::compose::PartialRequest;

/// Per-turn snapshot of cache-bust-sensitive components. Cheap to
/// compute (single hash per dimension) and cheap to store (a handful
/// of `u64`s plus the model string).
#[derive(Debug, Clone, Default)]
pub struct BreakDetectionSnapshot {
    /// Hash of system blocks with `cache_control` STRIPPED. Catches
    /// content changes without cache-marker churn muddying the
    /// diagnostic.
    pub system_hash: u64,

    /// Hash of system blocks WITH `cache_control` intact. Catches
    /// TTL / scope changes on markers specifically.
    pub cache_control_hash: u64,

    /// Hash of the tools schema serialisation.
    pub tools_hash: u64,

    /// Hash of the beta header set (sorted, joined).
    pub betas_hash: u64,

    /// Hash of message-level `cache_control` markers. Captures
    /// (message_index, role, cache_control) tuples for messages
    /// whose `options.cache_control` is set. Fed by
    /// [`Self::compute`] from the composer's pending
    /// [`BreakpointTracker`] placements (compose-time intent) and by
    /// [`Self::compute_from_chat`] from the post-finalize
    /// [`ChatRequest.messages`] (actualised state, including any
    /// post-compose splicing the orchestrator does for
    /// tool-continuation turns).
    pub message_markers_hash: u64,

    /// Model identifier at the time of snapshot.
    pub model: String,
}

impl BreakDetectionSnapshot {
    /// Compute a snapshot from a [`PartialRequest`]. Safe to call
    /// multiple times per turn — computations are hash-only with no
    /// allocations beyond a few short strings.
    pub fn compute(partial: &PartialRequest) -> Self {
        let mut sys_hasher = DefaultHasher::new();
        let mut cc_hasher = DefaultHasher::new();

        for block in &partial.system_blocks {
            // Content-only hash: ignore cache_control so marker changes
            // don't pollute the content-change signal.
            block.text.hash(&mut sys_hasher);

            // Full hash: include cache_control for the marker-shift signal.
            block.text.hash(&mut cc_hasher);
            // Use JSON serialization for stability across compiler versions;
            // CacheControl implements Serialize.
            let cc_repr =
                serde_json::to_string(&block.cache_control).unwrap_or_else(|_| "null".to_owned());
            cc_repr.hash(&mut cc_hasher);
        }

        let mut tools_hasher = DefaultHasher::new();
        for tool in &partial.tools {
            // JSON serialization is more stable than Debug formatting.
            let tool_repr = serde_json::to_string(tool).unwrap_or_else(|_| format!("{tool:?}"));
            tool_repr.hash(&mut tools_hasher);
        }

        let mut betas_hasher = DefaultHasher::new();
        // Extract the anthropic-beta header value (if any), sort comma-
        // separated values for stable hashing so ordering jitter in the
        // caller doesn't spuriously flag a cache-key change.
        if let Some(betas) = partial.extra_headers.get("anthropic-beta") {
            let mut parts: Vec<&str> = betas.split(',').map(|s| s.trim()).collect();
            parts.sort_unstable();
            for p in &parts {
                p.hash(&mut betas_hasher);
            }
        }

        // Hash message-level cache_control markers in placement
        // order. Sources from the BreakpointTracker (compose-time
        // intent) rather than walking `partial.messages` (their
        // `options.cache_control` is unset until finalize runs).
        let mut msg_markers_hasher = DefaultHasher::new();
        for placement in partial.breakpoints.placements() {
            if let crate::compose::breakpoints::BreakpointLocation::MessageBlock(idx) =
                placement.location
            {
                idx.hash(&mut msg_markers_hasher);
                // JSON-serialise the control for stability.
                let cc_repr =
                    serde_json::to_string(&placement.control).unwrap_or_else(|_| "null".to_owned());
                cc_repr.hash(&mut msg_markers_hasher);
            }
        }

        Self {
            system_hash: sys_hasher.finish(),
            cache_control_hash: cc_hasher.finish(),
            tools_hash: tools_hasher.finish(),
            betas_hash: betas_hasher.finish(),
            message_markers_hash: msg_markers_hasher.finish(),
            model: partial.model.clone(),
        }
    }

    /// Compute a snapshot from a post-finalize [`ChatRequest`] and
    /// model string. Used by the orchestrator AFTER any post-compose
    /// mutations (e.g. the segment-3 splice for tool-continuation
    /// turns) so the `message_markers_hash` reflects what actually
    /// ships on the wire.
    ///
    /// Does NOT hash message content (that changes every turn and
    /// would make the break-detection signal useless). Hashes only
    /// the set of `(index, role, cache_control)` tuples for messages
    /// whose `options.cache_control` is set.
    pub fn compute_from_chat(chat: &genai::chat::ChatRequest, model: &str) -> Self {
        let mut sys_hasher = DefaultHasher::new();
        let mut cc_hasher = DefaultHasher::new();

        if let Some(blocks) = &chat.system_blocks {
            for block in blocks {
                block.text.hash(&mut sys_hasher);
                block.text.hash(&mut cc_hasher);
                let cc_repr = serde_json::to_string(&block.cache_control)
                    .unwrap_or_else(|_| "null".to_owned());
                cc_repr.hash(&mut cc_hasher);
            }
        } else if let Some(system) = &chat.system {
            system.hash(&mut sys_hasher);
            system.hash(&mut cc_hasher);
        }

        let mut tools_hasher = DefaultHasher::new();
        if let Some(tools) = &chat.tools {
            for tool in tools {
                let tool_repr = serde_json::to_string(tool).unwrap_or_else(|_| format!("{tool:?}"));
                tool_repr.hash(&mut tools_hasher);
            }
        }

        let mut msg_markers_hasher = DefaultHasher::new();
        for (idx, msg) in chat.messages.iter().enumerate() {
            if let Some(opts) = &msg.options
                && let Some(ref cc) = opts.cache_control
            {
                idx.hash(&mut msg_markers_hasher);
                // Role included so identical cache_control on a
                // Tool-role vs User-role message isn't conflated.
                let role_repr = format!("{:?}", msg.role);
                role_repr.hash(&mut msg_markers_hasher);
                let cc_repr = serde_json::to_string(cc).unwrap_or_else(|_| "null".to_owned());
                cc_repr.hash(&mut msg_markers_hasher);
            }
        }

        // betas_hash is 0 here — ChatRequest doesn't carry extra
        // headers. The orchestrator can merge a PartialRequest-level
        // betas_hash into the snapshot if it needs to attribute
        // beta-set changes too. For now, leave as 0.
        Self {
            system_hash: sys_hasher.finish(),
            cache_control_hash: cc_hasher.finish(),
            tools_hash: tools_hasher.finish(),
            betas_hash: 0,
            message_markers_hash: msg_markers_hasher.finish(),
            model: model.to_owned(),
        }
    }

    /// Produce human-readable diff attributions between `self` and
    /// `previous`. Returns an empty `Vec` when the snapshots match.
    ///
    /// The cache-control dimension is further disambiguated: when
    /// `cache_control_hash` changed but `system_hash` did not, the diff
    /// notes a marker-placement shift rather than a content change, which
    /// narrows the investigation.
    pub fn diff(&self, previous: &BreakDetectionSnapshot) -> Vec<String> {
        let mut out = Vec::new();

        if self.system_hash != previous.system_hash {
            out.push("system content changed".into());
        }

        if self.cache_control_hash != previous.cache_control_hash {
            // When content didn't change but cache_control did, we know
            // it's a marker-placement shift. Distinguish that from raw
            // content changes by checking system_hash equality.
            if self.system_hash == previous.system_hash {
                out.push("cache_control markers moved (TTL or scope flipped)".into());
            } else {
                out.push("cache_control changed (alongside content)".into());
            }
        }

        if self.tools_hash != previous.tools_hash {
            out.push("tools schema changed".into());
        }

        if self.betas_hash != previous.betas_hash {
            out.push("anthropic-beta header set changed".into());
        }

        if self.message_markers_hash != previous.message_markers_hash {
            out.push(
                "message-level cache_control markers changed (segment-2/3 placement shift, \
                 tool-continuation splice, or post-compose mutation)"
                    .into(),
            );
        }

        if self.model != previous.model {
            out.push(format!(
                "model changed: {} \u{2192} {}",
                previous.model, self.model
            ));
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use genai::chat::{CacheControl, SystemBlock, Tool};

    use super::*;

    // Helper: build a PartialRequest with canned system blocks.
    fn partial_with_system(blocks: Vec<SystemBlock>) -> PartialRequest {
        let mut p = PartialRequest::new("claude-opus-4-7");
        p.system_blocks = blocks;
        p
    }

    #[test]
    fn identical_partials_produce_empty_diff() {
        let p = partial_with_system(vec![SystemBlock::new("hello")]);
        let a = BreakDetectionSnapshot::compute(&p);
        let b = BreakDetectionSnapshot::compute(&p);
        assert!(
            a.diff(&b).is_empty(),
            "identical snapshots should diff to empty"
        );
    }

    #[test]
    fn content_change_surfaces_system_content_changed() {
        let p1 = partial_with_system(vec![SystemBlock::new("hello")]);
        let p2 = partial_with_system(vec![SystemBlock::new("hi")]);
        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);
        let diff = s2.diff(&s1);
        assert!(
            diff.iter().any(|m| m.contains("system content changed")),
            "expected system content changed in diff, got: {diff:?}"
        );
    }

    #[test]
    fn cache_control_only_change_distinguishes_from_content() {
        let block_a = SystemBlock::new("hello").with_cache_control(CacheControl::Ephemeral5m);
        let block_b = SystemBlock::new("hello") // same content
            .with_cache_control(CacheControl::Ephemeral1h); // TTL flipped

        let p1 = partial_with_system(vec![block_a]);
        let p2 = partial_with_system(vec![block_b]);
        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);
        let diff = s2.diff(&s1);

        assert!(
            diff.iter()
                .any(|m| m.contains("cache_control markers moved")),
            "expected cache_control markers moved in diff, got: {diff:?}"
        );
        assert!(
            !diff.iter().any(|m| m.contains("system content changed")),
            "system_hash should not change when only cache_control differs, got: {diff:?}"
        );
    }

    #[test]
    fn model_change_surfaces_with_before_and_after() {
        let mut p1 = PartialRequest::new("claude-opus-4-7");
        let mut p2 = PartialRequest::new("claude-sonnet-4-7");
        p1.system_blocks = vec![SystemBlock::new("same")];
        p2.system_blocks = vec![SystemBlock::new("same")];

        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);
        let diff = s2.diff(&s1);

        assert!(
            diff.iter()
                .any(|m| m.contains("claude-opus-4-7") && m.contains("claude-sonnet-4-7")),
            "expected both model names in diff, got: {diff:?}"
        );
    }

    #[test]
    fn beta_header_order_doesnt_matter_for_hash() {
        let mut p1 = PartialRequest::new("m");
        let mut p2 = PartialRequest::new("m");
        p1.extra_headers
            .insert("anthropic-beta".into(), "a,b,c".into());
        p2.extra_headers
            .insert("anthropic-beta".into(), "c,a,b".into());

        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);

        assert_eq!(
            s1.betas_hash, s2.betas_hash,
            "beta ordering shouldn't bust cache-key detection"
        );
    }

    #[test]
    fn beta_header_change_surfaces() {
        let mut p1 = PartialRequest::new("m");
        let mut p2 = PartialRequest::new("m");
        p1.extra_headers.insert(
            "anthropic-beta".into(),
            "prompt-caching-scope-2026-01-05".into(),
        );
        p2.extra_headers.insert(
            "anthropic-beta".into(),
            "prompt-caching-scope-2026-01-05,interleaved-thinking-2025-05-14".into(),
        );

        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);
        let diff = s2.diff(&s1);

        assert!(
            diff.iter()
                .any(|m| m.contains("anthropic-beta header set changed")),
            "expected beta header change in diff, got: {diff:?}"
        );
    }

    #[test]
    fn tools_change_surfaces() {
        let mut p1 = PartialRequest::new("m");
        let mut p2 = PartialRequest::new("m");
        p1.tools = vec![Tool::new("tool_a").with_description("does a")];
        p2.tools = vec![Tool::new("tool_b").with_description("does b")];

        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);
        let diff = s2.diff(&s1);

        assert!(
            diff.iter().any(|m| m.contains("tools schema changed")),
            "expected tools schema changed in diff, got: {diff:?}"
        );
    }

    #[test]
    fn default_snapshot_exists_and_is_zero() {
        let s = BreakDetectionSnapshot::default();
        assert_eq!(s.model, "");
        assert_eq!(s.system_hash, 0);
        assert_eq!(s.cache_control_hash, 0);
        assert_eq!(s.tools_hash, 0);
        assert_eq!(s.betas_hash, 0);
    }

    #[test]
    fn no_beta_header_stable() {
        // Two partials with no beta header at all should hash identically.
        let p1 = PartialRequest::new("m");
        let p2 = PartialRequest::new("m");
        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);
        assert_eq!(s1.betas_hash, s2.betas_hash);
    }

    #[test]
    fn content_and_cache_control_both_change() {
        let block_a = SystemBlock::new("hello").with_cache_control(CacheControl::Ephemeral5m);
        let block_b =
            SystemBlock::new("different content").with_cache_control(CacheControl::Ephemeral1h);

        let p1 = partial_with_system(vec![block_a]);
        let p2 = partial_with_system(vec![block_b]);
        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);
        let diff = s2.diff(&s1);

        // When both change, both attributions appear.
        assert!(
            diff.iter().any(|m| m.contains("system content changed")),
            "expected system content changed, got: {diff:?}"
        );
        assert!(
            diff.iter()
                .any(|m| m.contains("cache_control changed (alongside content)")),
            "expected cache_control changed alongside content, got: {diff:?}"
        );
    }

    // ---- message_markers_hash coverage -------------------------------------

    /// `compute` hashes message-level markers from the tracker's
    /// placements. Two partials with the same markers should match;
    /// adding a marker should change the hash.
    #[test]
    fn message_markers_hash_tracks_tracker_placements() {
        use crate::compose::breakpoints::{BreakpointLocation, BreakpointTracker};
        use genai::chat::ChatMessage;

        let mut p1 = PartialRequest::new("claude-opus-4-7");
        p1.messages.push(ChatMessage::user("msg0"));
        p1.messages.push(ChatMessage::user("msg1"));

        // Baseline: no markers placed.
        let s_baseline = BreakDetectionSnapshot::compute(&p1);

        // Now place a marker on message index 1 via the tracker.
        let mut p2 = p1.clone();
        let _ = p2.breakpoints.place(
            BreakpointLocation::MessageBlock(1),
            CacheControl::Ephemeral1h,
            "test_pass",
        );
        let s_with_marker = BreakDetectionSnapshot::compute(&p2);

        assert_ne!(
            s_baseline.message_markers_hash, s_with_marker.message_markers_hash,
            "placing a message-level marker must change the hash"
        );
        let _ = BreakpointTracker::ANTHROPIC_MAX_BREAKPOINTS;
    }

    /// `compute_from_chat` sees message-level cache_control set
    /// directly on `msg.options` (the shape orchestrator splicing
    /// produces for tool-continuation turns). Flipping the control
    /// on a message must show up in the diff as "message-level
    /// cache_control markers changed".
    #[test]
    fn compute_from_chat_detects_post_compose_marker_splice() {
        use genai::chat::{ChatMessage, ChatRequest, MessageOptions};

        let mut m_a = ChatMessage::user("tool_result_stub");
        let mut m_b = m_a.clone();
        // Baseline: no message-level marker.
        let req_a = ChatRequest::default().append_message(m_a.clone());
        let s_a = BreakDetectionSnapshot::compute_from_chat(&req_a, "claude-opus-4-7");

        // Splice: add cache_control to the message.
        m_a.options = Some(MessageOptions::default().with_cache_control(CacheControl::Ephemeral1h));
        let req_b = ChatRequest::default().append_message(m_a);
        let s_b = BreakDetectionSnapshot::compute_from_chat(&req_b, "claude-opus-4-7");

        let diff = s_b.diff(&s_a);
        assert!(
            diff.iter()
                .any(|m| m.contains("message-level cache_control markers changed")),
            "expected message-marker diff, got: {diff:?}"
        );

        // Flip the control on the second request without changing
        // the message otherwise — the diff should still report.
        m_b.options = Some(MessageOptions::default().with_cache_control(CacheControl::Ephemeral5m));
        let req_c = ChatRequest::default().append_message(m_b);
        let s_c = BreakDetectionSnapshot::compute_from_chat(&req_c, "claude-opus-4-7");
        assert_ne!(
            s_b.message_markers_hash, s_c.message_markers_hash,
            "different cache_control values on the same message index must hash differently"
        );
    }
}
