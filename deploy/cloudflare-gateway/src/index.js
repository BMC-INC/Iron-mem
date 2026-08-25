const MCP_PATH = "/mcp";
const ORIGIN_URL = "http://127.0.0.1:37779/mcp";
const ALLOWED_METHODS = new Set(["GET", "POST", "DELETE"]);

const REQUEST_HEADERS = [
  "accept",
  "content-type",
  "last-event-id",
  "mcp-protocol-version",
  "mcp-session-id",
  "user-agent",
];

const RESPONSE_HEADERS = [
  "cache-control",
  "content-type",
  "expires",
  "mcp-session-id",
  "pragma",
];

function selectHeaders(source, names) {
  const selected = new Headers();
  for (const name of names) {
    const value = source.get(name);
    if (value !== null) {
      selected.set(name, value);
    }
  }
  return selected;
}

function jsonError(status, code, message) {
  return Response.json(
    { error: { code, message } },
    {
      status,
      headers: {
        "cache-control": "no-store",
      },
    },
  );
}

export async function handleRequest(request, env, ctx) {
  if (!ctx.access) {
    return jsonError(
      403,
      "ACCESS_REQUIRED",
      "Cloudflare Access authentication is required.",
    );
  }

  let identity;
  try {
    identity = await ctx.access.getIdentity();
  } catch {
    identity = null;
  }
  if (!identity?.email) {
    return jsonError(
      403,
      "ACCESS_IDENTITY_REQUIRED",
      "A signed-in Cloudflare Access identity is required.",
    );
  }

  const url = new URL(request.url);
  if (url.pathname !== MCP_PATH) {
    return jsonError(404, "NOT_FOUND", "Only the /mcp endpoint is available.");
  }

  if (!ALLOWED_METHODS.has(request.method)) {
    return new Response(null, {
      status: 405,
      headers: {
        allow: [...ALLOWED_METHODS].join(", "),
        "cache-control": "no-store",
      },
    });
  }

  const headers = selectHeaders(request.headers, REQUEST_HEADERS);
  const hasBody = request.method !== "GET" && request.method !== "DELETE";

  let upstream;
  try {
    upstream = await env.IRONMEM_ORIGIN.fetch(ORIGIN_URL, {
      method: request.method,
      headers,
      body: hasBody ? request.body : undefined,
      redirect: "manual",
    });
  } catch {
    return jsonError(
      503,
      "ORIGIN_UNAVAILABLE",
      "The local IronMem MCP origin is unavailable.",
    );
  }

  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers: selectHeaders(upstream.headers, RESPONSE_HEADERS),
  });
}

export default {
  fetch: handleRequest,
};
