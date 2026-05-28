# Pattern Dialect - Implementation Notes

## Extensibility Architecture

### Verb Registration Trait

```rust
pub trait DialectVerb: Send + Sync {
    /// Canonical name
    fn name(&self) -> &str;
    
    /// Aliases that map to this verb
    fn aliases(&self) -> &[&str];
    
    /// How strict should fuzzy matching be
    fn strictness(&self) -> Strictness { Strictness::Normal }
    
    /// Parse arguments into structured intent
    fn parse_args(&self, args: &str, ctx: &ParseContext) -> Result<Intent, ParseHint>;
    
    /// Execute the intent (or delegate to existing tool)
    async fn execute(&self, intent: Intent, meta: &ExecutionMeta) -> Result<ActionResult>;
    
    /// Short description for agent instructions
    fn description(&self) -> &str;
    
    /// Examples for few-shot prompting
    fn examples(&self) -> Vec<VerbExample>;
    
    /// Required authority level (if any)
    fn authority_required(&self) -> Option<AuthorityLevel> { None }
}

pub enum Strictness {
    /// Loose matching - maximize accessibility (recall, search)
    Loose,
    /// Normal matching - reasonable typo tolerance
    Normal,
    /// Strict matching - avoid accidental triggers (approve, deny, halt)
    Strict,
}

/// Hint for recoverable parse failures
pub enum ParseHint {
    Ambiguous { options: Vec<String>, question: String },
    MissingArg { name: String, suggestion: Option<String> },
    UnknownModifier { got: String, similar: Vec<String> },
}
```

### Config-Driven Extensions

```toml
# pattern.toml or agents/my_agent.toml

# Extend existing verbs with more aliases
[dialect.verbs.recall]
extra_aliases = ["memorize", "jot down", "note to self"]

# Create shorthand verbs that delegate to existing tools
[dialect.verbs.notify]
aliases = ["alert", "ping", "heads up"]
maps_to = "send_message"
default_target = { type = "agent", id = "anchor" }
description = "Quick notification to the coordinator"
examples = [
    "/notify something weird happened",
    "/heads up partner seems stressed",
]

# Platform-specific shortcuts
[dialect.verbs.toot]
aliases = ["mastodon"]
maps_to = "send"
default_target = { type = "mastodon" }
description = "Post to Mastodon"

# Agent-specific custom verbs
[dialect.verbs.shame]
aliases = ["call out", "roast"]
maps_to = "send"
default_target = { type = "bluesky" }
template = "shame post template: {content}"
requires_permission = true
```

### Intent Types

```rust
pub enum Intent {
    Recall(RecallIntent),
    Context(ContextIntent),
    Search(SearchIntent),
    Send(SendIntent),
    Fetch(FetchIntent),
    Web(WebIntent),
    Calc(CalcIntent),
    Authority(AuthorityIntent),
    Custom(CustomIntent),  // for config-defined verbs
}

pub enum RecallIntent {
    Read { label: String },
    Search { query: String },
    Insert { label: Option<String>, content: String },
    Append { label: String, content: String },
    Delete { label: String },
    Patch { label: String, patch: DiffPatch },
}

pub struct CustomIntent {
    pub verb: String,
    pub maps_to: String,
    pub args: serde_json::Value,
    pub template_applied: Option<String>,
}
```

### Wiring to Existing Tools

```rust
impl DialectVerb for RecallVerb {
    async fn execute(&self, intent: Intent, meta: &ExecutionMeta) -> Result<ActionResult> {
        let Intent::Recall(recall_intent) = intent else { 
            unreachable!() 
        };
        
        // Convert to existing tool input
        let tool_input = match recall_intent {
            RecallIntent::Read { label } => RecallInput {
                operation: ArchivalMemoryOperationType::Read,
                label: Some(label),
                content: None,
            },
            RecallIntent::Search { query } => {
                // This maps to SearchTool instead
                return self.handle.search_archival(&query, 20, false).await
                    .map(ActionResult::from);
            },
            RecallIntent::Insert { label, content } => RecallInput {
                operation: ArchivalMemoryOperationType::Insert,
                label,
                content: Some(content),
            },
            RecallIntent::Append { label, content } => RecallInput {
                operation: ArchivalMemoryOperationType::Append,
                label: Some(label),
                content: Some(content),
            },
            RecallIntent::Delete { label } => RecallInput {
                operation: ArchivalMemoryOperationType::Delete,
                label: Some(label),
                content: None,
            },
            RecallIntent::Patch { label, patch } => {
                // Load, apply patch, save
                let current = self.handle.get_archival_memory_by_label(&label).await?;
                let patched = patch.apply(&current.value)?;
                // Update via replace or direct write
                return self.handle.update_archival_memory(&label, &patched).await
                    .map(ActionResult::from);
            }
        };
        
        self.recall_tool.execute(tool_input, meta).await
            .map(ActionResult::from)
    }
}
```

---

## Fuzzy Verb Matching

### Matching Tiers

The matcher tries these in order, returning the first confident match:

```
1. Exact match (canonical or alias)
2. Case-insensitive exact match
3. Morphological variants (plurals, tenses, spellings)
4. Levenshtein distance (typos)
5. Phonetic similarity (soundex/metaphone)
6. [Optional] Semantic similarity (embeddings)
```

### Tier 1-2: Exact Matching

```rust
fn exact_match(input: &str, verbs: &[VerbSpec]) -> Option<&VerbSpec> {
    let input_lower = input.to_lowercase();
    
    for verb in verbs {
        if verb.canonical == input || verb.canonical.eq_ignore_ascii_case(input) {
            return Some(verb);
        }
        for alias in &verb.aliases {
            if *alias == input || alias.eq_ignore_ascii_case(input) {
                return Some(verb);
            }
        }
    }
    None
}
```

### Tier 3: Morphological Variants

Handle common English variations without needing explicit aliases:

```rust
pub struct MorphologicalMatcher {
    // Irregular forms we need to know about
    irregulars: HashMap<&'static str, &'static str>,
}

impl MorphologicalMatcher {
    pub fn new() -> Self {
        let mut irregulars = HashMap::new();
        // Irregular verbs relevant to our domain
        irregulars.insert("sent", "send");
        irregulars.insert("told", "tell");
        irregulars.insert("found", "find");
        irregulars.insert("forgot", "forget");
        irregulars.insert("wrote", "write");
        irregulars.insert("thought", "think");
        Self { irregulars }
    }
    
    pub fn normalize(&self, word: &str) -> String {
        let word = word.to_lowercase();
        
        // Check irregulars first
        if let Some(base) = self.irregulars.get(word.as_str()) {
            return base.to_string();
        }
        
        // Regular patterns
        let normalized = word
            // -ing: searching -> search
            .strip_suffix("ing")
            .map(|s| {
                // Handle doubling: stopping -> stop
                if s.len() > 2 && s.chars().last() == s.chars().nth(s.len() - 2) {
                    &s[..s.len()-1]
                } else if s.ends_with("e") {
                    // making -> make (we stripped 'ing', add back 'e')
                    return format!("{}e", s);
                } else {
                    s
                }
            })
            // -ed: searched -> search
            .or_else(|| word.strip_suffix("ed").map(|s| {
                if s.ends_with("i") {
                    // tried -> try
                    format!("{}y", &s[..s.len()-1])
                } else {
                    s.to_string()
                }
            }))
            // -s/-es: searches -> search
            .or_else(|| word.strip_suffix("es").map(|s| s.to_string()))
            .or_else(|| word.strip_suffix("s").map(|s| s.to_string()))
            // -ies: queries -> query
            .or_else(|| word.strip_suffix("ies").map(|s| format!("{}y", s)));
        
        normalized.unwrap_or(word)
    }
    
    pub fn spelling_variants(&self, word: &str) -> Vec<String> {
        let mut variants = vec![word.to_string()];
        
        // British/American spelling
        if word.contains("ise") {
            variants.push(word.replace("ise", "ize"));
        }
        if word.contains("ize") {
            variants.push(word.replace("ize", "ise"));
        }
        if word.contains("our") {
            variants.push(word.replace("our", "or"));
        }
        if word.contains("or") && !word.contains("our") {
            // Be careful not to create nonsense
            let replaced = word.replace("or", "our");
            if replaced != word {
                variants.push(replaced);
            }
        }
        
        // Common misspellings
        if word.contains("mem") {
            variants.push(word.replace("mem", "rem")); // remember/memory confusion
        }
        
        variants
    }
}
```

### Tier 4: Levenshtein Distance

```rust
pub fn levenshtein_match(
    input: &str, 
    verbs: &[VerbSpec],
    max_distance: usize,
) -> Vec<(VerbSpec, usize)> {
    let mut matches = Vec::new();
    
    for verb in verbs {
        // Adjust max distance by strictness
        let allowed = match verb.strictness {
            Strictness::Strict => 1,
            Strictness::Normal => max_distance,
            Strictness::Loose => max_distance + 1,
        };
        
        let dist = levenshtein(&input.to_lowercase(), verb.canonical);
        if dist <= allowed {
            matches.push((verb.clone(), dist));
        }
        
        // Also check aliases
        for alias in &verb.aliases {
            let dist = levenshtein(&input.to_lowercase(), alias);
            if dist <= allowed && dist < matches.iter()
                .find(|(v, _)| v.canonical == verb.canonical)
                .map(|(_, d)| *d)
                .unwrap_or(usize::MAX) 
            {
                // Better match via alias
                if let Some(existing) = matches.iter_mut()
                    .find(|(v, _)| v.canonical == verb.canonical) 
                {
                    existing.1 = dist;
                } else {
                    matches.push((verb.clone(), dist));
                }
            }
        }
    }
    
    matches.sort_by_key(|(_, dist)| *dist);
    matches
}

fn levenshtein(a: &str, b: &str) -> usize {
    // Standard Levenshtein implementation
    // Could use `strsim` crate instead
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    
    let mut matrix = vec![vec![0; b.len() + 1]; a.len() + 1];
    
    for i in 0..=a.len() { matrix[i][0] = i; }
    for j in 0..=b.len() { matrix[0][j] = j; }
    
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = if a[i-1] == b[j-1] { 0 } else { 1 };
            matrix[i][j] = (matrix[i-1][j] + 1)
                .min(matrix[i][j-1] + 1)
                .min(matrix[i-1][j-1] + cost);
        }
    }
    
    matrix[a.len()][b.len()]
}
```

### Tier 5: Phonetic Similarity

For when someone types "recal" or "serch" - sounds right but spelled wrong:

```rust
pub fn phonetic_match(input: &str, verbs: &[VerbSpec]) -> Vec<(VerbSpec, f32)> {
    let input_code = soundex(input);
    let mut matches = Vec::new();
    
    for verb in verbs {
        if verb.strictness == Strictness::Strict {
            continue; // Don't use phonetic for strict verbs
        }
        
        let verb_code = soundex(verb.canonical);
        if input_code == verb_code {
            matches.push((verb.clone(), 0.8)); // High but not perfect confidence
        }
        
        for alias in &verb.aliases {
            if soundex(alias) == input_code {
                matches.push((verb.clone(), 0.8));
                break;
            }
        }
    }
    
    matches
}

fn soundex(s: &str) -> String {
    // Standard Soundex algorithm
    // Could use `phonetics` crate instead
    if s.is_empty() { return String::new(); }
    
    let s = s.to_uppercase();
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    
    let codes: String = chars
        .filter_map(|c| match c {
            'B' | 'F' | 'P' | 'V' => Some('1'),
            'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => Some('2'),
            'D' | 'T' => Some('3'),
            'L' => Some('4'),
            'M' | 'N' => Some('5'),
            'R' => Some('6'),
            _ => None,
        })
        .collect();
    
    // Remove consecutive duplicates
    let mut result = String::new();
    result.push(first);
    let mut last = ' ';
    for c in codes.chars() {
        if c != last {
            result.push(c);
            last = c;
        }
        if result.len() >= 4 { break; }
    }
    
    // Pad to 4 characters
    while result.len() < 4 {
        result.push('0');
    }
    
    result
}
```

### Tier 6: Semantic Similarity (Optional/Advanced)

For when an agent says `/retrieve` instead of `/recall` - different word, same meaning:

```rust
pub struct SemanticMatcher<E: EmbeddingProvider> {
    embedder: E,
    verb_embeddings: HashMap<String, Vec<f32>>,
    similarity_threshold: f32,
}

impl<E: EmbeddingProvider> SemanticMatcher<E> {
    pub async fn new(embedder: E, verbs: &[VerbSpec]) -> Result<Self> {
        let mut verb_embeddings = HashMap::new();
        
        // Pre-compute embeddings for all verbs and aliases
        for verb in verbs {
            if verb.strictness == Strictness::Strict {
                continue; // Don't use semantic matching for strict verbs
            }
            
            // Embed the canonical name and use it as representative
            let embedding = embedder.embed(&[verb.canonical.to_string()]).await?;
            verb_embeddings.insert(verb.canonical.to_string(), embedding[0].clone());
        }
        
        Ok(Self {
            embedder,
            verb_embeddings,
            similarity_threshold: 0.85, // High threshold to avoid false positives
        })
    }
    
    pub async fn find_similar(&self, input: &str) -> Result<Vec<(String, f32)>> {
        let input_embedding = self.embedder.embed(&[input.to_string()]).await?;
        let input_vec = &input_embedding[0];
        
        let mut matches: Vec<(String, f32)> = self.verb_embeddings
            .iter()
            .map(|(verb, verb_vec)| {
                let similarity = cosine_similarity(input_vec, verb_vec);
                (verb.clone(), similarity)
            })
            .filter(|(_, sim)| *sim >= self.similarity_threshold)
            .collect();
        
        matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(matches)
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm_a * norm_b)
}
```

### Combined Matcher

```rust
pub struct VerbMatcher<E: EmbeddingProvider> {
    verbs: Vec<VerbSpec>,
    morphological: MorphologicalMatcher,
    semantic: Option<SemanticMatcher<E>>,
}

impl<E: EmbeddingProvider> VerbMatcher<E> {
    pub async fn match_verb(&self, input: &str) -> MatchResult {
        // Tier 1-2: Exact match
        if let Some(verb) = self.exact_match(input) {
            return MatchResult::Exact(verb.clone());
        }
        
        // Tier 3: Morphological normalization then exact match
        let normalized = self.morphological.normalize(input);
        if normalized != input {
            if let Some(verb) = self.exact_match(&normalized) {
                return MatchResult::Normalized(verb.clone(), normalized);
            }
        }
        
        // Also try spelling variants
        for variant in self.morphological.spelling_variants(input) {
            if let Some(verb) = self.exact_match(&variant) {
                return MatchResult::SpellingVariant(verb.clone(), variant);
            }
        }
        
        // Tier 4: Levenshtein
        let lev_matches = levenshtein_match(input, &self.verbs, 2);
        if let Some((verb, dist)) = lev_matches.first() {
            if *dist <= 1 {
                return MatchResult::Typo(verb.clone(), *dist);
            }
        }
        
        // Tier 5: Phonetic
        let phonetic_matches = phonetic_match(input, &self.verbs);
        if let Some((verb, conf)) = phonetic_matches.first() {
            return MatchResult::Phonetic(verb.clone(), *conf);
        }
        
        // Tier 6: Semantic (if enabled)
        if let Some(ref semantic) = self.semantic {
            if let Ok(semantic_matches) = semantic.find_similar(input).await {
                if let Some((verb_name, conf)) = semantic_matches.first() {
                    if let Some(verb) = self.verbs.iter().find(|v| v.canonical == *verb_name) {
                        return MatchResult::Semantic(verb.clone(), *conf);
                    }
                }
            }
        }
        
        // No match - return candidates for error message
        let mut candidates: Vec<_> = lev_matches.into_iter()
            .map(|(v, d)| (v.canonical.to_string(), 1.0 - (d as f32 / 5.0)))
            .collect();
        candidates.extend(phonetic_matches.into_iter()
            .map(|(v, c)| (v.canonical.to_string(), c)));
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        candidates.dedup_by(|a, b| a.0 == b.0);
        candidates.truncate(3);
        
        MatchResult::NoMatch { 
            input: input.to_string(),
            candidates,
        }
    }
}

pub enum MatchResult {
    Exact(VerbSpec),
    Normalized(VerbSpec, String),
    SpellingVariant(VerbSpec, String),
    Typo(VerbSpec, usize),
    Phonetic(VerbSpec, f32),
    Semantic(VerbSpec, f32),
    NoMatch { input: String, candidates: Vec<(String, f32)> },
}
```

---

## Parse Failure Logging

### Data Model

```rust
pub struct ParseFailure {
    pub id: ParseFailureId,
    pub raw_input: String,
    pub agent_id: AgentId,
    pub model: String,
    pub timestamp: DateTime<Utc>,
    pub failure_type: ParseFailureType,
    pub match_result: Option<MatchResult>,  // What the matcher found
    pub context_hint: Option<String>,       // What was agent trying to do
    pub session_id: Option<String>,         // For grouping related failures
}

pub enum ParseFailureType {
    UnknownVerb { 
        attempted: String, 
        candidates: Vec<(String, f32)>,
    },
    AmbiguousArgs { 
        verb: String, 
        args: String, 
        possibilities: Vec<String>,
    },
    MalformedSyntax { 
        verb: String, 
        issue: String,
        position: Option<usize>,
    },
    InvalidReference { 
        reference: String,
        available: Vec<String>,
    },
    UnrecognizedModifier {
        verb: String,
        modifier: String,
        valid_modifiers: Vec<String>,
    },
    PermissionMarkerInvalid { 
        marker: String,
    },
}
```

### Storage & Queries

```rust
pub struct ParseFailureLog {
    db: DatabaseConnection,
}

impl ParseFailureLog {
    pub async fn log(&self, failure: ParseFailure) -> Result<()> {
        // Insert into parse_failures table
    }
    
    /// What unknown verbs are agents trying to use?
    pub async fn unknown_verbs_frequency(
        &self, 
        since: DateTime<Utc>,
    ) -> Result<Vec<(String, usize, Vec<String>)>> {
        // Returns: (attempted_verb, count, example_contexts)
    }
    
    /// Failures grouped by model
    pub async fn by_model(
        &self, 
        model: &str, 
        limit: usize,
    ) -> Result<Vec<ParseFailure>> {
    }
    
    /// Most common failure patterns
    pub async fn common_patterns(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<FailurePattern>> {
    }
    
    /// Suggest new aliases based on failed attempts
    pub async fn suggest_aliases(&self) -> Result<Vec<AliasCandidate>> {
        // Find unknown verbs that:
        // 1. Occur frequently
        // 2. Have high semantic similarity to existing verbs
        // 3. Aren't already aliases
    }
}

pub struct AliasCandidate {
    pub new_alias: String,
    pub target_verb: String,
    pub occurrence_count: usize,
    pub confidence: f32,
    pub example_usages: Vec<String>,
}

pub struct FailurePattern {
    pub pattern_type: ParseFailureType,
    pub count: usize,
    pub affected_models: Vec<String>,
    pub suggested_fix: Option<String>,
}
```

### Auto-Suggestion Pipeline

```rust
pub async fn analyze_and_suggest(
    log: &ParseFailureLog,
    matcher: &VerbMatcher<impl EmbeddingProvider>,
    min_occurrences: usize,
    min_confidence: f32,
) -> Result<Vec<AliasCandidate>> {
    let unknown_verbs = log.unknown_verbs_frequency(
        Utc::now() - Duration::days(7)
    ).await?;
    
    let mut candidates = Vec::new();
    
    for (attempted, count, examples) in unknown_verbs {
        if count < min_occurrences {
            continue;
        }
        
        // Try semantic matching
        if let Some(ref semantic) = matcher.semantic {
            let similar = semantic.find_similar(&attempted).await?;
            if let Some((verb, confidence)) = similar.first() {
                if *confidence >= min_confidence {
                    candidates.push(AliasCandidate {
                        new_alias: attempted,
                        target_verb: verb.clone(),
                        occurrence_count: count,
                        confidence: *confidence,
                        example_usages: examples,
                    });
                }
            }
        }
    }
    
    // Sort by potential impact (count * confidence)
    candidates.sort_by(|a, b| {
        let score_a = a.occurrence_count as f32 * a.confidence;
        let score_b = b.occurrence_count as f32 * b.confidence;
        score_b.partial_cmp(&score_a).unwrap()
    });
    
    Ok(candidates)
}
```

---

## Diff/Patch Support

Using the `similar` crate for unified diff parsing and application:

```rust
use similar::{TextDiff, ChangeTag};

pub struct DiffPatch {
    pub hunks: Vec<Hunk>,
}

pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub changes: Vec<Change>,
}

pub enum Change {
    Context(String),
    Delete(String),
    Insert(String),
}

impl DiffPatch {
    /// Parse unified diff format
    pub fn parse(patch_text: &str) -> Result<Self, PatchParseError> {
        let mut hunks = Vec::new();
        let mut lines = patch_text.lines().peekable();
        
        while let Some(line) = lines.next() {
            // Skip header lines
            if line.starts_with("---") || line.starts_with("+++") {
                continue;
            }
            
            // Parse hunk header: @@ -start,count +start,count @@
            if line.starts_with("@@") {
                let hunk = Self::parse_hunk(line, &mut lines)?;
                hunks.push(hunk);
            }
        }
        
        Ok(Self { hunks })
    }
    
    fn parse_hunk(
        header: &str, 
        lines: &mut std::iter::Peekable<std::str::Lines>,
    ) -> Result<Hunk, PatchParseError> {
        // Parse @@ -old_start,old_count +new_start,new_count @@
        let re = regex::Regex::new(r"@@ -(\d+),?(\d*) \+(\d+),?(\d*) @@").unwrap();
        let caps = re.captures(header)
            .ok_or(PatchParseError::InvalidHunkHeader)?;
        
        let old_start: usize = caps[1].parse()?;
        let old_count: usize = caps.get(2)
            .map(|m| m.as_str().parse().unwrap_or(1))
            .unwrap_or(1);
        let new_start: usize = caps[3].parse()?;
        let new_count: usize = caps.get(4)
            .map(|m| m.as_str().parse().unwrap_or(1))
            .unwrap_or(1);
        
        let mut changes = Vec::new();
        
        while let Some(line) = lines.peek() {
            if line.starts_with("@@") || line.starts_with("---") || line.starts_with("+++") {
                break;
            }
            
            let line = lines.next().unwrap();
            
            if let Some(content) = line.strip_prefix('-') {
                changes.push(Change::Delete(content.to_string()));
            } else if let Some(content) = line.strip_prefix('+') {
                changes.push(Change::Insert(content.to_string()));
            } else if let Some(content) = line.strip_prefix(' ') {
                changes.push(Change::Context(content.to_string()));
            } else if !line.is_empty() {
                // Treat as context if no prefix
                changes.push(Change::Context(line.to_string()));
            }
        }
        
        Ok(Hunk {
            old_start,
            old_count,
            new_start,
            new_count,
            changes,
        })
    }
    
    /// Apply patch to original text
    pub fn apply(&self, original: &str) -> Result<String, PatchApplyError> {
        let mut lines: Vec<&str> = original.lines().collect();
        
        // Apply hunks in reverse order to preserve line numbers
        for hunk in self.hunks.iter().rev() {
            let start = hunk.old_start.saturating_sub(1); // Convert to 0-indexed
            
            // Verify context matches (fuzzy - allow some drift)
            // ...
            
            // Remove old lines and insert new ones
            let mut new_lines: Vec<String> = Vec::new();
            let mut old_idx = 0;
            
            for change in &hunk.changes {
                match change {
                    Change::Context(s) => {
                        new_lines.push(s.clone());
                        old_idx += 1;
                    }
                    Change::Delete(_) => {
                        old_idx += 1;
                    }
                    Change::Insert(s) => {
                        new_lines.push(s.clone());
                    }
                }
            }
            
            // Replace the range in original
            let end = start + hunk.old_count;
            let before: Vec<&str> = lines[..start].to_vec();
            let after: Vec<&str> = lines[end.min(lines.len())..].to_vec();
            
            lines = before.into_iter()
                .chain(new_lines.iter().map(|s| s.as_str()))
                .chain(after)
                .collect();
        }
        
        Ok(lines.join("\n"))
    }
}

/// Create a diff between two strings
pub fn create_diff(original: &str, modified: &str) -> String {
    let diff = TextDiff::from_lines(original, modified);
    
    let mut output = String::new();
    
    for hunk in diff.unified_diff().iter_hunks() {
        output.push_str(&format!("{}", hunk));
    }
    
    output
}
```

---

## Action Detection & Execution

### Detection Rules

Actions are detected anywhere in agent output, but only executed under specific conditions.

```rust
pub struct DetectedAction {
    pub action: ParsedAction,
    pub span: Span,           // position in message
    pub is_trailing: bool,    // no non-whitespace after
    pub has_do_marker: bool,  // prefixed with /do
}

pub struct ActionDetector {
    sigil: char,              // '/'
    do_marker: &'static str,  // "do"
}

impl ActionDetector {
    pub fn detect_all(&self, message: &str) -> Vec<DetectedAction> {
        // Find all /verb patterns in message
        // Mark each with position, trailing status, /do presence
    }
    
    pub fn executable_actions(&self, message: &str) -> Vec<ParsedAction> {
        self.detect_all(message)
            .into_iter()
            .filter(|a| should_execute(a, message))
            .map(|a| a.action)
            .collect()
    }
}
```

### Execution Decision

```rust
pub fn should_execute(
    action: &DetectedAction,
    message: &str,
) -> bool {
    // Must be trailing or explicitly marked with /do
    if !action.is_trailing && !action.has_do_marker {
        return false;
    }
    
    // Check for reconsideration in text after the action
    let text_after = &message[action.span.end..];
    if has_reconsideration(text_after) {
        return false;
    }
    
    true
}

fn has_reconsideration(text: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "actually",
        "wait", 
        "never mind",
        "nevermind",
        "scratch that",
        "ignore that",
        "don't need",
        "won't need", 
        "shouldn't",
        "hold on",
        "let me think",
        "on second thought",
        "no,",
        "nope",
        "nah",
        "forget that",
        "disregard",
        "not necessary",
        "don't bother",
    ];
    
    let lower = text.to_lowercase();
    PATTERNS.iter().any(|p| lower.contains(p))
}
```

### Examples

```
# EXECUTED - trailing action, no reconsideration
Let me check what we discussed.
/recall project deadlines

# NOT EXECUTED - reconsideration after
Let me check that.
/recall project deadlines
Actually wait, you just told me. Never mind.

# EXECUTED - explicit /do marker
/do /recall project deadlines
I'll keep talking while that runs.

# NOT EXECUTED - mid-message, no marker
I could /recall the notes but let's think first...

# NOT EXECUTED - /do but then reconsideration
/do /recall project deadlines
Hmm, actually scratch that, wrong label.

# EXECUTED - multiple trailing actions
Let me gather some context.
/recall project notes
/search conversations about deadline
```

### Current vs Future Streaming Behavior

**Current (no streaming):**
- Full message available before any execution
- Detect all actions, filter by rules, execute survivors
- Simple and safe

**Future (with streaming):**
- Partial responses shown to user as they generate
- Execution still waits for message completion by default
- Exception: `/do` + safe action subset + token window

```rust
pub enum ExecutionTiming {
    /// Wait for full message (default, always safe)
    PostCompletion,
    
    /// Stream-execute with safeguards (opt-in, restricted)
    Streaming {
        /// Additional tokens to wait after action for reconsideration
        confirmation_window: usize,  // e.g., 50 tokens
        /// Only these action types allowed
        allowed_verbs: HashSet<Verb>,
    },
}

/// Actions safe for stream-execution (no side effects, cancellable)
pub fn streaming_safe_verbs() -> HashSet<Verb> {
    hashset! {
        Verb::Recall,   // read-only recall (not insert/delete)
        Verb::Search,
        Verb::Calc,
    }
}

/// Full stream-execution rules (future)
pub async fn maybe_stream_execute(
    action: &DetectedAction,
    token_stream: &mut TokenStream,
    timing: ExecutionTiming,
) -> Option<ActionResult> {
    match timing {
        ExecutionTiming::PostCompletion => None,  // handled elsewhere
        
        ExecutionTiming::Streaming { confirmation_window, allowed_verbs } => {
            // Must have explicit /do marker
            if !action.has_do_marker {
                return None;
            }
            
            // Must be in safe subset
            if !allowed_verbs.contains(&action.action.verb) {
                return None;
            }
            
            // Must be read-only variant of the verb
            if !action.action.intent.is_read_only() {
                return None;
            }
            
            // Wait for confirmation window
            let mut tokens_seen = 0;
            while tokens_seen < confirmation_window {
                match token_stream.try_next().await {
                    Some(token) => {
                        if has_reconsideration(&token) {
                            return None;  // cancelled
                        }
                        tokens_seen += 1;
                    }
                    None => break,  // stream ended
                }
            }
            
            // Window passed, no reconsideration - execute
            Some(execute(action.action.clone()).await)
        }
    }
}
```

### Integration with Message Flow

```
Agent generates response
        │
        ▼
┌───────────────────────────────────────┐
│  Full message received                │
│  (or streamed to completion)          │
└───────────────────┬───────────────────┘
                    │
                    ▼
┌───────────────────────────────────────┐
│  ActionDetector.detect_all()          │
│  Find all /verb patterns              │
└───────────────────┬───────────────────┘
                    │
                    ▼
┌───────────────────────────────────────┐
│  Filter: trailing OR /do marked       │
│  Filter: no reconsideration after     │
└───────────────────┬───────────────────┘
                    │
                    ▼
┌───────────────────────────────────────┐
│  Parse surviving actions              │
│  (fuzzy verb match, arg extraction)   │
└───────────────────┬───────────────────┘
                    │
                    ▼
┌───────────────────────────────────────┐
│  Permission check                     │
│  (implicit + explicit markers)        │
└───────────────────┬───────────────────┘
                    │
        ┌───────────┴───────────┐
        ▼                       ▼
   Permitted               Needs approval
        │                       │
        ▼                       ▼
   Execute via             Queue for
   existing tools          authority
        │                       │
        ▼                       ▼
   Format result           Wait for
   for agent               decision
        │                       │
        └───────────┬───────────┘
                    │
                    ▼
┌───────────────────────────────────────┐
│  Return results to agent              │
│  (or error/permission denied msg)     │
└───────────────────────────────────────┘
```

---

## Crate Dependencies

```toml
[dependencies]
# Fuzzy string matching
strsim = "0.10"        # Levenshtein, Jaro-Winkler, etc.

# Phonetic matching (optional)
phonetics = "0.1"      # Soundex, Metaphone

# Diff/patch
similar = "2.0"        # Text diffing

# Regex for parsing
regex = "1.0"

# Async trait
async-trait = "0.1"
```
