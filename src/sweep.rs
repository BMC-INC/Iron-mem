use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;

use crate::config::{self, Config};
use crate::db::{self, Database, DreamCandidate, NewSweepEvent, SweepCandidate};
use crate::embedder::Embedder;
use crate::reflection;
use crate::vectorstore::{self, VectorStore};

#[derive(Debug, Clone)]
pub struct CompressionSweepOptions {
    pub compress_idle_secs: i64,
    pub min_observations: i64,
    pub limit: i64,
    pub dry_run: bool,
    pub lease_secs: i64,
}

#[derive(Debug, Clone)]
pub struct DreamSweepOptions {
    pub due_after_secs: i64,
    pub apply: bool,
    pub dry_run: bool,
    pub limit: i64,
}

#[derive(Debug, Clone, Default)]
pub struct SweepReport {
    pub dry_run: bool,
    pub candidates: usize,
    pub compressed: usize,
    pub dreamed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub actions: Vec<SweepAction>,
}

#[derive(Debug, Clone)]
pub struct SweepAction {
    pub session_id: Option<String>,
    pub project: String,
    pub idle_secs: Option<i64>,
    pub observation_count: Option<i64>,
    pub action: String,
    pub status: String,
    pub reason: String,
    pub detail: Option<String>,
}

#[async_trait]
pub trait SessionCompressor {
    async fn compress_session(&self, db: &Database, cfg: &Config, session_id: &str) -> Result<i64>;
}

pub struct RealSessionCompressor<'a> {
    pub embedder: Option<&'a dyn Embedder>,
    pub store: &'a dyn VectorStore,
}

#[async_trait]
impl SessionCompressor for RealSessionCompressor<'_> {
    async fn compress_session(&self, db: &Database, cfg: &Config, session_id: &str) -> Result<i64> {
        crate::compress::run(db, self.embedder, self.store, cfg, session_id).await
    }
}

impl CompressionSweepOptions {
    pub fn from_config(cfg: &Config, dry_run: bool) -> Self {
        Self {
            compress_idle_secs: (cfg.auto_compress.idle_minutes as i64).max(1) * 60,
            min_observations: cfg.auto_compress.min_observations.max(0),
            limit: cfg.auto_compress.limit.max(1),
            dry_run,
            lease_secs: (cfg.auto_compress.lease_minutes as i64).max(1) * 60,
        }
    }
}

pub fn parse_duration_secs(raw: &str) -> Result<i64> {
    let s = raw.trim();
    if s.is_empty() {
        anyhow::bail!("duration cannot be empty");
    }
    let (number, multiplier) = match s.chars().last().unwrap() {
        's' | 'S' => (&s[..s.len() - 1], 1),
        'm' | 'M' => (&s[..s.len() - 1], 60),
        'h' | 'H' => (&s[..s.len() - 1], 60 * 60),
        'd' | 'D' => (&s[..s.len() - 1], 24 * 60 * 60),
        c if c.is_ascii_digit() => (s, 1),
        _ => anyhow::bail!("duration must end with s, m, h, d, or be raw seconds"),
    };
    let value: i64 = number
        .parse()
        .with_context(|| format!("invalid duration value: {raw}"))?;
    if value <= 0 {
        anyhow::bail!("duration must be positive");
    }
    Ok(value * multiplier)
}

pub async fn run_compression_sweep(
    db: &Database,
    cfg: &Config,
    options: &CompressionSweepOptions,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
) -> Result<SweepReport> {
    let compressor = RealSessionCompressor { embedder, store };
    run_compression_sweep_with(db, cfg, options, &compressor).await
}

pub async fn run_compression_sweep_with<C: SessionCompressor + Sync>(
    db: &Database,
    cfg: &Config,
    options: &CompressionSweepOptions,
    compressor: &C,
) -> Result<SweepReport> {
    let now = Utc::now().timestamp();
    let idle_before = now - options.compress_idle_secs.max(1);
    let candidates =
        db::list_sweep_candidates(db, idle_before, options.min_observations, options.limit).await?;
    let mut report = SweepReport {
        dry_run: options.dry_run,
        candidates: candidates.len(),
        ..Default::default()
    };
    let owner = format!("ironmem-sweep-{}", uuid::Uuid::new_v4());

    for candidate in candidates {
        let idle_secs = now.saturating_sub(candidate.last_activity_at);
        let reason = candidate_reason(&candidate, idle_before, options.min_observations);
        if options.dry_run {
            report.actions.push(SweepAction {
                session_id: Some(candidate.session_id),
                project: candidate.project,
                idle_secs: Some(idle_secs),
                observation_count: Some(candidate.observation_count),
                action: "compress".to_string(),
                status: "dry_run".to_string(),
                reason,
                detail: None,
            });
            continue;
        }

        if !db::try_acquire_sweep_lease(
            db,
            &candidate.session_id,
            &owner,
            options.lease_secs.max(60),
        )
        .await?
        {
            report.skipped += 1;
            let action = SweepAction {
                session_id: Some(candidate.session_id),
                project: candidate.project,
                idle_secs: Some(idle_secs),
                observation_count: Some(candidate.observation_count),
                action: "compress".to_string(),
                status: "skipped".to_string(),
                reason: "leased".to_string(),
                detail: Some("another sweeper owns the session lease".to_string()),
            };
            record_action(db, &action, false, Some(candidate.last_activity_at)).await?;
            report.actions.push(action);
            continue;
        }

        let action = run_one_compression(db, cfg, compressor, &candidate, &reason, idle_secs).await;
        let release_result = db::release_sweep_lease(db, &candidate.session_id, &owner).await;
        if let Err(e) = release_result {
            tracing::warn!(
                "sweep: failed to release lease for {}: {e}",
                candidate.session_id
            );
        }

        match action {
            Ok(action) => {
                if action.status == "success" {
                    report.compressed += 1;
                } else if action.status == "skipped" {
                    report.skipped += 1;
                }
                record_action(db, &action, false, Some(candidate.last_activity_at)).await?;
                report.actions.push(action);
            }
            Err(e) => {
                report.failed += 1;
                let action = SweepAction {
                    session_id: Some(candidate.session_id),
                    project: candidate.project,
                    idle_secs: Some(idle_secs),
                    observation_count: Some(candidate.observation_count),
                    action: "compress".to_string(),
                    status: "failed".to_string(),
                    reason,
                    detail: Some(e.to_string()),
                };
                record_action(db, &action, false, Some(candidate.last_activity_at)).await?;
                report.actions.push(action);
            }
        }
    }

    Ok(report)
}

async fn run_one_compression<C: SessionCompressor + Sync>(
    db: &Database,
    cfg: &Config,
    compressor: &C,
    candidate: &SweepCandidate,
    reason: &str,
    idle_secs: i64,
) -> Result<SweepAction> {
    match db::get_session(db, &candidate.session_id).await? {
        Some(session) if session.compressed => {
            return Ok(SweepAction {
                session_id: Some(candidate.session_id.clone()),
                project: candidate.project.clone(),
                idle_secs: Some(idle_secs),
                observation_count: Some(candidate.observation_count),
                action: "compress".to_string(),
                status: "skipped".to_string(),
                reason: "already_compressed".to_string(),
                detail: None,
            });
        }
        Some(_) => {}
        None => {
            return Ok(SweepAction {
                session_id: Some(candidate.session_id.clone()),
                project: candidate.project.clone(),
                idle_secs: Some(idle_secs),
                observation_count: Some(candidate.observation_count),
                action: "compress".to_string(),
                status: "skipped".to_string(),
                reason: "missing_session".to_string(),
                detail: None,
            });
        }
    }

    db::end_session_if_open(db, &candidate.session_id).await?;
    let memory_id = compressor
        .compress_session(db, cfg, &candidate.session_id)
        .await?;
    Ok(SweepAction {
        session_id: Some(candidate.session_id.clone()),
        project: candidate.project.clone(),
        idle_secs: Some(idle_secs),
        observation_count: Some(candidate.observation_count),
        action: "compress".to_string(),
        status: "success".to_string(),
        reason: reason.to_string(),
        detail: Some(format!("memory_id={memory_id}")),
    })
}

fn candidate_reason(candidate: &SweepCandidate, idle_before: i64, min_observations: i64) -> String {
    let mut reasons = Vec::new();
    if candidate.last_activity_at <= idle_before {
        reasons.push("idle");
    }
    if candidate.observation_count >= min_observations.max(0) {
        reasons.push("volume");
    }
    if reasons.is_empty() {
        "candidate".to_string()
    } else {
        reasons.join(",")
    }
}

pub async fn run_dream_sweep(
    db: &Database,
    cfg: &Config,
    options: &DreamSweepOptions,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
) -> Result<SweepReport> {
    let now = Utc::now().timestamp();
    let due_before = now - options.due_after_secs.max(1);
    let candidates = db::list_dream_candidates(db, due_before, options.limit).await?;
    let mut report = SweepReport {
        dry_run: options.dry_run,
        candidates: candidates.len(),
        ..Default::default()
    };

    for candidate in candidates {
        let idle_secs = now.saturating_sub(candidate.last_activity);
        let action = if options.dry_run {
            SweepAction {
                session_id: None,
                project: candidate.project.clone(),
                idle_secs: Some(idle_secs),
                observation_count: None,
                action: "dream".to_string(),
                status: "dry_run".to_string(),
                reason: "dream_due".to_string(),
                detail: Some(format!("memories={}", candidate.memory_count)),
            }
        } else {
            match run_one_dream(db, cfg, options, embedder, store, &candidate).await {
                Ok(detail) => {
                    report.dreamed += 1;
                    SweepAction {
                        session_id: None,
                        project: candidate.project.clone(),
                        idle_secs: Some(idle_secs),
                        observation_count: None,
                        action: "dream".to_string(),
                        status: "success".to_string(),
                        reason: "dream_due".to_string(),
                        detail: Some(detail),
                    }
                }
                Err(e) => {
                    report.failed += 1;
                    SweepAction {
                        session_id: None,
                        project: candidate.project.clone(),
                        idle_secs: Some(idle_secs),
                        observation_count: None,
                        action: "dream".to_string(),
                        status: "failed".to_string(),
                        reason: "dream_due".to_string(),
                        detail: Some(e.to_string()),
                    }
                }
            }
        };

        if !options.dry_run {
            record_action(db, &action, false, Some(candidate.last_activity)).await?;
            let payload = serde_json::json!({
                "project": candidate.project,
                "trigger_reason": "daily",
                "idle_seconds": idle_secs,
                "apply": options.apply,
                "status": action.status,
                "detail": action.detail,
            })
            .to_string();
            let _ = db::append_memory_ledger(
                db,
                crate::governance::DEFAULT_NAMESPACE,
                None,
                "sleep_cycle_dream",
                Some("ironmem:scheduler"),
                &payload,
            )
            .await;
        }
        report.actions.push(action);
    }

    Ok(report)
}

async fn run_one_dream(
    db: &Database,
    cfg: &Config,
    options: &DreamSweepOptions,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    candidate: &DreamCandidate,
) -> Result<String> {
    let reflection = reflection::run(
        db,
        embedder,
        store,
        Some(&candidate.project),
        false,
        options.apply,
        options.limit,
    )
    .await?;

    let synthesis = reflection::synthesize(
        db,
        embedder,
        store,
        cfg,
        Some(&candidate.project),
        options.apply,
        options.limit,
        12,
    )
    .await?;

    Ok(format!(
        "proposals={} applied={} derived={} sources_reinforced={}",
        reflection.proposals, reflection.applied, synthesis.derived, synthesis.sources_reinforced
    ))
}

async fn record_action(
    db: &Database,
    action: &SweepAction,
    dry_run: bool,
    subject_last_activity: Option<i64>,
) -> Result<()> {
    db::record_sweep_event(
        db,
        NewSweepEvent {
            session_id: action.session_id.as_deref(),
            project: &action.project,
            action: &action.action,
            dry_run,
            status: &action.status,
            reason: &action.reason,
            detail: action.detail.as_deref(),
            subject_last_activity,
        },
    )
    .await
}

pub async fn run_scheduler(cfg: Config) -> Result<()> {
    let database = Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let (embedder, store) = vectorstore::build_semantic(&database, &cfg).await;
    let embedder_ref = embedder.as_deref();
    let store_ref = store.as_ref();

    let sweep_interval = Duration::from_secs(
        (cfg.scheduler.sweep_interval_minutes as u64)
            .max(1)
            .saturating_mul(60),
    );
    let provider_backoff = Duration::from_secs(
        (cfg.auto_compress.provider_backoff_minutes as u64)
            .max(1)
            .saturating_mul(60),
    );
    let dream_interval_secs = (cfg.scheduler.dream_interval_hours as i64).max(1) * 60 * 60;
    let mut next_dream_at = Utc::now().timestamp();

    tracing::info!(
        "sleep-cycle scheduler started (sweep every {} min, dream every {} h)",
        cfg.scheduler.sweep_interval_minutes,
        cfg.scheduler.dream_interval_hours
    );

    loop {
        let compression_options = CompressionSweepOptions::from_config(&cfg, false);
        let next_sweep_delay = match run_compression_sweep(
            &database,
            &cfg,
            &compression_options,
            embedder_ref,
            store_ref,
        )
        .await
        {
            Ok(report) => {
                tracing::info!(
                    "sleep-cycle sweep: candidates={} compressed={} skipped={} failed={}",
                    report.candidates,
                    report.compressed,
                    report.skipped,
                    report.failed
                );
                if report.failed > 0 {
                    provider_backoff
                } else {
                    sweep_interval
                }
            }
            Err(e) => {
                tracing::warn!("sleep-cycle sweep failed: {e}");
                provider_backoff
            }
        };

        let now = Utc::now().timestamp();
        if now >= next_dream_at {
            let dream_options = DreamSweepOptions {
                due_after_secs: dream_interval_secs,
                apply: cfg.auto_dream.enabled,
                dry_run: false,
                limit: 200,
            };
            match run_dream_sweep(&database, &cfg, &dream_options, embedder_ref, store_ref).await {
                Ok(report) => {
                    tracing::info!(
                        "sleep-cycle dream: candidates={} dreamed={} failed={}",
                        report.candidates,
                        report.dreamed,
                        report.failed
                    );
                }
                Err(e) => tracing::warn!("sleep-cycle dream failed: {e}"),
            }
            next_dream_at = now + dream_interval_secs;
        }

        tokio::select! {
            _ = tokio::time::sleep(next_sweep_delay) => {},
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("sleep-cycle scheduler stopped");
                return Ok(());
            }
        }
    }
}

pub fn install_launchd(cfg: &Config) -> Result<PathBuf> {
    let label = &cfg.scheduler.launchd_label;
    let exe = std::env::current_exe().context("resolve current ironmem executable")?;
    let home = dirs::home_dir().context("resolve home directory")?;
    let launch_dir = home.join("Library").join("LaunchAgents");
    let log_dir = config::ironmem_dir().join("logs");
    std::fs::create_dir_all(&launch_dir)?;
    std::fs::create_dir_all(&log_dir)?;
    let plist_path = launch_dir.join(format!("{label}.plist"));
    let stdout = log_dir.join("sleep.out.log");
    let stderr = log_dir.join("sleep.err.log");

    let mut env = String::from(
        "    <key>EnvironmentVariables</key>\n    <dict>\n      <key>PATH</key>\n      <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>\n",
    );
    if let Ok(path) = std::env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        if !path.trim().is_empty() {
            env.push_str("      <key>GOOGLE_APPLICATION_CREDENTIALS</key>\n");
            env.push_str(&format!("      <string>{}</string>\n", xml_escape(&path)));
        }
    }
    env.push_str("    </dict>\n");

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
      <string>{exe}</string>
      <string>scheduler</string>
      <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
{env}    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        exe = xml_escape(&exe.to_string_lossy()),
        env = env,
        stdout = xml_escape(&stdout.to_string_lossy()),
        stderr = xml_escape(&stderr.to_string_lossy())
    );
    std::fs::write(&plist_path, plist)?;
    Ok(plist_path)
}

pub fn uninstall_launchd(cfg: &Config) -> Result<PathBuf> {
    let home = dirs::home_dir().context("resolve home directory")?;
    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", cfg.scheduler.launchd_label));
    if plist_path.exists() {
        std::fs::remove_file(&plist_path)?;
    }
    Ok(plist_path)
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub async fn open_db_and_semantic(
    cfg: &Config,
) -> Result<(Database, Option<Arc<dyn Embedder>>, Arc<dyn VectorStore>)> {
    let database = Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let (embedder, store) = vectorstore::build_semantic(&database, cfg).await;
    Ok((database, embedder, store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CompressionResult;

    struct FakeCompressor {
        fail: bool,
    }

    #[async_trait]
    impl SessionCompressor for FakeCompressor {
        async fn compress_session(
            &self,
            db: &Database,
            _cfg: &Config,
            session_id: &str,
        ) -> Result<i64> {
            if self.fail {
                anyhow::bail!("provider unavailable");
            }
            let result = CompressionResult {
                summary: "compressed test session".to_string(),
                tags: "test".to_string(),
                ..Default::default()
            };
            crate::compress::persist(
                db,
                None,
                &crate::vectorstore::BruteForceStore,
                "project",
                session_id,
                result,
            )
            .await
        }
    }

    async fn temp_db() -> Database {
        let path = std::env::temp_dir().join(format!("ironmem-sweep-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db
    }

    async fn seed_session(db: &Database, project: &str, observations: usize) -> String {
        let session = db::create_session(db, project).await.unwrap();
        for idx in 0..observations {
            db::insert_observation(
                db,
                &session,
                project,
                "tool",
                Some(&format!("input {idx}")),
                Some(&format!("output {idx}")),
                4096,
            )
            .await
            .unwrap();
        }
        session
    }

    #[test]
    fn parses_duration_suffixes() {
        assert_eq!(parse_duration_secs("30m").unwrap(), 1800);
        assert_eq!(parse_duration_secs("2h").unwrap(), 7200);
        assert_eq!(parse_duration_secs("45").unwrap(), 45);
    }

    #[tokio::test]
    async fn dry_run_does_not_mutate() {
        let db = temp_db().await;
        let session = seed_session(&db, "project", 1).await;
        let cfg = Config::default();
        let options = CompressionSweepOptions {
            compress_idle_secs: 1,
            min_observations: 1,
            limit: 20,
            dry_run: true,
            lease_secs: 60,
        };
        let report =
            run_compression_sweep_with(&db, &cfg, &options, &FakeCompressor { fail: false })
                .await
                .unwrap();
        assert_eq!(report.candidates, 1);
        assert_eq!(report.compressed, 0);
        assert_eq!(db::sweep_event_count(&db).await.unwrap(), 0);
        assert!(
            !db::get_session(&db, &session)
                .await
                .unwrap()
                .unwrap()
                .compressed
        );
    }

    #[tokio::test]
    async fn repeated_sweep_is_idempotent() {
        let db = temp_db().await;
        let session = seed_session(&db, "project", 1).await;
        let cfg = Config::default();
        let options = CompressionSweepOptions {
            compress_idle_secs: 1,
            min_observations: 1,
            limit: 20,
            dry_run: false,
            lease_secs: 60,
        };
        let first =
            run_compression_sweep_with(&db, &cfg, &options, &FakeCompressor { fail: false })
                .await
                .unwrap();
        let second =
            run_compression_sweep_with(&db, &cfg, &options, &FakeCompressor { fail: false })
                .await
                .unwrap();
        assert_eq!(first.compressed, 1);
        assert_eq!(second.candidates, 0);
        assert!(
            db::get_session(&db, &session)
                .await
                .unwrap()
                .unwrap()
                .compressed
        );
    }

    #[tokio::test]
    async fn provider_failure_leaves_session_uncompressed() {
        let db = temp_db().await;
        let session = seed_session(&db, "project", 1).await;
        let cfg = Config::default();
        let options = CompressionSweepOptions {
            compress_idle_secs: 1,
            min_observations: 1,
            limit: 20,
            dry_run: false,
            lease_secs: 60,
        };
        let report =
            run_compression_sweep_with(&db, &cfg, &options, &FakeCompressor { fail: true })
                .await
                .unwrap();
        assert_eq!(report.failed, 1);
        assert!(
            !db::get_session(&db, &session)
                .await
                .unwrap()
                .unwrap()
                .compressed
        );
        assert_eq!(db::sweep_event_count(&db).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn candidate_query_includes_never_ended_sessions_and_respects_volume() {
        let db = temp_db().await;
        let two_obs = seed_session(&db, "project", 2).await;
        let one_obs = seed_session(&db, "project", 1).await;
        let candidates = db::list_sweep_candidates(&db, 0, 2, 20).await.unwrap();
        let ids: Vec<String> = candidates.into_iter().map(|c| c.session_id).collect();
        assert!(ids.contains(&two_obs));
        assert!(!ids.contains(&one_obs));
    }

    #[tokio::test]
    async fn lease_prevents_duplicate_sweeper_ownership() {
        let db = temp_db().await;
        let session = seed_session(&db, "project", 1).await;
        assert!(db::try_acquire_sweep_lease(&db, &session, "owner-a", 60)
            .await
            .unwrap());
        assert!(!db::try_acquire_sweep_lease(&db, &session, "owner-b", 60)
            .await
            .unwrap());
        db::release_sweep_lease(&db, &session, "owner-a")
            .await
            .unwrap();
        assert!(db::try_acquire_sweep_lease(&db, &session, "owner-b", 60)
            .await
            .unwrap());
    }
}
