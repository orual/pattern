// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Pool stress test: verifies that 20 concurrent callers can all obtain
//! connections and execute queries without deadlock or pool exhaustion.

use pattern_db::ConstellationDb;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_concurrent_callers_complete_without_deadlock() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem_path = tmp.path().join("memory.db");
    let msg_path = tmp.path().join("messages.db");

    let db = Arc::new(ConstellationDb::open(&mem_path, &msg_path).unwrap());

    let mut handles = Vec::new();
    for i in 0..20 {
        let db = Arc::clone(&db);
        handles.push(tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let conn = db.get().expect("failed to get connection from pool");
                let val: i64 = conn
                    .query_row("SELECT ?1", rusqlite::params![i as i64], |r| r.get(0))
                    .expect("query failed");
                assert_eq!(val, i as i64);
            })
            .await
            .expect("spawn_blocking panicked");
        }));
    }

    // All 20 tasks must complete within 10 seconds.
    let timeout_result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for handle in handles {
            handle.await.expect("task panicked");
        }
    })
    .await;

    assert!(
        timeout_result.is_ok(),
        "pool stress test timed out after 10s — possible deadlock or pool exhaustion"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_memory_pool_handles_sequential_access() {
    let db = ConstellationDb::open_in_memory().unwrap();

    // In-memory mode has pool size 1, but sequential access should work.
    for i in 0..10i64 {
        let conn = db.get().unwrap();
        let val: i64 = conn
            .query_row("SELECT ?1", rusqlite::params![i], |r| r.get(0))
            .unwrap();
        assert_eq!(val, i);
    }
}
