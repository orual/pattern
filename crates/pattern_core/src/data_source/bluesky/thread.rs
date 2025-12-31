//! Thread context display for Bluesky threads.
//!
//! Provides display formatting for thread trees from GetPostThread,
//! with highlighting for batch posts and [YOU] markers for agent posts.

use std::collections::HashSet;

use jacquard::api::app_bsky::feed::{
    PostView, ThreadViewPost, ThreadViewPostParent, ThreadViewPostRepliesItem,
};
use jacquard::common::types::string::{AtUri, Did};

use super::embed::{CollectedImage, EmbedDisplay, collect_images_from_embed, indent_multiline};

/// Reason why the parent chain was truncated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentTruncation {
    /// Chain ends naturally (reached root post)
    None,
    /// Parent post was blocked
    Blocked,
    /// Parent post was not found (deleted?)
    NotFound,
}

/// Thread context for display - wraps GetPostThread result with display helpers.
///
/// This provides a unified view of a thread tree with highlighting for batch
/// posts and [YOU] markers for agent posts.
pub struct ThreadContext<'a> {
    /// The thread tree from GetPostThread
    pub thread: ThreadViewPost<'a>,

    /// Post URIs in the current batch that should be highlighted
    pub batch_uris: HashSet<AtUri<'static>>,

    /// Agent's DID for [YOU] markers
    pub agent_did: Option<Did<'static>>,

    /// Whether this thread was recently shown (for abbreviated display)
    pub recently_shown: bool,

    /// Keywords to filter out from sibling branches during display.
    /// Posts containing these keywords are hidden (not shown) but don't vacate the thread.
    pub exclude_keywords: Vec<String>,
}

impl<'a> ThreadContext<'a> {
    /// Create a new thread context from a fetched thread.
    pub fn new(thread: ThreadViewPost<'a>) -> Self {
        Self {
            thread,
            batch_uris: HashSet::new(),
            agent_did: None,
            recently_shown: false,
            exclude_keywords: Vec::new(),
        }
    }

    /// Set keywords to filter out from sibling branches during display.
    pub fn with_exclude_keywords(mut self, keywords: Vec<String>) -> Self {
        self.exclude_keywords = keywords;
        self
    }

    /// Mark posts as part of the current batch (will be highlighted).
    pub fn with_batch_uris(mut self, uris: impl IntoIterator<Item = AtUri<'static>>) -> Self {
        self.batch_uris = uris.into_iter().collect();
        self
    }

    /// Set the agent DID for [YOU] markers.
    pub fn with_agent_did(mut self, did: Did<'static>) -> Self {
        self.agent_did = Some(did);
        self
    }

    /// Mark as recently shown (triggers abbreviated display).
    pub fn with_recently_shown(mut self, recently_shown: bool) -> Self {
        self.recently_shown = recently_shown;
        self
    }

    /// Check if a post should be marked as [YOU].
    pub fn is_agent_post(&self, author_did: &Did<'_>) -> bool {
        self.agent_did
            .as_ref()
            .is_some_and(|d| d.as_str() == author_did.as_str())
    }

    /// Check if a post URI is in the current batch.
    pub fn is_batch_post(&self, uri: &AtUri<'_>) -> bool {
        self.batch_uris.iter().any(|u| u.as_str() == uri.as_str())
    }

    /// Check if a post contains any excluded keywords.
    /// Used to hide sibling branches during display.
    pub fn contains_excluded_keyword(&self, post: &PostView<'_>) -> bool {
        if self.exclude_keywords.is_empty() {
            return false;
        }

        let text = post
            .record
            .get_at_path(".text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let text_lower = text.to_lowercase();

        self.exclude_keywords
            .iter()
            .any(|kw| text_lower.contains(&kw.to_lowercase()))
    }

    /// Format the full thread tree for display.
    ///
    /// Shows parent chain from root to main post, then replies.
    pub fn format_full(&self) -> String {
        let mut output = String::new();
        output.push_str("• Thread context:\n\n");

        // Collect parent chain (they come in child→parent order, we need parent→child)
        let mut parents: Vec<&PostView<'_>> = Vec::new();
        let truncation = self.collect_parents(&self.thread, &mut parents);
        parents.reverse();

        // Show truncation indicator at the top if chain was cut short
        match truncation {
            ParentTruncation::Blocked => {
                output.push_str("    [Thread continues above but is blocked]\n");
                output.push_str("    │\n");
            }
            ParentTruncation::NotFound => {
                output.push_str(
                    "    [Thread continues above but parent not found - may be deleted]\n",
                );
                output.push_str("    │\n");
            }
            ParentTruncation::None => {}
        }

        // Format parents from root down
        if !parents.is_empty() {
            // First parent is the root (or oldest visible if truncated)
            output.push_str(&format!("    {}\n", parents[0].format_as_root(self)));
            output.push_str("    │\n");

            // Middle parents
            for parent in parents.iter().skip(1) {
                output.push_str(&format!("    {}\n", parent.format_as_parent(self, "")));
                output.push_str("    │\n");
            }
        }

        // Format main post
        output.push_str("\n>>> MAIN POST >>>\n");
        output.push_str(&self.thread.post.format_as_main(self));
        output.push_str("\n");

        // Format replies
        if let Some(replies) = &self.thread.replies {
            if !replies.is_empty() {
                output.push_str(&format!("\n   ↳ {} direct replies:\n", replies.len()));
                self.format_replies(replies, "     ", 1, &mut output);
            }
        }

        output
    }

    /// Format abbreviated display for recently-shown threads.
    ///
    /// Shows summary info instead of full parent chain.
    pub fn format_abbreviated(&self) -> String {
        let mut output = String::new();
        output.push_str("Thread context trimmed - full context shown recently\n\n");

        // Count ancestors and agent replies
        let ancestor_count = self.count_ancestors(&self.thread);
        let agent_reply_count = self.count_agent_replies(&self.thread);

        if agent_reply_count > 0 {
            output.push_str(&format!(
                "ℹ️ You've replied {} time(s) in this thread\n\n",
                agent_reply_count
            ));
        }

        // Show just the immediate parent if any
        if let Some(parent) = &self.thread.parent {
            match parent {
                ThreadViewPostParent::ThreadViewPost(tvp) => {
                    output.push_str(&format!("  └─ {}\n", tvp.post.format_as_parent(self, "")));
                    output.push_str("  │\n");
                }
                ThreadViewPostParent::BlockedPost(_) => {
                    output.push_str("  [Parent post is blocked]\n");
                    output.push_str("  │\n");
                }
                ThreadViewPostParent::NotFoundPost(_) => {
                    output.push_str("  [Parent post not found - may be deleted]\n");
                    output.push_str("  │\n");
                }
                _ => {}
            }
        }

        // Format main post
        output.push_str("\n>>> MAIN POST >>>\n");
        output.push_str(&self.thread.post.format_as_main(self));
        output.push_str("\n");

        // Summary of thread structure
        let reply_count = self.thread.replies.as_ref().map(|r| r.len()).unwrap_or(0);
        if ancestor_count > 0 || reply_count > 0 {
            output.push_str(&format!(
                "\n  ℹ️ Thread has {} ancestors and {} replies (see recent history for full context)\n",
                ancestor_count, reply_count
            ));
        }

        output
    }

    /// Collect parent PostViews by walking the parent chain.
    ///
    /// Returns the reason the chain ended (None if reached root naturally).
    fn collect_parents<'b>(
        &self,
        thread: &'b ThreadViewPost<'_>,
        parents: &mut Vec<&'b PostView<'b>>,
    ) -> ParentTruncation
    where
        'a: 'b,
    {
        if let Some(parent) = &thread.parent {
            match parent {
                ThreadViewPostParent::ThreadViewPost(tvp) => {
                    parents.push(&tvp.post);
                    self.collect_parents(tvp, parents)
                }
                ThreadViewPostParent::NotFoundPost(_) => ParentTruncation::NotFound,
                ThreadViewPostParent::BlockedPost(_) => ParentTruncation::Blocked,
                _ => {
                    // Unknown variant - treat as natural end
                    ParentTruncation::None
                }
            }
        } else {
            ParentTruncation::None
        }
    }

    /// Format replies recursively, filtering out posts with excluded keywords.
    fn format_replies(
        &self,
        replies: &[ThreadViewPostRepliesItem<'_>],
        indent: &str,
        depth: usize,
        output: &mut String,
    ) {
        let max_depth = 3; // Don't go too deep

        // Pre-filter to get visible replies (for proper is_last calculation)
        let visible_replies: Vec<_> = replies
            .iter()
            .filter(|reply| {
                match reply {
                    ThreadViewPostRepliesItem::ThreadViewPost(tvp) => {
                        // Hide posts with excluded keywords
                        !self.contains_excluded_keyword(&tvp.post)
                    }
                    // Keep blocked/not found indicators
                    _ => true,
                }
            })
            .collect();

        let hidden_count = replies.len() - visible_replies.len();

        for (i, reply) in visible_replies.iter().enumerate() {
            let is_last = i == visible_replies.len() - 1 && hidden_count == 0;

            match reply {
                ThreadViewPostRepliesItem::ThreadViewPost(tvp) => {
                    output.push_str(&tvp.post.format_as_sibling(self, indent, is_last));
                    output.push('\n');

                    // Recurse into nested replies if not too deep
                    if depth < max_depth {
                        if let Some(nested) = &tvp.replies {
                            if !nested.is_empty() {
                                let new_indent = format!("{}  ", indent);
                                self.format_replies(nested, &new_indent, depth + 1, output);
                            }
                        }
                    }
                }
                ThreadViewPostRepliesItem::NotFoundPost(_) => {
                    output.push_str(&format!(
                        "{}[Post not found - may have been deleted]\n",
                        indent
                    ));
                }
                ThreadViewPostRepliesItem::BlockedPost(_) => {
                    output.push_str(&format!("{}[Blocked by author or viewer]\n", indent));
                }
                _ => {
                    // Unknown variant
                }
            }
        }

        // Show indicator if posts were hidden due to keyword filtering
        if hidden_count > 0 {
            output.push_str(&format!(
                "{}[{} post(s) hidden due to content filters]\n",
                indent, hidden_count
            ));
        }
    }

    /// Count ancestors in the parent chain.
    fn count_ancestors(&self, thread: &ThreadViewPost<'_>) -> usize {
        match &thread.parent {
            Some(ThreadViewPostParent::ThreadViewPost(tvp)) => 1 + self.count_ancestors(tvp),
            Some(_) => 1, // Blocked or not found still counts
            None => 0,
        }
    }

    /// Count how many times the agent has replied in this thread.
    fn count_agent_replies(&self, thread: &ThreadViewPost<'_>) -> usize {
        let mut count = 0;

        // Check if main post is from agent
        if self.is_agent_post(&thread.post.author.did) {
            count += 1;
        }

        // Check parents
        if let Some(ThreadViewPostParent::ThreadViewPost(tvp)) = &thread.parent {
            count += self.count_agent_replies_in_chain(tvp);
        }

        count
    }

    fn count_agent_replies_in_chain(&self, thread: &ThreadViewPost<'_>) -> usize {
        let mut count = if self.is_agent_post(&thread.post.author.did) {
            1
        } else {
            0
        };

        if let Some(ThreadViewPostParent::ThreadViewPost(tvp)) = &thread.parent {
            count += self.count_agent_replies_in_chain(tvp);
        }

        count
    }

    /// Collect images from the entire thread tree.
    ///
    /// Returns images with position values - higher position means newer post.
    /// Parents have lowest positions, main post in middle, replies have highest.
    pub fn collect_images(&self) -> Vec<CollectedImage> {
        let mut images = Vec::new();
        let mut position = 0usize;

        // Collect from parents (oldest first, lowest positions)
        self.collect_images_from_parents(&self.thread, &mut images, &mut position);

        // Main post
        position += 1;
        if let Some(embed) = &self.thread.post.embed {
            images.extend(collect_images_from_embed(embed, position));
        }

        // Replies are newer, get higher positions
        if let Some(replies) = &self.thread.replies {
            self.collect_images_from_replies(replies, &mut images, &mut position);
        }

        images
    }

    fn collect_images_from_parents(
        &self,
        thread: &ThreadViewPost<'_>,
        images: &mut Vec<CollectedImage>,
        position: &mut usize,
    ) {
        // Recurse to oldest parent first
        if let Some(parent) = &thread.parent {
            if let ThreadViewPostParent::ThreadViewPost(tvp) = parent {
                self.collect_images_from_parents(tvp, images, position);
                // Collect from this parent after recursing
                *position += 1;
                if let Some(embed) = &tvp.post.embed {
                    images.extend(collect_images_from_embed(embed, *position));
                }
            }
        }
    }

    fn collect_images_from_replies(
        &self,
        replies: &[ThreadViewPostRepliesItem<'_>],
        images: &mut Vec<CollectedImage>,
        position: &mut usize,
    ) {
        for reply in replies {
            if let ThreadViewPostRepliesItem::ThreadViewPost(tvp) = reply {
                *position += 1;
                if let Some(embed) = &tvp.post.embed {
                    images.extend(collect_images_from_embed(embed, *position));
                }
                // Recurse into nested replies
                if let Some(nested) = &tvp.replies {
                    self.collect_images_from_replies(nested, images, position);
                }
            }
        }
    }
}

/// Format a PostView for display at various positions in the thread tree.
pub trait PostDisplay {
    /// Format as the root post of a thread.
    fn format_as_root(&self, ctx: &ThreadContext<'_>) -> String;

    /// Format as a parent in the chain leading to the main post.
    fn format_as_parent(&self, ctx: &ThreadContext<'_>, indent: &str) -> String;

    /// Format as the main post (the one triggering the notification).
    fn format_as_main(&self, ctx: &ThreadContext<'_>) -> String;

    /// Format as a sibling reply (same parent as another post).
    fn format_as_sibling(&self, ctx: &ThreadContext<'_>, indent: &str, is_last: bool) -> String;

    /// Format as a reply in the tree.
    #[allow(dead_code)]
    fn format_as_reply(&self, ctx: &ThreadContext<'_>, indent: &str, depth: usize) -> String;
}

impl PostDisplay for PostView<'_> {
    fn format_as_root(&self, ctx: &ThreadContext<'_>) -> String {
        let you_marker = if ctx.is_agent_post(&self.author.did) {
            "[YOU] "
        } else {
            ""
        };
        let handle = self.author.handle.as_str();
        let text = self
            .record
            .get_at_path(".text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let uri = self.uri.as_str();

        let mut output = String::new();
        let first_prefix = format!("┌─ {}@{}: ", you_marker, handle);
        let continuation = "   ";
        output.push_str(&indent_multiline(text, &first_prefix, continuation));
        output.push_str(&format!("\n   🔗 {}", uri));

        // Add embed if present
        if let Some(embed) = &self.embed {
            output.push('\n');
            output.push_str(&embed.format_for_parent("   "));
        }

        output
    }

    fn format_as_parent(&self, ctx: &ThreadContext<'_>, indent: &str) -> String {
        let you_marker = if ctx.is_agent_post(&self.author.did) {
            "[YOU] "
        } else {
            ""
        };
        let handle = self.author.handle.as_str();
        let text = self
            .record
            .get_at_path(".text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let uri = self.uri.as_str();

        let mut output = String::new();
        let first_prefix = format!("{}├─ {}@{}: ", indent, you_marker, handle);
        let continuation = format!("{}   ", indent);
        output.push_str(&indent_multiline(text, &first_prefix, &continuation));
        output.push_str(&format!("\n{}   🔗 {}", indent, uri));

        // Add embed if present
        if let Some(embed) = &self.embed {
            output.push('\n');
            output.push_str(&embed.format_for_parent(&format!("{}   ", indent)));
        }

        output
    }

    fn format_as_main(&self, ctx: &ThreadContext<'_>) -> String {
        let you_marker = if ctx.is_agent_post(&self.author.did) {
            "[YOU] "
        } else {
            ""
        };
        let batch_marker = if ctx.is_batch_post(&self.uri) {
            ">>> "
        } else {
            ""
        };
        let handle = self.author.handle.as_str();
        let text = self
            .record
            .get_at_path(".text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let uri = self.uri.as_str();

        let mut output = String::new();
        let first_prefix = format!("{}{}@{}: ", batch_marker, you_marker, handle);
        let continuation = "│ ";
        output.push_str(&indent_multiline(text, &first_prefix, continuation));
        output.push_str(&format!("\n│ 🔗 {}", uri));

        // Add embed if present - main post gets full detail
        if let Some(embed) = &self.embed {
            output.push('\n');
            output.push_str(&embed.format_for_main("│ "));
        }

        output
    }

    fn format_as_sibling(&self, ctx: &ThreadContext<'_>, indent: &str, is_last: bool) -> String {
        let you_marker = if ctx.is_agent_post(&self.author.did) {
            "[YOU] "
        } else {
            ""
        };
        let connector = if is_last { "└─" } else { "├─" };
        let handle = self.author.handle.as_str();
        let text = self
            .record
            .get_at_path(".text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let uri = self.uri.as_str();

        let mut output = String::new();
        let first_prefix = format!("{}{} {}@{}: ", indent, connector, you_marker, handle);
        let continuation = format!("{}   ", indent);
        output.push_str(&indent_multiline(text, &first_prefix, &continuation));
        output.push_str(&format!("\n{}   🔗 {}", indent, uri));

        // Add embed if present
        if let Some(embed) = &self.embed {
            output.push('\n');
            output.push_str(&embed.format_for_reply(&format!("{}   ", indent)));
        }

        output
    }

    fn format_as_reply(&self, ctx: &ThreadContext<'_>, indent: &str, _depth: usize) -> String {
        let you_marker = if ctx.is_agent_post(&self.author.did) {
            "[YOU] "
        } else {
            ""
        };
        let handle = self.author.handle.as_str();
        let text = self
            .record
            .get_at_path(".text")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        let uri = self.uri.as_str();

        let mut output = String::new();
        let first_prefix = format!("{}↳ {}@{}: ", indent, you_marker, handle);
        let continuation = format!("{}  ", indent);
        output.push_str(&indent_multiline(text, &first_prefix, &continuation));
        output.push_str(&format!("\n{}  🔗 {}", indent, uri));

        // Add embed if present
        if let Some(embed) = &self.embed {
            output.push('\n');
            output.push_str(&embed.format_for_reply(&format!("{}  ", indent)));
        }

        output
    }
}
