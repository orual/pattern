// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! sqlite-vec smoke test: 100 test vectors inserted into a vec0 virtual table,
//! KNN query returns correct ordering.

use pattern_db::ConstellationDb;
use pattern_db::vector::{
    ContentType, ensure_embeddings_table, insert_embedding, knn_search, verify_sqlite_vec,
};

/// Embedding dimension matching the schema set by `ConstellationDb::open*`
/// (gemma-300m embedding model). Tests insert vectors of this width and
/// pad small "signal" prefixes with zeros — zero components don't affect
/// L2/cosine distances, so KNN ordering tests stay meaningful.
const DIM: usize = 768;

fn pad(prefix: &[f32]) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    v[..prefix.len()].copy_from_slice(prefix);
    v
}

#[test]
fn sqlite_vec_100_vector_knn_ordering() {
    let db = ConstellationDb::open_in_memory().unwrap();
    let conn = db.get().unwrap();

    // Verify extension loaded.
    let version = verify_sqlite_vec(&conn).unwrap();
    assert!(!version.is_empty());

    // Table is pre-created at DIM dims by ConstellationDb::open_in_memory;
    // this just exercises IF NOT EXISTS and asserts our assumption.
    ensure_embeddings_table(&conn, DIM).unwrap();

    // Insert 100 test vectors. Each vector has a single "hot" dimension
    // at position i (mod DIM), with a small base value everywhere else.
    for i in 0..100 {
        let mut embedding = vec![0.01f32; DIM];
        embedding[i % DIM] = 1.0;
        // Add a small gradient so vectors within the same hot-dimension
        // slot still have distinct distances.
        embedding[(i + 1) % DIM] = 0.1 * (i as f32 / 100.0);

        insert_embedding(
            &conn,
            ContentType::MemoryBlock,
            &format!("vec_{i}"),
            &embedding,
            None,
            None,
        )
        .unwrap();
    }

    let mut query = vec![0.01f32; DIM];
    query[0] = 1.0;

    let results = knn_search(&conn, &query, 5, None).unwrap();
    assert_eq!(results.len(), 5);

    assert_eq!(results[0].content_id, "vec_0");
    assert!(
        results[0].distance < 0.05,
        "expected very small distance for exact match, got {}",
        results[0].distance
    );

    for w in results.windows(2) {
        assert!(
            w[0].distance <= w[1].distance + f32::EPSILON,
            "KNN ordering violated: {} > {}",
            w[0].distance,
            w[1].distance
        );
    }
}

#[test]
fn sqlite_vec_on_disk_roundtrip() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mem_path = tmp.path().join("memory.db");
    let msg_path = tmp.path().join("messages.db");

    {
        let db = ConstellationDb::open(&mem_path, &msg_path).unwrap();
        let conn = db.get().unwrap();
        ensure_embeddings_table(&conn, DIM).unwrap();

        let emb = pad(&[1.0, 0.0, 0.0, 0.0]);
        insert_embedding(&conn, ContentType::Message, "m1", &emb, None, None).unwrap();
    }

    {
        let db = ConstellationDb::open(&mem_path, &msg_path).unwrap();
        let conn = db.get().unwrap();

        let query = pad(&[1.0, 0.0, 0.0, 0.0]);
        let results = knn_search(&conn, &query, 10, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content_id, "m1");
    }
}
