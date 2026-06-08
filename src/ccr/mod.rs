//! CCR — content-addressed, losslessly-reversible blob store.
//!
//! Public API: [`store_blob`] writes the verbatim original (content-addressed by
//! sha256, compressed by a byte-exact codec, deduplicated by hash) and returns a
//! [`BlobRef`]; [`load_blob`] returns the exact original bytes and **fails loudly**
//! if the decompressed bytes do not re-hash to the requested key. That hash check
//! is the runtime half of the reversibility contract — a corrupted or tampered
//! row can never silently return wrong bytes.

pub mod codec;
pub mod detect;
pub mod zstd_codec;

pub use codec::{codec_by_id, codec_for, Codec};
pub use detect::ContentType;

use crate::db::{get_blob, insert_blob, Database};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Handle to a stored blob, returned by [`store_blob`].
#[derive(Debug, Clone)]
#[allow(dead_code)] // descriptive fields (codec/lens) are part of the API surface
pub struct BlobRef {
    /// Hex sha256 of the ORIGINAL (uncompressed) bytes — the content address.
    pub hash: String,
    pub content_type: ContentType,
    /// Codec id used to compress the stored bytes (e.g. `"zstd"`).
    pub codec: &'static str,
    pub orig_len: usize,
    pub comp_len: usize,
}

/// Hex-encode bytes (lowercase). Avoids pulling in a `hex` dependency.
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// sha256 of `bytes`, hex-encoded — the content address used as the blob key.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

/// Store `bytes` verbatim: content-address by sha256, detect the content type,
/// compress with the matching byte-exact codec, and persist (deduped by hash).
/// Storing identical bytes twice yields the same hash and a single row (the
/// second store just bumps the reference count).
pub async fn store_blob(
    db: &Database,
    bytes: &[u8],
    path_hint: Option<&str>,
) -> anyhow::Result<BlobRef> {
    let hash = sha256_hex(bytes);
    let content_type = detect::detect(bytes, path_hint);
    let codec = codec_for(content_type);
    let compressed = codec.compress(bytes)?;
    insert_blob(
        db,
        &hash,
        content_type.as_str(),
        codec.id(),
        bytes.len() as i64,
        compressed.len() as i64,
        &compressed,
    )
    .await?;
    Ok(BlobRef {
        hash,
        content_type,
        codec: codec.id(),
        orig_len: bytes.len(),
        comp_len: compressed.len(),
    })
}

/// Load the verbatim original bytes for `hash`. Decompresses with the codec
/// recorded on the row, then re-hashes the result and errors if it does not
/// equal `hash` (corruption / tampering / codec mismatch). Returns an error if
/// no blob exists for `hash`.
pub async fn load_blob(db: &Database, hash: &str) -> anyhow::Result<Vec<u8>> {
    let row = get_blob(db, hash)
        .await?
        .ok_or_else(|| anyhow::anyhow!("CCR blob not found: {hash}"))?;
    let codec = codec_by_id(&row.codec)?;
    let original = codec.decompress(&row.data)?;
    let actual = sha256_hex(&original);
    if actual != hash {
        anyhow::bail!(
            "CCR blob integrity check failed for {hash}: decompressed bytes hash to {actual}"
        );
    }
    Ok(original)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    async fn test_db() -> anyhow::Result<(Database, String)> {
        let db_path =
            std::env::temp_dir().join(format!("ironmem-ccr-test-{}.db", uuid::Uuid::new_v4()));
        let db_path_string = db_path.to_string_lossy().to_string();
        let db = Database::new(&db_path_string).await?;
        db.migrate().await?;
        Ok((db, db_path_string))
    }

    #[tokio::test]
    async fn store_returns_ref_and_round_trips() -> anyhow::Result<()> {
        let (db, path) = test_db().await?;

        let original = b"{\"event\":\"build\",\"ok\":true,\"detail\":\"a fairly long json payload to compress\"}";
        let r = store_blob(&db, original, Some("json")).await?;
        assert_eq!(r.content_type, ContentType::Json);
        assert_eq!(r.codec, "zstd");
        assert_eq!(r.orig_len, original.len());
        assert_eq!(r.hash, sha256_hex(original));

        // Exact round-trip.
        assert_eq!(load_blob(&db, &r.hash).await?, original);

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn identical_bytes_dedup_to_one_row() -> anyhow::Result<()> {
        let (db, path) = test_db().await?;

        let bytes = b"the same exact content stored twice";
        let a = store_blob(&db, bytes, None).await?;
        let b = store_blob(&db, bytes, None).await?;
        assert_eq!(a.hash, b.hash, "same content => same hash");

        // Single row, refcount bumped to 2 by the second store.
        let row = get_blob(&db, &a.hash).await?.expect("row exists");
        assert_eq!(row.refcount, 2);
        assert_eq!(load_blob(&db, &a.hash).await?, bytes);

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn tampered_row_fails_hash_verification() -> anyhow::Result<()> {
        let (db, path) = test_db().await?;

        // Store the compressed bytes of "B" under the content address of "A".
        // load_blob will decompress to "B", whose hash != key => must error.
        let key = sha256_hex(b"A");
        let wrong = codec_for(ContentType::Text).compress(b"B")?;
        insert_blob(&db, &key, "text", "zstd", 1, wrong.len() as i64, &wrong).await?;

        let err = load_blob(&db, &key).await.unwrap_err();
        assert!(
            err.to_string().contains("integrity check failed"),
            "expected hash-mismatch error, got: {err}"
        );

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn missing_blob_errors() -> anyhow::Result<()> {
        let (db, path) = test_db().await?;
        assert!(load_blob(&db, "deadbeef").await.is_err());
        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
