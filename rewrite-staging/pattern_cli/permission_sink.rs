use tokio::task::JoinHandle;

use pattern_core::permission::{PermissionDecisionKind, PermissionRequest, broker};

use crate::output::Output;

pub fn spawn_cli_permission_listener(output: Output) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = broker().subscribe();
        loop {
            match rx.recv().await {
                Ok(PermissionRequest {
                    id,
                    tool_name,
                    scope,
                    ..
                }) => {
                    output.section("Permission Requested");
                    output.kv("Request ID", &id);
                    output.kv("Tool", &tool_name);
                    output.kv("Scope", &format!("{:?}", scope));
                    output.status("Type /permit <id> [once|always|ttl=600] or /deny <id>");
                }
                Err(_) => break,
            }
        }
    })
}

pub async fn cli_permit(id: &str, mode: Option<&str>) -> miette::Result<()> {
    let decision = match mode.unwrap_or("once").to_lowercase().as_str() {
        "once" => PermissionDecisionKind::ApproveOnce,
        "always" | "scope" => PermissionDecisionKind::ApproveForScope,
        s if s.starts_with("ttl=") => {
            let secs: u64 = s[4..].parse().unwrap_or(600);
            PermissionDecisionKind::ApproveForDuration(std::time::Duration::from_secs(secs))
        }
        _ => PermissionDecisionKind::ApproveOnce,
    };
    let ok = broker().resolve(id, decision).await;
    if ok {
        Ok(())
    } else {
        Err(miette::miette!("Unknown request id"))
    }
}

pub async fn cli_deny(id: &str) -> miette::Result<()> {
    let ok = broker().resolve(id, PermissionDecisionKind::Deny).await;
    if ok {
        Ok(())
    } else {
        Err(miette::miette!("Unknown request id"))
    }
}
