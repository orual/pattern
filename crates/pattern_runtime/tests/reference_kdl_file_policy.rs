// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Runtime-side regression test for the documented `.pattern.kdl`
//! reference at `docs/reference/pattern-kdl-reference.kdl`.
//!
//! The companion test in `pattern_memory/tests/reference_kdl.rs`
//! verifies the file parses cleanly and section values round-trip.
//! This test goes a step further and builds an actual `FilePolicy`
//! from the parsed `file-policy {}` block, then checks that the glob
//! semantics documented in the reference (specifically `**` recursive
//! matching) actually do what the docs claim.
//!
//! If the reference says `/project/**` covers nested paths and the
//! runtime disagrees, this test fails — which is the right kind of
//! tight coupling between docs and behaviour for a config reference.
//!
//! Note: `FilePolicy::check_access` canonicalizes paths before
//! matching. For non-existent paths (which is what we're feeding it
//! here), `canonicalize_best_effort` falls through to the path as-given
//! per the helper's contract, so glob matching runs against the
//! literal pattern strings.

use std::path::Path;

use pattern_memory::config::load_mount_config;
use pattern_runtime::file_manager::FilePolicy;

fn reference_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root reachable from CARGO_MANIFEST_DIR")
        .join("docs/reference/pattern-kdl-reference.kdl")
}

#[test]
fn reference_file_policy_globs_match_as_documented() {
    let path = reference_path();
    let config = load_mount_config(&path)
        .unwrap_or_else(|e| panic!("reference {} failed to parse: {e:?}", path.display()));

    let policy = FilePolicy::from_section(config.file_policy)
        .expect("reference file-policy compiles to a FilePolicy");

    // /project/** allows direct children…
    policy
        .check_access(Path::new("/project/foo.rs"))
        .expect("/project/foo.rs should be allowed by /project/**");

    // …and matches across path separators (the load-bearing `**` claim).
    policy
        .check_access(Path::new("/project/src/lib.rs"))
        .expect("/project/src/lib.rs should be allowed by /project/** (recursive)");
    policy
        .check_access(Path::new("/project/a/b/c/d/deep.rs"))
        .expect("/project/a/b/c/d/deep.rs should be allowed by /project/** (deeply nested)");

    // The .env carve-out wins as a later rule (last-match wins).
    policy
        .check_access(Path::new("/project/.env"))
        .expect_err("/project/.env should be denied by the later deny rule");

    // /project/secrets/** denies the whole subtree.
    policy
        .check_access(Path::new("/project/secrets/api-key"))
        .expect_err("/project/secrets/api-key should be denied");
    policy
        .check_access(Path::new("/project/secrets/nested/deep/leak"))
        .expect_err("/project/secrets/nested/deep/leak should be denied (recursive)");

    // /tmp/pattern-* matches single segments only (not `**`).
    policy
        .check_access(Path::new("/tmp/pattern-scratch"))
        .expect("/tmp/pattern-scratch should be allowed by /tmp/pattern-*");

    // No matching rule → default deny.
    policy
        .check_access(Path::new("/etc/passwd"))
        .expect_err("/etc/passwd should be denied (no matching rule, default deny)");
}
