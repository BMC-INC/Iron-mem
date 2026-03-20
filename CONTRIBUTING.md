# Contributing to Iron-Mem

Thanks for your interest in contributing. Iron-Mem is a small, focused tool — contributions are welcome as long as they serve the core use case: lightweight, provider-agnostic session memory for AI coding assistants.

## Before you open a PR

Open an issue first. Describe what you want to change and why. This saves both of us time — if the change doesn't fit the project's direction, better to know before you build it.

## What gets accepted

- Bug fixes
- Hook compatibility improvements for other AI coding tools (Cursor, Windsurf, Copilot, etc.)
- Performance improvements to the Rust worker
- Documentation improvements

## What won't be accepted

- Adding new runtime dependencies (no Bun, no Python, no external databases)
- Web UI or dashboard features
- Sync, cloud, or multi-machine features
- Anything that makes the install more complex

## How to submit a PR

1. Fork the repo
2. Create a branch: `git checkout -b fix/your-fix-name`
3. Make your changes
4. Test locally with `cargo build --release` and verify hooks work end-to-end
5. Open a PR against `main` with a clear description of what changed and why

## Code style

Standard Rust formatting. Run `cargo fmt` before submitting. No warnings allowed — `cargo clippy` should pass clean.

## Questions

Open an issue. Don't email directly.
