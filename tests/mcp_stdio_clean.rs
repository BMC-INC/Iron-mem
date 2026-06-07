//! Regression: in stdio MCP mode, stdout IS the JSON-RPC transport, so NOTHING
//! but JSON-RPC may be written there. A `tracing` line leaking to stdout (it
//! starts with an ISO timestamp like `2026-06-07T…`) makes the MCP client throw
//! `Unexpected token ', "2026-0"... is not valid JSON` the moment it connects.
//! This test spawns the real binary and asserts stdout is pure JSON.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn mcp_stdio_stdout_is_pure_json() {
    let bin = env!("CARGO_BIN_EXE_ironmem");
    let db = std::env::temp_dir().join(format!("ironmem-stdio-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);

    let mut child = Command::new(bin)
        .arg("mcp")
        .env("DATABASE_URL", format!("sqlite://{}?mode=rwc", db.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `ironmem mcp`");

    let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{init}\n").as_bytes())
        .unwrap();

    // Let it start up (embedder init logs here) and answer initialize. Debug
    // builds probe the embedder provider on boot, so give it generous headroom.
    std::thread::sleep(Duration::from_millis(3000));
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_file(&db);

    // Every non-empty stdout line must be valid JSON — no log/banner leakage.
    let mut saw_init_response = false;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout must be pure JSON-RPC, found non-JSON line ({e}): {line:?}"));
        if parsed.get("id").and_then(|v| v.as_i64()) == Some(0) && parsed.get("result").is_some() {
            saw_init_response = true;
        }
    }
    assert!(
        saw_init_response,
        "expected an initialize response on stdout; got: {stdout:?}"
    );
}
