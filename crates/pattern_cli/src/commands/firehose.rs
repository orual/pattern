//! Bluesky firehose testing commands
//!
//! ## Migration Status
//!
//! This module is STUBBED during the pattern_db migration. The following
//! functionality needs to be reimplemented:
//!
//! - BlueskyFirehoseSource from pattern_core::data_source
//! - BlueskyFilter for filtering events
//! - DataSource trait for subscription
//!
//! The data_source module is currently disabled in pattern_core.

use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::PatternConfig;

use crate::output::Output;

/// Listen to the Jetstream firehose with filters
///
/// NOTE: This is currently STUBBED during the pattern_db migration.
pub async fn listen(
    limit: usize,
    nsids: Vec<String>,
    dids: Vec<String>,
    mentions: Vec<String>,
    keywords: Vec<String>,
    languages: Vec<String>,
    endpoint: Option<String>,
    format: String,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();

    // Use endpoint from args or config
    let jetstream_endpoint = endpoint.unwrap_or_else(|| {
        config
            .bluesky
            .as_ref()
            .map(|b| b.jetstream_endpoint.clone())
            .unwrap_or_else(|| "wss://jetstream2.us-east.bsky.network/subscribe".to_string())
    });

    output.success("Bluesky Firehose Listener");
    output.info("Endpoint:", &jetstream_endpoint.bright_cyan().to_string());
    output.info("Limit:", &limit.to_string());
    output.info("Format:", &format);

    output.section("Requested Filters");
    if !nsids.is_empty() {
        output.list_item(&format!("NSIDs: {}", nsids.join(", ")));
    }
    if !dids.is_empty() {
        output.list_item(&format!("DIDs: {} DIDs", dids.len()));
    }
    if !mentions.is_empty() {
        output.list_item(&format!("Mentions: {}", mentions.join(", ")));
    }
    if !keywords.is_empty() {
        output.list_item(&format!("Keywords: {}", keywords.join(", ")));
    }
    if !languages.is_empty() {
        output.list_item(&format!("Languages: {}", languages.join(", ")));
    }

    output.print("");
    output.warning("Firehose commands temporarily disabled during data_source migration");
    output.status("Reason: data_source module is disabled in pattern_core");
    output.status("");
    output.status("Previous functionality:");
    output.list_item("Connect to Bluesky Jetstream WebSocket");
    output.list_item("Filter events by NSID, DID, mentions, keywords, languages");
    output.list_item("Display events in pretty, JSON, or raw format");
    output.list_item("Stop after receiving limit events");

    Ok(())
}

/// Test connection to Jetstream
///
/// NOTE: This is currently STUBBED during the pattern_db migration.
pub async fn test_connection(endpoint: Option<String>, config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    // Use endpoint from args or config
    let jetstream_endpoint = endpoint.unwrap_or_else(|| {
        config
            .bluesky
            .as_ref()
            .map(|b| b.jetstream_endpoint.clone())
            .unwrap_or_else(|| "wss://jetstream2.us-east.bsky.network/subscribe".to_string())
    });

    output.success("Jetstream Connection Test");
    output.info("Endpoint:", &jetstream_endpoint.bright_cyan().to_string());
    output.print("");

    output.warning("Firehose commands temporarily disabled during data_source migration");
    output.status("Reason: data_source module is disabled in pattern_core");
    output.status("");
    output.status("Previous functionality:");
    output.list_item("Connect to Jetstream endpoint");
    output.list_item("Wait for first event (10s timeout)");
    output.list_item("Report connection success or failure");

    Ok(())
}
