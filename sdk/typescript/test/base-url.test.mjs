import assert from "node:assert/strict";
import { performance } from "node:perf_hooks";
import test from "node:test";

import { IronMem } from "../dist/index.js";

function normalizedBaseUrl(value) {
  return new IronMem(value).baseUrl;
}

test("removes trailing slashes without changing the rest of the URL", () => {
  assert.equal(normalizedBaseUrl("https://example.com/api///"), "https://example.com/api");
  assert.equal(normalizedBaseUrl("https://example.com/a//b"), "https://example.com/a//b");
  assert.equal(normalizedBaseUrl("https://example.com"), "https://example.com");
  assert.equal(normalizedBaseUrl("////"), "");
});

test("normalizes slash-heavy uncontrolled input in linear time", () => {
  const input = `${"/".repeat(50_000)}x`;
  const startedAt = performance.now();

  assert.equal(normalizedBaseUrl(input), input);
  assert.ok(performance.now() - startedAt < 500, "normalization exceeded 500ms");
});
