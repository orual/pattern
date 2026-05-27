// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Mirror of `Pattern.Tasks` (`haskell/Pattern/Tasks.hs`).
//!
//! Eight task-operation variants supporting the SDK surface methods:
//! `create_task`, `update_task`, `transition_status`, `link`, `unlink`,
//! `list_tasks`, `query_graph`, and `add_comment`.

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Tasks` GADT.
#[derive(Debug, FromCore)]
pub enum TasksReq {
    #[core(module = "Pattern.Tasks", name = "Create")]
    Create(
        String, /* BlockHandle */
        String, /* TaskCreateRequest JSON: {block_description?:Text, items:[TaskSpec]} */
    ),

    #[core(module = "Pattern.Tasks", name = "Update")]
    Update(
        String, /* TaskEdgeRef */
        String, /* TaskPatch JSON */
    ),

    #[core(module = "Pattern.Tasks", name = "Transition")]
    Transition(
        String, /* TaskEdgeRef */
        String, /* TaskStatus JSON */
    ),

    #[core(module = "Pattern.Tasks", name = "Link")]
    Link(String, String),

    #[core(module = "Pattern.Tasks", name = "Unlink")]
    Unlink(String, String),

    #[core(module = "Pattern.Tasks", name = "List")]
    List(Option<String>, String /* TaskFilter JSON */),

    #[core(module = "Pattern.Tasks", name = "QueryGraph")]
    QueryGraph(
        String, /* root TaskEdgeRef */
        String, /* GraphQuery JSON */
    ),

    #[core(module = "Pattern.Tasks", name = "AddComment")]
    AddComment(String, String),
}
