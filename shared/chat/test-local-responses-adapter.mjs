import assert from "node:assert/strict";
import { adaptRequest, restoreNamespaces, pruneSchema, sliceContent } from "./local-responses-adapter.mjs";

// Test 1: Baseline namespace mapping
const adapted = adaptRequest({
  instructions: "base",
  input: [
    { role: "user", content: "hello" },
    { role: "developer", content: "policy" },
  ],
  tools: [{
    type: "namespace",
    name: "mcp__memq",
    description: "memory",
    tools: [{ type: "function", name: "memq_status", description: "status", parameters: {} }],
  }],
});

assert.equal(adapted.body.tools[0].name, "mcp__memq__memq_status");
assert.equal(adapted.body.instructions, "base\n\npolicy");
assert.deepEqual(adapted.body.input, [{ role: "user", content: "hello", type: "message" }]);
assert.deepEqual(
  restoreNamespaces(
    { type: "function_call", name: "mcp__memq__memq_status", arguments: "{}" },
    adapted.nameToNamespace,
  ),
  {
    type: "function_call",
    name: "memq_status",
    namespace: "mcp__memq",
    arguments: "{}",
  },
);

// Test 2: pruneSchema removes boilerplate and truncates long descriptions
const verboseSchema = {
  type: "object",
  title: "VerboseParams",
  additionalProperties: false,
  properties: {
    query: {
      type: "string",
      description: "A very long detailed parameter description that goes on and on and exceeds ninety characters in length easily without adding extra value.",
    },
    count: {
      type: "integer",
      description: "Short count",
    },
  },
};
const pruned = pruneSchema(verboseSchema);
assert.equal(pruned.title, undefined);
assert.equal(pruned.additionalProperties, undefined);
assert.ok(pruned.properties.query.description.length <= 90);
assert.ok(pruned.properties.query.description.endsWith("..."));
assert.equal(pruned.properties.count.description, "Short count");

// Test 3: sliceContent truncates large payloads
const largeText = "A".repeat(5000);
const slicedText = sliceContent(largeText, 2000);
assert.ok(slicedText.length < 2500);
assert.ok(slicedText.includes("[... sliced"));

// Test 4: Tool slicing when tool count is high (> 16 tools)
const manyTools = [];
for (let i = 0; i < 20; i++) {
  manyTools.push({
    type: "function",
    name: `specialized_tool_${i}`,
    description: `Specialized tool ${i}`,
    parameters: {},
  });
}
// Add core tools
manyTools.push({
  type: "function",
  name: "query_memory",
  description: "Query memq",
  parameters: {},
});
manyTools.push({
  type: "function",
  name: "temporal_graph_query",
  description: "Graph tool",
  parameters: {},
});

// Case 4a: Without graph intent, temporal_graph_query is omitted, query_memory is retained
const adaptedNoIntent = adaptRequest({
  input: [{ role: "user", content: "hello" }],
  tools: manyTools,
});
const toolNamesNoIntent = adaptedNoIntent.body.tools.map((t) => t.name);
assert.ok(toolNamesNoIntent.includes("query_memory"), "core tool query_memory must be included");
assert.ok(!toolNamesNoIntent.includes("temporal_graph_query"), "temporal_graph_query omitted without matching intent");
assert.ok(!toolNamesNoIntent.includes("specialized_tool_0"), "unmatched specialized tools omitted");

// Case 4b: With graph intent, temporal_graph_query is dynamically included
const adaptedWithIntent = adaptRequest({
  input: [{ role: "user", content: "can you query the temporal graph?" }],
  tools: manyTools,
});
const toolNamesWithIntent = adaptedWithIntent.body.tools.map((t) => t.name);
assert.ok(toolNamesWithIntent.includes("query_memory"));
assert.ok(toolNamesWithIntent.includes("temporal_graph_query"), "temporal_graph_query dynamically included on intent");

// Test 5: Web search tool is safely filtered out for llama.cpp compatibility
const adaptedWebSearch = adaptRequest({
  input: [{ role: "user", content: "test" }],
  tools: [{ type: "web_search", name: "search" }, { type: "function", name: "exec" }],
});
assert.equal(adaptedWebSearch.body.tools.length, 1);
assert.equal(adaptedWebSearch.body.tools[0].name, "exec");

// Test 6: the latest user task is preserved byte-for-byte when it fits.
const exactLatestTask = `START-${"A".repeat(14_000)}-MIDDLE-REQUIREMENT-${"B".repeat(14_000)}-END`;
const adaptedString = adaptRequest({ input: exactLatestTask });
assert.ok(Array.isArray(adaptedString.body.input));
assert.equal(adaptedString.body.input[0].content, exactLatestTask);
assert.equal(adaptedString.body.metadata.local_context_truncated_items, 0);
assert.equal(adaptedString.body.metadata.local_context_latest_user_preserved, true);
assert.equal(adaptedString.body.metadata.local_context_tokens_before, adaptedString.body.metadata.local_context_tokens_after);
assert.ok(adaptedString.body.metadata.local_context_raw_input_tokens > 8_000);

// Test 6b: an oversized latest task is rejected instead of losing a middle requirement.
const oversizedTask = `START-${"A".repeat(25_000)}-MIDDLE-MUST-SURVIVE-${"B".repeat(25_000)}-END`;
assert.throws(
  () => adaptRequest({ input: oversizedTask }),
  /Latest user message exceeds the local context budget/,
);

// Test 7: instructions and tool schemas are included in the fixed context overhead.
assert.throws(
  () => adaptRequest({
    instructions: "I".repeat(100_000),
    input: [{ role: "user", content: "hello" }],
    tools: [{ type: "function", name: "exec", description: "run", parameters: {} }],
  }),
  /exceeds the local context budget/,
);

process.stdout.write("local Responses adapter transforms: all tests passed cleanly\n");
