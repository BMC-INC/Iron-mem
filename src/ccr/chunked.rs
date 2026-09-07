//! Flat, versioned FastCDC objects. Ownership edges, not chunk refcounts, drive GC.
use super::{sha256_hex, BlobRef};
use crate::db::{BlobRow, Database};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::BTreeMap;

pub const FORMAT: &str = "fastcdc-v1";
const ALGORITHM: &str = "fastcdc-4.0.1-v2020-normalization1-seed0";
const MIN: u32 = 16_384;
const AVG: u32 = 65_536;
const MAX: u32 = 262_144;
const MAX_OBJECT: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    hash: String,
    length: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    algorithm: String,
    min: u32,
    avg: u32,
    max: u32,
    original_hash: String,
    original_length: usize,
    chunks: Vec<Chunk>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    manifest: Manifest,
    manifest_hash: String,
}

type EncodedObject = (Vec<u8>, BTreeMap<String, Vec<u8>>);

fn encode(bytes: &[u8]) -> Result<EncodedObject> {
    ensure!(
        bytes.len() <= MAX_OBJECT,
        "CCR chunked object exceeds 512 MiB safety limit"
    );
    let mut chunks = Vec::new();
    let mut unique = BTreeMap::new();
    for cut in fastcdc::v2020::FastCDC::new(bytes, MIN as usize, AVG as usize, MAX as usize) {
        let original = &bytes[cut.offset..cut.offset + cut.length];
        let hash = sha256_hex(original);
        if !unique.contains_key(&hash) {
            unique.insert(
                hash.clone(),
                super::codec_for(super::ContentType::Binary).compress(original)?,
            );
        }
        chunks.push(Chunk {
            hash,
            length: original.len(),
        });
    }
    let manifest = Manifest {
        version: 1,
        algorithm: ALGORITHM.into(),
        min: MIN,
        avg: AVG,
        max: MAX,
        original_hash: sha256_hex(bytes),
        original_length: bytes.len(),
        chunks,
    };
    let manifest_hash = sha256_hex(&serde_json::to_vec(&manifest)?);
    Ok((
        serde_json::to_vec(&Envelope {
            manifest,
            manifest_hash,
        })?,
        unique,
    ))
}

fn decode(row: &BlobRow) -> Result<Manifest> {
    ensure!(row.data.len() <= 8 * 1024 * 1024, "oversized CCR manifest");
    let envelope: Envelope = serde_json::from_slice(&row.data)?;
    let m = envelope.manifest;
    ensure!(
        sha256_hex(&serde_json::to_vec(&m)?) == envelope.manifest_hash,
        "CCR manifest hash mismatch"
    );
    ensure!(
        m.version == 1 && m.algorithm == ALGORITHM && (m.min, m.avg, m.max) == (MIN, AVG, MAX),
        "unsupported CCR chunk format"
    );
    ensure!(
        m.original_hash == row.hash
            && m.original_length <= MAX_OBJECT
            && row.orig_len == m.original_length as i64,
        "CCR object identity mismatch"
    );
    ensure!(
        m.chunks.len() <= MAX_OBJECT / MIN as usize + 1,
        "too many CCR chunks"
    );
    let mut total = 0usize;
    for c in &m.chunks {
        ensure!(
            c.hash.len() == 64
                && c.hash.bytes().all(|b| b.is_ascii_hexdigit())
                && c.length > 0
                && c.length <= MAX as usize,
            "invalid CCR chunk"
        );
        total = total
            .checked_add(c.length)
            .ok_or_else(|| anyhow::anyhow!("CCR length overflow"))?;
    }
    ensure!(total == m.original_length, "CCR manifest length mismatch");
    Ok(m)
}

/// Publication and ownership insertion commit together. Existing objects retain their format.
pub async fn store(db: &Database, bytes: &[u8], hint: Option<&str>) -> Result<BlobRef> {
    let (data, unique) = encode(bytes)?;
    let hash = sha256_hex(bytes);
    let content_type = super::detect::detect(bytes, hint);
    let mut tx = crate::db::begin_write(db).await?;
    let inserted = sqlx::query("INSERT INTO blobs(hash,content_type,codec,orig_len,comp_len,data,refcount,created_at,dict_hash) VALUES($1,$2,$3,$4,$5,$6,1,$7,NULL) ON CONFLICT(hash) DO NOTHING")
        .bind(&hash).bind(content_type.as_str()).bind(FORMAT).bind(bytes.len() as i64).bind(data.len() as i64).bind(&data).bind(chrono::Utc::now().timestamp()).execute(&mut *tx).await?.rows_affected() == 1;
    if inserted {
        for (chunk_hash, compressed) in unique {
            sqlx::query(
                "INSERT INTO ccr_chunks(hash,data) VALUES($1,$2) ON CONFLICT(hash) DO NOTHING",
            )
            .bind(&chunk_hash)
            .bind(compressed)
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO ccr_object_chunks(object_hash,chunk_hash) VALUES($1,$2)")
                .bind(&hash)
                .bind(chunk_hash)
                .execute(&mut *tx)
                .await?;
        }
    } else {
        sqlx::query("UPDATE blobs SET refcount=refcount+1 WHERE hash=$1")
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
    }
    let row = sqlx::query("SELECT codec,comp_len FROM blobs WHERE hash=$1")
        .bind(&hash)
        .fetch_one(&mut *tx)
        .await?;
    let codec: String = row.get("codec");
    let comp_len: i64 = row.get("comp_len");
    tx.commit().await?;
    Ok(BlobRef {
        hash,
        content_type,
        codec: match codec.as_str() {
            FORMAT => FORMAT,
            "dict+zstd" => "dict+zstd",
            "zstd" => "zstd",
            _ => anyhow::bail!("unsupported stored codec {codec}"),
        },
        orig_len: bytes.len(),
        comp_len: comp_len as usize,
    })
}

pub async fn load(db: &Database, row: &BlobRow) -> Result<Vec<u8>> {
    let manifest = decode(row)?;
    let mut tx = db.pool.begin().await?;
    let rows = sqlx::query("SELECT c.hash,c.data FROM ccr_chunks c JOIN ccr_object_chunks o ON o.chunk_hash=c.hash WHERE o.object_hash=$1")
        .bind(&row.hash).fetch_all(&mut *tx).await?;
    let compressed: BTreeMap<String, Vec<u8>> = rows
        .into_iter()
        .map(|r| (r.get("hash"), r.get("data")))
        .collect();
    tx.commit().await?;
    let mut original = Vec::with_capacity(manifest.original_length);
    for c in manifest.chunks {
        let data = compressed
            .get(&c.hash)
            .ok_or_else(|| anyhow::anyhow!("missing CCR chunk {}", c.hash))?;
        let chunk = zstd::bulk::decompress(data, c.length)?;
        ensure!(
            chunk.len() == c.length && sha256_hex(&chunk) == c.hash,
            "CCR chunk integrity mismatch"
        );
        original.extend_from_slice(&chunk);
    }
    ensure!(
        original.len() == manifest.original_length && sha256_hex(&original) == row.hash,
        "CCR original integrity mismatch"
    );
    Ok(original)
}

/// Resumable conversion: every object is verified before one transactional replacement.
/// Dry-run reports candidates without changing ownership or representation.
pub async fn convert(db: &Database, threshold: usize, apply: bool) -> Result<serde_json::Value> {
    ensure!(
        threshold >= 128 * 1024,
        "conversion threshold must be at least 128 KiB"
    );
    let rows =
        sqlx::query("SELECT hash FROM blobs WHERE codec <> $1 AND orig_len >= $2 ORDER BY hash")
            .bind(FORMAT)
            .bind(threshold as i64)
            .fetch_all(&db.pool)
            .await?;
    let mut converted = 0;
    for row in &rows {
        if !apply {
            continue;
        }
        let hash: String = row.get("hash");
        let bytes = super::load_blob(db, &hash).await?;
        let (data, unique) = encode(&bytes)?;
        let mut tx = crate::db::begin_write(db).await?;
        let changed = sqlx::query("UPDATE blobs SET codec=$1,data=$2,comp_len=$3,dict_hash=NULL WHERE hash=$4 AND codec <> $1")
            .bind(FORMAT).bind(&data).bind(data.len() as i64).bind(&hash).execute(&mut *tx).await?.rows_affected();
        if changed == 1 {
            for (chunk_hash, compressed) in unique {
                sqlx::query(
                    "INSERT INTO ccr_chunks(hash,data) VALUES($1,$2) ON CONFLICT(hash) DO NOTHING",
                )
                .bind(&chunk_hash)
                .bind(compressed)
                .execute(&mut *tx)
                .await?;
                sqlx::query("INSERT INTO ccr_object_chunks(object_hash,chunk_hash) VALUES($1,$2)")
                    .bind(&hash)
                    .bind(chunk_hash)
                    .execute(&mut *tx)
                    .await?;
            }
            converted += 1;
        }
        tx.commit().await?;
        ensure!(
            super::load_blob(db, &hash).await? == bytes,
            "converted source mismatch"
        );
    }
    Ok(
        serde_json::json!({"candidates":rows.len(),"converted":converted,"dry_run":!apply,"threshold_bytes":threshold}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shared_chunks_recover_after_gc_and_conversion_resumes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Database::new(dir.path().join("test.db").to_str().unwrap()).await?;
        db.migrate().await?;
        let bytes = crate::density::corpus(42).remove(2);
        let mut edited = bytes.clone();
        edited.splice(160_000..160_000, b"insertion".iter().copied());
        let a = store(&db, &bytes, None).await?;
        let b = store(&db, &edited, None).await?;
        assert_eq!(super::super::load_blob(&db, &a.hash).await?, bytes);
        let sharing: i64 = sqlx::query("SELECT COUNT(*) AS n FROM ccr_object_chunks a JOIN ccr_object_chunks b ON a.chunk_hash=b.chunk_hash WHERE a.object_hash=$1 AND b.object_hash=$2").bind(&a.hash).bind(&b.hash).fetch_one(&db.pool).await?.get("n");
        assert!(sharing > 0);
        crate::db::decref_blob(&db, &a.hash).await?;
        crate::db::gc_blobs(&db).await?;
        assert_eq!(super::super::load_blob(&db, &b.hash).await?, edited);
        let legacy = super::super::store_blob(&db, &bytes, None).await?;
        let before = crate::db::get_blob(&db, &legacy.hash)
            .await?
            .unwrap()
            .refcount;
        convert(&db, 128 * 1024, true).await?;
        assert_eq!(convert(&db, 128 * 1024, true).await?["converted"], 0);
        assert_eq!(
            crate::db::get_blob(&db, &legacy.hash)
                .await?
                .unwrap()
                .refcount,
            before
        );
        assert_eq!(super::super::load_blob(&db, &legacy.hash).await?, bytes);
        sqlx::query("UPDATE ccr_chunks SET data=$1")
            .bind(b"corrupt".to_vec())
            .execute(&db.pool)
            .await?;
        assert!(super::super::load_blob(&db, &b.hash).await.is_err());
        db.pool.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn publication_is_atomic_and_concurrent_stores_count_once_each() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Database::new(dir.path().join("race.db").to_str().unwrap()).await?;
        db.migrate().await?;
        let bytes = crate::density::corpus(137).remove(2);
        sqlx::query("CREATE TRIGGER fail_chunk BEFORE INSERT ON ccr_chunks BEGIN SELECT RAISE(ABORT,'injected failure'); END").execute(&db.pool).await?;
        assert!(store(&db, &bytes, None).await.is_err());
        assert!(crate::db::get_blob(&db, &sha256_hex(&bytes))
            .await?
            .is_none());
        sqlx::query("DROP TRIGGER fail_chunk")
            .execute(&db.pool)
            .await?;
        let (a, b) = tokio::join!(store(&db, &bytes, None), store(&db, &bytes, None));
        let a = a?;
        let b = b?;
        assert_eq!(a.hash, b.hash);
        assert_eq!(
            crate::db::get_blob(&db, &a.hash).await?.unwrap().refcount,
            2
        );
        assert_eq!(super::super::load_blob(&db, &a.hash).await?, bytes);
        db.pool.close().await;
        Ok(())
    }

    #[test]
    fn manifests_are_deterministic_and_tampering_is_rejected() -> Result<()> {
        let (a, _) = encode("Unicode ✓".as_bytes())?;
        assert_eq!(a, encode("Unicode ✓".as_bytes())?.0);
        assert_eq!(
            sha256_hex(&a),
            "f450661e9854de0ed856999fc1af236820952e04870be0f720ed2e2c2a69a40c"
        );
        let mut envelope: Envelope = serde_json::from_slice(&a)?;
        envelope.manifest.original_length += 1;
        let row = BlobRow {
            hash: envelope.manifest.original_hash.clone(),
            content_type: "text".into(),
            codec: FORMAT.into(),
            orig_len: 11,
            comp_len: 0,
            data: serde_json::to_vec(&envelope)?,
            refcount: 1,
            created_at: 0,
            dict_hash: None,
        };
        assert!(decode(&row).is_err());
        Ok(())
    }
}
