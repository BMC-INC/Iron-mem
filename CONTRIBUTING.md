# Contributing to IronMem

IronMem provides persistent project memory for coding assistants. Keep the default SQLite installation local and lightweight.

## Before you open a PR

Open an issue first. Describe what you want to change and why. This saves both of us time — if the change doesn't fit the project's direction, better to know before you build it.

## Scope

Bug fixes, regression tests, performance improvements and documentation are welcome for the CLI, MCP server, REST API, workbench, hooks and supported optional integrations. The [reference](docs/reference.md) describes supported storage backends and configuration. Discuss new runtime dependencies, storage backends and remote services in an issue before adding them. Keep optional dependencies off the default runtime path.

## Tests

Add automated tests for every major new capability. Include a regression test when fixing a bug where practical. Exercise the affected interface and its failure cases. Describe the commands and results in the pull request. For documentation-only changes, check the links and examples you changed.

## How to submit a PR

1. Fork the repo
2. Create a branch: `git checkout -b fix/your-fix-name`
3. Make your changes
4. For Rust changes, run `cargo test --locked` and `cargo clippy --locked --all-targets -- -D warnings`. Verify hooks end to end when changing them
5. Open a PR against `main` with a clear description of what changed and why

## Code style

Standard Rust formatting. Run `cargo fmt` before submitting. No warnings allowed — `cargo clippy` should pass clean.

## Questions

Use [Issues](https://github.com/BMC-INC/Iron-mem/issues) for general questions. For a suspected vulnerability, follow the [security policy](SECURITY.md).
