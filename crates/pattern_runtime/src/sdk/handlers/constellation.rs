//! Handler for `Pattern.Constellation` (v3-multi-agent Phase 6 Task 5).
//!
//! Read-only surface to agent code: query the constellation registry for
//! persona records and groups. Mutation paths (`register`, `set_status`, etc.)
//! are daemon-level RPCs and have no SDK surface.
//!
//! # Capability gate
//!
//! All three constructors (`List`, `Find`, `Groups`) require
//! [`pattern_core::EffectCategory::Constellation`] on the agent's capability
//! set. Sessions without it receive a [`crate::policy::CAPABILITY_DENIED_PREFIX`]
//! error.
//!
//! # Missing-registry path
//!
//! If the session's [`crate::session::SessionContext`] has no
//! `ConstellationRegistry` wired (`constellation_registry()` returns `None`),
//! the handler returns a [`CONSTELLATION_NOT_WIRED_PREFIX`]-marked error. This
//! covers test sessions and single-agent (non-daemon) paths.
//!
//! # Sync→async bridge
//!
//! The trait is async; the eval worker is sync. We `tokio_handle().block_on(...)`
//! the registry future. The await is bounded — registry methods complete in
//! finite time (DB query, no plugin code, no network).

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use pattern_core::ConstellationRegistry;
use pattern_core::EffectCategory;
use pattern_core::constellation::{RegistryError, RegistryScope};

use crate::policy::CAPABILITY_DENIED_PREFIX;
use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::ConstellationReq;
use crate::sdk::requests::constellation::{
    WirePersonaGroup, WirePersonaRecord, parse_relationship_kind,
};
use crate::session::SessionContext;

/// Prefix attached to "registry not wired" errors so tests / the UI can
/// distinguish them from capability denials and backend failures.
pub const CONSTELLATION_NOT_WIRED_PREFIX: &str = "ConstellationNotWired: ";

/// Handler for the `Pattern.Constellation` effect.
#[derive(Default, Clone)]
pub struct ConstellationHandler;

impl DescribeEffect for ConstellationHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Constellation",
            description: "Read persona records and groups from the constellation registry.",
            constructors: std::borrow::Cow::Borrowed(&[
                "List   :: Maybe Text -> Constellation [PersonaRecord]",
                "Find   :: Maybe Text -> Maybe Text -> Constellation [PersonaRecord]",
                "Groups :: Maybe Text -> Constellation [PersonaGroup]",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "data PersonaStatus = PersonaActive | PersonaDraft | PersonaInactive",
                "data RelationshipKind = RelSupervisorOf | RelSpecialistFor | RelPeerWith | RelObserverOf",
                "data EdgeDirection = DirOutgoing | DirIncoming",
                "data RelationshipEdge = RelationshipEdge { other :: Text, kind :: RelationshipKind, direction :: EdgeDirection }",
                "data PersonaRecord = PersonaRecord { personaId :: Text, name :: Text, status :: PersonaStatus, configPath :: Maybe Text, projectAttachments :: [Text], relationships :: [RelationshipEdge], groupMemberships :: [Text] }",
                "data PersonaGroup = PersonaGroup { groupId :: Text, name :: Text, projectId :: Maybe Text, members :: [Text] }",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "list :: Member Constellation effs => Maybe Text -> Eff effs [PersonaRecord]\nlist scope = send (List scope)",
                "find :: Member Constellation effs => Maybe Text -> Maybe Text -> Eff effs [PersonaRecord]\nfind project kind = send (Find project kind)",
                "groups :: Member Constellation effs => Maybe Text -> Eff effs [PersonaGroup]\ngroups scope = send (Groups scope)",
            ]),
        }
    }
}

impl EffectHandler<SessionContext> for ConstellationHandler {
    type Request = ConstellationReq;

    fn handle(
        &mut self,
        req: ConstellationReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let user: &SessionContext = cx.user();

        // Effect-class runtime guard. Runs BEFORE the category capability gate.
        // All Constellation constructors are Observe/Enforce.
        let constructor_name = match &req {
            ConstellationReq::List(_) => "List",
            ConstellationReq::Find(_, _) => "Find",
            ConstellationReq::Groups(_) => "Groups",
        };
        crate::sdk::effect_classes::check_effect_class(
            user.capabilities(),
            "Constellation",
            constructor_name,
        )?;

        // Capability gate. `None` capabilities is fail-closed.
        let allowed = user
            .capabilities()
            .map(|c| c.contains(EffectCategory::Constellation))
            .unwrap_or(false);
        if !allowed {
            return Err(EffectError::Handler(format!(
                "{CAPABILITY_DENIED_PREFIX}{}",
                EffectCategory::Constellation.type_name()
            )));
        }

        let registry: Arc<dyn ConstellationRegistry> =
            user.constellation_registry().cloned().ok_or_else(|| {
                EffectError::Handler(format!(
                    "{CONSTELLATION_NOT_WIRED_PREFIX}Pattern.Constellation handler invoked \
                     but no ConstellationRegistry is wired on the SessionContext"
                ))
            })?;

        match req {
            ConstellationReq::List(scope) => handle_list(scope, registry, cx),
            ConstellationReq::Find(project, kind) => handle_find(project, kind, registry, cx),
            ConstellationReq::Groups(scope) => handle_groups(scope, registry, cx),
        }
    }
}

fn parse_scope(scope: Option<String>) -> RegistryScope {
    match scope {
        None => RegistryScope::All,
        Some(s) => RegistryScope::Project(PathBuf::from(s)),
    }
}

fn map_registry_err(e: RegistryError) -> EffectError {
    EffectError::Handler(format!("constellation registry error: {e}"))
}

fn handle_list(
    scope: Option<String>,
    registry: Arc<dyn ConstellationRegistry>,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let handle = cx.user().tokio_handle().clone();
    let parsed = parse_scope(scope);
    let records = handle
        .block_on(async move { registry.list(parsed).await })
        .map_err(map_registry_err)?;
    let wires: Vec<WirePersonaRecord> = records.into_iter().map(Into::into).collect();
    cx.respond(wires)
}

fn handle_find(
    project: Option<String>,
    kind: Option<String>,
    registry: Arc<dyn ConstellationRegistry>,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let parsed_kind = match kind {
        None => None,
        Some(s) => match parse_relationship_kind(&s) {
            Some(k) => Some(k),
            None => {
                return Err(EffectError::Handler(format!(
                    "unknown relationship kind {s:?}; expected one of \
                     supervisor_of, specialist_for, peer_with, observer_of"
                )));
            }
        },
    };
    let proj_buf: Option<PathBuf> = project.map(PathBuf::from);

    let handle = cx.user().tokio_handle().clone();
    let records = handle
        .block_on(async move { registry.find(proj_buf.as_deref(), parsed_kind).await })
        .map_err(map_registry_err)?;
    let wires: Vec<WirePersonaRecord> = records.into_iter().map(Into::into).collect();
    cx.respond(wires)
}

fn handle_groups(
    scope: Option<String>,
    registry: Arc<dyn ConstellationRegistry>,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let handle = cx.user().tokio_handle().clone();
    let parsed = parse_scope(scope);
    let groups = handle
        .block_on(async move { registry.groups(parsed).await })
        .map_err(map_registry_err)?;
    let wires: Vec<WirePersonaGroup> = groups.into_iter().map(Into::into).collect();
    cx.respond(wires)
}

// Handler-dispatch tests live in `tests/constellation_sdk.rs` (integration
// test) where building a real `SessionContext` via `from_persona` is
// available without exposing a test-only constructor in production code.
