//! Vector KNN ordering regression test with insta snapshots.
//!
//! Verifies that KNN search returns consistent nearest-neighbor ordering
//! on a canonical vector structure with known clusters.

use pattern_db::ConstellationDb;
use pattern_db::vector::{ContentType, ensure_embeddings_table, insert_embedding, knn_search};

/// Create a set of 30 vectors clustered around 3 centroids in 4-d space.
/// Centroid A: [1, 0, 0, 0], Centroid B: [0, 1, 0, 0], Centroid C: [0, 0, 1, 0].
/// Each cluster has 10 points with small perturbations.
fn insert_clustered_vectors(conn: &rusqlite::Connection) {
    ensure_embeddings_table(conn, 4).unwrap();

    let centroids: [(f32, f32, f32, f32); 3] = [
        (1.0, 0.0, 0.0, 0.0), // cluster A
        (0.0, 1.0, 0.0, 0.0), // cluster B
        (0.0, 0.0, 1.0, 0.0), // cluster C
    ];

    for (ci, (cx, cy, cz, cw)) in centroids.iter().enumerate() {
        for j in 0..10 {
            let offset = j as f32 * 0.02;
            let embedding = vec![
                cx + offset,
                cy + offset * 0.5,
                cz + offset * 0.3,
                cw + offset * 0.1,
            ];
            let id = format!("cluster_{ci}_vec_{j}");
            insert_embedding(conn, ContentType::MemoryBlock, &id, &embedding, None, None).unwrap();
        }
    }
}

#[test]
fn knn_ordering_cluster_a_snapshot() {
    let db = ConstellationDb::open_in_memory().unwrap();
    let conn = db.get().unwrap();
    insert_clustered_vectors(&conn);

    // Query near centroid A.
    let query = vec![1.0f32, 0.0, 0.0, 0.0];
    let results = knn_search(&conn, &query, 10, None).unwrap();

    let snapshot: Vec<(String, f32)> = results
        .iter()
        .map(|r| {
            (
                r.content_id.clone(),
                (r.distance * 10000.0).round() / 10000.0,
            )
        })
        .collect();

    insta::assert_yaml_snapshot!("knn_cluster_a_nearest_10", snapshot);
}

#[test]
fn knn_ordering_cluster_b_snapshot() {
    let db = ConstellationDb::open_in_memory().unwrap();
    let conn = db.get().unwrap();
    insert_clustered_vectors(&conn);

    let query = vec![0.0f32, 1.0, 0.0, 0.0];
    let results = knn_search(&conn, &query, 5, None).unwrap();

    let snapshot: Vec<(String, f32)> = results
        .iter()
        .map(|r| {
            (
                r.content_id.clone(),
                (r.distance * 10000.0).round() / 10000.0,
            )
        })
        .collect();

    insta::assert_yaml_snapshot!("knn_cluster_b_nearest_5", snapshot);
}

#[test]
fn knn_all_clusters_returns_mixed() {
    let db = ConstellationDb::open_in_memory().unwrap();
    let conn = db.get().unwrap();
    insert_clustered_vectors(&conn);

    // Query equidistant from all centroids.
    let query = vec![0.577f32, 0.577, 0.577, 0.0];
    let results = knn_search(&conn, &query, 30, None).unwrap();

    // Should have all 30 vectors.
    assert_eq!(results.len(), 30);

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
