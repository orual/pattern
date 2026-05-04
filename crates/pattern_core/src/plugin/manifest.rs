//! Plugin manifest types.
//!
//! Pure domain types. Parsing logic (KDL, CC JSON) lives in
//! `pattern_runtime::plugin::manifest`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// Pattern-native plugin manifest.
///
/// Produced by either the Pattern KDL parser or the CC JSON translator.
/// Both preserve unknown fields for forward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PluginManifest {
    pub name: SmolStr,
    pub version: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub author: Option<Author>,
    pub keywords: Vec<String>,

    // Component declarations.
    pub skills: Vec<ComponentSpec>,
    pub commands: Vec<ComponentSpec>,
    pub agents: Vec<ComponentSpec>,
    pub hooks: Vec<ComponentSpec>,
    pub mcp_servers: Vec<ComponentSpec>,
    pub monitors: Vec<ComponentSpec>,
    pub bin: Vec<ComponentSpec>,
    pub dependencies: Vec<DependencySpec>,

    // Pattern-native fields (CC plugins do not declare these).
    pub transport: Option<TransportPreference>,
    pub declared_effects: Option<CapabilitiesBlock>,
    pub pattern: Option<PatternBlock>,

    // CC-specific fields preserved from plugin.json translation.
    pub cc: Option<Cc>,
}

impl PluginManifest {
    /// Construct an empty manifest (all fields default/empty).
    pub fn empty() -> Self {
        Self {
            name: SmolStr::default(),
            version: None,
            description: None,
            homepage: None,
            repository: None,
            license: None,
            author: None,
            keywords: Vec::new(),
            skills: Vec::new(),
            commands: Vec::new(),
            agents: Vec::new(),
            hooks: Vec::new(),
            mcp_servers: Vec::new(),
            monitors: Vec::new(),
            bin: Vec::new(),
            dependencies: Vec::new(),
            transport: None,
            declared_effects: None,
            pattern: None,
            cc: None,
        }
    }
}

impl Default for PluginManifest {
    fn default() -> Self {
        Self::empty()
    }
}

// ---- Supporting types -------------------------------------------------------

/// Plugin author information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Author {
    pub name: String,
    pub email: Option<String>,
    pub url: Option<String>,
}

/// A component declared by a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ComponentSpec {
    /// Single path reference.
    Path(PathBuf),
    /// Multiple path references.
    Paths(Vec<PathBuf>),
    /// Inline configuration (JSON).
    Inline(serde_json::Value),
}

/// A plugin dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencySpec {
    pub id: SmolStr,
    pub version: Option<String>,
}

/// Transport preference for plugin communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TransportPreference {
    Stdio,
    Http { port: Option<u16> },
    Irpc,
}

/// Pattern-specific metadata block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternBlock {
    /// Minimum Pattern version required.
    pub min_version: Option<String>,
    /// Extra Pattern-specific configuration.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Capabilities declared by the plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitiesBlock {
    pub effects: Vec<crate::EffectCategory>,
}

/// Residue from CC `plugin.json` parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Cc {
    /// Original source format identifier.
    pub source_format: SmolStr,
    /// CC-specific fields not mapped to Pattern equivalents.
    pub fields: BTreeMap<String, serde_json::Value>,
}
