//! Fronting set, routing table, and message dispatch resolver.
//!
//! A `FrontingSet` describes which persona(s) are currently "fronting" — the
//! active interface to a human partner. An incoming message is resolved to one
//! or more target `PersonaId`s via `FrontingResolver::resolve`, which applies
//! the following decision sequence:
//!
//! 1. Strip `@persona-id` prefix → `ResolveOutcome::Direct`.
//! 2. Evaluate `RoutingTable.rules` in descending priority order; first match
//!    → `ResolveOutcome::Rule`.
//! 3. If a fallback persona is configured → `ResolveOutcome::Fallback`.
//! 4. If multiple personas are actively fronting → `ResolveOutcome::FanOut`.
//! 5. Consult the `ConstellationRegistry` for the first `Active` persona
//!    (sorted by id for determinism) → `ResolveOutcome::DefaultPersona`.
//! 6. No active personas exist → `ResolveOutcome::SystemDefault`.
//!
//! Messages never fail-close: every path returns a delivery target or the
//! system-default ack marker.

use std::sync::Arc;

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::constellation::{ConstellationRegistry, PersonaStatus, RegistryScope};
use crate::types::ids::PersonaId;

// ── FrontingSet ───────────────────────────────────────────────────────────────

/// The active fronting configuration for a runtime instance.
///
/// Pure serializable data — no compiled state. Build a `FrontingResolver` to
/// get a version that can evaluate routing rules efficiently.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[non_exhaustive]
pub struct FrontingSet {
    /// Personas that are currently fronting (may be more than one for
    /// co-fronting configurations).
    pub active: Vec<PersonaId>,
    /// Default delivery target when no routing rule matches and co-fronting
    /// fan-out is undesirable.
    pub fallback: Option<PersonaId>,
    /// Routing rules applied when neither direct addressing nor fallback
    /// applies.
    pub routing: RoutingTable,
}

// ── RoutingTable ──────────────────────────────────────────────────────────────

/// A set of routing rules and their compiled regex cache.
///
/// Built via `RoutingTable::try_from_rules` to guarantee regex compilation
/// succeeds before the table enters service. The compiled-regex cache is
/// serde-skipped; it is rebuilt from the rule source strings on
/// deserialization by calling `RoutingTable::compile` explicitly (or by
/// going through `try_from_rules` again).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingTable {
    /// The source rules in their original order. Evaluated in
    /// descending priority order by `FrontingResolver`.
    pub rules: Vec<RoutingRule>,

    /// Compiled regexes for rules whose pattern is `MessagePattern::Regex`.
    ///
    /// Indexed by position in `rules` — entries for non-regex patterns are
    /// `None`. Serde-skipped; callers must call `compile()` after
    /// deserialization (the `try_from_rules` constructor does this
    /// automatically).
    #[serde(skip)]
    compiled: Vec<Option<Regex>>,
}

impl RoutingTable {
    /// Construct a `RoutingTable` from a list of rules, compiling any
    /// `MessagePattern::Regex` variants.
    ///
    /// Returns `Err(FrontingLoadError::InvalidRegex)` if any regex pattern
    /// fails to compile.
    pub fn try_from_rules(rules: Vec<RoutingRule>) -> Result<Self, FrontingLoadError> {
        let mut table = Self {
            rules,
            compiled: Vec::new(),
        };
        table.compile()?;
        Ok(table)
    }

    /// (Re-)compile all `MessagePattern::Regex` patterns.
    ///
    /// Called automatically by `try_from_rules`. Must be called manually after
    /// serde-deserialization if the caller wants hot-path evaluation.
    pub fn compile(&mut self) -> Result<(), FrontingLoadError> {
        self.compiled = self
            .rules
            .iter()
            .map(|rule| match &rule.pattern {
                MessagePattern::Regex(src) => {
                    let re = Regex::new(src).map_err(|e| FrontingLoadError::InvalidRegex {
                        rule_id: rule.id.clone(),
                        source: src.clone(),
                        inner: e,
                    })?;
                    Ok(Some(re))
                }
                _ => Ok(None),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(())
    }

    /// Evaluate all rules against `msg_body` in descending priority order.
    ///
    /// Returns the `(rule_id, target)` pair for the first matching rule, or
    /// `None` if no rule matches.
    ///
    /// Callers must have called `compile()` or used `try_from_rules` for
    /// `Regex` patterns to be evaluated; without compiled regexes, `Regex`
    /// patterns silently fail to match.
    pub fn first_match(&self, msg_body: &str) -> Option<(&str, &PersonaId)> {
        // Collect indices sorted by priority descending, then iterate.
        let mut indices: Vec<usize> = (0..self.rules.len()).collect();
        indices.sort_by(|&a, &b| self.rules[b].priority.cmp(&self.rules[a].priority));

        for idx in indices {
            let rule = &self.rules[idx];
            let compiled_re = self.compiled.get(idx).and_then(|o| o.as_ref());

            if rule.pattern.matches(msg_body, compiled_re) {
                return Some((&rule.id, &rule.target));
            }
        }
        None
    }
}

// ── RoutingRule ────────────────────────────────────────────────────────────────

/// A single routing rule: if `pattern` matches, deliver to `target`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RoutingRule {
    /// Stable identifier for this rule. Used in `ResolveOutcome::Rule` and
    /// in error reporting.
    pub id: String,
    /// The message content pattern to match.
    pub pattern: MessagePattern,
    /// Delivery target when the pattern matches.
    pub target: PersonaId,
    /// Priority: higher values are evaluated before lower values.
    pub priority: u32,
}

impl RoutingRule {
    /// Construct a routing rule with the given fields.
    ///
    /// Required because `RoutingRule` is `#[non_exhaustive]` — external crates
    /// cannot use struct literal syntax.
    pub fn new(
        id: impl Into<String>,
        pattern: MessagePattern,
        target: impl Into<PersonaId>,
        priority: u32,
    ) -> Self {
        Self {
            id: id.into(),
            pattern,
            target: target.into(),
            priority,
        }
    }
}

// ── FrontingSet constructors ──────────────────────────────────────────────────

impl FrontingSet {
    /// Construct a `FrontingSet` with the given active personas, fallback, and
    /// routing table.
    ///
    /// Required because `FrontingSet` is `#[non_exhaustive]`.
    pub fn from_parts(
        active: Vec<PersonaId>,
        fallback: Option<PersonaId>,
        routing: RoutingTable,
    ) -> Self {
        Self {
            active,
            fallback,
            routing,
        }
    }
}

// ── MessagePattern ────────────────────────────────────────────────────────────

/// The matching criterion for a routing rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MessagePattern {
    /// Matches when the message body starts with the given string.
    Prefix(String),
    /// Matches when the message body contains the given string.
    Contains(String),
    /// Matches when the body contains the hashtag `#<tag>` at a word boundary.
    TopicTag(String),
    /// Matches when the compiled regex is found in the message body.
    ///
    /// The source string is stored for serialization; the compiled form is
    /// cached in `RoutingTable.compiled` (serde-skipped).
    Regex(String),
}

impl MessagePattern {
    /// Returns `true` if this pattern matches `msg_body`.
    ///
    /// `compiled_re` must be `Some` for `Regex` patterns and is ignored for
    /// all other variants.
    fn matches(&self, msg_body: &str, compiled_re: Option<&Regex>) -> bool {
        match self {
            Self::Prefix(s) => msg_body.starts_with(s.as_str()),
            Self::Contains(s) => msg_body.contains(s.as_str()),
            Self::TopicTag(tag) => {
                // Match `#<tag>` with non-alphanumeric (or string boundary) on each side.
                // Uses a compiled-on-the-fly regex for correctness. The regex is NOT
                // cached here because TopicTag patterns don't participate in the
                // RoutingTable compiled cache — only MessagePattern::Regex does. For the
                // small number of TopicTag rules expected in practice, the compile cost
                // is negligible.
                let pattern = format!(r"(^|\W)#{}(\W|$)", regex::escape(tag));
                Regex::new(&pattern)
                    .map(|re| re.is_match(msg_body))
                    .unwrap_or(false)
            }
            Self::Regex(_) => compiled_re.map(|re| re.is_match(msg_body)).unwrap_or(false),
        }
    }
}

// ── FrontingLoadError ─────────────────────────────────────────────────────────

/// Errors produced when constructing a `RoutingTable` or `FrontingResolver`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FrontingLoadError {
    /// A `MessagePattern::Regex` rule contains a pattern that does not compile.
    #[error("invalid regex in rule '{rule_id}' (pattern: {source:?}): {inner}")]
    InvalidRegex {
        rule_id: String,
        source: String,
        #[source]
        inner: regex::Error,
    },
}

// ── ResolveOutcome ────────────────────────────────────────────────────────────

/// The result of `FrontingResolver::resolve`.
///
/// `PersonaId` is a `SmolStr` alias — cheap to clone (inlines ≤22 bytes, Arc
/// for longer strings), so each variant owns its ids rather than borrowing.
#[derive(Debug, Clone)]
pub enum ResolveOutcome {
    /// The message addressed a persona directly via `@persona-id` prefix.
    Direct(PersonaId),
    /// A routing rule matched.
    Rule { rule_id: String, target: PersonaId },
    /// No rule matched; the configured fallback persona receives the message.
    Fallback(PersonaId),
    /// No fallback is configured; all active personas receive a copy.
    FanOut(Vec<PersonaId>),
    /// The fronting set is empty; the registry's first Active persona (sorted
    /// by id) receives the message.
    DefaultPersona(PersonaId),
    /// No Active persona exists anywhere in the registry; the message is acked
    /// by the system-default path.
    SystemDefault,
}

// ── FrontingResolver ─────────────────────────────────────────────────────────

/// Combines a `FrontingSet` and a `ConstellationRegistry` to resolve incoming
/// messages to delivery targets.
///
/// The `set` field holds serializable configuration (routing rules, active
/// personas, fallback). The `registry` is queried only for the empty-fronting
/// default-persona fallback path.
pub struct FrontingResolver {
    pub set: FrontingSet,
    pub registry: Arc<dyn ConstellationRegistry>,
}

impl FrontingResolver {
    /// Construct a resolver from a fronting set and a registry.
    pub fn new(set: FrontingSet, registry: Arc<dyn ConstellationRegistry>) -> Self {
        Self { set, registry }
    }

    /// Resolve `msg_body` to one or more delivery targets.
    ///
    /// Async because the default-persona fallback path consults the registry.
    /// All other paths are synchronous (rule evaluation, direct address parsing).
    ///
    /// # Decision sequence
    ///
    /// 1. `@persona-id` prefix → `Direct`.
    /// 2. Highest-priority matching routing rule → `Rule`.
    /// 3. Fallback persona configured → `Fallback`.
    /// 4. Active set non-empty → `FanOut` over all active personas.
    /// 5. Registry has Active personas → `DefaultPersona` (lowest id).
    /// 6. Registry has no Active personas → `SystemDefault`.
    pub async fn resolve(&self, msg_body: &str) -> ResolveOutcome {
        // Step 1: direct address.
        if let Some(id) = parse_direct_address(msg_body) {
            return ResolveOutcome::Direct(id);
        }

        // Step 2: routing rules.
        if let Some((rule_id, target)) = self.set.routing.first_match(msg_body) {
            return ResolveOutcome::Rule {
                rule_id: rule_id.to_owned(),
                target: target.clone(),
            };
        }

        // Step 3: fallback.
        if let Some(fb) = &self.set.fallback {
            return ResolveOutcome::Fallback(fb.clone());
        }

        // Step 4: fan-out over active personas.
        if !self.set.active.is_empty() {
            return ResolveOutcome::FanOut(self.set.active.clone());
        }

        // Step 5: empty fronting set — consult registry.
        match self.registry.list(RegistryScope::All).await {
            Ok(personas) => {
                let mut active: Vec<_> = personas
                    .into_iter()
                    .filter(|p| p.status == PersonaStatus::Active)
                    .collect();

                if active.is_empty() {
                    return ResolveOutcome::SystemDefault;
                }

                // Determinism: sort by id (SmolStr → lexicographic).
                active.sort_by(|a, b| a.id.cmp(&b.id));
                ResolveOutcome::DefaultPersona(active.remove(0).id)
            }
            // If the registry is unavailable, fall through to SystemDefault
            // rather than crashing — message delivery must never fail-close.
            Err(e) => {
                tracing::warn!(
                    target = "pattern_core::fronting",
                    error = ?e,
                    "ConstellationRegistry::list failed during default-persona fallback; \
                     using SystemDefault outcome"
                );
                ResolveOutcome::SystemDefault
            }
        }
    }
}

// ── parse_direct_address ──────────────────────────────────────────────────────

/// Scan `msg_body` for a `@<persona-id>` direct-address token.
///
/// Returns the first match's `PersonaId`. The body is NOT modified — agents
/// receive the @-mention verbatim, just as in normal chat conventions.
///
/// # Matching rules
///
/// 1. The `@` must be at start-of-string or immediately preceded by whitespace
///    (so email addresses like `me@example.com` do not match).
/// 2. The id starts at the character after `@` and ends at the first
///    whitespace, `:`, or end-of-string.
/// 3. A `.` inside the would-be id is treated as a domain marker if it is
///    followed by a non-whitespace character — the token is rejected. A `.`
///    followed by whitespace or end-of-string is treated as sentence-ending
///    and terminates the id (the `.` itself is excluded).
/// 4. Empty ids (`@` followed immediately by whitespace, `:`, end, or a
///    rejecting `.`) do not match.
/// 5. The first valid match in the body wins; rejected `@` tokens cause the
///    scan to advance and look for the next candidate.
///
/// | Input                            | Result                  |
/// |----------------------------------|-------------------------|
/// | `"@alice"`                       | `Some("alice")`         |
/// | `"@alice: msg"`                  | `Some("alice")`         |
/// | `"hello @alice"`                 | `Some("alice")`         |
/// | `"@alice and @bob"`              | `Some("alice")`         |
/// | `"@pattern"`                     | `Some("pattern")`       |
/// | `"@pattern. trailing"`           | `Some("pattern")`       |
/// | `"@pattern.atproto.systems"`     | `None` (domain pattern) |
/// | `"contact me@example.com"`       | `None` (not preceded by ws) |
/// | `"@"`                            | `None` (empty id)       |
/// | `""`                             | `None`                  |
pub fn parse_direct_address(msg_body: &str) -> Option<PersonaId> {
    let chars: Vec<(usize, char)> = msg_body.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].1 != '@' {
            i += 1;
            continue;
        }
        let preceded_ok = i == 0 || chars[i - 1].1.is_whitespace();
        if !preceded_ok {
            i += 1;
            continue;
        }
        let mut end = i + 1;
        let mut rejected = false;
        while end < chars.len() {
            let c = chars[end].1;
            if c.is_whitespace() || c == ':' {
                break;
            }
            if c == '.' {
                let next = chars.get(end + 1).map(|p| p.1);
                match next {
                    None => break,
                    Some(nc) if nc.is_whitespace() => break,
                    _ => {
                        rejected = true;
                        break;
                    }
                }
            }
            end += 1;
        }
        if !rejected && end > i + 1 {
            let start_byte = chars[i + 1].0;
            let end_byte = chars.get(end).map(|p| p.0).unwrap_or(msg_body.len());
            return Some(PersonaId::new(&msg_body[start_byte..end_byte]));
        }
        i += 1;
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::constellation::{
        ConstellationRegistry, PersonaRecord, PersonaStatus, RegistryError, RegistryScope,
    };

    // ── Minimal test registry ─────────────────────────────────────────────────

    /// A minimal test registry backed by a HashMap.
    /// The full `InMemoryConstellationRegistry` lives in `pattern_runtime::testing`.
    #[derive(Debug)]
    struct TestRegistry {
        records: Mutex<HashMap<PersonaId, PersonaRecord>>,
    }

    impl TestRegistry {
        fn new() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
            }
        }

        fn seed(&self, record: PersonaRecord) {
            self.records
                .lock()
                .unwrap()
                .insert(record.id.clone(), record);
        }
    }

    #[async_trait]
    impl ConstellationRegistry for TestRegistry {
        async fn list(&self, scope: RegistryScope) -> Result<Vec<PersonaRecord>, RegistryError> {
            let records = self.records.lock().unwrap();
            let filtered: Vec<_> = match &scope {
                RegistryScope::All => records.values().cloned().collect(),
                RegistryScope::Project(p) => records
                    .values()
                    .filter(|r| r.project_attachments.contains(p))
                    .cloned()
                    .collect(),
            };
            Ok(filtered)
        }

        async fn get(&self, id: &PersonaId) -> Result<Option<PersonaRecord>, RegistryError> {
            Ok(self.records.lock().unwrap().get(id).cloned())
        }

        // Phase 6 methods: not exercised by these fronting tests; stub to
        // BackendUnavailable so the tests fail loudly if they ever call them.
        async fn find(
            &self,
            _project: Option<&std::path::Path>,
            _kind: Option<crate::spawn::RelationshipKind>,
        ) -> Result<Vec<PersonaRecord>, RegistryError> {
            Err(RegistryError::BackendUnavailable)
        }
        async fn register(&self, _record: PersonaRecord) -> Result<(), RegistryError> {
            Err(RegistryError::BackendUnavailable)
        }
        async fn set_status(
            &self,
            _id: &PersonaId,
            _status: PersonaStatus,
        ) -> Result<(), RegistryError> {
            Err(RegistryError::BackendUnavailable)
        }
        async fn add_relationship(
            &self,
            _edge: crate::constellation::RelationshipSpec,
        ) -> Result<(), RegistryError> {
            Err(RegistryError::BackendUnavailable)
        }
        async fn groups(
            &self,
            _scope: RegistryScope,
        ) -> Result<Vec<crate::constellation::PersonaGroup>, RegistryError> {
            Err(RegistryError::BackendUnavailable)
        }
        async fn create_group(
            &self,
            _name: String,
            _project_id: Option<String>,
        ) -> Result<crate::constellation::PersonaGroup, RegistryError> {
            Err(RegistryError::BackendUnavailable)
        }
    }

    fn active_record(id: &str) -> PersonaRecord {
        PersonaRecord::new(id, format!("{id} name"), PersonaStatus::Active)
    }

    fn draft_record(id: &str) -> PersonaRecord {
        PersonaRecord::new(id, format!("{id} name"), PersonaStatus::Draft)
    }

    fn make_resolver(
        set: FrontingSet,
        registry: Arc<dyn ConstellationRegistry>,
    ) -> FrontingResolver {
        FrontingResolver::new(set, registry)
    }

    // ── parse_direct_address ──────────────────────────────────────────────────

    #[test]
    fn parse_direct_address_bare_at_name() {
        let id = parse_direct_address("@alice").expect("should parse");
        assert_eq!(id.as_str(), "alice");
    }

    #[test]
    fn parse_direct_address_with_colon_separator() {
        let id = parse_direct_address("@alice: hello there").expect("should parse");
        assert_eq!(id.as_str(), "alice");
    }

    #[test]
    fn parse_direct_address_with_space_separator() {
        let id = parse_direct_address("@alice hello").expect("should parse");
        assert_eq!(id.as_str(), "alice");
    }

    #[test]
    fn parse_direct_address_mid_message_with_leading_text() {
        let id = parse_direct_address("hello @alice").expect("should parse mid-message");
        assert_eq!(id.as_str(), "alice");
    }

    #[test]
    fn parse_direct_address_first_match_wins() {
        let id = parse_direct_address("@alice and @bob").expect("should parse");
        assert_eq!(id.as_str(), "alice");
    }

    #[test]
    fn parse_direct_address_email_does_not_match() {
        assert!(
            parse_direct_address("contact me@example.com please").is_none(),
            "email-style @ (not preceded by whitespace) must not match"
        );
    }

    #[test]
    fn parse_direct_address_domain_like_rejected() {
        assert!(
            parse_direct_address("@pattern.atproto.systems").is_none(),
            "domain-like @ (period followed by non-whitespace) must not match"
        );
    }

    #[test]
    fn parse_direct_address_period_at_end_terminates_id() {
        let id = parse_direct_address("@pattern.").expect("should parse");
        assert_eq!(id.as_str(), "pattern");
    }

    #[test]
    fn parse_direct_address_period_then_space_terminates_id() {
        let id = parse_direct_address("@pattern. continue").expect("should parse");
        assert_eq!(id.as_str(), "pattern");
    }

    #[test]
    fn parse_direct_address_skips_domain_finds_next() {
        // First @ is domain-like and rejected; second @ is a real address.
        let id = parse_direct_address("see admin@host.example then @alice")
            .expect("should fall through to second @");
        assert_eq!(id.as_str(), "alice");
    }

    #[test]
    fn parse_direct_address_bare_at_is_none() {
        assert!(parse_direct_address("@").is_none());
    }

    #[test]
    fn parse_direct_address_empty_string() {
        assert!(parse_direct_address("").is_none());
    }

    #[test]
    fn parse_direct_address_with_hyphenated_id() {
        let id = parse_direct_address("@math-specialist: solve this").expect("should parse");
        assert_eq!(id.as_str(), "math-specialist");
    }

    // ── Direct addressing wins over routing rules ─────────────────────────────

    #[tokio::test]
    async fn direct_address_wins_over_matching_rule() {
        let registry = Arc::new(TestRegistry::new());
        registry.seed(active_record("alice"));
        registry.seed(active_record("bob"));

        let rule = RoutingRule {
            id: "always-bob".to_string(),
            pattern: MessagePattern::Contains("hello".to_string()),
            target: "bob".into(),
            priority: 100,
        };

        let set = FrontingSet {
            active: vec!["bob".into()],
            fallback: None,
            routing: RoutingTable::try_from_rules(vec![rule]).unwrap(),
        };

        let resolver = make_resolver(set, registry);
        // This message has a prefix rule match AND a direct address.
        let outcome = resolver.resolve("@alice: hello").await;
        assert!(
            matches!(outcome, ResolveOutcome::Direct(id) if id.as_str() == "alice"),
            "direct address must win over matching rule"
        );
    }

    // ── Highest priority rule wins ────────────────────────────────────────────

    #[tokio::test]
    async fn highest_priority_rule_wins() {
        let registry = Arc::new(TestRegistry::new());

        let rules = vec![
            RoutingRule {
                id: "low".to_string(),
                pattern: MessagePattern::Contains("hello".to_string()),
                target: "low-target".into(),
                priority: 1,
            },
            RoutingRule {
                id: "high".to_string(),
                pattern: MessagePattern::Contains("hello".to_string()),
                target: "high-target".into(),
                priority: 10,
            },
            RoutingRule {
                id: "mid".to_string(),
                pattern: MessagePattern::Contains("hello".to_string()),
                target: "mid-target".into(),
                priority: 5,
            },
        ];

        let set = FrontingSet {
            active: Vec::new(),
            fallback: None,
            routing: RoutingTable::try_from_rules(rules).unwrap(),
        };

        let resolver = make_resolver(set, registry);
        let outcome = resolver.resolve("say hello").await;
        match outcome {
            ResolveOutcome::Rule { rule_id, target } => {
                assert_eq!(rule_id, "high", "highest priority rule must match first");
                assert_eq!(target.as_str(), "high-target");
            }
            other => panic!("expected Rule outcome, got {other:?}"),
        }
    }

    // ── Co-fronting fan-out ───────────────────────────────────────────────────

    #[tokio::test]
    async fn co_fronting_fan_out_when_no_fallback() {
        let registry = Arc::new(TestRegistry::new());

        let set = FrontingSet {
            active: vec!["alice".into(), "bob".into()],
            fallback: None,
            routing: RoutingTable::default(),
        };

        let resolver = make_resolver(set, registry);
        let outcome = resolver.resolve("unrouted message").await;
        match outcome {
            ResolveOutcome::FanOut(ids) => {
                assert_eq!(ids.len(), 2);
                assert!(ids.iter().any(|id| id.as_str() == "alice"));
                assert!(ids.iter().any(|id| id.as_str() == "bob"));
            }
            other => panic!("expected FanOut outcome, got {other:?}"),
        }
    }

    // ── Fallback used over fan-out when both applicable ───────────────────────

    #[tokio::test]
    async fn fallback_used_when_configured() {
        let registry = Arc::new(TestRegistry::new());

        let set = FrontingSet {
            active: vec!["alice".into(), "bob".into()],
            fallback: Some("alice".into()),
            routing: RoutingTable::default(),
        };

        let resolver = make_resolver(set, registry);
        let outcome = resolver.resolve("unrouted message").await;
        match outcome {
            ResolveOutcome::Fallback(id) => assert_eq!(id.as_str(), "alice"),
            other => panic!("expected Fallback outcome, got {other:?}"),
        }
    }

    // ── Empty fronting + Active personas → DefaultPersona (lowest id) ─────────

    #[tokio::test]
    async fn empty_fronting_returns_default_persona_lowest_id() {
        let registry = Arc::new(TestRegistry::new());
        // Seed three active personas. "aardvark" must win (lexicographically lowest).
        registry.seed(active_record("zebra"));
        registry.seed(active_record("monkey"));
        registry.seed(active_record("aardvark"));
        // Draft should not be selected.
        registry.seed(draft_record("aaa-draft"));

        let set = FrontingSet::default();
        let resolver = make_resolver(set, registry);
        let outcome = resolver.resolve("any message").await;
        match outcome {
            ResolveOutcome::DefaultPersona(id) => {
                assert_eq!(
                    id.as_str(),
                    "aardvark",
                    "must select the lexicographically lowest Active persona"
                );
            }
            other => panic!("expected DefaultPersona outcome, got {other:?}"),
        }
    }

    // ── Empty fronting + zero Active personas → SystemDefault ─────────────────

    #[tokio::test]
    async fn empty_fronting_no_active_personas_system_default() {
        let registry = Arc::new(TestRegistry::new());
        registry.seed(draft_record("pending-setup"));

        let set = FrontingSet::default();
        let resolver = make_resolver(set, registry);
        let outcome = resolver.resolve("hello").await;
        assert!(
            matches!(outcome, ResolveOutcome::SystemDefault),
            "must resolve to SystemDefault when no Active personas exist"
        );
    }

    #[tokio::test]
    async fn completely_empty_registry_gives_system_default() {
        let registry = Arc::new(TestRegistry::new());
        let set = FrontingSet::default();
        let resolver = make_resolver(set, registry);
        let outcome = resolver.resolve("hello").await;
        assert!(
            matches!(outcome, ResolveOutcome::SystemDefault),
            "must resolve to SystemDefault for empty registry"
        );
    }

    // ── MessagePattern matching ───────────────────────────────────────────────

    #[test]
    fn message_pattern_prefix_matches() {
        let p = MessagePattern::Prefix("!math".to_string());
        assert!(p.matches("!math 2+2", None));
        assert!(!p.matches("do !math", None));
        assert!(!p.matches("math", None));
    }

    #[test]
    fn message_pattern_contains_matches() {
        let p = MessagePattern::Contains("hello".to_string());
        assert!(p.matches("say hello please", None));
        assert!(p.matches("hello", None));
        assert!(!p.matches("goodbye", None));
    }

    #[test]
    fn message_pattern_topic_tag_matches() {
        let p = MessagePattern::TopicTag("rust".to_string());
        // Word-boundary on both sides.
        assert!(p.matches("#rust is great", None));
        assert!(p.matches("I like #rust", None));
        assert!(p.matches("#rust", None));
        assert!(p.matches("topics: #rust, #cargo", None));
        // Must NOT match if it's embedded in another word.
        assert!(!p.matches("#rusty", None));
        assert!(!p.matches("outrust", None));
    }

    #[test]
    fn message_pattern_regex_matches() {
        let re = Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap();
        let p = MessagePattern::Regex(r"\d{4}-\d{2}-\d{2}".to_string());
        assert!(p.matches("Date: 2026-04-25", Some(&re)));
        assert!(!p.matches("No date here", Some(&re)));
    }

    // ── RoutingTable compilation failure ─────────────────────────────────────

    #[test]
    fn routing_table_invalid_regex_fails_with_clear_error() {
        let rules = vec![RoutingRule {
            id: "bad-rule".to_string(),
            pattern: MessagePattern::Regex("[invalid regex".to_string()),
            target: "target".into(),
            priority: 1,
        }];

        let err = RoutingTable::try_from_rules(rules).unwrap_err();
        match err {
            FrontingLoadError::InvalidRegex {
                rule_id, source, ..
            } => {
                assert_eq!(rule_id, "bad-rule");
                assert_eq!(source, "[invalid regex");
            }
        }
    }

    #[test]
    fn routing_table_valid_regex_compiles() {
        let rules = vec![RoutingRule {
            id: "date-rule".to_string(),
            pattern: MessagePattern::Regex(r"\d{4}-\d{2}-\d{2}".to_string()),
            target: "date-handler".into(),
            priority: 1,
        }];

        let table = RoutingTable::try_from_rules(rules).expect("valid regex must compile");
        assert_eq!(table.rules.len(), 1);
    }

    // ── FrontingSet serde round-trip ──────────────────────────────────────────

    #[test]
    fn fronting_set_serde_round_trip() {
        let set = FrontingSet {
            active: vec!["alice".into(), "bob".into()],
            fallback: Some("alice".into()),
            routing: RoutingTable::try_from_rules(vec![
                RoutingRule {
                    id: "rule-1".to_string(),
                    pattern: MessagePattern::Prefix("!cmd".to_string()),
                    target: "cmd-handler".into(),
                    priority: 10,
                },
                RoutingRule {
                    id: "rule-2".to_string(),
                    pattern: MessagePattern::Contains("help".to_string()),
                    target: "support".into(),
                    priority: 5,
                },
            ])
            .unwrap(),
        };

        let json = serde_json::to_string(&set).expect("serialize");
        let mut recovered: FrontingSet = serde_json::from_str(&json).expect("deserialize");

        // Compiled cache is serde-skipped; re-compile after deserialization.
        recovered.routing.compile().expect("recompile must succeed");

        assert_eq!(recovered.active.len(), 2);
        assert_eq!(recovered.fallback.as_deref(), Some("alice"));
        assert_eq!(recovered.routing.rules.len(), 2);
        assert_eq!(recovered.routing.rules[0].id, "rule-1");
    }

    #[test]
    fn fronting_set_default_is_empty() {
        let set = FrontingSet::default();
        assert!(set.active.is_empty());
        assert!(set.fallback.is_none());
        assert!(set.routing.rules.is_empty());
    }

    // ── Routing regex variant round-trip via FrontingSet ─────────────────────

    #[test]
    fn fronting_set_with_regex_rule_round_trip() {
        let set = FrontingSet {
            active: vec!["alice".into()],
            fallback: None,
            routing: RoutingTable::try_from_rules(vec![RoutingRule {
                id: "date-route".to_string(),
                pattern: MessagePattern::Regex(r"\d{4}-\d{2}-\d{2}".to_string()),
                target: "date-handler".into(),
                priority: 5,
            }])
            .unwrap(),
        };

        let json = serde_json::to_string(&set).expect("serialize");
        let mut recovered: FrontingSet = serde_json::from_str(&json).expect("deserialize");
        recovered.routing.compile().expect("must compile");

        // Verify the rule re-compiles and functions correctly.
        let m = recovered.routing.first_match("deadline: 2026-04-25");
        assert!(m.is_some(), "regex rule must match after re-compile");
        assert_eq!(m.unwrap().1.as_str(), "date-handler");
    }
}

// ── proptest serde round-trip ─────────────────────────────────────────────────

#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn fronting_set_proptest_round_trip(
            active_count in 0usize..=4,
            has_fallback in proptest::bool::ANY,
            rule_count in 0usize..=3,
        ) {
            let active: Vec<PersonaId> = (0..active_count)
                .map(|i| PersonaId::new(format!("persona-{i}")))
                .collect();

            let fallback = if has_fallback && !active.is_empty() {
                Some(active[0].clone())
            } else {
                None
            };

            // Only use non-Regex patterns to avoid the need for compilation
            // in the round-trip check (deserialized form has empty cache).
            let rules: Vec<RoutingRule> = (0..rule_count)
                .map(|i| RoutingRule {
                    id: format!("rule-{i}"),
                    pattern: if i % 2 == 0 {
                        MessagePattern::Prefix(format!("!cmd{i}"))
                    } else {
                        MessagePattern::Contains(format!("keyword{i}"))
                    },
                    target: PersonaId::new(format!("target-{i}")),
                    priority: i as u32,
                })
                .collect();

            let set = FrontingSet {
                active: active.clone(),
                fallback: fallback.clone(),
                routing: RoutingTable::try_from_rules(rules).unwrap(),
            };

            let json = serde_json::to_string(&set).expect("serialize");
            let recovered: FrontingSet = serde_json::from_str(&json).expect("deserialize");

            prop_assert_eq!(recovered.active, active);
            prop_assert_eq!(recovered.fallback, fallback);
        }
    }
}
