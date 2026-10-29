# Release integration checks

IronMem release preparation tests the actual executable with disposable settings and a temporary SQLite store. It does not load a personal memory database, change assistant settings or replace a running installation.

- `scripts/smoke_mcp_sdk.py --binary target/release/ironmem --expected-version 0.4.1` uses the official MCP Python SDK 1.30.0. It requires CLI/MCP/release version agreement, initializes the transport, lists tools, saves a synthetic fact and retrieves it.
- The dedicated Cursor plugin's `scripts/smoke_mcp.py` invokes its real `mcp.json` command, verifies required tools, save/search, malformed wipe rejection, preview behavior and deletion within a disposable store.
- Installed Claude Code 2.1.306 was checked through a separate temporary client configuration and reported the release server Connected. This is a client handshake check, without a paid model request or a desktop GUI acceptance claim.
- The release workflow builds default-feature binaries for Linux x86-64, macOS Intel/Apple Silicon and Windows x86-64. CI separately covers installation/recovery, memory-quality fixtures, Clippy and PostgreSQL foundations.

The SDK smoke dependency belongs in a test-only virtual environment (`pip install mcp==1.30.0`). It is not a runtime dependency of IronMem. The expected-version argument prevents an old binary or stale MCP metadata from passing release validation.

Marketplace approval, every graphical client, optional ONNX/GPU backends and a live service upgrade are separate acceptance steps. The archive does not modify existing client configurations; follow the documented backup and stopped-process upgrade procedure before replacing a live executable.
