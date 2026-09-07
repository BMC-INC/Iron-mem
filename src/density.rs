//! Reproducible local storage/context frontier. Unmeasured quantities stay null.
use crate::{
    ccr,
    db::{self, Database, Memory},
    hooks,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{path::Path, time::Instant};

pub const BUDGETS: [usize; 7] = [2_000, 4_000, 8_000, 16_000, 24_000, 48_000, 96_000];

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    pub code_sha: String,
    pub source_sha256: String,
    pub dataset_sha256: String,
    pub seed: u64,
    pub budgets: Vec<usize>,
    pub storage_mode: String,
    pub hardware: String,
    pub cache_condition: String,
    pub tokenizer: Option<String>,
    pub answer_model: Option<String>,
    pub judge_model: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Measurement {
    pub budget_bytes: usize,
    pub injected_bytes: usize,
    pub injected_memories: usize,
    pub retrieval_payload_bytes: usize,
    pub expansion_bytes: usize,
    pub total_exposure_bytes: usize,
    pub injected_tokens: Option<usize>,
    pub accuracy: Option<f64>,
    pub storage_read_bytes: Option<usize>,
    pub exact_recovery: bool,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub manifest: Manifest,
    pub logical_original_bytes: usize,
    pub unique_original_bytes: i64,
    pub unique_payload_bytes: i64,
    pub dictionary_bytes: i64,
    pub chunk_payload_bytes: i64,
    pub manifest_bytes: i64,
    pub database_bytes: u64,
    pub database_overhead_bytes: u64,
    pub store_wall_ms: u128,
    pub load_p50_us: u128,
    pub load_p95_us: u128,
    pub cpu_ms: Option<u128>,
    pub peak_memory_bytes: Option<u64>,
    pub measurements: Vec<Measurement>,
}

pub fn corpus(seed: u64) -> Vec<Vec<u8>> {
    let mut state = seed;
    let mut random = Vec::with_capacity(1_048_576);
    for _ in 0..1_048_576 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        random.push(state as u8);
    }
    let logs = (0..12_000)
        .map(|i| {
            format!(
                "event={i} tool=build project=density status=success detail=repeatable workload\n"
            )
        })
        .collect::<String>()
        .into_bytes();
    let mut edited = random.clone();
    edited.splice(170_000..170_000, b"an inserted region".iter().copied());
    vec![
        logs.clone(),
        logs,
        random.clone(),
        random,
        edited,
        b"small Unicode source: \xe2\x9c\x93".to_vec(),
    ]
}

fn percentile(samples: &mut [u128], percent: usize) -> u128 {
    samples.sort_unstable();
    samples[(samples.len().saturating_sub(1) * percent) / 100]
}

pub async fn run(out: &Path) -> Result<()> {
    let seed = 0x49524f4e_u64;
    let sources = corpus(seed);
    let dataset = serde_json::to_vec(&sources)?;
    let code_sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()?;
    ensure!(
        code_sha.status.success(),
        "cannot identify benchmark code revision"
    );
    let manifest = Manifest {
        version: 2,
        source_sha256: env!("IRONMEM_SOURCE_SHA").into(),
        code_sha: String::from_utf8(code_sha.stdout)?.trim().into(),
        dataset_sha256: format!("{:x}", Sha256::digest(&dataset)),
        seed,
        budgets: BUDGETS.to_vec(),
        storage_mode: format!(
            "fastcdc_threshold={:?}; zstd=19",
            crate::config::ccr_chunk_threshold()?
        ),
        hardware: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        cache_condition:
            "fresh database; first read then warm repeated reads; OS cache uncontrolled".into(),
        tokenizer: None,
        answer_model: None,
        judge_model: None,
    };
    std::fs::create_dir_all(out)?;
    let manifest_path = out.join("manifest.json");
    if manifest_path.exists() {
        let prior: Manifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        ensure!(
            prior == manifest,
            "density resume identity mismatch; use a new output directory"
        );
    }
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("density.db");
    let db = Database::new(path.to_str().unwrap()).await?;
    db.migrate().await?;
    let mut memories = Vec::new();
    let mut hashes = Vec::new();
    let started = Instant::now();
    for (index, source) in sources.iter().enumerate() {
        let blob = ccr::store_blob(&db, source, None).await?;
        hashes.push(blob.hash.clone());
        let session = db::create_session(&db, "/density").await?;
        let summary = format!(
            "Density source {index}: build event workload. {}",
            "repeatable context ✓ ".repeat(250)
        );
        let id =
            db::insert_memory(&db, "/density", &session, &summary, Some("build workload")).await?;
        db::set_memory_session_blob(&db, id, &blob.hash).await?;
        memories.push(Memory {
            id,
            project: "/density".into(),
            session_id: session,
            summary,
            tags: None,
            created_at: 0,
        });
    }
    let store_wall_ms = started.elapsed().as_millis();
    let mut loads = Vec::new();
    for _ in 0..3 {
        for (hash, source) in hashes.iter().zip(&sources) {
            let started = Instant::now();
            ensure!(
                ccr::load_blob(&db, hash).await? == *source,
                "source reconstruction mismatch"
            );
            loads.push(started.elapsed().as_micros());
        }
    }
    let mut measurements = Vec::new();
    for budget in BUDGETS {
        let (text, report) = hooks::render_memories(&memories, budget);
        ensure!(text.len() <= budget, "context exceeded budget");
        // Fixed query and top-k across budgets. Gold answers never enter retrieval.
        let hits = db::search_memories(&db, "/density", "build", 3).await?;
        let retrieval_payload_bytes = serde_json::to_vec(&hits)?.len();
        let mut expansion_bytes = 0;
        for hit in hits {
            let expanded =
                crate::expansion::retrieve_original(&db, None, Some(hit.id), None, None).await?;
            expansion_bytes += serde_json::to_vec(&expanded)?.len();
        }
        measurements.push(Measurement {
            budget_bytes: budget,
            injected_bytes: text.len(),
            injected_memories: report.written_ids.len(),
            retrieval_payload_bytes,
            expansion_bytes,
            total_exposure_bytes: text.len() + retrieval_payload_bytes + expansion_bytes,
            injected_tokens: None,
            accuracy: None,
            storage_read_bytes: None,
            exact_recovery: true,
        });
    }
    let sizes = sqlx::query("SELECT COALESCE(SUM(orig_len),0) AS original, COALESCE(SUM(comp_len),0) AS payload FROM blobs").fetch_one(&db.pool).await?;
    let unique_original_bytes: i64 = sizes.get("original");
    let object_payload_bytes: i64 = sizes.get("payload");
    let dictionary_bytes: i64 =
        sqlx::query("SELECT COALESCE(SUM(length(data)),0) AS n FROM ccr_dicts")
            .fetch_one(&db.pool)
            .await?
            .get("n");
    let chunk_payload_bytes: i64 =
        sqlx::query("SELECT COALESCE(SUM(length(data)),0) AS n FROM ccr_chunks")
            .fetch_one(&db.pool)
            .await?
            .get("n");
    let unique_payload_bytes = object_payload_bytes + chunk_payload_bytes;
    let manifest_bytes: i64 =
        sqlx::query("SELECT COALESCE(SUM(comp_len),0) AS n FROM blobs WHERE codec='fastcdc-v1'")
            .fetch_one(&db.pool)
            .await?
            .get("n");
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&db.pool)
        .await?;
    db.pool.close().await;
    let database_bytes = std::fs::metadata(&path)?.len();
    let report = Report {
        manifest,
        logical_original_bytes: sources.iter().map(Vec::len).sum(),
        unique_original_bytes,
        unique_payload_bytes,
        dictionary_bytes,
        chunk_payload_bytes,
        manifest_bytes,
        database_bytes,
        database_overhead_bytes: database_bytes
            .saturating_sub((unique_payload_bytes + dictionary_bytes) as u64),
        store_wall_ms,
        load_p50_us: percentile(&mut loads, 50),
        load_p95_us: percentile(&mut loads, 95),
        cpu_ms: None,
        peak_memory_bytes: None,
        measurements,
    };
    std::fs::write(out.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    let mut csv = "budget_bytes,injected_bytes,injected_memories,retrieval_payload_bytes,expansion_bytes,total_exposure_bytes,accuracy\n".to_string();
    for m in &report.measurements {
        csv.push_str(&format!(
            "{},{},{},{},{},{},\n",
            m.budget_bytes,
            m.injected_bytes,
            m.injected_memories,
            m.retrieval_payload_bytes,
            m.expansion_bytes,
            m.total_exposure_bytes
        ));
    }
    std::fs::write(out.join("frontier.csv"), csv)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn local_frontier_accounts_for_all_exposure_and_rejects_resume_drift() -> Result<()> {
        let out = tempfile::tempdir()?;
        run(out.path()).await?;
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.path().join("report.json"))?)?;
        assert_eq!(value["measurements"].as_array().unwrap().len(), 7);
        for m in value["measurements"].as_array().unwrap() {
            assert!(m["accuracy"].is_null());
            assert!(m["injected_bytes"].as_u64() <= m["budget_bytes"].as_u64());
            assert_eq!(
                m["total_exposure_bytes"].as_u64().unwrap(),
                m["injected_bytes"].as_u64().unwrap()
                    + m["retrieval_payload_bytes"].as_u64().unwrap()
                    + m["expansion_bytes"].as_u64().unwrap()
            );
        }
        let path = out.path().join("manifest.json");
        let mut manifest: Manifest = serde_json::from_slice(&std::fs::read(&path)?)?;
        manifest.seed += 1;
        std::fs::write(path, serde_json::to_vec(&manifest)?)?;
        assert!(run(out.path()).await.is_err());
        Ok(())
    }
}

/// LoCoMo categories 1-4 remain separate; adversarial category 5 is excluded
/// from the headline denominator, matching the existing external harness.
pub fn load_locomo(path: &Path) -> Result<Vec<crate::bench::LmeQuestion>> {
    use crate::bench::{LmeQuestion, LmeTurn};
    let conversations: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut questions = Vec::new();
    for (index, raw) in conversations.iter().enumerate() {
        let conversation = raw["conversation"]
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("missing LoCoMo conversation"))?;
        let mut numbers = conversation
            .keys()
            .filter_map(|key| {
                key.strip_prefix("session_")
                    .and_then(|n| n.parse::<usize>().ok())
            })
            .collect::<Vec<_>>();
        numbers.sort_unstable();
        let mut sessions = Vec::new();
        let mut dates = Vec::new();
        for n in numbers {
            let turns = conversation[&format!("session_{n}")]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid LoCoMo session"))?;
            sessions.push(
                turns
                    .iter()
                    .map(|t| {
                        let mut content = t["text"].as_str().unwrap_or("").to_string();
                        if let Some(caption) = t
                            .get("blip_caption")
                            .or_else(|| t.get("caption"))
                            .and_then(|v| v.as_str())
                        {
                            content.push_str(&format!(" [shared an image: {caption}]"));
                        }
                        LmeTurn {
                            role: t["speaker"].as_str().unwrap_or("").into(),
                            content,
                        }
                    })
                    .collect::<Vec<_>>(),
            );
            dates.push(
                conversation
                    .get(&format!("session_{n}_date_time"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
        }
        for (q, qa) in raw["qa"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing LoCoMo QA"))?
            .iter()
            .enumerate()
        {
            let category = qa["category"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("invalid LoCoMo category"))?;
            let kind = match category {
                1 => "locomo-multi-hop",
                2 => "locomo-temporal",
                3 => "locomo-open-domain",
                4 => "locomo-single-hop",
                5 => continue,
                _ => anyhow::bail!("unknown LoCoMo category"),
            };
            let mut answer = qa["answer"].clone();
            if category == 3 {
                if let Some(text) = answer.as_str() {
                    answer = serde_json::json!(text.split(';').next().unwrap_or("").trim());
                }
            }
            questions.push(LmeQuestion {
                question_id: format!("locomo-{index}-{q}"),
                question_type: kind.into(),
                question: qa["question"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing question"))?
                    .into(),
                answer,
                question_date: None,
                haystack_dates: dates.clone(),
                haystack_sessions: sessions.clone(),
            });
        }
    }
    ensure!(!questions.is_empty(), "no scored LoCoMo questions");
    Ok(questions)
}
