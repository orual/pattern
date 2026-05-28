// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for plugin manifest parsing (KDL + CC JSON).

use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("plugins")
        .join(name)
}

mod kdl {
    use super::*;
    use pattern_runtime::plugin::manifest;

    #[test]
    fn minimal_kdl_manifest_parses() {
        let m = manifest::from_kdl_file(&fixture("pattern_minimal.kdl"))
            .expect("minimal KDL manifest should parse");
        assert_eq!(m.name.as_str(), "test-plugin");
        assert_eq!(m.version.as_deref(), Some("0.1.0"));
        assert_eq!(m.description.as_deref(), Some("A minimal test plugin"));
        assert_eq!(m.skills.len(), 1);
    }

    #[test]
    fn full_kdl_manifest_parses_all_fields() {
        let m = manifest::from_kdl_file(&fixture("pattern_full.kdl"))
            .expect("full KDL manifest should parse");
        assert_eq!(m.name.as_str(), "full-test-plugin");
        assert_eq!(m.version.as_deref(), Some("1.0.0"));
        assert_eq!(m.homepage.as_deref(), Some("https://example.com"));
        assert_eq!(m.license.as_deref(), Some("MIT"));

        // Author
        let author = m.author.as_ref().expect("should have author");
        assert_eq!(author.name, "Test Author");
        assert_eq!(author.email.as_deref(), Some("test@example.com"));

        // Keywords
        assert_eq!(m.keywords, vec!["test", "plugin", "example"]);

        // Components
        assert!(!m.skills.is_empty(), "should have skills");
        assert!(!m.commands.is_empty(), "should have commands");

        // Transport
        assert!(
            matches!(
                m.transport,
                Some(pattern_core::plugin::manifest::TransportPreference::Stdio)
            ),
            "transport should be Stdio"
        );

        // Capabilities
        let caps = m
            .declared_effects
            .as_ref()
            .expect("should have capabilities");
        assert!(!caps.effects.is_empty(), "should have declared effects");
    }

    #[test]
    fn kdl_missing_name_is_error() {
        // Write a temp fixture with no name
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_name.kdl");
        std::fs::write(&path, "version \"1.0.0\"\n").unwrap();
        let result = manifest::from_kdl_file(&path);
        assert!(result.is_err(), "missing name should error");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("name"),
            "error should mention 'name': {err}"
        );
    }
}

mod cc_json {
    use super::*;
    use pattern_runtime::plugin::manifest;

    #[test]
    fn minimal_cc_json_parses() {
        let m = manifest::from_cc_json_file(&fixture("cc_minimal.json"))
            .expect("minimal CC JSON should parse");
        assert_eq!(m.name.as_str(), "cc-test-plugin");
        assert_eq!(m.description.as_deref(), Some("A minimal CC plugin"));
    }

    #[test]
    fn full_cc_json_parses_components() {
        let m = manifest::from_cc_json_file(&fixture("cc_full.json"))
            .expect("full CC JSON should parse");
        assert_eq!(m.name.as_str(), "cc-full-plugin");
        assert_eq!(m.version.as_deref(), Some("2.0.0"));
        assert_eq!(m.skills.len(), 2, "should have 2 skills");
        assert_eq!(m.commands.len(), 1, "should have 1 command");
        assert!(!m.hooks.is_empty(), "should have hooks");
        assert!(!m.mcp_servers.is_empty(), "should have mcp servers");
    }

    #[test]
    fn cc_json_preserves_unknown_fields() {
        let m = manifest::from_cc_json_file(&fixture("cc_unknown_fields.json"))
            .expect("CC JSON with unknowns should parse");
        assert_eq!(m.name.as_str(), "cc-unknown-test");

        let cc = m.cc.as_ref().expect("should have cc block");
        assert_eq!(cc.source_format.as_str(), "plugin.json");

        // Unknown fields preserved
        assert_eq!(
            cc.fields.get("fooBar").and_then(|v| v.as_str()),
            Some("preserved-string"),
            "fooBar should be preserved"
        );
        let baz = cc.fields.get("baz").expect("baz should be preserved");
        assert!(baz.is_object(), "baz should be an object");
        assert_eq!(baz.get("y").and_then(|v| v.as_i64()), Some(1));

        // Skills (known field) should NOT be in cc.fields
        assert!(
            !cc.fields.contains_key("skills"),
            "known field 'skills' should not be in cc.fields"
        );
    }

    #[test]
    fn cc_json_preserves_user_config() {
        let m = manifest::from_cc_json_file(&fixture("cc_full.json"))
            .expect("full CC JSON should parse");
        let cc = m.cc.as_ref().expect("should have cc block");
        assert!(
            cc.fields.contains_key("userConfig"),
            "userConfig should be preserved in cc.fields"
        );
    }

    #[test]
    fn cc_json_single_skill_string_coerces_to_path() {
        let m = manifest::from_cc_json_file(&fixture("cc_unknown_fields.json"))
            .expect("CC JSON should parse");
        assert_eq!(
            m.skills.len(),
            1,
            "single string skill should become one component"
        );
    }
}
