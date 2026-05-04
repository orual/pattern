//! Error types for the plugin subsystem.

use std::path::PathBuf;

/// Errors from manifest parsing (KDL or CC JSON).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    #[error("missing required field {field:?} in manifest at {path}")]
    MissingField {
        field: &'static str,
        path: PathBuf,
    },

    #[error("failed to read manifest at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse KDL manifest at {path}: {message}")]
    Kdl { path: PathBuf, message: String },

    #[error("failed to parse CC JSON manifest at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Errors from registry operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    #[error("plugin {id:?} already registered in scope {scope:?}")]
    Collision {
        id: smol_str::SmolStr,
        scope: super::PluginScope,
    },

    #[error("registry IO at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse registry KDL at {path}: {message}")]
    Kdl { path: PathBuf, message: String },

    #[error("plugin {id:?} not found in any scope")]
    NotFound { id: smol_str::SmolStr },

    #[error("cache directory unavailable")]
    NoCacheDir,

    #[error("destination already exists: {0}")]
    DestinationExists(PathBuf),
}

/// Umbrella error for higher layers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),

    #[error(transparent)]
    Registry(#[from] RegistryError),
}
