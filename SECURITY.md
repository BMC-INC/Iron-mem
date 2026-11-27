# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in IronMem, **please do not open a public GitHub issue.**

Use GitHub’s private HTTPS [Report a vulnerability](https://github.com/BMC-INC/Iron-mem/security/advisories/new) form to send the report directly to the IronMem maintainers.

Include:
- A description of the vulnerability
- Steps to reproduce
- The potential impact
- Any suggested fix (optional but appreciated)

We will acknowledge your report within 48 hours and aim to provide a fix or mitigation within 7 days for critical issues.

## Scope

IronMem stores data on the machine where it runs. It does not send product analytics. Local access counters stay in the database. Memory text leaves the machine only on the paths described in [PRIVACY.md](PRIVACY.md).

Network listeners:

- `ironmem server` binds the REST API to `127.0.0.1` only.
- When MCP transport is `sse` (`mcp_transport` in settings, or `IRONMEM_MCP_TRANSPORT=sse`), the MCP listener also defaults to `127.0.0.1`. Set `--mcp-sse-bind` or `IRONMEM_MCP_SSE_BIND` to listen on another interface. Binding a non-loopback address without `auth_token` prints a warning.
- `ironmem serve` binds to `127.0.0.1` only and requires a bearer token unless you pass `--no-auth`.

## Cryptographic defaults and deployment settings

- The native runtime defaults to local SQLite. Content and evidence hashes use SHA-256. HTTPS provider clients use rustls. Its ring provider offers TLS 1.3 and TLS 1.2 with ephemeral key agreement and authenticated encryption. See the [dependency configuration](Cargo.toml) and [pinned dependency versions](Cargo.lock).
- Purpose enforcement defaults to advisory mode with both key files unset. Strict remote attestations and operator confirmations are optional. They use HMAC-SHA-256 over a versioned message. Use separate, randomly generated keys of at least 32 bytes for these roles. Protect the files with operating-system access controls. See the [defaults](src/config.rs), [verification](src/purpose.rs) and [enforcement](src/egress.rs).
- For an optional PostgreSQL server across a network, use a trusted server CA and `sslmode=verify-full`, and configure the server for SCRAM-SHA-256 authentication. SQLx's default `prefer` permits a plaintext fallback. A local SQLite installation has no database network connection.
- For remote MCP, the [optional gateway](deploy/cloudflare-gateway/README.md) delegates identity and public TLS to the operator’s edge service. Restrict the origin to its local interface or private network. Set the public TLS policy at the edge. The default CLI and stdio transport require no hosted gateway.

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 0.4.x   | Yes       |

We only support the latest release. Please update before reporting.
