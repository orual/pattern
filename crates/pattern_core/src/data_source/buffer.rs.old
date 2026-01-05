use std::collections::VecDeque;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::{Connection, Surreal};

use super::traits::{Searchable, StreamEvent};

/// Buffer for stream history with optional persistence
#[derive(Debug)]
pub struct StreamBuffer<T, C, D: Connection = surrealdb::engine::any::Any> {
    items: VecDeque<StreamEvent<T, C>>,
    max_items: usize,
    max_age: Duration,
    db: Option<Surreal<D>>,
    /// Optional queue for rate-limited processing
    processing_queue: Option<VecDeque<StreamEvent<T, C>>>,
    /// Max queue size before dropping new items
    max_queue_size: Option<usize>,
}

impl<T, C, D> StreamBuffer<T, C, D>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
    C: Serialize + for<'de> Deserialize<'de> + Clone,
    D: Connection,
{
    pub fn new(max_items: usize, max_age: Duration) -> Self {
        Self {
            items: VecDeque::with_capacity(max_items),
            max_items,
            max_age,
            db: None,
            processing_queue: None,
            max_queue_size: None,
        }
    }

    pub fn with_persistence(mut self, db: Surreal<D>) -> Self {
        self.db = Some(db);
        self
    }

    /// Enable rate-limited processing queue
    pub fn with_processing_queue(mut self, max_queue_size: usize) -> Self {
        self.processing_queue = Some(VecDeque::with_capacity(max_queue_size));
        self.max_queue_size = Some(max_queue_size);
        self
    }

    /// Add an item to the buffer
    pub fn push(&mut self, event: StreamEvent<T, C>) {
        // Remove old items if at capacity
        while self.items.len() >= self.max_items {
            self.items.pop_front();
        }

        // Remove items older than max_age
        let cutoff = Utc::now() - chrono::Duration::from_std(self.max_age).unwrap();
        while let Some(front) = self.items.front() {
            if front.timestamp < cutoff {
                self.items.pop_front();
            } else {
                break;
            }
        }

        self.items.push_back(event);
    }

    /// Get items within a time range
    pub fn get_range(
        &self,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> Vec<&StreamEvent<T, C>> {
        self.items
            .iter()
            .filter(|event| {
                let after_start = start.map_or(true, |s| event.timestamp >= s);
                let before_end = end.map_or(true, |e| event.timestamp <= e);
                after_start && before_end
            })
            .collect()
    }

    /// Get items after a specific cursor
    pub fn get_after_cursor(&self, cursor: &C) -> Vec<&StreamEvent<T, C>>
    where
        C: PartialEq,
    {
        let mut found = false;
        self.items
            .iter()
            .filter(|event| {
                if found {
                    true
                } else if event.cursor == *cursor {
                    found = true;
                    false
                } else {
                    false
                }
            })
            .collect()
    }

    /// Get buffer statistics
    pub fn stats(&self) -> BufferStats {
        BufferStats {
            item_count: self.items.len(),
            oldest_item: self.items.front().map(|e| e.timestamp),
            newest_item: self.items.back().map(|e| e.timestamp),
            max_items: self.max_items,
            max_age: self.max_age,
            queue_size: self.processing_queue.as_ref().map(|q| q.len()).unwrap_or(0),
        }
    }

    /// Clear the buffer
    pub fn clear(&mut self) {
        self.items.clear();
        if let Some(queue) = &mut self.processing_queue {
            queue.clear();
        }
    }

    /// Add an item to the processing queue
    pub fn queue_for_processing(&mut self, event: StreamEvent<T, C>) -> bool {
        if let Some(queue) = &mut self.processing_queue {
            if let Some(max_size) = self.max_queue_size {
                if queue.len() >= max_size {
                    // Queue is full, drop the item
                    return false;
                }
            }
            queue.push_back(event);
            true
        } else {
            // No processing queue configured
            false
        }
    }

    /// Get the next item from the processing queue
    pub fn dequeue_for_processing(&mut self) -> Option<StreamEvent<T, C>> {
        self.processing_queue.as_mut()?.pop_front()
    }

    /// Requeue an item to the front of the processing queue (used on backpressure)
    pub fn requeue_front_for_processing(&mut self, event: StreamEvent<T, C>) -> bool {
        if let Some(queue) = &mut self.processing_queue {
            // If we're at capacity, drop the oldest from the back to make room
            if let Some(max_size) = self.max_queue_size {
                if queue.len() >= max_size {
                    queue.pop_back();
                }
            }
            queue.push_front(event);
            true
        } else {
            false
        }
    }

    /// Get the current queue size
    pub fn queue_len(&self) -> usize {
        self.processing_queue.as_ref().map(|q| q.len()).unwrap_or(0)
    }

    /// Search buffer contents if items implement Searchable
    pub fn search(&self, query: &str, limit: usize) -> Vec<&StreamEvent<T, C>>
    where
        T: Searchable,
    {
        let mut results: Vec<(&StreamEvent<T, C>, f32)> = self
            .items
            .iter()
            .filter_map(|event| {
                let relevance = event.item.relevance(query);
                if relevance > 0.0 {
                    Some((event, relevance))
                } else {
                    None
                }
            })
            .collect();

        // Sort by relevance descending, then by timestamp descending
        results.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.0.timestamp.cmp(&a.0.timestamp))
        });

        results
            .into_iter()
            .take(limit)
            .map(|(event, _)| event)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferStats {
    pub item_count: usize,
    pub oldest_item: Option<DateTime<Utc>>,
    pub newest_item: Option<DateTime<Utc>>,
    pub max_items: usize,
    pub max_age: Duration,
    pub queue_size: usize,
}

/// Configuration for stream buffering
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferConfig {
    pub max_items: usize,
    pub max_age: Duration,
    pub persist_to_db: bool,
    pub index_content: bool,
    pub notify_changes: bool,
}
