// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Error logging utilities for better miette formatting in tracing

/// Log an error with miette's nice formatting
///
/// This macro logs errors at ERROR level with miette's nice debug formatting.
#[macro_export]
macro_rules! log_error {
    ($err:expr) => {{
        let err = &$err;
        // Use Debug formatting to get miette's nice output
        tracing::error!("{:?}", err);
    }};
    ($msg:expr, $err:expr) => {{
        let err = &$err;
        // Log with context and use Debug for nice miette formatting
        tracing::error!("{}: {:?}", $msg, err);
    }};
}

/// Log an error with its full cause chain
///
/// This macro logs errors at ERROR level with their complete cause chain,
/// useful for debugging complex error scenarios.
#[macro_export]
macro_rules! log_error_chain {
    ($err:expr) => {{
        let err = &$err;
        // Log the main error with nice formatting
        tracing::error!("{:?}", err);

        // Log the cause chain if available
        use std::error::Error;
        if let Some(source) = err.source() {
            tracing::error!("Caused by:");
            let mut current = source;
            let mut depth = 1;
            loop {
                tracing::error!("  {}: {:?}", depth, current);
                match current.source() {
                    Some(next) => {
                        current = next;
                        depth += 1;
                    }
                    None => break,
                }
            }
        }
    }};
    ($msg:expr, $err:expr) => {{
        let err = &$err;
        // Log the main error with context
        tracing::error!("{}: {:?}", $msg, err);

        // Log the cause chain if available
        use std::error::Error;
        if let Some(source) = err.source() {
            tracing::error!("Caused by:");
            let mut current = source;
            let mut depth = 1;
            loop {
                tracing::error!("  {}: {:?}", depth, current);
                match current.source() {
                    Some(next) => {
                        current = next;
                        depth += 1;
                    }
                    None => break,
                }
            }
        }
    }};
}

/// Helper trait to format errors nicely for logging
pub trait ErrorLogging {
    /// Format the error with its full chain for logging
    fn log_format(&self) -> String;
}

impl<E: std::error::Error> ErrorLogging for E {
    fn log_format(&self) -> String {
        use std::fmt::Write;
        let mut output = format!("{}", self);

        if let Some(source) = self.source() {
            let _ = write!(output, "\n\nCaused by:");
            let mut current = source;
            let mut depth = 1;
            loop {
                let _ = write!(output, "\n  {}: {}", depth, current);
                match current.source() {
                    Some(next) => {
                        current = next;
                        depth += 1;
                    }
                    None => break,
                }
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("Test error")]
    struct TestError {
        #[source]
        source: std::io::Error,
    }

    #[test]
    #[tracing_test::traced_test]
    fn test_error_logging() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let test_err = TestError { source: io_err };

        let formatted = test_err.log_format();
        assert!(formatted.contains("Test error"));
        assert!(formatted.contains("Caused by:"));
        assert!(formatted.contains("file not found"));
    }

    #[test]
    #[tracing_test::traced_test]
    fn test_miette_error_logging() {
        // Create a miette error with context
        let err = miette::miette!(
            code = "pattern::test_error",
            help = "Try checking your configuration",
            "Something went wrong while processing"
        );

        // Show the difference between Display and Debug
        println!("\n=== Display formatting (plain) ===");
        println!("{}", err);

        println!("\n=== Debug formatting (fancy) ===");
        println!("{:?}", err);

        // Test our macros
        println!("\n=== Using log_error! macro ===");
        crate::log_error!(err);

        println!("\n=== Using log_error! with context ===");
        crate::log_error!("Failed to initialize system", err);
    }

    #[test]
    #[tracing_test::traced_test]
    fn test_error_chain_logging() {
        use std::io;

        // Create a chain of errors
        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("Database connection failed")]
        #[diagnostic(code(pattern::db::connection_failed))]
        struct DbError {
            #[source]
            source: io::Error,
        }

        #[derive(Debug, thiserror::Error, miette::Diagnostic)]
        #[error("Failed to start service")]
        #[diagnostic(
            code(pattern::service::start_failed),
            help("Check that the database is running and accessible")
        )]
        struct ServiceError {
            #[source]
            source: DbError,
        }

        let io_err = io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused");
        let db_err = DbError { source: io_err };
        let service_err = ServiceError { source: db_err };

        println!("\n=== Error chain with log_error_chain! ===");
        crate::log_error_chain!("Service startup failed", service_err);

        // Also test with raw CoreError
        let core_err = crate::CoreError::ConfigurationError {
            config_path: "/path/to/config.toml".to_string(),
            field: "api_key".to_string(),
            expected: "valid API key".to_string(),
            cause: crate::error::ConfigError::Io("File not found".to_string()),
        };

        println!("\n=== CoreError with miette formatting ===");
        println!("Direct debug print: {:?}", core_err);

        println!("\nAs miette::Report:");
        let report = miette::Report::from(core_err);
        println!("{:?}", report);

        println!("\nUsing log_error with Report:");
        crate::log_error!(report);
    }
}
