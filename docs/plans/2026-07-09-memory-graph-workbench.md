# Memory Graph Workbench Implementation Plan

> **For Codex:** Implement this plan task-by-task with test-driven development and verification before completion.

**Goal:** Build IronMem's flagship local-first graph investigation UI with temporal filtering, evidence provenance, original-source expansion, and retrieval tracing.

**Architecture:** Add bounded graph-window and memory-evidence REST contracts over the existing SQLite/Postgres data model, then replace the static memory browser with a dependency-free canvas workbench. Preserve all retrieval, governance, and storage behavior; the workbench is an additive read surface except for existing explicitly confirmed maintenance actions.

**Tech Stack:** Rust, Axum, SQLx Any, SQLite/Postgres, static HTML/CSS/JavaScript, Canvas 2D.

---

### Task 1: Bounded Graph Window

**Files:**
- Modify: `src/db.rs`

1. Add a failing database test covering global recent edges, project filtering, normalized source/relation/target matching, superseded history, valid-time filtering, and limit enforcement.
2. Run the focused test and confirm it fails because `memory_graph_window` is absent.
3. Implement `memory_graph_window` with bound parameters and a hard caller-supplied limit.
4. Run the focused graph tests and confirm they pass.

### Task 2: Workbench APIs

**Files:**
- Modify: `src/server.rs`

1. Add route-contract tests for parameter validation and evidence response shaping.
2. Register `GET /api/graph/window` and `GET /api/memories/{id}/evidence`.
3. Expose only useful governance metadata; keep full source expansion behind `POST /retrieve_original`.
4. Run focused tests and isolated-database HTTP smoke checks.

### Task 3: Production Workbench UI

**Files:**
- Replace: `src/web_ui.html`

1. Build the responsive shell, filters, canvas, relationship list, evidence inspector, retrieval trace, status, loading, empty, and error states.
2. Implement a bounded force layout, selection, dragging, zooming, panning, fit, focus, history styling, and trace highlighting.
3. Connect project discovery, graph window, evidence detail, context trace, and original expansion APIs.
4. Keep all rendering escaped or assigned through safe text APIs where memory content enters the DOM.

### Task 4: Documentation And Verification

**Files:**
- Modify: `README.md`

1. Document the Memory Graph Workbench and its local URL.
2. Run `cargo test --bin ironmem` and `cargo test --test mcp_stdio_clean`.
3. Run `cargo clippy --bin ironmem --features local-onnx -- -D warnings`.
4. Run `cargo build --release --features local-onnx`.
5. Start an isolated server and verify API behavior with `curl`.
6. Verify desktop and mobile UI behavior in a real browser, including screenshots, console, network, accessibility, and nonblank canvas pixels.
7. Review the final diff, commit once, push, create and merge one PR, sync `main`, deploy the release binary, and verify live `/status`, `/ui`, and graph APIs.
