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
pub mod dict;
pub mod zstd_codec;

pub use codec::{codec_by_id, codec_for, Codec};
pub use detect::ContentType;

use crate::db::{get_blob, get_dict, insert_blob, Database};
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

    // The plain zstd floor is the always-reversible baseline.
    let floor = codec_for(content_type);
    let mut codec_id = floor.id();
    let mut chosen = floor.compress(bytes)?;
    let mut chosen_dict: Option<String> = None;

    // Try a per-type dictionary; keep it only if it actually wins, so a
    // dict-compressed blob is never larger than the floor.
    if !matches!(content_type, ContentType::Binary) {
        if let Some((dict_hash, dict_bytes)) = dict::select_dict(db, content_type.as_str()).await? {
            let dcodec = dict::DictZstdCodec::new(dict_bytes, codec::ZSTD_LEVEL);
            if let Ok(dbytes) = dcodec.compress(bytes) {
                if dbytes.len() < chosen.len() {
                    codec_id = dcodec.id();
                    chosen = dbytes;
                    chosen_dict = Some(dict_hash);
                }
            }
        }
    }

    insert_blob(
        db,
        &hash,
        content_type.as_str(),
        codec_id,
        bytes.len() as i64,
        chosen.len() as i64,
        &chosen,
        chosen_dict.as_deref(),
    )
    .await?;
    Ok(BlobRef {
        hash,
        content_type,
        codec: codec_id,
        orig_len: bytes.len(),
        comp_len: chosen.len(),
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

    let original = if row.codec == "dict+zstd" {
        let dict_hash = row
            .dict_hash
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("CCR dict+zstd blob {hash} has no dict_hash"))?;
        let dict = get_dict(db, dict_hash).await?.ok_or_else(|| {
            anyhow::anyhow!("CCR dictionary {dict_hash} for blob {hash} not found")
        })?;
        dict::DictZstdCodec::new(dict, codec::ZSTD_LEVEL).decompress(&row.data)?
    } else {
        codec_by_id(&row.codec)?.decompress(&row.data)?
    };

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
        insert_blob(&db, &key, "text", "zstd", 1, wrong.len() as i64, &wrong, None).await?;

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

    #[tokio::test]
    async fn per_type_dict_kicks_in_and_round_trips() -> anyhow::Result<()> {
        let (db, path) = test_db().await?;

        // Below the sample threshold: floor only, no dictionary.
        let early = store_blob(&db, b"{\"i\":0,\"msg\":\"first\"}", Some("json")).await?;
        assert_eq!(early.codec, "zstd");
        assert!(get_blob(&db, &early.hash).await?.unwrap().dict_hash.is_none());

        // Store enough similar JSON records to trigger lazy dictionary training.
        let mut last_hash = early.hash.clone();
        let mut last_src = String::new();
        for i in 1..60 {
            last_src = format!("{{\"i\":{i},\"msg\":\"event number {i} happened\",\"ok\":true}}");
            last_hash = store_blob(&db, last_src.as_bytes(), Some("json")).await?.hash;
        }

        // A dictionary now exists for json and the latest blob used it...
        assert!(crate::db::latest_dict_hash(&db, "json").await?.is_some());
        let row = get_blob(&db, &last_hash).await?.unwrap();
        assert_eq!(row.codec, "dict+zstd");
        assert!(row.dict_hash.is_some());
        // ...and it still round-trips byte-exact through the dict decode path.
        assert_eq!(load_blob(&db, &last_hash).await?, last_src.as_bytes());

        // Binary content never takes the dict path.
        let bin = store_blob(&db, &[0u8, 1, 2, 3, 255, 254], Some("bin")).await?;
        assert_eq!(bin.codec, "zstd");

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "benchmark — run with `cargo test --bin ironmem -- --ignored bench_ccr`"]
    async fn bench_ccr_dict_vs_floor() -> anyhow::Result<()> {
        let (db, path) = test_db().await?;
        let floor = codec_for(ContentType::Json);

        let mut floor_total = 0usize;
        let mut stored_total = 0usize;
        for i in 0..500 {
            let s = format!(
                "{{\"ts\":\"2026-06-08T00:00:{:02}Z\",\"level\":\"INFO\",\"req\":{i},\"path\":\"/api/v1/items/{}\",\"ms\":{}}}",
                i % 60, i % 50, (i * 7) % 800
            );
            floor_total += floor.compress(s.as_bytes())?.len();
            stored_total += store_blob(&db, s.as_bytes(), Some("json")).await?.comp_len;
        }
        let pct = 100.0 * (1.0 - stored_total as f64 / floor_total as f64);
        eprintln!(
            "CCR json corpus: floor={floor_total} B, stored(dict-or-floor)={stored_total} B, saved={pct:.1}%"
        );
        assert!(stored_total <= floor_total, "store must never be worse than the floor");

        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
