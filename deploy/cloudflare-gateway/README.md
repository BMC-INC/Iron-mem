# Authenticated Cloudflare gateway

This gateway gives a cloud MCP client a stable HTTPS path to a local IronMem
database without moving the database into the cloud or requiring a domain on
Cloudflare.

The request path is:

```text
MCP client -> Cloudflare Access Managed OAuth -> Worker -> Workers VPC
           -> named Cloudflare Tunnel -> 127.0.0.1:37779/mcp
```

The Worker fails closed unless Cloudflare Access supplies authenticated identity
context. It accepts only `GET`, `POST`, and `DELETE` on `/mcp`, removes browser,
Access, and forwarding credentials before proxying, and never forwards origin
cookies to the client.

## Requirements

- A Cloudflare account with Workers, Zero Trust, Access, and Workers VPC enabled
- `cloudflared`, Node.js, and the IronMem release binary
- A named Cloudflare Tunnel and a Workers VPC HTTP service targeting
  `127.0.0.1:37779`
- A Cloudflare Access policy limited to the intended user or service identity

Workers VPC is currently a Cloudflare beta feature. Confirm its availability in
the target account before relying on this topology in production.

## Deploy the gateway

The `service_id` in `wrangler.jsonc` identifies the Workers VPC service in the
deployment account. Create a VPC service that targets the named tunnel at
`127.0.0.1:37779`, then set that service's ID in `wrangler.jsonc` before the
first deployment.

```bash
cd deploy/cloudflare-gateway
npm ci
npm run check
npm run deploy
```

`npm run check` runs the security and proxy tests, then asks Wrangler to resolve
the production bindings with a dry-run deployment.

## Protect the Worker

In Cloudflare Zero Trust:

1. Create a self-hosted Access application for the deployed Worker.
2. Cover all traffic to its `workers.dev` hostname, including `/mcp`.
3. Add an allow policy for the exact intended identity. Do not add a public
   bypass or `Everyone` policy.
4. Enable Managed OAuth on the Access application.
5. Keep the access token short-lived and the login grant session bounded.
6. Confirm an unauthenticated request receives the Access OAuth challenge before
   connecting any client.

The Worker contains a second fail-closed check, but that check is defense in
depth. The Access application remains the public authentication boundary.

## Run the local services after login and reboot

Run the MCP origin without its own bearer token only because it is restricted
to loopback and the public edge authenticates every request:

```bash
ironmem serve --no-auth
```

Run the named tunnel using a token file with owner-only permissions:

```bash
chmod 600 "$HOME/.cloudflared/ironmem-local.token"
cloudflared tunnel --token-file "$HOME/.cloudflared/ironmem-local.token" run ironmem-local
```

Install both commands as operating-system services so a reboot cannot silently
disconnect the remote connector. On macOS, use separate LaunchAgents and keep
the token out of the property-list arguments and logs.

Verify that IronMem is listening on loopback only:

```bash
lsof -nP -iTCP:37779 -sTCP:LISTEN
```

The listener must show `127.0.0.1:37779`, never `*:37779` or
`0.0.0.0:37779`.

## Connect and verify

Add the Worker's stable URL with `/mcp` to the remote client's custom connector
and complete its OAuth flow. Then verify the full path with these MCP calls:

1. `get_status`
2. `remember` with a unique harmless marker
3. `search_memories` for that exact marker

Also verify that a request without an authenticated Access session cannot reach
the Worker origin. A successful MCP result from an unauthenticated request is a
deployment failure.

Local clients do not need this gateway. Claude Code, Claude Desktop, and Codex
should use the `ironmem mcp` stdio transport directly.

## Development

`wrangler.jsonc` includes a development-only simulated Access identity so
`wrangler dev --remote` can exercise the VPC route. Production requests do not
receive this identity; Cloudflare Access must populate it at the edge.

```bash
npm test
npx wrangler dev --remote
```

Never add bearer tokens, tunnel tokens, Access service tokens, or Wrangler OAuth
credentials to this directory or to Git.
