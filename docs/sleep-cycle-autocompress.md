# IronMem Sleep Cycle Auto-Compression

IronMem can compact idle or high-volume sessions without waiting for a client to
call `session_end` or `compress_session`. The implementation is a product
feature, not a cron-only wrapper: the core sweep logic lives in IronMem, uses DB
leases for idempotency, records audit events, and preserves raw observations and
CCR originals.

## Commands

Preview candidates without mutating anything:

```bash
ironmem sweep --compress-idle 30m --min-observations 50 --limit 20 --dry-run
```

Run one compression sweep:

```bash
ironmem sweep --compress-idle 30m --min-observations 50 --limit 20
```

Run due dream/reflection work. Without `--apply`, dream writes proposals first;
with `--apply`, it promotes accepted consolidations and derived facts.

```bash
ironmem sweep --dream-due --limit 200
ironmem sweep --dream-due --apply --limit 200
```

Run the unattended scheduler loop:

```bash
ironmem scheduler run
```

Install or remove the macOS launchd worker:

```bash
ironmem scheduler install-launchd
ironmem scheduler uninstall-launchd
```

The launchd worker writes logs under `~/.ironmem/logs/` and runs the long-lived
`ironmem scheduler run` process. It does not repeatedly spawn overlapping jobs.

## Candidate Rules

A session is eligible when all of these are true:

- `sessions.compressed = 0`
- it has at least one observation
- it has at least `--min-observations` observations, or its last observation /
  end / start timestamp is older than `--compress-idle`

Never-ended sessions are included. Before real compression, IronMem marks the
session ended if it is still open, then calls the same shared compression path
used by CLI, REST, and MCP. Provider failures do not mark the session compressed.

## Idempotency and Audit

The sweep creates a DB lease in `sweep_leases` before compressing a session. A
second sweeper cannot acquire the same active lease, and repeated sweeps skip
already-compressed sessions. Real attempts write `sweep_events` rows with action,
status, reason, detail, and the source activity timestamp.

Dry-runs write nothing.

## Auth Posture

Do not rely on interactive user ADC for unattended Vertex calls.

Cloud deployments should use an attached service account on GCE, Cloud Run, or
Cloud Scheduler with least-privilege Vertex permissions such as
`roles/aiplatform.user`. That lets Vertex authenticate through the metadata
server without a key file.

Local macOS unattended mode may use:

```bash
export GOOGLE_APPLICATION_CREDENTIALS=/private/path/outside/repo/ironmem-sa.json
```

Only use a service-account key deliberately, store it outside the repo, keep it
private, and rotate it. IronMem never prints key contents; the launchd plist may
include the key path if that environment variable is present at install time.

## Config

`~/.ironmem/settings.json` can include:

```json
{
  "auto_compress": {
    "enabled": true,
    "idle_minutes": 30,
    "min_observations": 50,
    "limit": 20,
    "provider_backoff_minutes": 30,
    "lease_minutes": 30
  },
  "scheduler": {
    "enabled": true,
    "sweep_interval_minutes": 15,
    "dream_interval_hours": 24,
    "launchd_label": "com.execlayer.ironmem.sleep"
  }
}
```

`ironmem sweep` uses CLI flags first. `ironmem scheduler run` uses the configured
thresholds and backs off after provider failures to avoid tight retry loops.
