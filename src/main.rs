mod ccr;
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
mod hooks;
mod mcp;
mod profile;
mod provider;
mod retrieval;
mod server;
mod strutil;
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
    },

    /// Search memories across all projects
    SearchGlobal {
        /// Search query
        query: String,
        /// Max results
        #[arg(short, long, default_value = "10")]
        limit: i64,
    },

    /// List recent memories for a project
    List {
        /// Project root path (defaults to current directory)
        #[arg(short, long)]
        project: Option<String>,
        /// Max results
        #[arg(short, long, default_value = "5")]
        limit: i64,
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

#[tokio::main]
async fn main() -> Result<()> {
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
        } => run_search(&cfg, &query, project.as_deref(), limit).await?,
        Commands::SearchGlobal { query, limit } => run_search_global(&cfg, &query, limit).await?,
        Commands::List { project, limit } => run_list(&cfg, project.as_deref(), limit).await?,
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
        } => {
            run_remember(
                &cfg,
                &text,
                project.as_deref(),
                &scope,
                &kind,
                tags.as_deref(),
            )
            .await?
        }
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
        Commands::Gc => run_gc(&cfg).await?,
        Commands::Embed {
            project,
            all,
            force,
        } => run_embed(&cfg, project.as_deref(), all, force).await?,
        Commands::Eval { out } => run_eval(&cfg, &out).await?,
        Commands::Config => {
            println!("{}", serde_json::to_string_pretty(&cfg)?);
        }
    }

    Ok(())
}

async fn run_server(cfg: config::Config) -> Result<()> {
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
) -> Result<()> {
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let memories = db::search_memories(&database, &project, query, limit).await?;

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

async fn run_search_global(cfg: &config::Config, query: &str, limit: i64) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let memories = db::search_all_memories(&database, query, limit).await?;

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

async fn run_list(cfg: &config::Config, project: Option<&str>, limit: i64) -> Result<()> {
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let memories = db::get_recent_memories(&database, &project, limit).await?;

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
    let count = db::delete_memories_for_project(&database, &project).await?;
    for id in ids {
        if let Err(e) = vectorstore::purge_memory(&database, store.as_ref(), id).await {
            tracing::warn!("vector/meta cleanup failed for memory {id}: {e}");
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

    hooks::write_ironmem_file(&project, &memories)?;
    hooks::ensure_claude_md_import(&project)?;

    println!(
        "Injected {} memories into IRONMEM.md for {}",
        memories.len(),
        project
    );
    Ok(())
}

async fn run_remember(
    cfg: &config::Config,
    text: &str,
    project: Option<&str>,
    scope: &str,
    kind: &str,
    tags: Option<&str>,
) -> Result<()> {
    if text.trim().is_empty() {
        anyhow::bail!("memory text must not be empty");
    }
    let project = resolve_project(project)?;
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;

    let (embedder, store) = vectorstore::build_semantic(&database, cfg).await;
    let id = compress::remember(
        &database,
        embedder.as_deref(),
        store.as_ref(),
        &project,
        scope,
        kind,
        text,
        tags,
    )
    .await?;

    println!(
        "✅ Remembered memory id={} (scope={}, kind={}) for {}",
        id,
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
