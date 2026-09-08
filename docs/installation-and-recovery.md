# Install, connect, upgrade and recover

## Install

Run `./install.sh` on Unix or `./install.ps1` on Windows from a source checkout.
Python 3.9+ and Rust/Cargo are required for source builds. The streamed Unix
installer also requires Git, creates one shallow temporary source checkout, and
removes that checkout and its build outputs on exit. Both installers use the
same implementation and `cargo build --release --locked`. For an already built
binary, pass `--binary /absolute/path/to/ironmem` (use `ironmem.exe` on Windows).

The installer preserves existing IronMem settings and unrelated Claude hooks.
Unix lifecycle hooks are merged using nested hook groups, including migration of
IronMem's old flat entries. An identical reinstall writes nothing and creates no
backup. A changed installation keeps a private upgrade directory with previous
files and a verified, consistent SQLite backup before replacement. Individual
files are atomically replaced; an ordinary installation error restores files
already replaced. This is not a filesystem-wide power-loss transaction.

Local extraction requires no cloud key. No provider credential is installed or
printed. The installer does not restart running services. Add the printed binary
folder to PATH yourself if necessary.

## Connect an assistant

The installer writes `.ironmem/mcp.json` with an absolute binary path and explicit
settings file. Merge its `mcpServers.ironmem` entry into an assistant that accepts
that format, preserving the assistant's other servers. Other clients can use the
same executable and `--config /absolute/path/to/settings.json mcp` arguments.
Native Windows and custom `--prefix` installations use MCP; Bash lifecycle hooks
are not silently bound to a different default data directory.

Start a new assistant connection, call `get_status`, remember a harmless project
fact, and retrieve it. The automated newcomer rehearsal exercises real stdio MCP
initialization, tool listing and diagnostics, then CLI save/search and independent
checkpoint export/import. It does not claim an observed GUI click-through in every
third-party assistant.

Hook configuration follows the [Claude Code hook reference](https://code.claude.com/docs/en/hooks).

## Isolate a trial

`ironmem --config /absolute/path/to/settings.json ...` reads that file and persists
settings changes back to it. It does not initialize the default settings file.
A minimal isolated file can specify only an absolute `db_path`; remaining fields
use defaults. `DATABASE_URL` still has its documented override precedence, so
remove it from a trial's environment. Included fixture runners use a small clean
environment with no inherited API keys or database overrides.

## Upgrade and rollback

Stop workers/schedulers/MCP processes before a real upgrade. The installer does
not stop them for you. Review its backup path and preserve it outside the repository.
Native SQLite backups use the backup API, including committed WAL state, and an
integrity check. A configured external database or a `DATABASE_URL` override
requires an operator-created backup and `--external-backup-confirmed`. Relative
configured database paths are refused during backup because they depend on a
process's working directory.

The backup manifest maps each retained file to its original path. To roll back,
stop all processes, restore the previous binary/settings/hooks from that manifest,
and restore the matching pre-upgrade database backup. Remove WAL/SHM sidecars only
while the database is closed and when replacing the database with that backup.
Do not run an old binary against a newly migrated database. Keep service processes
stopped until the restored backup passes an integrity check. Backups are private
recovery artifacts; the installer does not automatically prune them.

## Project recovery

Use `snapshot create --project /workspace/example --incremental`, then
`snapshot export SNAPSHOT_ID /new/path/checkpoint.json` to create an independent
project checkpoint. On a fresh isolated store, run `snapshot import PATH --dry-run`
first, then `snapshot import PATH`, and verify a remembered fact. Full database
backups remain necessary for complete operational telemetry and namespace audit
history. Current revocations and immutable assertion history have the semantics
in the temporal assertion documentation; project restore is not physical erasure.

## Run the newcomer rehearsal

```sh
cargo build
python3 scripts/test_reliability_tools.py
```

It uses temporary settings, databases and install directories. It checks MCP
transport, retrieval, export/import, idempotent installation, backup recovery,
hook merging, stale purge plans, legal holds, writer locks and empty-store
reinitialization. No installed service or personal memory is modified.
