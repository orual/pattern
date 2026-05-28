// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for the projects registry that maps project paths
//! to standalone mode project IDs.
//!
//! These tests exercise the registry data layer plus the registry-based
//! mount resolution path. Together they pin down the contract that:
//!
//! - `pattern mount init --mode standalone` records a `path → project_id`
//!   entry so subsequent commands launched from that project path can
//!   resolve the mount.
//! - Auto-derived project IDs are readable slugs of the directory name,
//!   not opaque hashes.
//! - One project ID can map to multiple paths (jj workspaces / persistent
//!   forks attach to the same project state).
//! - `attach` from a project path or any subdirectory resolves through
//!   the registry when no in-repo marker is present.

use std::fs;
use std::path::{Path, PathBuf};

use pattern_memory::PatternPaths;
use pattern_memory::jj::JjAdapter;
use pattern_memory::modes::{StorageMode, standalone};
use pattern_memory::mount;
use pattern_memory::projects::ProjectRegistry;
use tempfile::TempDir;

fn skip_if_no_jj() -> Option<JjAdapter> {
    JjAdapter::detect().ok().flatten()
}

fn make_dir(parent: &Path, name: &str) -> PathBuf {
    let p = parent.join(name);
    fs::create_dir_all(&p).unwrap();
    p.canonicalize().unwrap()
}

#[test]
fn registry_round_trips_through_disk() {
    let home = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(home.path());
    let project_dir = TempDir::new().unwrap();
    let project_path = project_dir.path().canonicalize().unwrap();

    let mut reg = ProjectRegistry::load(&paths).unwrap();
    let id = reg.register_project(&project_path, None).unwrap();
    reg.save(&paths).unwrap();

    let reloaded = ProjectRegistry::load(&paths).unwrap();
    assert_eq!(
        reloaded.project_id_for_path(&project_path),
        Some(id.as_str())
    );
}

#[test]
fn auto_derived_id_is_readable_slug_of_dirname() {
    let home = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(home.path());
    let project_dir = TempDir::new().unwrap();
    let project_path = make_dir(project_dir.path(), "My Cool Project!");

    let mut reg = ProjectRegistry::load(&paths).unwrap();
    let id = reg.register_project(&project_path, None).unwrap();

    assert_eq!(
        id, "my-cool-project",
        "auto-derived id should slugify the directory basename"
    );
}

#[test]
fn auto_derived_id_collisions_get_numeric_suffix() {
    let home = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(home.path());

    let project_a = TempDir::new().unwrap();
    let foo_a = make_dir(project_a.path(), "foo");
    let project_b = TempDir::new().unwrap();
    let foo_b = make_dir(project_b.path(), "foo");
    let project_c = TempDir::new().unwrap();
    let foo_c = make_dir(project_c.path(), "foo");

    let mut reg = ProjectRegistry::load(&paths).unwrap();
    let id_a = reg.register_project(&foo_a, None).unwrap();
    let id_b = reg.register_project(&foo_b, None).unwrap();
    let id_c = reg.register_project(&foo_c, None).unwrap();

    assert_eq!(id_a, "foo");
    assert_eq!(id_b, "foo-2");
    assert_eq!(id_c, "foo-3");
}

#[test]
fn explicit_id_is_used_when_provided() {
    let home = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(home.path());
    let project_dir = TempDir::new().unwrap();
    let project_path = project_dir.path().canonicalize().unwrap();

    let mut reg = ProjectRegistry::load(&paths).unwrap();
    let id = reg
        .register_project(&project_path, Some("custom-name"))
        .unwrap();
    assert_eq!(id, "custom-name");
}

#[test]
fn registry_supports_multiple_paths_per_project() {
    let home = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(home.path());
    let primary_dir = TempDir::new().unwrap();
    let primary = primary_dir.path().canonicalize().unwrap();
    let workspace_dir = TempDir::new().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();

    let mut reg = ProjectRegistry::load(&paths).unwrap();
    let id = reg
        .register_project(&primary, Some("multi-path"))
        .unwrap();
    reg.add_path(&id, &workspace).unwrap();
    reg.save(&paths).unwrap();

    let reloaded = ProjectRegistry::load(&paths).unwrap();
    assert_eq!(
        reloaded.project_id_for_path(&primary),
        Some("multi-path")
    );
    assert_eq!(
        reloaded.project_id_for_path(&workspace),
        Some("multi-path")
    );
    assert_eq!(reloaded.paths_for_project("multi-path").count(), 2);
}

#[test]
fn registry_lookup_walks_up_to_registered_ancestor() {
    let home = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(home.path());
    let project_dir = TempDir::new().unwrap();
    let project_path = project_dir.path().canonicalize().unwrap();

    let mut reg = ProjectRegistry::load(&paths).unwrap();
    let id = reg.register_project(&project_path, None).unwrap();
    reg.save(&paths).unwrap();

    let sub = project_path.join("src").join("lib");
    fs::create_dir_all(&sub).unwrap();

    let reloaded = ProjectRegistry::load(&paths).unwrap();
    assert_eq!(
        reloaded.project_id_for_path(&sub),
        Some(id.as_str()),
        "lookup from a subdirectory must resolve via walk-up"
    );
}

#[test]
fn registering_same_path_twice_returns_existing_id() {
    let home = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(home.path());
    let project_dir = TempDir::new().unwrap();
    let project_path = project_dir.path().canonicalize().unwrap();

    let mut reg = ProjectRegistry::load(&paths).unwrap();
    let first = reg.register_project(&project_path, Some("foo")).unwrap();
    let second = reg.register_project(&project_path, Some("foo")).unwrap();
    assert_eq!(first, second);
}

#[test]
fn registering_path_under_different_id_is_an_error() {
    let home = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(home.path());
    let project_dir = TempDir::new().unwrap();
    let project_path = project_dir.path().canonicalize().unwrap();

    let mut reg = ProjectRegistry::load(&paths).unwrap();
    reg.register_project(&project_path, Some("first")).unwrap();
    let result = reg.register_project(&project_path, Some("second"));
    assert!(
        result.is_err(),
        "re-registering the same path under a different id must fail"
    );
}

#[test]
fn standalone_init_then_attach_resolves_via_registry() {
    let Some(jj_adapter) = skip_if_no_jj() else {
        eprintln!("skipping: jj not available");
        return;
    };

    let home = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(home.path());
    let project_dir = TempDir::new().unwrap();
    let project_path = project_dir.path().canonicalize().unwrap();

    // Init standalone and register the path → id mapping.
    let id = {
        let mut reg = ProjectRegistry::load(&paths).unwrap();
        let id = reg.register_project(&project_path, None).unwrap();
        reg.save(&paths).unwrap();
        standalone::init(&id, &jj_adapter, &paths).unwrap();
        id
    };

    // Attach from the project path: must resolve to the standalone mount
    // via the registry, not via a `.pattern/shared/` walk-up.
    let store =
        mount::attach_with_paths(&project_path, &paths, None, None).expect("attach should succeed");

    match &store.mode {
        StorageMode::Standalone {
            project_id,
            mount_path,
        } => {
            assert_eq!(project_id, &id);
            assert_eq!(mount_path, &paths.standalone_mount_path(&id));
        }
        other => panic!("expected Standalone mode, got {other:?}"),
    }

    store.detach();
}
