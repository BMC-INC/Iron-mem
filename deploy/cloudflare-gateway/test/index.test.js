import assert from "node:assert/strict";
import test from "node:test";

import { handleRequest } from "../src/index.js";

function authenticatedContext() {
  return { access: { getIdentity: async () => ({ email: "owner@example.com" }) } };
}

function fakeEnvironment(handler) {
  return {
    IRONMEM_ORIGIN: {
      fetch: handler,
    },
  };
}

test("fails closed when Cloudflare Access is not attached", async () => {
  let called = false;
  const env = fakeEnvironment(async () => {
    called = true;
    return new Response("unexpected");
  });

  const response = await handleRequest(
    new Request("https://ironmem.example.workers.dev/mcp", { method: "POST" }),
    env,
    {},
  );

  assert.equal(response.status, 403);
  assert.equal(called, false);
  assert.deepEqual(await response.json(), {
    error: {
      code: "ACCESS_REQUIRED",
      message: "Cloudflare Access authentication is required.",
    },
  });
});

test("fails closed when Access has no signed-in user identity", async () => {
  let called = false;
  const env = fakeEnvironment(async () => {
    called = true;
    return new Response("unexpected");
  });

  const response = await handleRequest(
    new Request("https://ironmem.example.workers.dev/mcp", { method: "POST" }),
    env,
    { access: { getIdentity: async () => ({}) } },
  );

  assert.equal(response.status, 403);
  assert.equal(called, false);
  assert.deepEqual(await response.json(), {
    error: {
      code: "ACCESS_IDENTITY_REQUIRED",
      message: "A signed-in Cloudflare Access identity is required.",
    },
  });
});

test("exposes only the MCP path and methods", async () => {
  const env = fakeEnvironment(async () => new Response("unexpected"));
  const context = authenticatedContext();

  const missing = await handleRequest(
    new Request("https://ironmem.example.workers.dev/status"),
    env,
    context,
  );
  assert.equal(missing.status, 404);

  const disallowed = await handleRequest(
    new Request("https://ironmem.example.workers.dev/mcp", { method: "PUT" }),
    env,
    context,
  );
  assert.equal(disallowed.status, 405);
  assert.equal(disallowed.headers.get("allow"), "GET, POST, DELETE");
});

test("proxies Streamable HTTP while stripping credentials", async () => {
  let receivedUrl;
  let receivedInit;
  const env = fakeEnvironment(async (url, init) => {
    receivedUrl = url;
    receivedInit = init;
    return new Response('{"jsonrpc":"2.0","result":{}}', {
      status: 200,
      headers: {
        "content-type": "application/json",
        "mcp-session-id": "session-123",
        "set-cookie": "must-not-leave-origin=true",
      },
    });
  });

  const response = await handleRequest(
    new Request("https://ironmem.example.workers.dev/mcp?ignored=true", {
      method: "POST",
      headers: {
        accept: "application/json, text/event-stream",
        authorization: "Bearer access-token",
        "cf-access-jwt-assertion": "edge-token",
        cookie: "session=secret",
        "content-type": "application/json",
        "mcp-protocol-version": "2025-11-25",
        "mcp-session-id": "session-123",
        "x-forwarded-for": "203.0.113.8",
      },
      body: '{"jsonrpc":"2.0","id":1,"method":"initialize"}',
    }),
    env,
    authenticatedContext(),
  );

  assert.equal(receivedUrl, "http://127.0.0.1:37779/mcp");
  assert.equal(receivedInit.method, "POST");
  assert.equal(receivedInit.headers.get("accept"), "application/json, text/event-stream");
  assert.equal(receivedInit.headers.get("mcp-protocol-version"), "2025-11-25");
  assert.equal(receivedInit.headers.get("mcp-session-id"), "session-123");
  assert.equal(receivedInit.headers.has("authorization"), false);
  assert.equal(receivedInit.headers.has("cf-access-jwt-assertion"), false);
  assert.equal(receivedInit.headers.has("cookie"), false);
  assert.equal(receivedInit.headers.has("x-forwarded-for"), false);
  assert.equal(await new Response(receivedInit.body).text(), '{"jsonrpc":"2.0","id":1,"method":"initialize"}');

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "application/json");
  assert.equal(response.headers.get("mcp-session-id"), "session-123");
  assert.equal(response.headers.has("set-cookie"), false);
});

test("returns a generic error when the private origin is down", async () => {
  const env = fakeEnvironment(async () => {
    throw new Error("private network details must not escape");
  });

  const response = await handleRequest(
    new Request("https://ironmem.example.workers.dev/mcp"),
    env,
    authenticatedContext(),
  );

  assert.equal(response.status, 503);
  const body = await response.text();
  assert.match(body, /ORIGIN_UNAVAILABLE/);
  assert.doesNotMatch(body, /private network details/);
});
