use anyhow::Result;
use serde::Serialize;

use crate::db::{self, Database};

#[derive(Debug, Clone, Serialize)]
pub struct ExpandedOriginal {
    pub hash: Option<String>,
    pub bytes: usize,
    pub original: String,
    pub chunk_id: Option<String>,
    pub memory_id: Option<i64>,
    pub source_start: Option<i64>,
    pub source_end: Option<i64>,
}

pub async fn retrieve_original(
    db: &Database,
    observation_id: Option<i64>,
    memory_id: Option<i64>,
    hash: Option<&str>,
    chunk_id: Option<&str>,
) -> Result<ExpandedOriginal> {
    if let Some(cid) = chunk_id {
        return retrieve_chunk(db, cid).await;
    }

    let resolved_hash = if let Some(h) = hash {
        h.to_string()
    } else if let Some(oid) = observation_id {
        db::get_observation_output_blob(db, oid)
            .await?
            .ok_or_else(|| anyhow::anyhow!("observation {oid} has no stored original"))?
    } else if let Some(mid) = memory_id {
        db::get_memory_session_blob(db, mid)
            .await?
            .ok_or_else(|| anyhow::anyhow!("memory {mid} has no stored session transcript"))?
    } else {
        anyhow::bail!("provide 'observation_id', 'memory_id', 'hash', or 'chunk_id'");
    };

    let bytes = crate::ccr::load_blob(db, &resolved_hash).await?;
    Ok(ExpandedOriginal {
        hash: Some(resolved_hash),
        bytes: bytes.len(),
        original: String::from_utf8_lossy(&bytes).into_owned(),
        chunk_id: None,
        memory_id,
        source_start: None,
        source_end: None,
    })
}

async fn retrieve_chunk(db: &Database, chunk_id: &str) -> Result<ExpandedOriginal> {
    let chunk = db::get_memory_chunk(db, chunk_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("memory chunk not found: {chunk_id}"))?;

    if let Some(hash) = chunk.source_hash.as_deref() {
        let bytes = crate::ccr::load_blob(db, hash).await?;
        if let (Some(start), Some(end)) = (chunk.source_start, chunk.source_end) {
            let start = start.max(0) as usize;
            let end = end.max(start as i64) as usize;
            if end <= bytes.len() {
                let slice = &bytes[start..end];
                return Ok(ExpandedOriginal {
                    hash: Some(hash.to_string()),
                    bytes: slice.len(),
                    original: String::from_utf8_lossy(slice).into_owned(),
                    chunk_id: Some(chunk.chunk_id),
                    memory_id: Some(chunk.memory_id),
                    source_start: chunk.source_start,
                    source_end: chunk.source_end,
                });
            }
        }

        return Ok(ExpandedOriginal {
            hash: Some(hash.to_string()),
            bytes: bytes.len(),
            original: String::from_utf8_lossy(&bytes).into_owned(),
            chunk_id: Some(chunk.chunk_id),
            memory_id: Some(chunk.memory_id),
            source_start: None,
            source_end: None,
        });
    }

    if let Some(hash) = db::get_memory_session_blob(db, chunk.memory_id).await? {
        let bytes = crate::ccr::load_blob(db, &hash).await?;
        return Ok(ExpandedOriginal {
            hash: Some(hash),
            bytes: bytes.len(),
            original: String::from_utf8_lossy(&bytes).into_owned(),
            chunk_id: Some(chunk.chunk_id),
            memory_id: Some(chunk.memory_id),
            source_start: None,
            source_end: None,
        });
    }

    Ok(ExpandedOriginal {
        hash: None,
        bytes: chunk.summary.len(),
        original: chunk.summary,
        chunk_id: Some(chunk.chunk_id),
        memory_id: Some(chunk.memory_id),
        source_start: None,
        source_end: None,
    })
}
