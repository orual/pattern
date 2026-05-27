// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Error types for `.pattern.kdl` config parsing and validation.

use std::path::PathBuf;

/// Errors produced when loading or validating a `.pattern.kdl` mount config.
#[non_exhaustive]
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum ConfigError {
    /// I/O failure reading the config file from disk.
    #[error("io error reading {path}: {source}")]
    #[diagnostic(code(pattern_memory::config::io))]
    Io {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// KDL parse error. The inner `knus::Error` is miette-native and carries
    /// line/column spans that surface in diagnostic output.
    #[error("parse error in {path}: {source}")]
    #[diagnostic(code(pattern_memory::config::parse))]
    Parse {
        /// Path of the config file that failed to parse.
        path: PathBuf,
        /// KDL parse error with source span information.
        #[source]
        source: knus::Error,
    },

    /// The config parsed successfully but fails a cross-field constraint that
    /// KDL syntax alone cannot enforce (e.g. Standalone mode requires `jj.enabled=true`).
    #[error("invalid mount config in {path}: {reason}")]
    #[diagnostic(code(pattern_memory::config::validation))]
    Validation {
        /// Path of the config file that failed validation.
        path: PathBuf,
        /// Human-readable explanation of the constraint violation.
        reason: String,
    },
}
