# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in IronMem, **please do not open a public GitHub issue.**

Instead, email us at: **security@execlayer.com**

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

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 0.4.x   | Yes       |

We only support the latest release. Please update before reporting.
