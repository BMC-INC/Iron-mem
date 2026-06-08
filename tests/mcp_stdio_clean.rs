//! Regression: in stdio MCP mode, stdout IS the JSON-RPC transport, so NOTHING
//! but JSON-RPC may be written there. A `tracing` line leaking to stdout (it
//! starts with an ISO timestamp like `2026-06-07T…`) makes the MCP client throw
//! `Unexpected token ', "2026-0"... is not valid JSON` the moment it connects.
//! This test spawns the real binary and asserts stdout is pure JSON.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn mcp_stdio_stdout_is_pure_json() {
    let bin = env!("CARGO_BIN_EXE_ironmem");
    let db = std::env::temp_dir().join(format!("ironmem-stdio-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);

    // Build a sqlx-valid sqlite URL on BOTH platforms (mirrors db::sqlite_file_url):
    // a Unix abs path already starts with '/', but a Windows `C:\…` path needs an
    // extra leading '/' and forward slashes → `sqlite:///C:/…`. Passing the raw
    // `C:\…` (two slashes) makes sqlx treat the drive as the URL authority and the
    // server fails to open the DB, producing empty stdout.
    let p = db.to_string_lossy().replace('\\', "/");
    let url = if p.as_bytes().get(1) == Some(&b':') {
        format!("sqlite:///{p}?mode=rwc")
    } else {
        format!("sqlite://{p}?mode=rwc")
    };

    let mut child = Command::new(bin)
        .arg("mcp")
        .env("DATABASE_URL", &url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `ironmem mcp`");

    // Drain stdout on a reader thread so we can wait for the response by content
    // rather than by a fixed sleep — the embedder probe on boot makes startup
    // latency wildly variable across CI runners (a fixed sleep raced on slow
    // Windows boxes). We succeed as soon as the init response arrives.
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        let mut br = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match br.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{init}\n").as_bytes())
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut lines: Vec<String> = Vec::new();
    let mut saw_init_response = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(l) => {
                let is_init = serde_json::from_str::<serde_json::Value>(l.trim())
                    .ok()
                    .map(|v| {
                        v.get("id").and_then(|x| x.as_i64()) == Some(0)
                            && v.get("result").is_some()
                    })
                    .unwrap_or(false);
                lines.push(l);
                if is_init {
                    saw_init_response = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    while let Ok(l) = rx.try_recv() {
        lines.push(l);
    }
    let _ = reader.join();
    let _ = std::fs::remove_file(&db);

    // Every non-empty stdout line must be valid JSON — no log/banner leakage.
    for line in lines.iter().filter(|l| !l.trim().is_empty()) {
        serde_json::from_str::<serde_json::Value>(line.trim()).unwrap_or_else(|e| {
            panic!("stdout must be pure JSON-RPC, found non-JSON line ({e}): {line:?}")
        });
    }
    assert!(
        saw_init_response,
        "expected an initialize response on stdout; got: {lines:?}"
    );
}
