// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Message` (`haskell/Pattern/Message.hs`).

use tidepool_bridge_derive::FromCore;

/// Wire record for `Message.Delegate` — carries the task's block reference
/// plus the routing target and message body.
///
/// The `Delegate` constructor in the Haskell `Message` GADT takes this
/// record as its single argument. On the Rust side, the handler unpacks it
/// via `WireDelegateReq` and constructs a `BlockRef` from the three task
/// fields before routing.
///
/// Haskell record selectors are prefix-disambiguated (`delegate*`) so that
/// multiple records can be in scope without `DuplicateRecordFields`.
#[derive(Debug, FromCore)]
#[core(module = "Pattern.Message", name = "DelegateReq")]
pub struct WireDelegateReq {
    /// Human-readable label for the task block (for snapshot display).
    pub task_label: String,
    /// Storage block ID of the task to pin into the recipient's context.
    pub task_block_id: String,
    /// Agent ID that owns the task block.
    pub task_agent_id: String,
    /// Routing target — typically `"agent:<persona-id>"`.
    pub recipient: String,
    /// Message body sent to the recipient alongside the task pin.
    pub body: String,
}

/// Rust mirror of the Haskell `Message` GADT.
#[derive(Debug, FromCore)]
pub enum MessageReq {
    #[core(module = "Pattern.Message", name = "Ask")]
    Ask(String),
    #[core(module = "Pattern.Message", name = "Send")]
    Send(String, String),
    #[core(module = "Pattern.Message", name = "Reply")]
    Reply(String, String),
    #[core(module = "Pattern.Message", name = "Notify")]
    Notify(String, String),
    /// Delegate a task to another agent.
    ///
    /// Takes a [`WireDelegateReq`] carrying the task's block reference plus
    /// routing info. The handler constructs a `Message` with the task's
    /// `BlockRef` in `block_refs`, causing the recipient's snapshot composer
    /// to pin the task into the working-memory selection for that turn (AC6.3).
    ///
    /// See [`WireDelegateReq`] for field descriptions.
    #[core(module = "Pattern.Message", name = "Delegate")]
    Delegate(WireDelegateReq),
}
