mod auto_dream;
mod ccr;
mod code_anchor;
mod compress;
mod config;
mod context;
mod corrections;
mod db;
#[cfg(test)]
mod e2e;
mod embedder;
mod embedding_codec;
mod eval;
mod expansion;
mod governance;
mod hooks;
mod mcp;
mod metrics;
mod profile;
mod provider;
mod reflection;
mod reranker;
mod retrieval;
mod server;
mod snapshot;
mod storage;
mod strutil;
mod sweep;
mod sync;
mod vectorstore;

use anyhow::Result;
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ironmem",
    about = "Persistent session memory for Claude Code and AI coding assistants",
    version = env!("CARGO_PKG_VERSION")
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum SnapshotCommands {
    /// Create a CCR-backed brain snapshot
    Create {
        /// Optional human label
        #[arg(short, long)]
        label: Option<String>,
        /// Project root path (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,
    },
    /// List brain snapshots
    List {
        #[arg(short, long, default_value = "20")]
        limit: i64,
    },
    /// Restore a project brain snapshot
    Restore {
        snapshot_id: String,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum SyncCommands {
    /// Publish an immutable sync event
    Publish {
        #[arg(long)]
        node: String,
        #[arg(short, long)]
        project: Option<String>,
        #[arg(long)]
        op: String,
        #[arg(long)]
        payload: String,
    },
    /// Export sync events as JSON
    Export {
        #[arg(short, long)]
        project: Option<String>,
        #[arg(long, default_value = "0")]
        after_lamport: i64,
        #[arg(short, long, default_value = "100")]
        limit: i64,
    },
}

#[derive(Subcommand)]
enum SchedulerCommands {
    /// Run the long-lived sleep-cycle scheduler loop
    Run,
    /// Install and start the macOS launchd sleep-cycle agent
    InstallLaunchd,
    /// Stop and remove the macOS launchd sleep-cycle agent
    UninstallLaunchd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Start the ironmem worker server
    Server,

    /// Show database stats and worker status
    Status,

    /// Search memories for a project
    Search {
        /// Search query
        query: String,
        /// Project root path (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,
        /// Max results
        #[arg(short, long, default_value = "10")]
        limit: i64,
        /// Governance namespace/realm boundary
        #[arg(long, default_value = "local")]
        namespace: String,
    },

    /// Search memories across all projects
    SearchGlobal {
        /// Search query
        query: String,
        /// Max results
        #[arg(short, long, default_value = "10")]
        limit: i64,
        /// Governance namespace/realm boundary
        #[arg(long, default_value = "local")]
        namespace: String,
    },

    /// List recent memories for a project
    List {
        /// Project root path (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,
        /// Max results
        #[arg(short, long, default_value = "5")]
        limit: i64,
        /// Governance namespace/realm boundary
        #[arg(long, default_value = "local")]
        namespace: String,
    },

    /// List projects with stored memories
    Projects {
        /// Max results
        #[arg(short, long, default_value = "50")]
        limit: i64,
    },

    /// List session history for a project
    Sessions {
        /// Project root path (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,
        /// Max results
        #[arg(short, long, default_value = "20")]
        limit: i64,
    },

    /// Delete all memories for a project
    Wipe {
        /// Project root path (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        force: bool,
    },

    /// Inject context into project files (IRONMEM.md + CLAUDE.md)
    /// Called by session-start hook
    Inject {
        /// Project root path (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,
        /// Max memories to inject
        #[arg(short, long, default_value = "5")]
        limit: i64,
    },

    /// Store an explicit memory (use --scope user for cross-project facts)
    Remember {
        /// The memory text to store
        text: String,
        /// Project root path (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,
        /// Scope: project (default) or user (cross-project)
        #[arg(short, long, default_value = "project")]
        scope: String,
        /// Kind: session|error_solution|preference|architecture|learned_pattern|project_config|profile
        #[arg(short, long, default_value = "preference")]
        kind: String,
        /// Optional space-separated tags
        #[arg(short, long)]
        tags: Option<String>,
        /// Governance namespace/realm boundary (default: local)
        #[arg(long, default_value = "local")]
        namespace: String,
        /// Provenance source: user_input|tool_output|agent_generated|derived|external|sync_peer
        #[arg(long, default_value = "user_input")]
        source_type: String,
        /// Trust tier: high|medium|low|untrusted
        #[arg(long, default_value = "high")]
        trust_tier: String,
        /// Writer identity recorded in the tamper-evident ledger
        #[arg(long)]
        writer: Option<String>,
        /// Classification: public|internal|confidential|restricted|phi|pii
        #[arg(long, default_value = "internal")]
        classification: String,
        /// Consent state: required|granted|denied|withdrawn. PHI/PII require granted.
        #[arg(long)]
        consent_state: Option<String>,
        /// Residency tag such as us-east-1 or eu-west-1
        #[arg(long)]
        residency: Option<String>,
        /// Retention policy identifier
        #[arg(long)]
        retention_policy_id: Option<String>,
        /// Expiration timestamp as Unix seconds
        #[arg(long)]
        expires_at: Option<i64>,
        /// Prevent governed forget until legal hold is cleared
        #[arg(long, default_value_t = false)]
        legal_hold: bool,
        /// External source reference, receipt id, URL, or tool event id
        #[arg(long)]
        source_ref: Option<String>,
    },

    /// Governed delete for one memory: ledger entry + CCR release + vector/meta purge
    Forget {
        /// Memory id to forget
        memory_id: i64,
        /// Actor written into the memory ledger
        #[arg(long, default_value = "ironmem:cli")]
        actor: String,
        /// Human-readable reason written into the memory ledger
        #[arg(long)]
        reason: Option<String>,
    },

    /// Show the user profile (durable cross-project facts + recent activity)
    Profile {
        /// Regenerate the profile from user memories before showing it
        #[arg(short, long)]
        refresh: bool,
    },

    /// List mined error→fix corrections (kind=error_solution)
    Corrections {
        /// Project root path (defaults to current directory; use --all for every project)
        #[arg(short, long)]
        project: Option<String>,
        /// List corrections across every project
        #[arg(short, long)]
        all: bool,
        /// Max results
        #[arg(short, long, default_value = "10")]
        limit: i64,
    },

    /// Query the temporal memory graph for an entity
    Graph {
        /// Entity to query (person, project, organization, concept)
        entity: String,
        /// Project root path (defaults to current directory; use --all for every project)
        #[arg(short, long)]
        project: Option<String>,
        /// Query graph edges across every project
        #[arg(short, long)]
        all: bool,
        /// Include superseded/duplicate historical edges
        #[arg(long)]
        history: bool,
        /// Query graph edges valid at a YYYY-MM-DD date
        #[arg(long)]
        at: Option<String>,
        /// Max graph edges
        #[arg(short, long, default_value = "20")]
        limit: i64,
    },

    /// Delete a graph edge by marking it user-deleted
    GraphDelete { edge_id: i64 },

    /// Update a graph edge after human curation
    GraphUpdate {
        edge_id: i64,
        #[arg(long)]
        source: String,
        #[arg(long)]
        relation: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        valid_from: Option<String>,
        #[arg(long)]
        valid_until: Option<String>,
        #[arg(long, default_value = "1.0")]
        confidence: f64,
    },

    /// Reconcile duplicate/superseded temporal graph edges
    Reconcile {
        /// Project root path (defaults to current directory; use --all for every project)
        #[arg(short, long)]
        project: Option<String>,
        /// Reconcile graph edges across every project
        #[arg(short, long)]
        all: bool,
        /// Report what would change without mutating graph history
        #[arg(long)]
        dry_run: bool,
    },

    /// Backfill temporal graph relations for memories created before RELATIONS extraction
    GraphBackfill {
        /// Project root path (defaults to current directory; use --all for every project)
        #[arg(short, long)]
        project: Option<String>,
        /// Backfill across every project
        #[arg(short, long)]
        all: bool,
        /// Max memories to inspect
        #[arg(short, long, default_value = "50")]
        limit: i64,
        /// Report extracted relation counts without writing edges
        #[arg(long)]
        dry_run: bool,
    },

    /// Manually compress a session
    Compress {
        /// Session ID to compress
        session_id: String,
    },

    /// Sweep idle/large sessions into memories or run due dream consolidation
    Sweep {
        /// Idle age before a session is compressible. Supports s/m/h/d suffixes.
        #[arg(long, default_value = "30m")]
        compress_idle: String,
        /// Compress sessions with at least this many observations even if not idle
        #[arg(long, default_value_t = 50)]
        min_observations: i64,
        /// Max sessions/projects to sweep in one run
        #[arg(short, long, default_value_t = 20)]
        limit: i64,
        /// Report candidates without mutating the database
        #[arg(long)]
        dry_run: bool,
        /// Run due dream/reflection work instead of compression
        #[arg(long)]
        dream_due: bool,
        /// Apply dream synthesis/consolidation. Without this, dream is proposal-first.
        #[arg(long)]
        apply: bool,
    },

    /// Manage the unattended sleep-cycle scheduler
    Scheduler {
        #[command(subcommand)]
        action: SchedulerCommands,
    },

    /// Garbage-collect unreferenced CCR blobs (reclaim space after wipes)
    Gc,

    /// Backfill semantic embeddings for existing memories
    Embed {
        /// Project root path (defaults to all projects)
        #[arg(short, long)]
        project: Option<String>,
        /// Embed across every project (ignore --project)
        #[arg(short, long)]
        all: bool,
        /// Rebuild the index from scratch, re-embedding every memory
        #[arg(short, long)]
        force: bool,
    },

    /// Run deterministic memory-quality evaluation suites
    Eval {
        /// Directory for markdown eval reports
        #[arg(long, default_value = "docs/evals")]
        out: String,
    },

    /// Record usage feedback for a memory
    Feedback {
        memory_id: i64,
        #[arg(short, long)]
        project: Option<String>,
        #[arg(long)]
        signal: String,
        #[arg(long, default_value = "1.0")]
        weight: f64,
        #[arg(long)]
        detail: Option<String>,
    },

    /// Run a reflection/consolidation pass
    Reflect {
        #[arg(short, long)]
        project: Option<String>,
        #[arg(short, long)]
        all: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
        #[arg(short, long, default_value = "200")]
        limit: i64,
        #[arg(long)]
        list: bool,
    },

    /// Run a dream/sleep consolidation pass (safe proposal store by default)
    Dream {
        #[arg(short, long)]
        project: Option<String>,
        #[arg(short, long)]
        all: bool,
        /// Show proposed consolidations without writing proposals
        #[arg(long)]
        dry_run: bool,
        /// Promote accepted proposals into consolidated memories
        #[arg(long)]
        apply: bool,
        #[arg(short, long, default_value = "200")]
        limit: i64,
        /// List existing dream/reflection proposals
        #[arg(long)]
        list: bool,
    },

    /// Anchor/relink memories to Rust AST symbols
    CodeRelink {
        #[arg(short, long)]
        project: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        anchor_only: bool,
        #[arg(long)]
        relink_only: bool,
    },

    /// Brain-state snapshots
    Snapshot {
        #[command(subcommand)]
        action: SnapshotCommands,
    },

    /// Multi-agent sync event log
    Sync {
        #[command(subcommand)]
        action: SyncCommands,
    },

    /// Start the MCP server (stdio transport, for Claude Desktop/Code)
    Mcp,

    /// Start the SSE server for remote MCP clients
    Serve {
        /// Expose via Cloudflare Tunnel for remote access
        #[arg(long)]
        public: bool,
        /// Disable auth entirely. Useful for claude.ai custom connectors, which
        /// currently support authless and OAuth remote MCP servers but not static
        /// bearer-token auth.
        #[arg(long)]
        no_auth: bool,
    },

    /// Print current configuration
    Config,
}

#[cfg(windows)]
fn main() -> Result<()> {
    // Windows' default main-thread stack is small enough for the debug MCP
    // startup path to overflow before stdio can answer `initialize`.
    std::thread::Builder::new()
        .name("ironmem-main".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(async_main)?
        .join()
        .map_err(|panic| {
            if let Some(message) = panic.downcast_ref::<&str>() {
                anyhow::anyhow!("ironmem main thread panicked: {message}")
            } else if let Some(message) = panic.downcast_ref::<String>() {
                anyhow::anyhow!("ironmem main thread panicked: {message}")
            } else {
                anyhow::anyhow!("ironmem main thread panicked")
            }
        })?
}

#[cfg(not(windows))]
fn main() -> Result<()> {
    async_main()
}

#[tokio::main]
async fn async_main() -> Result<()> {
    // Logs MUST go to stderr, never stdout: in `ironmem mcp` (stdio transport)
    // stdout carries the JSON-RPC stream, so a single log line there corrupts it
    // — the MCP client sees the `2026-…` timestamp and rejects the message
    // ("Unexpected token … is not valid JSON"). ANSI off so captured logs
    // (server.log via launchd) stay free of escape codes too.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ironmem=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    let cfg = config::load()?;

    match cli.command {
        Commands::Server => run_server(cfg).await?,
        Commands::Mcp => run_mcp(cfg).await?,
        Commands::Serve { public, no_auth } => run_serve(cfg, public, no_auth).await?,
        Commands::Status => run_status(&cfg).await?,
        Commands::Search {
            query,
            project,
            limit,
            namespace,
        } => run_search(&cfg, &query, project.as_deref(), limit, &namespace).await?,
        Commands::SearchGlobal {
            query,
            limit,
            namespace,
        } => run_search_global(&cfg, &query, limit, &namespace).await?,
        Commands::List {
            project,
            limit,
            namespace,
        } => run_list(&cfg, project.as_deref(), limit, &namespace).await?,
        Commands::Projects { limit } => run_projects(&cfg, limit).await?,
        Commands::Sessions { project, limit } => {
            run_sessions(&cfg, project.as_deref(), limit).await?
        }
        Commands::Wipe { project, force } => run_wipe(&cfg, project.as_deref(), force).await?,
        Commands::Inject { project, limit } => run_inject(&cfg, project.as_deref(), limit).await?,
        Commands::Remember {
            text,
            project,
            scope,
            kind,
            tags,
            namespace,
            source_type,
            trust_tier,
            writer,
            classification,
            consent_state,
            residency,
            retention_policy_id,
            expires_at,
            legal_hold,
            source_ref,
        } => {
            let governance = build_cli_governance(
                &namespace,
                &source_type,
                &trust_tier,
                writer,
                &classification,
                consent_state.as_deref(),
                residency,
                retention_policy_id,
                expires_at,
                legal_hold,
                source_ref,
            );
            run_remember(
                &cfg,
                &text,
                project.as_deref(),
                &scope,
                &kind,
                tags.as_deref(),
                governance,
            )
            .await?
        }
        Commands::Forget {
            memory_id,
            actor,
            reason,
        } => run_forget(&cfg, memory_id, &actor, reason.as_deref()).await?,
        Commands::Profile { refresh } => run_profile(&cfg, refresh).await?,
        Commands::Corrections {
            project,
            all,
            limit,
        } => run_corrections(&cfg, project.as_deref(), all, limit).await?,
        Commands::Graph {
            entity,
            project,
            all,
            history,
            at,
            limit,
        } => {
            run_graph(
                &cfg,
                &entity,
                project.as_deref(),
                all,
                history,
                at.as_deref(),
                limit,
            )
            .await?
        }
        Commands::GraphDelete { edge_id } => run_graph_delete(&cfg, edge_id).await?,
        Commands::GraphUpdate {
            edge_id,
            source,
            relation,
            target,
            valid_from,
            valid_until,
            confidence,
        } => {
            run_graph_update(
                &cfg,
                edge_id,
                &source,
                &relation,
                &target,
                valid_from.as_deref(),
                valid_until.as_deref(),
                confidence,
            )
            .await?
        }
        Commands::Reconcile {
            project,
            all,
            dry_run,
        } => run_reconcile(&cfg, project.as_deref(), all, dry_run).await?,
        Commands::GraphBackfill {
            project,
            all,
            limit,
            dry_run,
        } => run_graph_backfill(&cfg, project.as_deref(), all, limit, dry_run).await?,
        Commands::Compress { session_id } => run_compress_cmd(&cfg, &session_id).await?,
        Commands::Sweep {
            compress_idle,
            min_observations,
            limit,
            dry_run,
            dream_due,
            apply,
        } => {
            run_sweep_cmd(
                &cfg,
                &compress_idle,
                min_observations,
                limit,
                dry_run,
                dream_due,
                apply,
            )
            .await?
        }
        Commands::Scheduler { action } => run_scheduler_cmd(cfg, action).await?,
        Commands::Gc => run_gc(&cfg).await?,
        Commands::Embed {
            project,
            all,
            force,
        } => run_embed(&cfg, project.as_deref(), all, force).await?,
        Commands::Eval { out } => run_eval(&cfg, &out).await?,
        Commands::Feedback {
            memory_id,
            project,
            signal,
            weight,
            detail,
        } => {
            run_feedback(
                &cfg,
                memory_id,
                project.as_deref(),
                &signal,
                weight,
                detail.as_deref(),
            )
            .await?
        }
        Commands::Reflect {
            project,
            all,
            dry_run,
            apply,
            limit,
            list,
        } => run_reflect(&cfg, project.as_deref(), all, dry_run, apply, limit, list).await?,
        Commands::Dream {
            project,
            all,
            dry_run,
            apply,
            limit,
            list,
        } => run_reflect(&cfg, project.as_deref(), all, dry_run, apply, limit, list).await?,
        Commands::CodeRelink {
            project,
            dry_run,
            anchor_only,
            relink_only,
        } => run_code_relink(&cfg, project.as_deref(), dry_run, anchor_only, relink_only).await?,
        Commands::Snapshot { action } => run_snapshot(&cfg, action).await?,
        Commands::Sync { action } => run_sync(&cfg, action).await?,
        Commands::Config => {
            println!("{}", serde_json::to_string_pretty(&cfg)?);
        }
    }

    Ok(())
}

async fn run_server(mut cfg: config::Config) -> Result<()> {
    // (Wave 4) Env overrides let the cross-encoder rerank backend be selected
    // without editing settings.json (e.g. for an A/B against the LLM reranker).
    if let Ok(b) = std::env::var("IRONMEM_RERANK_BACKEND") {
        cfg.rerank.backend = b;
    }
    if let Ok(m) = std::env::var("IRONMEM_RERANK_CROSS_ENCODER_MODEL") {
        cfg.rerank.cross_encoder_model = m;
    }
    let db_url = cfg.effective_database_url();
    let database = db::Database::new(&db_url).await?;
    database.migrate().await?;
    let db = std::sync::Arc::new(database);

    // Start REST server
    let rest_db = db.clone();
    let rest_cfg = cfg.clone();
    let addr = format!("127.0.0.1:{}", cfg.port);
    tracing::info!("ironmem REST server listening on http://{}", addr);

    let (embedder, store) = vectorstore::build_semantic(&rest_db, &rest_cfg).await;
    // (Wave 4) Load the cross-encoder reranker once at startup if selected. The
    // load is blocking (model download + ONNX init); on failure the rerank path
    // falls back to the LLM reranker.
    if rest_cfg
        .rerank
        .backend
        .eq_ignore_ascii_case("cross_encoder")
    {
        let model = rest_cfg.rerank.cross_encoder_model.clone();
        let _ = tokio::task::spawn_blocking(move || crate::reranker::init(&model)).await;
    }
    // (#3) Clone handles for the auto-dream watcher before embedder/store move
    // into AppState (cheap Arc clones; only when the feature is enabled).
    let auto_dream_handles = if cfg.auto_dream.enabled {
        Some((
            (*rest_db).clone(),
            cfg.clone(),
            embedder.clone(),
            store.clone(),
        ))
    } else {
        None
    };
    let state = server::AppState {
        db: (*rest_db).clone(),
        config: rest_cfg,
        embedder,
        store,
    };
    let app = server::router(state);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    let mcp_transport = cfg.effective_mcp_transport();
    if mcp_transport == "sse" {
        let sse_addr: std::net::SocketAddr =
            format!("0.0.0.0:{}", cfg.mcp_sse_port).parse().unwrap();
        let sse_db = db.clone();
        let sse_cfg = cfg.clone();
        tokio::spawn(async move {
            if let Err(e) = mcp::run_streamable_http(sse_db, sse_cfg, sse_addr).await {
                tracing::error!("MCP Streamable HTTP server error: {}", e);
            }
        });
    }

    // (#3) Heuristic auto-dream watcher (opt-in via auto_dream.enabled).
    if let Some((dream_db, dream_cfg, dream_embedder, dream_store)) = auto_dream_handles {
        tokio::spawn(async move {
            auto_dream::watch(dream_db, dream_cfg, dream_embedder, dream_store).await;
        });
    }

    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_eval(cfg: &config::Config, out: &str) -> Result<()> {
    let report = eval::run(cfg, std::path::Path::new(out)).await?;
    if let Some(path) = &report.output_path {
        println!(
            "IronMem eval: {}/{} passed; report={}",
            report.passed(),
            report.cases.len(),
            path.display()
        );
    } else {
        println!(
            "IronMem eval: {}/{} passed",
            report.passed(),
            report.cases.len()
        );
    }
    eval::ensure_passed(&report)?;
    Ok(())
}

async fn run_mcp(cfg: config::Config) -> Result<()> {
    // MCP stdio mode — tracing to stderr only (stdout is the MCP transport)
    let db_url = cfg.effective_database_url();
    let database = db::Database::new(&db_url).await?;
    database.migrate().await?;
    let db = std::sync::Arc::new(database);
    mcp::run_stdio(db, cfg).await?;
    Ok(())
}

async fn run_serve(mut cfg: config::Config, public: bool, no_auth: bool) -> Result<()> {
    let db_url = cfg.effective_database_url();
    let database = db::Database::new(&db_url).await?;
    database.migrate().await?;
    let db = std::sync::Arc::new(database);

    let sse_port = cfg.mcp_sse_port;
    let bind: std::net::SocketAddr = format!("0.0.0.0:{}", sse_port).parse().unwrap();
    let auth_token = if no_auth {
        cfg.auth_token = None;
        None
    } else {
        Some(cfg.ensure_auth_token())
    };

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  IronMem MCP Server");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Local:  http://127.0.0.1:{}/mcp", sse_port);
    match auth_token.as_deref() {
        Some(token) => println!("  Auth:   Bearer {}", token),
        None => println!("  Auth:   Disabled (--no-auth)"),
    }
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    if public {
        // Launch Cloudflare tunnel in background
        let tunnel_port = sse_port;
        let tunnel_auth_token = auth_token.clone();
        tokio::spawn(async move {
            // Give the SSE server a moment to bind
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            run_cloudflare_tunnel(tunnel_port, tunnel_auth_token).await;
        });
    }

    let serve_cfg = cfg.clone();
    mcp::run_streamable_http(db, serve_cfg, bind).await?;

    Ok(())
}

async fn run_cloudflare_tunnel(port: u16, auth_token: Option<String>) {
    // Try cloudflared first (installed binary), then npx fallback
    let url = format!("http://localhost:{}", port);

    // Check for cloudflared binary
    let cloudflared = tokio::process::Command::new("cloudflared")
        .arg("--version")
        .output()
        .await;

    if cloudflared.is_ok() {
        println!("\n  Starting Cloudflare Tunnel (cloudflared)...\n");
        let mut child = match tokio::process::Command::new("cloudflared")
            .args(["tunnel", "--url", &url])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  Failed to start cloudflared: {}", e);
                return;
            }
        };

        // Read stderr for the tunnel URL
        if let Some(stderr) = child.stderr.take() {
            let reader = tokio::io::BufReader::new(stderr);
            use tokio::io::AsyncBufReadExt;
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains("https://") && line.contains(".trycloudflare.com") {
                    if let Some(url) = line.split_whitespace().find(|s| s.starts_with("https://")) {
                        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                        println!("  Public URL: {}", url);
                        println!();
                        println!("  Remote MCP setup:");
                        println!("    URL:   {}/mcp", url);
                        match auth_token.as_deref() {
                            Some(token) => println!("    Auth:  Bearer {}", token),
                            None => println!("    Auth:  None"),
                        }
                        println!(
                            "    Note:  trycloudflare URLs are ephemeral and change on restart."
                        );
                        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                    }
                }
                tracing::debug!("cloudflared: {}", line);
            }
        }

        let _ = child.wait().await;
    } else {
        // Fallback: try npx with cloudflared
        println!("\n  cloudflared not found. Install it for best results:");
        #[cfg(target_os = "macos")]
        println!("    brew install cloudflared");
        #[cfg(target_os = "windows")]
        println!("    winget install Cloudflare.cloudflared");
        #[cfg(target_os = "linux")]
        println!("    See https://pkg.cloudflare.com/ for your distro");
        println!("    # or: https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/\n");
        println!("  Trying npx fallback...\n");

        let mut child = match tokio::process::Command::new("npx")
            .args(["-y", "cloudflared", "tunnel", "--url", &url])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  Failed to start tunnel: {}", e);
                #[cfg(target_os = "macos")]
                eprintln!("  Install cloudflared manually: brew install cloudflared");
                #[cfg(target_os = "windows")]
                eprintln!("  Install cloudflared manually: winget install Cloudflare.cloudflared");
                #[cfg(target_os = "linux")]
                eprintln!("  Install cloudflared manually: see https://pkg.cloudflare.com/");
                return;
            }
        };

        if let Some(stderr) = child.stderr.take() {
            let reader = tokio::io::BufReader::new(stderr);
            use tokio::io::AsyncBufReadExt;
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains("https://") && line.contains(".trycloudflare.com") {
                    if let Some(url) = line.split_whitespace().find(|s| s.starts_with("https://")) {
                        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                        println!("  Public URL: {}", url);
                        println!();
                        println!("  Remote MCP setup:");
                        println!("    URL:   {}/mcp", url);
                        match auth_token.as_deref() {
                            Some(token) => println!("    Auth:  Bearer {}", token),
                            None => println!("    Auth:  None"),
                        }
                        println!(
                            "    Note:  trycloudflare URLs are ephemeral and change on restart."
                        );
                        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                    }
                }
                tracing::debug!("cloudflared: {}", line);
            }
        }

        let _ = child.wait().await;
    }
}

async fn run_gc(cfg: &config::Config) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let (count, bytes) = db::gc_blobs(&database).await?;
    println!("🧹 CCR gc: removed {count} unreferenced blob(s), freed {bytes} compressed bytes");
    Ok(())
}

async fn run_feedback(
    cfg: &config::Config,
    memory_id: i64,
    project: Option<&str>,
    signal: &str,
    weight: f64,
    detail: Option<&str>,
) -> Result<()> {
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let id =
        db::record_memory_feedback(&database, memory_id, &project, signal, weight, detail).await?;
    println!(
        "Recorded feedback #{id} for memory {memory_id}: signal={signal}, weight={:.2}",
        weight.clamp(-2.0, 2.0)
    );
    Ok(())
}

async fn run_reflect(
    cfg: &config::Config,
    project: Option<&str>,
    all: bool,
    dry_run: bool,
    apply: bool,
    limit: i64,
    list: bool,
) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let filter = if all {
        None
    } else {
        Some(resolve_project(project)?)
    };
    if list {
        let proposals = reflection::list(&database, filter.as_deref(), None, limit.max(1)).await?;
        println!("{} reflection proposal(s):", proposals.len());
        for p in proposals {
            println!(
                "\n#{} [{}:{}] {} source(s)\n{}",
                p.id,
                p.status,
                p.kind,
                p.source_memory_ids.len(),
                p.proposed_summary
            );
        }
        return Ok(());
    }

    let (embedder, store) = vectorstore::build_semantic(&database, cfg).await;
    let report = reflection::run(
        &database,
        embedder.as_deref(),
        store.as_ref(),
        filter.as_deref(),
        dry_run,
        apply,
        limit.max(1),
    )
    .await?;
    println!(
        "Reflection{}: scanned={}, proposals={}, written={}, applied={}",
        if report.dry_run { " dry-run" } else { "" },
        report.scanned,
        report.proposals,
        report.proposals_written.len(),
        report.applied
    );
    Ok(())
}

async fn run_code_relink(
    cfg: &config::Config,
    project: Option<&str>,
    dry_run: bool,
    anchor_only: bool,
    relink_only: bool,
) -> Result<()> {
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    let mut created = 0;
    let mut relinked = 0;
    let mut scanned = 0;
    if !relink_only {
        let r = code_anchor::anchor_project(&database, &project, dry_run).await?;
        created += r.anchors_created;
        scanned = scanned.max(r.scanned_symbols);
    }
    if !anchor_only {
        let r = code_anchor::relink_project(&database, &project, dry_run).await?;
        relinked += r.anchors_relinked;
        scanned = scanned.max(r.scanned_symbols);
    }
    println!(
        "Code relink{}: scanned_symbols={}, anchors_created={}, anchors_relinked={}",
        if dry_run { " dry-run" } else { "" },
        scanned,
        created,
        relinked
    );
    Ok(())
}

async fn run_snapshot(cfg: &config::Config, action: SnapshotCommands) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    match action {
        SnapshotCommands::Create { label, project } => {
            let project = match project {
                Some(p) => Some(resolve_project(Some(&p))?),
                None => Some(resolve_project(None)?),
            };
            let snap = snapshot::create(&database, label.as_deref(), project.as_deref()).await?;
            println!(
                "Created snapshot {} (memories={}, edges={}, blob={})",
                snap.id, snap.memory_count, snap.edge_count, snap.blob_hash
            );
        }
        SnapshotCommands::List { limit } => {
            let snaps = db::list_brain_snapshots(&database, limit.max(1)).await?;
            println!("{} snapshot(s):", snaps.len());
            for s in snaps {
                println!(
                    "\n{} | {:?} | project={:?} | memories={} edges={} | {}",
                    s.id, s.label, s.project, s.memory_count, s.edge_count, s.blob_hash
                );
            }
        }
        SnapshotCommands::Restore {
            snapshot_id,
            dry_run,
        } => {
            let report = snapshot::restore(&database, &snapshot_id, dry_run).await?;
            println!(
                "Snapshot restore{} {}: memories_in_snapshot={}, edges_in_snapshot={}, restored_memories={}, restored_edges={}",
                if report.dry_run { " dry-run" } else { "" },
                report.snapshot_id,
                report.memories_in_snapshot,
                report.edges_in_snapshot,
                report.restored_memories,
                report.restored_edges
            );
        }
    }
    Ok(())
}

async fn run_sync(cfg: &config::Config, action: SyncCommands) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    match action {
        SyncCommands::Publish {
            node,
            project,
            op,
            payload,
        } => {
            let parsed: serde_json::Value = serde_json::from_str(&payload)
                .unwrap_or_else(|_| serde_json::json!({ "text": payload }));
            let sync_payload = sync::SyncPayload {
                kind: op.clone(),
                memory_id: parsed.get("memory_id").and_then(|v| v.as_i64()),
                edge_id: parsed.get("edge_id").and_then(|v| v.as_i64()),
                body: parsed,
            };
            let result =
                sync::publish(&database, &node, project.as_deref(), &op, &sync_payload).await?;
            println!(
                "Published sync event {} lamport={} inserted={}",
                result.event_id, result.lamport, result.inserted
            );
        }
        SyncCommands::Export {
            project,
            after_lamport,
            limit,
        } => {
            let events =
                sync::export_events(&database, project.as_deref(), after_lamport, limit.max(1))
                    .await?;
            println!("{}", serde_json::to_string_pretty(&events)?);
        }
    }
    Ok(())
}

async fn run_status(cfg: &config::Config) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/status", cfg.port);
    match reqwest::get(&url).await {
        Ok(resp) => {
            let text = resp.text().await?;
            // Pretty print
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{}", text);
            }
        }
        Err(_) => {
            println!("❌ ironmem server is not running (port {})", cfg.port);
            println!("   Start with: ironmem server");
        }
    }
    Ok(())
}

async fn run_search(
    cfg: &config::Config,
    query: &str,
    project: Option<&str>,
    limit: i64,
    namespace: &str,
) -> Result<()> {
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let memories =
        db::search_memories_in_namespace(&database, namespace, &project, query, limit).await?;

    if memories.is_empty() {
        println!("No memories found for query: {}", query);
        return Ok(());
    }

    println!("Found {} memories:\n", memories.len());
    for m in memories {
        println!("─────────────────────────────────");
        println!("ID: {} | Session: {}", m.id, m.session_id);
        println!("{}", m.summary);
        if let Some(tags) = m.tags {
            println!("Tags: {}", tags);
        }
        println!();
    }
    Ok(())
}

async fn run_search_global(
    cfg: &config::Config,
    query: &str,
    limit: i64,
    namespace: &str,
) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let memories = db::search_all_memories_in_namespace(&database, namespace, query, limit).await?;

    if memories.is_empty() {
        println!("No memories found across any project for query: {}", query);
        return Ok(());
    }

    println!("Found {} memories across all projects:\n", memories.len());
    for m in memories {
        println!("─────────────────────────────────");
        println!("Project: {}", m.project);
        println!("ID: {} | Session: {}", m.id, m.session_id);
        println!("{}", m.summary);
        if let Some(tags) = m.tags {
            println!("Tags: {}", tags);
        }
        println!();
    }
    Ok(())
}

async fn run_list(
    cfg: &config::Config,
    project: Option<&str>,
    limit: i64,
    namespace: &str,
) -> Result<()> {
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let memories =
        db::get_recent_memories_in_namespace(&database, namespace, &project, limit).await?;

    if memories.is_empty() {
        println!("No memories for project: {}", project);
        return Ok(());
    }

    println!("Recent memories for {}:\n", project);
    for m in memories {
        println!("─────────────────────────────────");
        println!("ID: {}", m.id);
        println!("{}", m.summary);
        if let Some(tags) = m.tags {
            println!("Tags: {}", tags);
        }
        println!();
    }
    Ok(())
}

async fn run_projects(cfg: &config::Config, limit: i64) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let projects = db::list_projects(&database, limit).await?;

    if projects.is_empty() {
        println!("No projects with stored memories yet.");
        return Ok(());
    }

    println!("Projects with stored memories:\n");
    for project in projects {
        println!("─────────────────────────────────");
        println!("Project: {}", project.project);
        println!("Memories: {}", project.memory_count);
        println!("Last activity: {}", format_timestamp(project.last_activity));
        println!();
    }
    Ok(())
}

async fn run_sessions(cfg: &config::Config, project: Option<&str>, limit: i64) -> Result<()> {
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let sessions = db::list_session_history(&database, &project, limit).await?;

    if sessions.is_empty() {
        println!("No sessions for project: {}", project);
        return Ok(());
    }

    println!("Session history for {}:\n", project);
    for session in sessions {
        println!("─────────────────────────────────");
        println!("Session: {}", session.id);
        println!("Started: {}", format_timestamp(session.started_at));
        match session.ended_at {
            Some(ended_at) => println!("Ended: {}", format_timestamp(ended_at)),
            None => println!("Ended: still running"),
        }
        println!("Compressed: {}", session.compressed);
        println!("Observations: {}", session.observation_count);
        if let Some(tags) = session.tags {
            println!("Tags: {}", tags);
        }
        println!();
    }
    Ok(())
}

async fn run_wipe(cfg: &config::Config, project: Option<&str>, force: bool) -> Result<()> {
    let project = resolve_project(project)?;

    if !force {
        print!(
            "Delete all memories for {}? This cannot be undone. [y/N] ",
            project
        );
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    let (_embedder, store) = vectorstore::build_semantic(&database, cfg).await;
    let ids = db::memory_ids_for_project(&database, &project).await?;
    let mut count = 0_u64;
    for id in ids {
        match db::governed_delete_memory(&database, id, Some("ironmem:wipe"), Some("project wipe"))
            .await
        {
            Ok(true) => {
                count += 1;
                if let Err(e) = vectorstore::purge_memory(&database, store.as_ref(), id).await {
                    tracing::warn!("vector/meta cleanup failed for memory {id}: {e}");
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("governed wipe failed for memory {id}: {e}"),
        }
    }

    // Clean up IRONMEM.md and CLAUDE.md import
    let _ = std::fs::remove_file(std::path::Path::new(&project).join("IRONMEM.md"));
    let _ = hooks::remove_claude_md_import(&project);

    println!("Wiped {} memories for {}", count, project);
    Ok(())
}

async fn run_inject(cfg: &config::Config, project: Option<&str>, limit: i64) -> Result<()> {
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    let (embedder, store) = vectorstore::build_semantic(&database, cfg).await;
    let memories = retrieval::rank_for_injection(
        &database,
        embedder.as_deref(),
        store.as_ref(),
        &project,
        &cfg.embedding.weights,
        cfg.embedding.recency_half_life_days,
        limit as usize,
    )
    .await?;

    db::record_injection_events(&database, &project, None, Some("session-start"), &memories)
        .await
        .ok();
    hooks::write_ironmem_file(&project, &memories)?;
    hooks::ensure_claude_md_import(&project)?;

    println!(
        "Injected {} memories into IRONMEM.md for {}",
        memories.len(),
        project
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_cli_governance(
    namespace: &str,
    source_type: &str,
    trust_tier: &str,
    writer: Option<String>,
    classification: &str,
    consent_state: Option<&str>,
    residency: Option<String>,
    retention_policy_id: Option<String>,
    expires_at: Option<i64>,
    legal_hold: bool,
    source_ref: Option<String>,
) -> governance::MemoryGovernance {
    governance::MemoryGovernance {
        namespace: governance::normalize_namespace(namespace),
        source_type: governance::parse_source_type(source_type),
        trust_tier: governance::parse_trust_tier(trust_tier),
        writer_identity: writer.or_else(|| Some("ironmem:cli".to_string())),
        source_ref,
        parent_memory_id: None,
        classification: governance::parse_classification(classification),
        consent_state: consent_state.and_then(governance::parse_consent_state),
        residency,
        retention_policy_id,
        expires_at,
        legal_hold,
    }
}

async fn run_forget(
    cfg: &config::Config,
    memory_id: i64,
    actor: &str,
    reason: Option<&str>,
) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let (_embedder, store) = vectorstore::build_semantic(&database, cfg).await;
    let deleted = db::governed_delete_memory(&database, memory_id, Some(actor), reason).await?;
    if deleted {
        vectorstore::purge_memory(&database, store.as_ref(), memory_id).await?;
        println!("Forgot memory id={} (ledger recorded)", memory_id);
    } else {
        println!("No memory found for id={}", memory_id);
    }
    Ok(())
}

async fn run_remember(
    cfg: &config::Config,
    text: &str,
    project: Option<&str>,
    scope: &str,
    kind: &str,
    tags: Option<&str>,
    governance: governance::MemoryGovernance,
) -> Result<()> {
    if text.trim().is_empty() {
        anyhow::bail!("memory text must not be empty");
    }
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    let (embedder, store) = vectorstore::build_semantic(&database, cfg).await;
    let namespace = governance::normalize_namespace(&governance.namespace);
    let id = compress::remember_with_governance(
        &database,
        embedder.as_deref(),
        store.as_ref(),
        &project,
        scope,
        kind,
        text,
        tags,
        governance,
    )
    .await?;

    println!(
        "✅ Remembered memory id={} (namespace={}, scope={}, kind={}) for {}",
        id,
        namespace,
        db::clamp_scope(scope),
        db::clamp_kind(kind),
        project
    );
    Ok(())
}

async fn run_profile(cfg: &config::Config, refresh: bool) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let (embedder, store) = vectorstore::build_semantic(&database, cfg).await;

    if refresh {
        match profile::regenerate(&database, embedder.as_deref(), store.as_ref(), Some(cfg)).await?
        {
            Some(_) => println!("✅ Profile regenerated."),
            None => {
                println!("No user-scope memories yet — nothing to profile.");
                return Ok(());
            }
        }
    }

    match db::get_profile_memory(&database).await? {
        Some(m) => println!("\n{}", m.summary),
        None => println!(
            "(no profile yet — add facts with `ironmem remember --scope user ...`, then `ironmem profile --refresh`)"
        ),
    }
    Ok(())
}

async fn run_corrections(
    cfg: &config::Config,
    project: Option<&str>,
    all: bool,
    limit: i64,
) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    let filter = if all {
        None
    } else {
        Some(resolve_project(project)?)
    };
    let corrections =
        db::get_memories_by_kind(&database, filter.as_deref(), "error_solution", limit).await?;

    if corrections.is_empty() {
        match &filter {
            Some(p) => println!("No corrections recorded for {p}."),
            None => println!("No corrections recorded."),
        }
        return Ok(());
    }

    println!("{} correction(s):", corrections.len());
    for c in &corrections {
        println!("\n• {}", c.summary);
    }
    Ok(())
}

async fn run_graph(
    cfg: &config::Config,
    entity: &str,
    project: Option<&str>,
    all: bool,
    history: bool,
    at: Option<&str>,
    limit: i64,
) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    if let Some(at) = at {
        anyhow::ensure!(
            provider::is_valid_memory_date(at),
            "--at must be a valid YYYY-MM-DD date"
        );
    }
    let filter = if all {
        None
    } else {
        Some(resolve_project(project)?)
    };
    let edges = db::memory_edges_for_entity_at(
        &database,
        filter.as_deref(),
        entity,
        history,
        at,
        limit.max(1) as usize,
    )
    .await?;

    if edges.is_empty() {
        match &filter {
            Some(p) => println!("No graph edges for {entity:?} in {p}."),
            None => println!("No graph edges for {entity:?}."),
        }
        return Ok(());
    }

    match at {
        Some(at) => println!("{} graph edge(s) for {entity:?} at {at}:", edges.len()),
        None => println!("{} graph edge(s) for {entity:?}:", edges.len()),
    }
    for e in &edges {
        let mut suffix = String::new();
        if let Some(from) = &e.valid_from {
            suffix.push_str(&format!(" from {from}"));
        }
        if let Some(until) = &e.valid_until {
            suffix.push_str(&format!(" until {until}"));
        }
        if let Some(reason) = &e.superseded_reason {
            suffix.push_str(&format!(" [{reason} -> {:?}]", e.superseded_by));
        }
        println!(
            "\n• {} --{}--> {}{} (memory {}, confidence {:.2})",
            e.source, e.relation, e.target, suffix, e.memory_id, e.confidence
        );
    }
    Ok(())
}

async fn run_graph_delete(cfg: &config::Config, edge_id: i64) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let deleted = db::curate_memory_edge_delete(&database, edge_id).await?;
    if deleted {
        println!("Graph edge {edge_id} marked user_deleted.");
    } else {
        println!("Graph edge {edge_id} was not active or does not exist.");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_graph_update(
    cfg: &config::Config,
    edge_id: i64,
    source: &str,
    relation: &str,
    target: &str,
    valid_from: Option<&str>,
    valid_until: Option<&str>,
    confidence: f64,
) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    if let Some(v) = valid_from {
        anyhow::ensure!(
            provider::is_valid_memory_date(v),
            "--valid-from must be YYYY-MM-DD"
        );
    }
    if let Some(v) = valid_until {
        anyhow::ensure!(
            provider::is_valid_memory_date(v),
            "--valid-until must be YYYY-MM-DD"
        );
    }
    let updated = db::curate_memory_edge_update(
        &database,
        edge_id,
        source,
        relation,
        target,
        valid_from,
        valid_until,
        confidence,
    )
    .await?;
    if updated {
        println!("Graph edge {edge_id} updated.");
    } else {
        println!("Graph edge {edge_id} does not exist.");
    }
    Ok(())
}

async fn run_reconcile(
    cfg: &config::Config,
    project: Option<&str>,
    all: bool,
    dry_run: bool,
) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    let filter = if all {
        None
    } else {
        Some(resolve_project(project)?)
    };
    let report = db::reconcile_memory_graph(&database, filter.as_deref(), dry_run).await?;
    println!(
        "Graph reconciliation{}: scanned={}, duplicates={}, current_state_updates={}, active_edges={}",
        if dry_run { " dry-run" } else { "" },
        report.scanned,
        report.duplicates,
        report.current_state_updates,
        report.active_edges
    );
    Ok(())
}

async fn run_graph_backfill(
    cfg: &config::Config,
    project: Option<&str>,
    all: bool,
    limit: i64,
    dry_run: bool,
) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    if let Err(e) = provider::resolve_api_key(cfg.provider) {
        println!("Graph backfill skipped: provider is not configured ({e}).");
        return Ok(());
    }

    let filter = if all {
        None
    } else {
        Some(resolve_project(project)?)
    };
    let memories =
        db::memories_without_edges(&database, filter.as_deref(), limit.max(1) as usize).await?;
    let mut scanned = 0_usize;
    let mut with_relations = 0_usize;
    let mut inserted = 0_usize;

    for memory in &memories {
        scanned += 1;
        let relations = provider::extract_relations_from_memory_text(
            &memory.summary,
            memory.tags.as_deref(),
            cfg,
        )
        .await?;
        if !relations.is_empty() {
            with_relations += 1;
        }
        if dry_run {
            inserted += relations.len();
            continue;
        }
        for relation in relations {
            let edge = db::NewMemoryEdge {
                project: memory.project.clone(),
                memory_id: memory.id,
                source: relation.source.clone(),
                relation: relation.relation.clone(),
                target: relation.target.clone(),
                valid_from: relation.valid_from.clone(),
                valid_until: relation.valid_until.clone(),
                confidence: relation.confidence,
            };
            match db::insert_memory_edge(&database, &edge).await {
                Ok(_) => {
                    inserted += 1;
                    let _ = db::insert_memory_entity(&database, memory.id, &relation.source).await;
                    let _ = db::insert_memory_entity(&database, memory.id, &relation.target).await;
                }
                Err(e) => tracing::warn!("graph backfill edge skipped (memory {}): {e}", memory.id),
            }
        }
    }

    println!(
        "Graph backfill{}: scanned={}, memories_with_relations={}, edges={}",
        if dry_run { " dry-run" } else { "" },
        scanned,
        with_relations,
        inserted
    );
    Ok(())
}

async fn run_compress_cmd(cfg: &config::Config, session_id: &str) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    // Surface a friendly error early if the session doesn't exist.
    db::get_session(&database, session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

    let observations = db::get_observations_for_session(&database, session_id).await?;
    if observations.is_empty() {
        println!("No observations for session {}", session_id);
        return Ok(());
    }

    println!(
        "Compressing {} observations via {}...",
        observations.len(),
        cfg.model
    );

    let (embedder, store) = vectorstore::build_semantic(&database, cfg).await;
    let memory_id = compress::run(
        &database,
        embedder.as_deref(),
        store.as_ref(),
        cfg,
        session_id,
    )
    .await?;

    println!("✅ Memory created (id={})", memory_id);
    if let Some(memory) = db::get_memory_by_id(&database, memory_id).await? {
        println!("\nSummary:\n{}", memory.summary);
        println!("\nTags: {}", memory.tags.unwrap_or_default());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_sweep_cmd(
    cfg: &config::Config,
    compress_idle: &str,
    min_observations: i64,
    limit: i64,
    dry_run: bool,
    dream_due: bool,
    apply: bool,
) -> Result<()> {
    let (database, embedder, store) = sweep::open_db_and_semantic(cfg).await?;
    if dream_due {
        let due_after_secs = (cfg.scheduler.dream_interval_hours as i64).max(1) * 60 * 60;
        let options = sweep::DreamSweepOptions {
            due_after_secs,
            apply,
            dry_run,
            limit: limit.max(1),
        };
        let report = sweep::run_dream_sweep(
            &database,
            cfg,
            &options,
            embedder.as_deref(),
            store.as_ref(),
        )
        .await?;
        print_sweep_report(
            "dream",
            dry_run,
            &format!(
                "dream_due=true apply={} due_after={}h limit={}",
                apply,
                cfg.scheduler.dream_interval_hours,
                limit.max(1)
            ),
            &report,
        );
        return Ok(());
    }

    let options = sweep::CompressionSweepOptions {
        compress_idle_secs: sweep::parse_duration_secs(compress_idle)?,
        min_observations: min_observations.max(0),
        limit: limit.max(1),
        dry_run,
        lease_secs: (cfg.auto_compress.lease_minutes as i64).max(1) * 60,
    };
    let report = sweep::run_compression_sweep(
        &database,
        cfg,
        &options,
        embedder.as_deref(),
        store.as_ref(),
    )
    .await?;
    print_sweep_report(
        "compress",
        dry_run,
        &format!(
            "compress_idle={} min_observations={} limit={}",
            compress_idle,
            min_observations.max(0),
            limit.max(1)
        ),
        &report,
    );
    Ok(())
}

fn print_sweep_report(kind: &str, dry_run: bool, header: &str, report: &sweep::SweepReport) {
    let dry_run = dry_run || report.dry_run;
    println!("SWEEP kind={kind} dry_run={dry_run} {header}");
    for action in &report.actions {
        let session = action.session_id.as_deref().unwrap_or("-");
        let idle = action
            .idle_secs
            .map(format_duration)
            .unwrap_or_else(|| "-".to_string());
        let observations = action
            .observation_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "candidate session={} project={} idle={} observations={} action={} status={} reason={}{}",
            session,
            action.project,
            idle,
            observations,
            action.action,
            action.status,
            action.reason,
            action
                .detail
                .as_deref()
                .map(|d| format!(" detail={}", d.replace('\n', " ")))
                .unwrap_or_default()
        );
    }
    println!(
        "summary candidates={} compressed={} dreamed={} skipped={} failed={}",
        report.candidates, report.compressed, report.dreamed, report.skipped, report.failed
    );
}

fn format_duration(secs: i64) -> String {
    if secs >= 3600 {
        format!("{:.1}h", secs as f64 / 3600.0)
    } else if secs >= 60 {
        format!("{:.1}m", secs as f64 / 60.0)
    } else {
        format!("{secs}s")
    }
}

async fn run_scheduler_cmd(cfg: config::Config, action: SchedulerCommands) -> Result<()> {
    match action {
        SchedulerCommands::Run => sweep::run_scheduler(cfg).await,
        SchedulerCommands::InstallLaunchd => {
            let path = sweep::install_launchd(&cfg)?;
            println!("Installed launchd agent: {}", path.display());
            #[cfg(target_os = "macos")]
            {
                let domain = launchd_gui_domain()?;
                let _ = std::process::Command::new("launchctl")
                    .arg("bootout")
                    .arg(&domain)
                    .arg(&path)
                    .status();
                match std::process::Command::new("launchctl")
                    .arg("bootstrap")
                    .arg(&domain)
                    .arg(&path)
                    .status()
                {
                    Ok(status) if status.success() => {
                        println!("launchd agent started: {}", cfg.scheduler.launchd_label);
                    }
                    Ok(status) => {
                        println!(
                            "launchd plist written, but bootstrap exited with status {status}. Start manually with: launchctl bootstrap {} {}",
                            domain,
                            path.display()
                        );
                    }
                    Err(e) => {
                        println!(
                            "launchd plist written, but bootstrap failed: {e}. Start manually with: launchctl bootstrap {} {}",
                            domain,
                            path.display()
                        );
                    }
                }
            }
            Ok(())
        }
        SchedulerCommands::UninstallLaunchd => {
            #[cfg(target_os = "macos")]
            {
                let domain = launchd_gui_domain()?;
                let label = format!("{}/{}", domain, cfg.scheduler.launchd_label);
                let _ = std::process::Command::new("launchctl")
                    .arg("bootout")
                    .arg(&label)
                    .status();
            }
            let path = sweep::uninstall_launchd(&cfg)?;
            println!("Removed launchd agent: {}", path.display());
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
fn launchd_gui_domain() -> Result<String> {
    let output = std::process::Command::new("id").arg("-u").output()?;
    if !output.status.success() {
        anyhow::bail!("failed to resolve uid with id -u");
    }
    let uid = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(format!("gui/{uid}"))
}

async fn run_embed(
    cfg: &config::Config,
    project: Option<&str>,
    all: bool,
    force: bool,
) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    let embedder = match embedder::resolve_embedder(cfg).await {
        Some(e) => e,
        None => anyhow::bail!(
            "No embedder configured. Set `embedding.provider` in settings, or run a local \
             Ollama (e.g. `ollama pull nomic-embed-text`), then retry."
        ),
    };
    let store = vectorstore::make_vector_store(&database, embedder.dim()).await;

    let filter = if all { None } else { project };
    let scope = filter.unwrap_or("all projects");
    println!(
        "Embedding memories ({}) with {}{}...",
        scope,
        embedder.id(),
        if force { ", rebuilding index" } else { "" }
    );

    let count =
        vectorstore::backfill(&database, embedder.as_ref(), store.as_ref(), filter, force).await?;

    println!("✅ Embedded {} memory(ies)", count);
    Ok(())
}

fn resolve_project(project: Option<&str>) -> Result<String> {
    match project {
        Some(p) => Ok(p.to_string()),
        None => {
            let cwd = std::env::current_dir()?;
            // Walk up to find git root
            let mut dir = cwd.as_path();
            loop {
                if dir.join(".git").exists() {
                    return Ok(dir.to_string_lossy().to_string());
                }
                match dir.parent() {
                    Some(parent) => dir = parent,
                    None => return Ok(cwd.to_string_lossy().to_string()),
                }
            }
        }
    }
}

fn format_timestamp(timestamp: i64) -> String {
    match Local.timestamp_opt(timestamp, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S %Z").to_string(),
        None => timestamp.to_string(),
    }
}
