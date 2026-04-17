//! Mirror of `Pattern.Display` (`haskell/Pattern/Display.hs`).
//!
//! The Display effect is broadcast-style: the Haskell agent emits one-shot
//! envelopes describing observable output. Subscribers registered Rust-side
//! receive them in realtime. See `sdk/handlers/display.rs` for subscriber
//! shape.

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Display` GADT.
#[derive(Debug, FromCore)]
pub enum DisplayReq {
    /// A partial chunk during a streaming provider response. Forwarded to
    /// every registered subscriber as-is.
    #[core(name = "Chunk")]
    Chunk(String),
    /// Final assembled content for the turn's Message.Ask. Fires once,
    /// after the provider stream completes.
    #[core(name = "Final")]
    Final(String),
    /// Agent-visible note (typing indicator, tool-call progress, etc.) that
    /// isn't part of the LLM response stream. Subscribers decide whether
    /// to render.
    #[core(name = "Note")]
    Note(String),
}
