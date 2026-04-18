//! Fully-implemented handler for `Pattern.Display`.
//!
//! Broadcast-style: every registered [`DisplaySubscriber`] receives every
//! event in the order the handler sees it. Subscribers run synchronously
//! on the effect dispatch thread; work that might block (remote sinks,
//! slow terminals) should push onto a channel and return immediately.

use std::sync::{Arc, RwLock};

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::DisplayReq;

/// Subscriber to Display events. Implementors forward chunks / final /
/// notes to output surfaces: CLI terminal, telemetry, test capture, etc.
pub trait DisplaySubscriber: Send + Sync {
    /// Receive a single Display event. Must not block for long; async
    /// work should be offloaded via a channel.
    fn on_event(&self, event: &DisplayEvent);
}

/// Observable event dispatched by the Display handler. Cloneable so
/// subscribers can take ownership if they need to.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DisplayEvent {
    /// Incremental chunk during a streaming provider response.
    Chunk(String),
    /// Terminal assembled content for the turn's Message.Ask. Fires once.
    Final(String),
    /// Agent-visible note (typing indicator, tool-call progress, etc.).
    Note(String),
}

/// Broadcast-style handler: every registered subscriber receives every
/// event in the order the handler sees it.
///
/// Cloneable: cloning shares the subscriber list via `Arc<RwLock<_>>`.
#[derive(Default, Clone)]
pub struct DisplayHandler {
    subscribers: Arc<RwLock<Vec<Arc<dyn DisplaySubscriber>>>>,
}

impl DisplayHandler {
    /// Construct an empty handler with no subscribers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a subscriber. Order of registration is the order of
    /// notification. Phase 3 does not implement deregistration; subscriber
    /// lifecycles are one-shot at CLI startup.
    pub fn subscribe(&self, subscriber: Arc<dyn DisplaySubscriber>) {
        self.subscribers
            .write()
            .expect("DisplayHandler subscribers lock poisoned")
            .push(subscriber);
    }

    /// Number of currently-registered subscribers (exposed for tests).
    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .read()
            .expect("DisplayHandler subscribers lock poisoned")
            .len()
    }
}

impl DescribeEffect for DisplayHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Display",
            description: "One-way broadcast of observable agent output to UX surfaces (Chunk/Final/Note)",
            constructors: &[
                "Chunk :: Text -> Display ()",
                "Final :: Text -> Display ()",
                "Note  :: Text -> Display ()",
            ],
            type_defs: &[],
            helpers: &[
                "chunk :: Member Display effs => Text -> Eff effs ()\nchunk t = send (Chunk t)",
                "final_ :: Member Display effs => Text -> Eff effs ()\nfinal_ t = send (Final t)",
                "note :: Member Display effs => Text -> Eff effs ()\nnote t = send (Note t)",
            ],
        }
    }
}

impl<U> EffectHandler<U> for DisplayHandler {
    type Request = DisplayReq;

    fn handle(&mut self, req: DisplayReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        let event = match req {
            DisplayReq::Chunk(s) => DisplayEvent::Chunk(s),
            DisplayReq::Final(s) => DisplayEvent::Final(s),
            DisplayReq::Note(s) => DisplayEvent::Note(s),
        };
        let subs = self
            .subscribers
            .read()
            .expect("DisplayHandler subscribers lock poisoned");
        for s in subs.iter() {
            s.on_event(&event);
        }
        cx.respond(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tidepool_repr::{DataCon, DataConId, DataConTable};

    fn unit_table() -> DataConTable {
        let mut table = DataConTable::new();
        table.insert(DataCon {
            id: DataConId(0),
            name: "()".to_string(),
            tag: 1,
            rep_arity: 0,
            field_bangs: vec![],
            qualified_name: Some("GHC.Tuple.()".to_string()),
        });
        table
    }

    /// Test subscriber: records every event it sees.
    struct Recorder {
        events: Mutex<Vec<DisplayEvent>>,
    }

    impl Recorder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                events: Mutex::new(Vec::new()),
            })
        }
    }

    impl DisplaySubscriber for Recorder {
        fn on_event(&self, event: &DisplayEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn chunk_final_note_broadcast_to_single_subscriber() {
        let table = unit_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = DisplayHandler::new();
        let rec = Recorder::new();
        h.subscribe(rec.clone());

        h.handle(DisplayReq::Chunk("c".into()), &cx).unwrap();
        h.handle(DisplayReq::Final("f".into()), &cx).unwrap();
        h.handle(DisplayReq::Note("n".into()), &cx).unwrap();

        let events = rec.events.lock().unwrap();
        assert_eq!(events.len(), 3);
        match &events[0] {
            DisplayEvent::Chunk(s) => assert_eq!(s, "c"),
            other => panic!("expected Chunk, got {other:?}"),
        }
        match &events[1] {
            DisplayEvent::Final(s) => assert_eq!(s, "f"),
            other => panic!("expected Final, got {other:?}"),
        }
        match &events[2] {
            DisplayEvent::Note(s) => assert_eq!(s, "n"),
            other => panic!("expected Note, got {other:?}"),
        }
    }

    #[test]
    fn every_subscriber_receives_every_event() {
        let table = unit_table();
        let cx = EffectContext::with_user(&table, &());
        let mut h = DisplayHandler::new();
        let a = Recorder::new();
        let b = Recorder::new();
        h.subscribe(a.clone());
        h.subscribe(b.clone());

        h.handle(DisplayReq::Chunk("x".into()), &cx).unwrap();

        assert_eq!(a.events.lock().unwrap().len(), 1);
        assert_eq!(b.events.lock().unwrap().len(), 1);
    }

    #[test]
    fn clone_shares_subscriber_list() {
        let h1 = DisplayHandler::new();
        let h2 = h1.clone();
        h1.subscribe(Recorder::new());
        assert_eq!(h2.subscriber_count(), 1);
    }
}
