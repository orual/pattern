//! CAR file utilities.

use cid::Cid;
use multihash_codetable::{Code, MultihashDigest};
use serde::Serialize;
use serde_ipld_dagcbor::to_vec as encode_dag_cbor;

use super::MAX_BLOCK_BYTES;
use crate::error::{CoreError, Result};

/// DAG-CBOR codec identifier
pub const DAG_CBOR_CODEC: u64 = 0x71;

/// Create a CID from serialized data using Blake3-256.
pub fn create_cid(data: &[u8]) -> Cid {
    let hash = Code::Blake3_256.digest(data);
    Cid::new_v1(DAG_CBOR_CODEC, hash)
}

/// Encode a value to DAG-CBOR and create its CID.
pub fn encode_block<T: Serialize>(value: &T, type_name: &str) -> Result<(Cid, Vec<u8>)> {
    let data = encode_dag_cbor(value).map_err(|e| CoreError::ExportError {
        operation: format!("encoding {}", type_name),
        cause: e.to_string(),
    })?;

    if data.len() > MAX_BLOCK_BYTES {
        return Err(CoreError::ExportError {
            operation: format!("encoding {}", type_name),
            cause: format!(
                "block exceeds {} bytes (got {})",
                MAX_BLOCK_BYTES,
                data.len()
            ),
        });
    }

    let cid = create_cid(&data);
    Ok((cid, data))
}

/// Chunk binary data into blocks under the size limit.
pub fn chunk_bytes(data: &[u8], max_chunk_size: usize) -> Vec<Vec<u8>> {
    data.chunks(max_chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect()
}

/// Estimate serialized size of a value.
pub fn estimate_size<T: Serialize>(value: &T) -> Result<usize> {
    let data = encode_dag_cbor(value).map_err(|e| CoreError::ExportError {
        operation: "estimating size".to_string(),
        cause: e.to_string(),
    })?;
    Ok(data.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct TestData {
        name: String,
        value: i32,
    }

    #[test]
    fn test_create_cid_deterministic() {
        let data = b"test data for CID creation";
        let cid1 = create_cid(data);
        let cid2 = create_cid(data);
        assert_eq!(cid1, cid2);

        // Different data should produce different CID
        let cid3 = create_cid(b"different data");
        assert_ne!(cid1, cid3);
    }

    #[test]
    fn test_encode_block_success() {
        let test_value = TestData {
            name: "test".to_string(),
            value: 42,
        };

        let (cid, data) = encode_block(&test_value, "TestData").unwrap();

        // Verify we can decode it back
        let decoded: TestData = serde_ipld_dagcbor::from_slice(&data).unwrap();
        assert_eq!(decoded, test_value);

        // Verify CID matches the data
        assert_eq!(create_cid(&data), cid);
    }

    #[test]
    fn test_chunk_bytes() {
        let data: Vec<u8> = (0..100).collect();

        // Chunk into blocks of 30
        let chunks = chunk_bytes(&data, 30);
        assert_eq!(chunks.len(), 4); // 30 + 30 + 30 + 10

        assert_eq!(chunks[0].len(), 30);
        assert_eq!(chunks[1].len(), 30);
        assert_eq!(chunks[2].len(), 30);
        assert_eq!(chunks[3].len(), 10);

        // Verify data integrity
        let reconstructed: Vec<u8> = chunks.into_iter().flatten().collect();
        assert_eq!(reconstructed, data);
    }

    #[test]
    fn test_chunk_bytes_empty() {
        let chunks = chunk_bytes(&[], 100);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_bytes_exact_multiple() {
        let data: Vec<u8> = (0..100).collect();
        let chunks = chunk_bytes(&data, 50);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 50);
        assert_eq!(chunks[1].len(), 50);
    }

    #[test]
    fn test_estimate_size() {
        let test_value = TestData {
            name: "test".to_string(),
            value: 42,
        };

        let estimated = estimate_size(&test_value).unwrap();
        let (_, actual_data) = encode_block(&test_value, "TestData").unwrap();

        assert_eq!(estimated, actual_data.len());
    }
}
