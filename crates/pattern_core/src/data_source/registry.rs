//! Plugin registry for custom data sources.
//!
//! This module provides the infrastructure for registering custom data sources
//! that can be instantiated from configuration. Uses the `inventory` crate for
//! distributed static registration.
//!
//! # Example
//!
//! To register a custom block source:
//!
//! ```ignore
//! use pattern_core::data_source::{DataBlock, CustomBlockSourceFactory};
//! use std::sync::Arc;
//!
//! struct MyCustomSource { /* ... */ }
//! impl DataBlock for MyCustomSource { /* ... */ }
//!
//! inventory::submit! {
//!     CustomBlockSourceFactory {
//!         source_type: "my_custom",
//!         create: |config| {
//!             let cfg: MyConfig = serde_json::from_value(config.clone())?;
//!             Ok(Arc::new(MyCustomSource::from_config(cfg)))
//!         },
//!     }
//! }
//! ```

use std::sync::Arc;

use crate::error::Result;

use super::{DataBlock, DataStream};

/// Factory for creating custom block sources from configuration.
///
/// Register these using `inventory::submit!` to make them available
/// for instantiation from config files.
pub struct CustomBlockSourceFactory {
    /// Type identifier used in config (e.g., "s3", "git", "database")
    pub source_type: &'static str,

    /// Factory function that creates a source from JSON config
    pub create: fn(&serde_json::Value) -> Result<Arc<dyn DataBlock>>,
}

// Make CustomBlockSourceFactory collectable by inventory
inventory::collect!(CustomBlockSourceFactory);

/// Factory for creating custom stream sources from configuration.
///
/// Register these using `inventory::submit!` to make them available
/// for instantiation from config files.
pub struct CustomStreamSourceFactory {
    /// Type identifier used in config (e.g., "webhook", "mqtt", "kafka")
    pub source_type: &'static str,

    /// Factory function that creates a source from JSON config
    pub create: fn(&serde_json::Value) -> Result<Arc<dyn DataStream>>,
}

// Make CustomStreamSourceFactory collectable by inventory
inventory::collect!(CustomStreamSourceFactory);

/// Look up and create a custom block source by type name.
///
/// Searches registered `CustomBlockSourceFactory` entries for a matching
/// `source_type` and calls its `create` function with the provided config.
pub fn create_custom_block(
    source_type: &str,
    config: &serde_json::Value,
) -> Result<Option<Arc<dyn DataBlock>>> {
    for factory in inventory::iter::<CustomBlockSourceFactory> {
        if factory.source_type == source_type {
            let source = (factory.create)(config)?;
            return Ok(Some(source));
        }
    }
    Ok(None)
}

/// Look up and create a custom stream source by type name.
///
/// Searches registered `CustomStreamSourceFactory` entries for a matching
/// `source_type` and calls its `create` function with the provided config.
pub fn create_custom_stream(
    source_type: &str,
    config: &serde_json::Value,
) -> Result<Option<Arc<dyn DataStream>>> {
    for factory in inventory::iter::<CustomStreamSourceFactory> {
        if factory.source_type == source_type {
            let source = (factory.create)(config)?;
            return Ok(Some(source));
        }
    }
    Ok(None)
}

/// List all registered custom block source types.
pub fn available_custom_block_types() -> Vec<&'static str> {
    inventory::iter::<CustomBlockSourceFactory>
        .into_iter()
        .map(|f| f.source_type)
        .collect()
}

/// List all registered custom stream source types.
pub fn available_custom_stream_types() -> Vec<&'static str> {
    inventory::iter::<CustomStreamSourceFactory>
        .into_iter()
        .map(|f| f.source_type)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_factories_registered_returns_none() {
        // This test verifies the lookup behavior when no factories match
        let result = create_custom_block("nonexistent", &serde_json::json!({}));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        let result = create_custom_stream("nonexistent", &serde_json::json!({}));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
