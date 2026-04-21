use crate::types::memory_types::MemoryPermission;

/// Memory operation types we gate by permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOp {
    Read,
    Append,
    Overwrite,
    Delete,
}

/// Result of permission check for a memory operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryGate {
    /// Operation can proceed without additional consent.
    Allow,
    /// Operation may proceed with human/partner consent.
    RequireConsent { reason: String },
    /// Operation is not allowed under current policy.
    Deny { reason: String },
}

/// Check whether `op` is allowed under `perm`.
/// Policy:
/// - Read: always allowed.
/// - Append: allowed for Append/ReadWrite/Admin; Human/Partner require consent; ReadOnly denied.
/// - Overwrite: allowed for ReadWrite/Admin; Human/Partner require consent; ReadOnly/Append denied (unless consent elevates).
/// - Delete: allowed for Admin only; others denied (may later support explicit high-risk consent).
pub fn check(op: MemoryOp, perm: MemoryPermission) -> MemoryGate {
    use MemoryGate::*;
    use MemoryOp::*;
    use MemoryPermission as P;

    match op {
        Read => Allow,
        Append => match perm {
            P::Append | P::ReadWrite | P::Admin => Allow,
            P::Human => RequireConsent {
                reason: "Requires human approval to append".into(),
            },
            P::Partner => RequireConsent {
                reason: "Requires partner approval to append".into(),
            },
            P::ReadOnly => Deny {
                reason: "Block is read-only; appending is not allowed".into(),
            },
        },
        Overwrite => match perm {
            P::ReadWrite | P::Admin => Allow,
            P::Human => RequireConsent {
                reason: "Requires human approval to overwrite".into(),
            },
            P::Partner => RequireConsent {
                reason: "Requires partner approval to overwrite".into(),
            },
            P::Append | P::ReadOnly => Deny {
                reason: "Insufficient permission (append-only or read-only) for overwrite".into(),
            },
        },
        Delete => match perm {
            P::Admin => Allow,
            _ => Deny {
                reason: "Deleting memory requires admin permission".into(),
            },
        },
    }
}

/// Build a human-friendly reason string for consent prompts.
pub fn consent_reason(key: &str, op: MemoryOp, current: MemoryPermission) -> String {
    format!(
        "Request to {:?} memory '{}' (current permission: {:?})",
        op, key, current
    )
}
