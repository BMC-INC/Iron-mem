mod config;
mod db;
mod hooks;
mod mcp;
mod provider;
mod server;

use anyhow::Result;
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "ironmem",
    about = "Persistent session memory for Claude Code and AI coding assistants",
    version = "0.1.0"
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

    /// Manually compress a session
    Compress {
        /// Session ID to compress
        session_id: String,
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ironmem=info".parse().unwrap()),
        )
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
        Commands::Compress { session_id } => run_compress_cmd(&cfg, &session_id).await?,
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

    let state = server::AppState {
        db: (*rest_db).clone(),
        config: rest_cfg,
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
    let count = db::delete_memories_for_project(&database, &project).await?;

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
    let memories = db::get_recent_memories(&database, &project, limit).await?;

    hooks::write_ironmem_file(&project, &memories)?;
    hooks::ensure_claude_md_import(&project)?;

    println!(
        "Injected {} memories into IRONMEM.md for {}",
        memories.len(),
        project
    );
    Ok(())
}

async fn run_compress_cmd(cfg: &config::Config, session_id: &str) -> Result<()> {
    let database = db::Database::new(&cfg.effective_database_url()).await?;
    database.migrate().await?;
    let session = db::get_session(&database, session_id)
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

    let result = provider::compress(&observations, cfg).await?;
    let memory_id = db::insert_memory(
        &database,
        &session.project,
        session_id,
        &result.summary,
        Some(&result.tags),
    )
    .await?;
    db::mark_compressed(&database, session_id).await?;

    println!("✅ Memory created (id={})", memory_id);
    println!("\nSummary:\n{}", result.summary);
    println!("\nTags: {}", result.tags);
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
