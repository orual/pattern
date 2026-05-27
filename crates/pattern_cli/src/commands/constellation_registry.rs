// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Phase 6 T7: CLI subcommands for constellation registry operations.
//!
//! `pattern constellation list / promote / relate / groups list / groups create`
//! talk to the daemon over IRPC. Each command:
//!
//! 1. Auto-starts the daemon if not running.
//! 2. Connects via [`DaemonClient::connect`].
//! 3. Sends `InitSession` so the daemon mounts the current project and the
//!    per-mount registry is available.
//! 4. Calls the relevant RPC and renders the result to stdout.

use miette::{Result as MietteResult, miette};

use pattern_server::client::DaemonClient;

use crate::commands::daemon::ensure_daemon_running;

/// Connect to the daemon (auto-starting if needed) and run `InitSession`
/// against the current working directory so the constellation registry RPCs
/// have a mount to work against.
async fn connect_and_init() -> MietteResult<DaemonClient> {
    let _ = ensure_daemon_running();

    let client = DaemonClient::connect()
        .await
        .map_err(|e| miette!("failed to connect to daemon: {e}"))?;

    // Mount the current directory's project so the daemon's `current_mount`
    // is populated. The daemon ignores agent_id when `default_agent` does
    // not resolve — registry RPCs do not require an agent to be open.
    let cwd =
        std::env::current_dir().map_err(|e| miette!("failed to read current directory: {e}"))?;
    let info = client
        .init_session(cwd, "default".into())
        .await
        .map_err(|e| miette!("InitSession failed: {e}"))?;
    if let Some(err) = info.error
        && !err.contains("default")
    {
        // Mount failures matter; agent-id resolution failure ("agent not found
        // in personas/") is fine for registry-only ops.
        return Err(miette!("daemon reported mount error: {err}"));
    }

    Ok(client)
}

/// `pattern constellation list [--project PATH]`.
pub async fn cmd_list(project: Option<String>) -> MietteResult<()> {
    let client = connect_and_init().await?;
    let resp = client
        .list_personas(project)
        .await
        .map_err(|e| miette!("ListPersonas RPC failed: {e}"))?;

    if let Some(err) = resp.error {
        return Err(miette!("daemon error: {err}"));
    }
    if resp.personas.is_empty() {
        println!("(no personas registered)");
        return Ok(());
    }
    println!("{:<24}  {:<8}  {}", "ID", "STATUS", "NAME");
    println!("{}", "-".repeat(60));
    for p in resp.personas {
        println!("{:<24}  {:<8}  {}", p.id, p.status, p.name);
    }
    Ok(())
}

/// `pattern constellation promote <ID>`.
pub async fn cmd_promote(persona_id: String) -> MietteResult<()> {
    let client = connect_and_init().await?;
    let resp = client
        .promote_draft(persona_id.clone())
        .await
        .map_err(|e| miette!("PromoteDraft RPC failed: {e}"))?;
    if !resp.success {
        return Err(miette!(
            "daemon refused to promote: {}",
            resp.error.unwrap_or_default()
        ));
    }
    println!("promoted persona {persona_id} to Active");
    Ok(())
}

/// `pattern constellation relate <FROM> <TO> <KIND>`.
pub async fn cmd_relate(from: String, to: String, kind: String) -> MietteResult<()> {
    let client = connect_and_init().await?;
    let resp = client
        .add_relationship(from.clone(), to.clone(), kind.clone())
        .await
        .map_err(|e| miette!("AddRelationship RPC failed: {e}"))?;
    if !resp.success {
        return Err(miette!(
            "daemon refused: {}",
            resp.error.unwrap_or_default()
        ));
    }
    println!("added relationship {from} -[{kind}]-> {to}");
    Ok(())
}

/// `pattern constellation groups list [--project PATH]`.
pub async fn cmd_groups_list(project: Option<String>) -> MietteResult<()> {
    let client = connect_and_init().await?;
    let resp = client
        .list_groups(project)
        .await
        .map_err(|e| miette!("ListGroups RPC failed: {e}"))?;

    if let Some(err) = resp.error {
        return Err(miette!("daemon error: {err}"));
    }
    if resp.groups.is_empty() {
        println!("(no groups created)");
        return Ok(());
    }
    println!("{:<32}  {:<32}  {}", "NAME", "PROJECT", "MEMBERS");
    println!("{}", "-".repeat(80));
    for g in resp.groups {
        println!(
            "{:<32}  {:<32}  {}",
            g.name,
            g.project_id.as_deref().unwrap_or("(global)"),
            g.members.join(", ")
        );
    }
    Ok(())
}

/// `pattern constellation groups create <NAME> [--project-id ID]`.
pub async fn cmd_groups_create(name: String, project_id: Option<String>) -> MietteResult<()> {
    let client = connect_and_init().await?;
    let resp = client
        .create_group(name.clone(), project_id.clone())
        .await
        .map_err(|e| miette!("CreateGroup RPC failed: {e}"))?;
    if let Some(err) = resp.error {
        return Err(miette!("daemon refused: {err}"));
    }
    let g = resp
        .group
        .ok_or_else(|| miette!("daemon returned success but no group payload"))?;
    println!(
        "created group {} (id: {}, project: {})",
        g.name,
        g.id,
        g.project_id.as_deref().unwrap_or("(global)")
    );
    Ok(())
}
