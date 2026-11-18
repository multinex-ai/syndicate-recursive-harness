#!/usr/bin/env node

import http from "node:http";
import { managedInference } from './managed-inference.mjs';
import { compactInput, contextPolicy, estimateTokens } from './context-manager.mjs';

const listenHost = process.env.MNX_ADAPTER_HOST ?? "127.0.0.1";
const listenPort = Number(process.env.MNX_ADAPTER_PORT ?? "18087");
const upstream = new URL(process.env.MNX_LLAMA_BASE_URL ?? "http://127.0.0.1:18088");
const controllers = new Set();

export const CORE_TOOLS = new Set([
  // Core coding tools
  "exec", "apply_patch", "read_file", "write_file", "view_image",
  // Core MemQ tools
  "query_memory", "search_memory", "add_memory", "recent_memory", "save_context",
  "memory_status", "memq_status", "slicer_slice", "slice_project", "reunion",
  // Core local agent worker tools
  "local_agents_status", "start_local_agent", "local_agent_job", "cancel_local_agent", "local_agent_log"
]);

export const INTENT_TOOL_MAP = [
  { keywords: ["graph", "temporal", "timeline", "relation"], tools: ["temporal_graph_query", "bridge_sync"] },
  { keywords: ["journal", "log", "diary"], tools: ["journal_record", "journal_search", "journal_distill"] },
  { keywords: ["brain", "sleep", "consolidat", "predict", "associat"], tools: ["brain_discover", "brain_associate", "brain_recall_episode", "brain_consolidate_sleep", "brain_reinforce", "brain_predict"] },
  { keywords: ["plan", "step", "checkpoint", "state"], tools: ["plan_state_read", "plan_state_write", "plan_state_checkpoint", "plan_state_resume"] },
  { keywords: ["commons", "resonance", "promotion"], tools: ["commons_search", "commons_resonance", "commons_promotion_status"] },
  { keywords: ["reflect"], tools: ["reflect_memory", "reflection_handoff"] },
  { keywords: ["hybrid"], tools: ["hybrid_retrieve"] },
  { keywords: ["namespace"], tools: ["namespace_info"] },
  { keywords: ["mnemosyne"], tools: ["mnemosyne_context"] },
];

export function pruneSchema(schema, depth = 0) {
  if (!schema || typeof schema !== "object" || depth > 4) return schema;
  if (Array.isArray(schema)) return schema.map((item) => pruneSchema(item, depth + 1));
  const pruned = {};
  for (const [k, v] of Object.entries(schema)) {
    if (k === "title" || (k === "additionalProperties" && v === false)) continue;
    if (k === "description" && typeof v === "string") {
      pruned[k] = v.length > 90 ? v.slice(0, 87) + "..." : v;
    } else if (k === "properties" && typeof v === "object" && v !== null) {
      pruned[k] = {};
      for (const [pk, pv] of Object.entries(v)) {
        pruned[k][pk] = pruneSchema(pv, depth + 1);
      }
    } else if (k === "items" && typeof v === "object" && v !== null) {
      pruned[k] = pruneSchema(v, depth + 1);
    } else {
      pruned[k] = v;
    }
  }
  return pruned;
}

export function shouldIncludeTool(tool, intentText, totalToolsCount) {
  if (totalToolsCount <= 16) return true;
  const baseName = tool.name.includes("__") ? tool.name.split("__").pop() : tool.name;
  if (CORE_TOOLS.has(baseName) || CORE_TOOLS.has(tool.name)) return true;
  const text = (intentText ?? "").toLowerCase();
  for (const entry of INTENT_TOOL_MAP) {
    if (entry.tools.includes(baseName) || entry.tools.includes(tool.name)) {
      if (entry.keywords.some((kw) => text.includes(kw))) {
        return true;
      }
    }
  }
  return false;
}

export function sliceContent(content, maxChars = 2400) {
  if (typeof content !== "string") return content;
  if (content.length <= maxChars) return content;
  const head = Math.floor(maxChars * 0.6);
  const tail = Math.floor(maxChars * 0.35);
  const omitted = content.length - head - tail;
  return `${content.slice(0, head)}\n\n[... sliced ${omitted} chars for context efficiency ...]\n\n${content.slice(-tail)}`;
}

export function sliceInputItem(item) {
  if (!item || typeof item !== "object") return item;
  const cloned = { ...item };
  if (!cloned.type && (cloned.role === "user" || cloned.role === "assistant" || cloned.role === "developer" || cloned.role === "system")) {
    cloned.type = "message";
  }
  if (typeof cloned.content === "string") {
    cloned.content = sliceContent(cloned.content);
  } else if (Array.isArray(cloned.content)) {
    cloned.content = cloned.content.map((part) => {
      if (part && typeof part.text === "string") {
        return { ...part, text: sliceContent(part.text) };
      }
      return part;
    });
  }
  return cloned;
}

export function normalizeInputItem(item) {
  if (!item || typeof item !== "object") return item;
  const cloned = structuredClone(item);
  if (!cloned.type && ["user", "assistant", "developer", "system"].includes(cloned.role)) {
    cloned.type = "message";
  }
  return cloned;
}

export function adaptRequest(body) {
  const nameToNamespace = new Map();
  const allCandidateTools = [];
  for (const tool of body.tools ?? []) {
    if (tool?.type === "web_search") {
      continue;
    }
    if (tool?.type !== "namespace") {
      if (tool?.type === "function") {
        allCandidateTools.push(tool);
      }
      continue;
    }
    for (const nested of tool.tools ?? []) {
      if (nested?.type !== "function") continue;
      const flatName = `${tool.name}__${nested.name}`;
      nameToNamespace.set(flatName, { namespace: tool.name, name: nested.name });
      allCandidateTools.push({ ...nested, name: flatName });
    }
  }

  const input = body.input;
  const leading = [];
  const remaining = [];
  let userIntentText = "";
  if (Array.isArray(input)) {
    for (const item of input) {
      if (item?.role === "system" || item?.role === "developer") {
        const content = typeof item.content === "string"
          ? item.content
          : (item.content ?? []).map((part) => part?.text ?? "").filter(Boolean).join("\n");
        if (content) leading.push(content);
      } else {
        const normalized = normalizeInputItem(item);
        if (normalized.role === "system" || normalized.role === "developer") {
          normalized.role = "user";
        }
        remaining.push(normalized);
        if (item?.role === "user") {
          const uText = typeof item.content === "string"
            ? item.content
            : (item.content ?? []).map((p) => p?.text ?? "").join(" ");
          userIntentText += " " + uText;
        }
      }
    }
  } else if (typeof input === "string") {
    remaining.push({ type: "message", role: "user", content: input });
    userIntentText = input;
  }

  const tools = [];
  for (const tool of allCandidateTools) {
    if (shouldIncludeTool(tool, userIntentText, allCandidateTools.length)) {
      const pruned = {
        ...tool,
        ...(tool.description ? { description: tool.description.length > 90 ? tool.description.slice(0, 87) + "..." : tool.description } : {}),
        ...(tool.parameters ? { parameters: pruneSchema(tool.parameters) } : {}),
      };
      tools.push(pruned);
    }
  }

  const instructions = [body.instructions, ...leading].filter(Boolean).join("\n\n");
  const overheadTokens = estimateTokens({ instructions, tools });
  const context = compactInput(remaining, contextPolicy(), overheadTokens);
  const slicedRemaining = context.items;
  return {
    body: {
      ...body,
      ...(body.tools ? { tools } : {}),
      ...(instructions ? { instructions } : {}),
      ...(input !== undefined ? { input: slicedRemaining } : {}),
      metadata: {
        ...(body.metadata ?? {}),
        local_context_raw_input_tokens: estimateTokens(input ?? []),
        local_context_compacted: context.compacted,
        local_context_tokens_before: context.beforeTokens,
        local_context_tokens_after: context.afterTokens,
        local_context_tokens_after_item_limits: context.retainedBeforeCompactionTokens,
        local_context_truncated_items: context.truncatedItems,
        local_context_latest_user_preserved: context.latestUserPreserved,
        local_context_fixed_overhead_tokens: context.overheadTokens,
      },
    },
    nameToNamespace,
  };
}

export function restoreNamespaces(value, nameToNamespace) {
  if (Array.isArray(value)) return value.map((item) => restoreNamespaces(item, nameToNamespace));
  if (!value || typeof value !== "object") return value;
  const restored = {};
  for (const [key, child] of Object.entries(value)) {
    restored[key] = restoreNamespaces(child, nameToNamespace);
  }
  if (restored.type === "function_call" && typeof restored.name === "string") {
    const original = nameToNamespace.get(restored.name);
    if (original) {
      restored.name = original.name;
      if (!restored.namespace) restored.namespace = original.namespace;
    }
  }
  return restored;
}

function transformSseChunk(text, nameToNamespace) {
  return text.split("\n").map((line) => {
    if (!line.startsWith("data: ") || line === "data: [DONE]") return line;
    try {
      return `data: ${JSON.stringify(restoreNamespaces(JSON.parse(line.slice(6)), nameToNamespace))}`;
    } catch {
      return line;
    }
  }).join("\n");
}

async function handle(req, res) {
  if (process.env.MNX_MANAGED_INFERENCE === '1' && req.method === 'GET' && req.url === '/health') {
    res.writeHead(200, {'content-type':'application/json'});
    res.end(JSON.stringify({status:'ok',mode:'managed-on-demand',active_requests:controllers.size}));return;
  }
  if (process.env.MNX_MANAGED_INFERENCE === '1' && (req.method !== 'POST' || req.url !== '/v1/responses')) {
    res.writeHead(404);res.end('Unsupported local endpoint');return;
  }
  const chunks = [];
  let bytes = 0;
  for await (const chunk of req) {
    bytes += chunk.length;
    if (bytes > 4 * 1024 * 1024) { res.writeHead(413);res.end('Request too large');return; }
    chunks.push(chunk);
  }
  let payload;
  try {
    payload = chunks.length ? JSON.parse(Buffer.concat(chunks).toString("utf8")) : undefined;
  } catch {
    res.writeHead(400, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: { message: "Invalid JSON request" } }));
    return;
  }

  let adapted;
  try {
    adapted = payload ? adaptRequest(payload) : { body: undefined, nameToNamespace: new Map() };
  } catch (error) {
    res.writeHead(413, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: { type: "context_budget_error", message: error instanceof Error ? error.message : String(error) } }));
    return;
  }
  const controller = new AbortController();
  controllers.add(controller);
  res.on('close',()=>{if(!res.writableEnded)controller.abort();});
  let status, contentType, text;
  if (process.env.MNX_MANAGED_INFERENCE === '1') {
    try { ({status,contentType,text}=await managedInference(adapted.body,controller.signal)); }
    finally { controllers.delete(controller); }
  } else {
   const target = new URL(req.url ?? "/", upstream);
   const upstreamResponse = await fetch(target, {
    method: req.method,
    headers: { "content-type": req.headers["content-type"] ?? "application/json" },
    body: adapted.body === undefined ? undefined : JSON.stringify(adapted.body),
    signal: controller.signal,
  });
   status=upstreamResponse.status;
   contentType=upstreamResponse.headers.get("content-type") ?? "application/json";
   text=await upstreamResponse.text();
   controllers.delete(controller);
  }
  res.writeHead(status, { "content-type": contentType });
  if (contentType.includes("text/event-stream")) {
    res.end(transformSseChunk(text, adapted.nameToNamespace));
  } else {
    try {
      res.end(JSON.stringify(restoreNamespaces(JSON.parse(text), adapted.nameToNamespace)));
    } catch {
      res.end(text);
    }
  }
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  const server = http.createServer((req, res) => {
    handle(req, res).catch((error) => {
      res.writeHead(502, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: error.message } }));
    });
  }).listen(listenPort, listenHost, () => {
    process.stderr.write(`local Responses adapter: http://${listenHost}:${listenPort} -> ${upstream}\n`);
  });
  for (const event of ['SIGINT','SIGTERM']) process.on(event,()=>{
    for (const controller of controllers) controller.abort();
    server.close(()=>process.exit(0));
    setTimeout(()=>process.exit(1),25000).unref();
  });
}
