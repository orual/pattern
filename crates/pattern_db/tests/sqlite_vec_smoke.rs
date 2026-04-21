//! sqlite-vec smoke test: 100 test vectors inserted into a vec0 virtual table,
//! KNN query returns correct ordering.

use pattern_db::ConstellationDb;
use pattern_db::vector::{
    ContentType, ensure_embeddings_table, insert_embedding, knn_search, verify_sqlite_vec,
};

#[test]
fn sqlite_vec_100_vector_knn_ordering() {
    let db = ConstellationDb::open_in_memory().unwrap();
    let conn = db.get().unwrap();

    // Verify extension loaded.
    let version = verify_sqlite_vec(&conn).unwrap();
    assert!(!version.is_empty());

    // Create embeddings table with 384 dimensions.
    ensure_embeddings_table(&conn, 384).unwrap();

    // Insert 100 test vectors. Each vector has a single "hot" dimension
    // at position i (mod 384), with a small base value everywhere else.
    for i in 0..100 {
        let mut embedding = vec![0.01f32; 384];
        embedding[i % 384] = 1.0;
        // Add a small gradient so vectors within the same hot-dimension
        // slot still have distinct distances.
        embedding[(i + 1) % 384] = 0.1 * (i as f32 / 100.0);

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

    // Query: find vectors closest to a vector with dimension 0 hot.
    let mut query = vec![0.01f32; 384];
    query[0] = 1.0;

    let results = knn_search(&conn, &query, 5, None).unwrap();
    assert_eq!(results.len(), 5);

    // The closest should be vec_0 (exact match on hot dimension 0).
    assert_eq!(results[0].content_id, "vec_0");
    assert!(
        results[0].distance < 0.05,
        "expected very small distance for exact match, got {}",
        results[0].distance
    );

    // Distances should be monotonically non-decreasing.
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

    // Insert vectors with one connection.
    {
        let db = ConstellationDb::open(&mem_path, &msg_path).unwrap();
        let conn = db.get().unwrap();
        ensure_embeddings_table(&conn, 4).unwrap();

        let emb = vec![1.0f32, 0.0, 0.0, 0.0];
        insert_embedding(&conn, ContentType::Message, "m1", &emb, None, None).unwrap();
    }

    // Reopen and query.
    {
        let db = ConstellationDb::open(&mem_path, &msg_path).unwrap();
        let conn = db.get().unwrap();

        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let results = knn_search(&conn, &query, 10, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content_id, "m1");
    }
}
