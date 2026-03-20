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

IronMem runs entirely on your local machine. There is no cloud component, no telemetry, and no data leaves your machine (except API calls to your configured LLM provider, which you control).

Security concerns most relevant to this project:
- Local SQLite database access permissions
- API key handling and storage
- The HTTP server binds to `127.0.0.1` only (localhost)

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 0.1.x   | Yes       |

We only support the latest release. Please update before reporting.
