//! Optional selection from an already relevant, authorized candidate pool.
//! No content cache: every injection re-reads policy and source handles.
use crate::{
    access,
    db::{Database, Memory},
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    Generic,
    Coding,
    Planning,
    Debug,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub profile: Profile,
    pub budget_bytes: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            profile: Profile::Generic,
            budget_bytes: 24_000,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (2_000..=24_000).contains(&self.budget_bytes),
            "working_set.budget_bytes must be 2000..=24000 inclusive"
        );
        Ok(())
    }
    pub fn candidate_limit(&self, limit: usize) -> usize {
        if self.enabled {
            limit.saturating_mul(4).clamp(16, 200)
        } else {
            limit
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Temperature {
    Cold,
    Warm,
    Hot,
}

/// Seven-day half-life and bounded demand. Injection alone never makes a memory hot.
/// Hysteresis: a peak qualifying as hot (>=4) stays hot down to 2; a warm peak
/// (>=1) stays warm down to 0.5. Time alone only cools, never promotes.
pub fn temperature(stats: &access::Stats, now: i64) -> Temperature {
    let latest = stats
        .last_recalled_at
        .into_iter()
        .chain(stats.last_expanded_at)
        .max();
    let Some(last) = latest else {
        return Temperature::Cold;
    };
    let peak =
        (stats.recall_count.max(0) as f64 + 2.0 * stats.expansion_count.max(0) as f64).min(8.0);
    let heat = peak * 2f64.powf(-(now.saturating_sub(last).max(0) as f64) / (7.0 * 86400.0));
    if peak >= 4.0 && heat >= 2.0 {
        Temperature::Hot
    } else if peak >= 1.0 && heat >= 0.5 {
        Temperature::Warm
    } else {
        Temperature::Cold
    }
}

/// Preserve relevance bands (four positions) and reserve one fitting entry per
/// useful kind. Temperature only breaks ties within a band; no memory is deleted.
pub async fn select(
    db: &Database,
    candidates: &[Memory],
    config: &Config,
    limit: usize,
    now: i64,
) -> Result<Vec<Memory>> {
    config.validate()?;
    if !config.enabled {
        return Ok(candidates.to_vec());
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    let ids = candidates.iter().map(|m| m.id).collect::<Vec<_>>();
    let stats = access::stats(db, &ids).await?;
    let mut pool = Vec::new();
    for (rank, memory) in candidates.iter().enumerate() {
        let meta = crate::db::get_memory_meta_full(db, memory.id).await?;
        // Defense in depth for expiry/deletion between ranking and selection.
        if meta.tombstoned_at.is_some() || meta.expires_at.is_some_and(|t| t <= now) {
            continue;
        }
        let temp = stats
            .get(&memory.id)
            .map(|s| temperature(s, now))
            .unwrap_or(Temperature::Cold);
        let correction = crate::db::contradiction_sets_for_memory(db, memory.id, &meta.namespace)
            .await?
            .iter()
            .any(|set| {
                matches!(
                    set.status,
                    crate::contradiction::ContradictionStatus::Unresolved
                        | crate::contradiction::ContradictionStatus::Preferred
                )
            });
        pool.push((rank, memory, meta.kind, temp, correction));
    }
    pool.sort_by_key(|(rank, _, _, temp, _)| (rank / 4, std::cmp::Reverse(*temp), *rank));
    let categories: &[&str] = match config.profile {
        Profile::Generic => &[
            "project_config",
            "architecture",
            "procedural",
            "error_solution",
        ],
        Profile::Coding => &[
            "project_config",
            "procedural",
            "error_solution",
            "architecture",
        ],
        Profile::Planning => &[
            "project_config",
            "architecture",
            "procedural",
            "error_solution",
        ],
        Profile::Debug => &[
            "error_solution",
            "project_config",
            "procedural",
            "architecture",
        ],
    };
    let mut chosen = Vec::new();
    // Always try the highest-ranked candidate first, retaining user-profile priority.
    if let Some(memory) = candidates.first() {
        if pool.iter().any(|(_, m, _, _, _)| m.id == memory.id) {
            try_add(&mut chosen, memory, limit, config.budget_bytes);
        }
    }
    // Unresolved contradictions/corrections have explicit capacity after authorization.
    if let Some((_, m, _, _, _)) = pool.iter().find(|(_, _, _, _, c)| *c) {
        try_add(&mut chosen, m, limit, config.budget_bytes);
    }
    for kind in categories {
        for (_, m, k, _, _) in &pool {
            if k == kind && try_add(&mut chosen, m, limit, config.budget_bytes) {
                break;
            }
        }
    }
    for (_, m, _, _, _) in pool {
        try_add(&mut chosen, m, limit, config.budget_bytes);
    }
    Ok(chosen)
}
fn try_add(chosen: &mut Vec<Memory>, memory: &Memory, limit: usize, budget: usize) -> bool {
    if chosen.len() >= limit || chosen.iter().any(|m| m.id == memory.id) {
        return false;
    }
    chosen.push(memory.clone());
    // Reserve the worst-case omission footer used when rendering the full pool.
    let (_, report) = crate::hooks::render_memories(chosen, budget.saturating_sub(128));
    if report.written_ids.len() != chosen.len() {
        chosen.pop();
        false
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn working_set_temperature_decays_with_hysteresis_and_ignores_injection() {
        let mut s = access::Stats {
            recall_count: 4,
            last_recalled_at: Some(100),
            ..Default::default()
        };
        assert_eq!(temperature(&s, 100), Temperature::Hot);
        assert_eq!(temperature(&s, 100 + 7 * 86400), Temperature::Hot);
        assert_eq!(temperature(&s, 101 + 7 * 86400), Temperature::Warm);
        assert_eq!(temperature(&s, 101 + 28 * 86400), Temperature::Cold);
        s.last_recalled_at = None;
        s.injection_count = 1_000_000;
        s.last_injected_at = Some(100);
        assert_eq!(temperature(&s, 100), Temperature::Cold);
        assert!(Config {
            budget_bytes: 24_001,
            ..Default::default()
        }
        .validate()
        .is_err());
    }
    #[tokio::test]
    async fn working_set_profiles_fit_and_keep_cold_memories_searchable() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Database::new(dir.path().join("set.db").to_str().unwrap()).await?;
        db.migrate().await?;
        let session = crate::db::create_session(&db, "p").await?;
        let mut memories = Vec::new();
        for kind in [
            "session",
            "project_config",
            "architecture",
            "procedural",
            "error_solution",
        ] {
            let id =
                crate::db::insert_memory(&db, "p", &session, &format!("{kind} fact"), None).await?;
            crate::db::set_memory_scope_kind(&db, id, "project", kind).await?;
            assert_eq!(crate::db::get_memory_meta_full(&db, id).await?.kind, kind);
            memories.push(crate::db::get_memory_by_id(&db, id).await?.unwrap());
        }
        for profile in [
            Profile::Generic,
            Profile::Coding,
            Profile::Planning,
            Profile::Debug,
        ] {
            for budget in [2000, 4000, 8000, 16000, 24000] {
                let config = Config {
                    enabled: true,
                    profile,
                    budget_bytes: budget,
                };
                let selected = select(&db, &memories, &config, 5, 100).await?;
                let (text, report) = crate::hooks::render_memories(&selected, budget);
                assert!(text.len() <= budget);
                assert_eq!(report.written_ids.len(), selected.len());
                assert!(!selected.is_empty());
                assert_eq!(selected.first().unwrap().id, memories[0].id);
                if budget == 24000 {
                    assert_eq!(selected.len(), 5, "all reserved kinds must survive");
                    if profile == Profile::Debug {
                        assert_eq!(selected[1].id, memories[4].id);
                    }
                    if profile == Profile::Planning {
                        assert_eq!(selected[1].id, memories[1].id);
                    }
                }
            }
        }
        assert_eq!(
            crate::db::get_recent_memories(&db, "p", 100).await?.len(),
            5
        );
        db.pool.close().await;
        Ok(())
    }
}
