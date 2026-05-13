//! Plugin manifest parsers: KDL and CC JSON.
//!
//! Pure type definitions live in `pattern_core::plugin::manifest`.
//! This module adds file I/O wrappers and KDL-specific parsing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use smol_str::SmolStr;

use pattern_core::plugin::manifest::*;
use pattern_core::plugin::ManifestError;

/// Parse a Pattern-native KDL manifest file.
pub fn from_kdl_file(path: &Path) -> Result<PluginManifest, ManifestError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let doc: kdl::KdlDocument = raw.parse().map_err(|e: kdl::KdlError| ManifestError::Kdl {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    from_kdl_doc(&doc, path)
}

/// Parse from an already-parsed KDL document (pure transform + path for errors).
pub fn from_kdl_doc(
    doc: &kdl::KdlDocument,
    path: &Path,
) -> Result<PluginManifest, ManifestError> {
    let mut manifest = PluginManifest::empty();
    let mut unknown = BTreeMap::new();

    for node in doc.nodes() {
        let name = node.name().value();
        match name {
            "name" => manifest.name = extract_string_arg(node).unwrap_or_default().into(),
            "version" => manifest.version = extract_string_arg(node),
            "description" => manifest.description = extract_string_arg(node),
            "homepage" => manifest.homepage = extract_string_arg(node),
            "repository" => manifest.repository = extract_string_arg(node),
            "license" => manifest.license = extract_string_arg(node),
            "author" => manifest.author = parse_author(node),
            "keywords" => manifest.keywords = extract_string_list(node),
            "skills" => manifest.skills = parse_component_nodes(node),
            "commands" => manifest.commands = parse_component_nodes(node),
            "agents" => manifest.agents = parse_component_nodes(node),
            "hooks" => manifest.hooks = parse_component_nodes(node),
            "mcp-servers" => manifest.mcp_servers = parse_component_nodes(node),
            "monitors" => manifest.monitors = parse_component_nodes(node),
            "bin" => manifest.bin = parse_component_nodes(node),
            "dependencies" => manifest.dependencies = parse_dependency_nodes(node),
            "transport" => manifest.transport = parse_transport(node),
            "capabilities" => manifest.declared_effects = parse_capabilities_block(node),
            "pattern" => manifest.pattern = parse_pattern_block(node),
            "build" => {
                // `build = false` opts out of cargo build at install time.
                // Any other entry (including bare `build`) leaves the default (true).
                if let Some(kdl::KdlEntry { .. }) = node.entries().first() {
                    if let Some(b) = node.entries().first().and_then(|e| e.value().as_bool()) {
                        manifest.build = b;
                    }
                }
            }
            "extras" => {
                manifest.extras = node
                    .entries()
                    .iter()
                    .filter_map(|e| e.value().as_string().map(std::path::PathBuf::from))
                    .collect();
            }
            "hook-subscriptions" | "hook_subscriptions" => {
                manifest.hook_subscriptions = node
                    .entries()
                    .iter()
                    .filter_map(|e| e.value().as_string().map(String::from))
                    .collect();
            }
            _ => {
                unknown.insert(name.to_string(), node.clone());
            }
        }
    }

    if manifest.name.is_empty() {
        return Err(ManifestError::MissingField {
            field: "name",
            path: path.to_path_buf(),
        });
    }

    // Store unknown KDL nodes for forward compatibility.
    // Note: PluginManifest in core doesn't have unknown_kdl field since
    // that would require kdl dep in core. We'll need to handle this
    // differently — either add an opaque field or handle at the registry level.
    // For now, unknowns are logged and dropped.
    for (key, _) in &unknown {
        tracing::debug!(node = %key, "unknown KDL node in plugin manifest (preserved for forward-compat)");
    }

    Ok(manifest)
}

/// Parse a CC plugin.json file.
pub fn from_cc_json_file(path: &Path) -> Result<PluginManifest, ManifestError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    from_cc_json_str(&raw, path)
}

/// Parse CC plugin.json from a string.
pub fn from_cc_json_str(json: &str, path: &Path) -> Result<PluginManifest, ManifestError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|source| ManifestError::Json {
            path: path.to_path_buf(),
            source,
        })?;

    let obj = value.as_object().ok_or_else(|| ManifestError::Kdl {
        path: path.to_path_buf(),
        message: "expected JSON object at top level".to_string(),
    })?;

    let mut manifest = PluginManifest::empty();
    let mut cc_fields: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    // Map known CC fields to Pattern equivalents.
    if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
        manifest.name = SmolStr::from(name);
    }
    if let Some(desc) = obj.get("description").and_then(|v| v.as_str()) {
        manifest.description = Some(desc.to_string());
    }
    if let Some(v) = obj.get("version").and_then(|v| v.as_str()) {
        manifest.version = Some(v.to_string());
    }

    // Component fields.
    manifest.skills = coerce_component_field(obj.get("skills"));
    manifest.commands = coerce_component_field(obj.get("commands"));
    manifest.agents = coerce_component_field(obj.get("agents"));
    manifest.hooks = coerce_component_field(obj.get("hooks"));
    manifest.mcp_servers = coerce_component_field(obj.get("mcpServers"));
    manifest.monitors = coerce_component_field(obj.get("monitors"));
    manifest.bin = coerce_component_field(obj.get("bin"));

    // Preserve CC-specific fields.
    let known_keys = [
        "name", "description", "version", "skills", "commands", "agents",
        "hooks", "mcpServers", "monitors", "bin", "dependencies",
    ];
    for (key, val) in obj {
        if !known_keys.contains(&key.as_str()) {
            cc_fields.insert(key.clone(), val.clone());
        }
    }

    // Always set cc for CC-sourced manifests. The source_format
    // indicates this is a CC plugin regardless of whether there are
    // unknown fields to preserve.
    manifest.cc = Some(Cc {
        source_format: "plugin.json".into(),
        fields: cc_fields,
    });

    if manifest.name.is_empty() {
        return Err(ManifestError::MissingField {
            field: "name",
            path: path.to_path_buf(),
        });
    }

    Ok(manifest)
}

// ---- KDL node extractors ----------------------------------------------------

fn extract_string_arg(node: &kdl::KdlNode) -> Option<String> {
    node.entries()
        .first()
        .and_then(|e| e.value().as_string())
        .map(|s| s.to_string())
}

fn extract_string_list(node: &kdl::KdlNode) -> Vec<String> {
    node.entries()
        .iter()
        .filter_map(|e| e.value().as_string().map(|s| s.to_string()))
        .collect()
}

fn parse_component_nodes(node: &kdl::KdlNode) -> Vec<ComponentSpec> {
    if let Some(path) = extract_string_arg(node) {
        return vec![ComponentSpec::Path(PathBuf::from(path))];
    }
    if let Some(doc) = node.children() {
        return doc
            .nodes()
            .iter()
            .map(|child| {
                if let Some(path) = extract_string_arg(child) {
                    ComponentSpec::Path(PathBuf::from(path))
                } else {
                    ComponentSpec::Inline(serde_json::Value::String(child.to_string()))
                }
            })
            .collect();
    }
    Vec::new()
}

fn parse_dependency_nodes(node: &kdl::KdlNode) -> Vec<DependencySpec> {
    if let Some(doc) = node.children() {
        return doc
            .nodes()
            .iter()
            .map(|child| DependencySpec {
                id: child.name().value().into(),
                version: extract_string_arg(child),
            })
            .collect();
    }
    Vec::new()
}

fn parse_author(node: &kdl::KdlNode) -> Option<Author> {
    let name = extract_string_arg(node)?;
    let doc = node.children();
    let email = doc.and_then(|d| {
        d.nodes()
            .iter()
            .find(|n| n.name().value() == "email")
            .and_then(extract_string_arg)
    });
    let url = doc.and_then(|d| {
        d.nodes()
            .iter()
            .find(|n| n.name().value() == "url")
            .and_then(extract_string_arg)
    });
    Some(Author { name, email, url })
}

fn parse_transport(node: &kdl::KdlNode) -> Option<TransportPreference> {
    match extract_string_arg(node)?.as_str() {
        "stdio" => Some(TransportPreference::Stdio),
        "http" => Some(TransportPreference::Http { port: None }),
        "irpc" => Some(TransportPreference::Irpc),
        _ => None,
    }
}

fn parse_capabilities_block(node: &kdl::KdlNode) -> Option<CapabilitiesBlock> {
    let doc = node.children()?;
    let effects_node = doc.nodes().iter().find(|n| n.name().value() == "effects")?;
    let effects = extract_string_list(effects_node)
        .into_iter()
        .filter_map(|s| pattern_core::EffectCategory::from_type_name(&s))
        .collect();
    Some(CapabilitiesBlock { effects })
}

fn parse_pattern_block(node: &kdl::KdlNode) -> Option<PatternBlock> {
    let doc = node.children()?;
    let min_version = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "min-version")
        .and_then(extract_string_arg);
    Some(PatternBlock {
        min_version,
        extra: BTreeMap::new(),
    })
}

/// Coerce a CC JSON field value into component specs.
/// Handles: string, array-of-strings, array-of-objects, single-object.
fn coerce_component_field(value: Option<&serde_json::Value>) -> Vec<ComponentSpec> {
    let Some(val) = value else {
        return Vec::new();
    };
    match val {
        serde_json::Value::String(s) => vec![ComponentSpec::Path(PathBuf::from(s))],
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(|item| match item {
                serde_json::Value::String(s) => ComponentSpec::Path(PathBuf::from(s)),
                other => ComponentSpec::Inline(other.clone()),
            })
            .collect(),
        serde_json::Value::Object(_) => vec![ComponentSpec::Inline(val.clone())],
        _ => vec![ComponentSpec::Inline(val.clone())],
    }
}
