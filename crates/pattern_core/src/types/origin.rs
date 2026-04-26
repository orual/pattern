//! `MessageOrigin` and its composing types: provenance for every inbound
//! message that reaches an agent.
//!
//! [`MessageOrigin`] replaces the pre-v3 `Caller` + `source_descriptor` split
//! with a unified provenance value that answers three questions at once:
//!
//! - **Who authored this message?** → [`Author`]
//! - **What visibility sphere was it published into?** → [`Sphere`]
//! - **What transport / data-source surfaced it to us?** — future work; the
//!   current phase captures only author + sphere.
//!
//! # Why this type exists
//!
//! Pre-v3 Pattern routed on a loose combination of `Caller`, endpoint kind,
//! and ad-hoc fields scattered across message metadata. The result was that
//! visibility decisions (can this agent post back? to whom?) were rederived
//! at each routing site from whatever context happened to be nearby. V3
//! threads a single `MessageOrigin` value through the turn input so that
//! every consumer — endpoint registry, context composer, ACL layer — reads
//! from the same provenance record.
//!
//! [`Sphere`] enumerates the canonical visibility classes used across
//! Pattern's transports; [`Author`] enumerates the canonical authorship
//! classes. Supporting types [`Partner`], [`Human`], and [`AgentAuthor`]
//! carry the transport-specific identity for each authorship class.

use jiff::Span;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::types::block_ref::BlockRef;
use crate::types::ids::{AgentId, UserId};
use crate::types::memory_types::TaskEdgeRef;

/// `jiff::Span` wrapper that opts into fieldwise equality.
///
/// `Span` itself does not implement `PartialEq<Self>` — span equality is
/// calendar-dependent (e.g. "1 month" vs. "30 days" cannot be decided
/// without a reference instant). [`SpanCompare`] commits to fieldwise
/// equality (same y/m/w/d/h/m/s/ms/us/ns), so it can derive
/// `PartialEq`/`Eq`/`Hash` for embedding in types like [`SystemReason`]
/// that participate in derived comparisons. Serializes transparently as
/// the underlying `Span`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpanCompare(pub Span);

impl From<Span> for SpanCompare {
    fn from(s: Span) -> Self {
        Self(s)
    }
}

impl PartialEq for SpanCompare {
    fn eq(&self, other: &Self) -> bool {
        self.0.fieldwise() == other.0.fieldwise()
    }
}

impl Eq for SpanCompare {}

impl std::hash::Hash for SpanCompare {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Hash via the canonical field tuple. `SpanFieldwise` impls
        // Hash directly; delegate to it so two `SpanCompare`s that
        // compare equal also hash equal.
        self.0.fieldwise().hash(state);
    }
}

/// Visibility sphere — where a message was published.
///
/// Spheres are ordered from least to most public. Agents use the sphere on
/// [`MessageOrigin`] to decide whether to reply, how to format, and whether
/// to persist the exchange to long-term memory.
///
/// # Examples
///
/// ```
/// use pattern_core::types::origin::Sphere;
///
/// let s = Sphere::Private;
/// assert_eq!(format!("{s:?}"), "Private");
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Sphere {
    /// System-internal: framework-emitted messages (scheduler wakeups,
    /// runtime signals, pseudo-messages from memory changes).
    System,
    /// Internal to a constellation of agents sharing one runtime — not
    /// visible to any external human, but visible across cooperating
    /// agents.
    Internal,
    /// Private between the partner and the agent (1:1 channel, DM, etc.).
    Private,
    /// Semi-private: a small shared group (private Discord thread, small
    /// group chat) where all members are known to the partner.
    SemiPrivate,
    /// Publicly visible (public Discord channel, ATProto post, etc.).
    Public,
}

/// The identity of the partner (the human who owns this agent constellation).
///
/// A partner is distinguished from a generic [`Human`] by being the *owner*
/// of the constellation — the person whose persona the agent is supporting.
///
/// # Examples
///
/// ```
/// use pattern_core::types::origin::Partner;
/// use pattern_core::types::ids::new_id;
///
/// let p = Partner { user_id: new_id() };
/// assert_eq!(p.user_id.len(), 32);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Partner {
    /// The partner's stable user id.
    pub user_id: UserId,
}

/// The identity of a non-partner human participant.
///
/// A `Human` is someone other than the partner — a third party in a group
/// chat, a reply-to on a public post, etc. The `display_name` is optional
/// and transport-dependent; use it for formatting only, never for identity
/// matching.
///
/// # Examples
///
/// ```
/// use pattern_core::types::origin::Human;
/// use pattern_core::types::ids::new_id;
///
/// let h = Human { user_id: new_id(), display_name: Some("alex".into()) };
/// assert_eq!(h.display_name.as_deref(), Some("alex"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Human {
    /// Stable user id (may be transport-scoped, e.g. Discord ID).
    pub user_id: UserId,
    /// Display name for formatting purposes.
    pub display_name: Option<String>,
}

/// The identity of an agent author — another agent in the same or a
/// cooperating constellation.
///
/// # Examples
///
/// ```
/// use pattern_core::types::origin::AgentAuthor;
/// use smol_str::SmolStr;
///
/// let a = AgentAuthor { agent_id: SmolStr::new("anchor") };
/// assert_eq!(a.agent_id.as_str(), "anchor");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentAuthor {
    /// The authoring agent's id.
    pub agent_id: AgentId,
}

/// Who authored the incoming message.
///
/// [`Author`] is the canonical authorship enum. It is `#[non_exhaustive]` so
/// future transports may add kinds (e.g. a `Plugin` variant) without breaking
/// match arms.
///
/// # Examples
///
/// ```
/// use pattern_core::types::origin::{Author, Partner};
/// use pattern_core::types::ids::new_id;
///
/// let a = Author::Partner(Partner { user_id: new_id() });
/// matches!(a, Author::Partner(_));
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Author {
    /// The constellation's partner (the human who owns this agent).
    Partner(Partner),
    /// A non-partner human participant.
    Human(Human),
    /// Another agent, typically in a cooperating constellation.
    Agent(AgentAuthor),
    /// The system itself (scheduler, pseudo-message emitter, runtime).
    ///
    /// The [`SystemReason`] discriminates the trigger kind so anti-loop,
    /// rate-limit, and attribution code can key off cause without adding
    /// another axis to [`Author`].
    System { reason: SystemReason },
}

/// Why the system triggered a message.
///
/// Used on [`Author::System`] to distinguish the concrete cause of a
/// system-authored message. `#[non_exhaustive]` so plugin/integration code
/// can add variants in future phases without breaking match arms.
///
/// Variants are data-bearing where the cause has structured payload —
/// rather than dropping a positional `block_refs[0]` convention on every
/// caller, the affected refs / spans / ids live on the variant itself.
/// Phase 4 wake-condition primitives (`TaskTimeout`, `TaskDependencyResolved`,
/// `BlockChanged`, `Interval`, `CustomWake`) all use this shape.
///
/// `Copy` is dropped because some payloads (e.g. `BlockRef`, `SmolStr`)
/// allocate. Equality + hashing work because span values are stored as
/// [`jiff::SpanFieldwise`] (calendar-independent fieldwise compare).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemReason {
    /// A generic timer effect fired. Use a more specific variant below when
    /// the cause is known (sleeptime/wakeup/tool-call); `Timer` is the
    /// fallback for agent-scheduled timers that don't fit those cases.
    Timer,
    /// Scheduled sleeptime processing (nightly consolidation, etc.).
    Sleeptime,
    /// A scheduled wakeup fired.
    Wakeup,
    /// Message surfaced by pseudo-message emission after a memory write.
    MemoryChange,
    /// Turn was triggered by a tool-call follow-up.
    ToolCall,
    /// A task's deadline elapsed without the agent completing it.
    TaskTimeout {
        /// The task whose timer fired.
        task: BlockRef,
        /// How long the timer was set for. Echoed back so the agent
        /// can branch on duration without re-reading the task.
        elapsed: SpanCompare,
    },
    /// A dependency task transitioned to `Completed`.
    ///
    /// Tasks live as items inside `BlockSchema::TaskList` blocks; the
    /// reference identifies both the parent block (`task.block`) and
    /// the specific item (`task.task_item`). For block-level
    /// references the agent is woken when *any* item in the block
    /// reaches `Completed`.
    TaskDependencyResolved {
        /// The dependency that just resolved.
        task: TaskEdgeRef,
    },
    /// A specific block's content changed (any author). Used when the
    /// agent registered explicit interest in a memory location.
    BlockChanged {
        /// The block whose content changed.
        block: BlockRef,
    },
    /// A periodic interval wake fired.
    Interval {
        /// The interval period.
        period: SpanCompare,
    },
    /// A Haskell-registered custom wake fired. Phase 4 ships the
    /// registration path; the evaluator that runs the user's condition is
    /// deferred.
    CustomWake {
        /// User-supplied identifier from `ctx.wake.register`.
        id: SmolStr,
    },
}

/// Provenance for a single inbound message.
///
/// Every [`crate::types::TurnInput`] carries a `MessageOrigin` so that
/// downstream consumers — routing, context composition, ACL checks — can
/// make decisions from a single source of truth rather than rederiving
/// provenance per site.
///
/// # Examples
///
/// ```
/// use pattern_core::types::origin::{Author, MessageOrigin, Sphere};
///
/// # use pattern_core::types::origin::SystemReason;
/// let origin = MessageOrigin::new(
///     Author::System { reason: SystemReason::Wakeup },
///     Sphere::System,
/// );
/// assert_eq!(origin.sphere, Sphere::System);
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageOrigin {
    /// Who authored the message.
    pub author: Author,
    /// What visibility sphere it was published into.
    pub sphere: Sphere,
    /// A transport-specific hint for displaying the message (e.g.
    /// channel name). Phase 4 also uses this slot for the
    /// custom-wake id when [`Author::System`] carries
    /// [`SystemReason::CustomWake`].
    pub transport_hint: Option<SmolStr>,
}

impl MessageOrigin {
    /// Construct a `MessageOrigin` from its two mandatory axes. Use this
    /// constructor rather than struct-literal syntax so future
    /// `#[non_exhaustive]` fields can be added without breakage.
    pub fn new(author: Author, sphere: Sphere) -> Self {
        Self {
            author,
            sphere,
            transport_hint: None,
        }
    }

    pub fn with_transport_hint(mut self, transport_hint: SmolStr) -> Self {
        self.transport_hint = Some(transport_hint);
        self
    }

    /// Whether this origin should short-circuit the permission gate.
    ///
    /// Partner-driven turns (the constellation owner directly addressing
    /// an agent) bypass approval — they're acting as an authenticated
    /// human-in-the-loop and don't need to gate themselves through the
    /// broker. All other authorship classes (other humans in shared
    /// channels, sibling agents, system-emitted messages) flow through
    /// the normal `PolicySet` + `PermissionBroker` pipeline.
    pub fn bypasses_permission_gate(&self) -> bool {
        matches!(self.author, Author::Partner(_))
    }
}
