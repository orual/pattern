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
            let cc_repr = serde_json::to_string(&block.cache_control)
                .unwrap_or_else(|_| "null".to_owned());
            cc_repr.hash(&mut cc_hasher);
        }

        let mut tools_hasher = DefaultHasher::new();
        for tool in &partial.tools {
            // JSON serialization is more stable than Debug formatting.
            let tool_repr =
                serde_json::to_string(tool).unwrap_or_else(|_| format!("{tool:?}"));
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

        Self {
            system_hash: sys_hasher.finish(),
            cache_control_hash: cc_hasher.finish(),
            tools_hash: tools_hasher.finish(),
            betas_hash: betas_hasher.finish(),
            model: partial.model.clone(),
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
        assert!(a.diff(&b).is_empty(), "identical snapshots should diff to empty");
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
        let block_a = SystemBlock::new("hello")
            .with_cache_control(CacheControl::Ephemeral5m);
        let block_b = SystemBlock::new("hello") // same content
            .with_cache_control(CacheControl::Ephemeral1h); // TTL flipped

        let p1 = partial_with_system(vec![block_a]);
        let p2 = partial_with_system(vec![block_b]);
        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);
        let diff = s2.diff(&s1);

        assert!(
            diff.iter().any(|m| m.contains("cache_control markers moved")),
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
        p1.extra_headers
            .insert("anthropic-beta".into(), "prompt-caching-scope-2026-01-05".into());
        p2.extra_headers
            .insert("anthropic-beta".into(), "prompt-caching-scope-2026-01-05,interleaved-thinking-2025-05-14".into());

        let s1 = BreakDetectionSnapshot::compute(&p1);
        let s2 = BreakDetectionSnapshot::compute(&p2);
        let diff = s2.diff(&s1);

        assert!(
            diff.iter().any(|m| m.contains("anthropic-beta header set changed")),
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
        let block_a = SystemBlock::new("hello")
            .with_cache_control(CacheControl::Ephemeral5m);
        let block_b = SystemBlock::new("different content")
            .with_cache_control(CacheControl::Ephemeral1h);

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
}
