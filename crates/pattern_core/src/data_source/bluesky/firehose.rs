//! FirehosePost - parsed post from Jetstream with metadata.

use jacquard::api::app_bsky::feed::post::Post;
use jacquard::common::IntoStatic;
use jacquard::common::types::string::{AtUri, Cid, Did};

/// A post from the firehose with metadata from Jetstream.
///
/// This combines the parsed `Post` record with the DID, URI, and CID
/// that come from the Jetstream commit message (not the record itself).
#[derive(Debug, Clone)]
pub struct FirehosePost {
    /// The parsed Post record from the commit
    pub post: Post<'static>,
    /// Author DID (from Jetstream message)
    pub did: Did<'static>,
    /// Post URI (constructed from did/collection/rkey)
    pub uri: AtUri<'static>,
    /// Content ID (from Jetstream commit)
    #[allow(dead_code)]
    pub cid: Option<Cid<'static>>,
    /// Jetstream timestamp (microseconds)
    #[allow(dead_code)]
    pub time_us: i64,
    /// Whether this mentions our agent
    pub is_mention: bool,
    /// Whether this is a reply to another post
    pub is_reply: bool,
}

impl FirehosePost {
    /// Get the thread root URI for this post.
    ///
    /// Returns a clone of the root URI - either from the reply reference
    /// or the post's own URI if it's a root post.
    pub fn thread_root(&self) -> AtUri<'static> {
        self.post
            .reply
            .as_ref()
            .map(|r| r.root.uri.clone().into_static())
            .unwrap_or_else(|| self.uri.clone())
    }

    /// Get the post text
    pub fn text(&self) -> &str {
        self.post.text.as_ref()
    }

    /// Get languages as strings
    pub fn langs(&self) -> Vec<String> {
        self.post
            .langs
            .as_ref()
            .map(|langs| langs.iter().map(|l| l.as_str().to_string()).collect())
            .unwrap_or_default()
    }
}
