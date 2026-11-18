import assert from "node:assert/strict";
import test from "node:test";
import { compactInput, contextPolicy, estimateTokens } from "./context-manager.mjs";

test("defaults reserve output before the context edge", () => {
  const policy = contextPolicy({});
  assert.ok(policy.compactAtTokens < policy.hardStopTokens);
  assert.ok(policy.hardStopTokens + policy.reservedOutputTokens <= policy.contextWindowTokens);
});

test("small conversations pass through", () => {
  const input = [{ role: "user", content: "hello" }];
  const result = compactInput(input);
  assert.equal(result.compacted, false);
  assert.equal(result.items[0].content, "hello");
});

test("long history becomes one checkpoint plus a sliding tail", () => {
  const input = Array.from({ length: 30 }, (_, index) => ({
    type: "message",
    role: index % 2 ? "assistant" : "user",
    content: `turn-${index} ${"x".repeat(1800)}`,
  }));
  const result = compactInput(input, { compactAtTokens: 4_000, hardStopTokens: 8_000, recentItems: 6 });
  assert.equal(result.compacted, true);
  assert.match(result.items[0].content, /LOCAL CONTEXT CHECKPOINT/);
  assert.match(result.items.at(-1).content, /turn-29/);
  assert.ok(result.afterTokens < result.beforeTokens);
});

test("tool call is retained with a result in the tail", () => {
  const input = [
    { role: "user", content: "x".repeat(20_000) },
    { type: "function_call", id: "call-7", name: "read_file", arguments: "{}" },
    { type: "function_call_output", call_id: "call-7", output: "done" },
  ];
  const result = compactInput(input, { compactAtTokens: 1_000, hardStopTokens: 9_000, recentItems: 1 });
  assert.equal(result.items.at(-2).id, "call-7");
  assert.equal(result.items.at(-1).call_id, "call-7");
});

test("assistant tool_calls remain paired when the message id differs", () => {
  const input = [
    { role: "user", content: "x".repeat(20_000) },
    { type: "message", id: "message-1", role: "assistant", content: "", tool_calls: [
      { id: "call-1", type: "function", function: { name: "read_file", arguments: "{}" } },
    ] },
    { type: "function_call_output", call_id: "call-1", output: "done" },
  ];
  const result = compactInput(input, { compactAtTokens: 1_000, hardStopTokens: 9_000, recentItems: 1 });
  assert.equal(result.items.at(-2).id, "message-1");
  assert.equal(result.items.at(-1).call_id, "call-1");
});

test("interleaved tool results compute the complete transitive tail", () => {
  const input = [
    { role: "user", content: "x".repeat(20_000) },
    { type: "function_call", id: "call-a", name: "first", arguments: "{}" },
    { type: "function_call", id: "call-b", name: "second", arguments: "{}" },
    { type: "function_call_output", call_id: "call-a", output: "a" },
    { type: "function_call_output", call_id: "call-b", output: "b" },
  ];
  const result = compactInput(input, { compactAtTokens: 1_000, hardStopTokens: 9_000, recentItems: 1 });
  assert.deepEqual(result.items.slice(-4).map((item) => item.id ?? item.call_id), ["call-a", "call-b", "call-a", "call-b"]);
});

test("fixed instructions and tool schemas count against the hard stop", () => {
  assert.throws(
    () => compactInput([{ role: "user", content: "small" }], { compactAtTokens: 100, hardStopTokens: 1_000 }, 1_200),
    /exceeds the local context budget/,
  );
});

test("token estimate is conservative and deterministic", () => {
  assert.equal(estimateTokens("a".repeat(34)), 10);
});

