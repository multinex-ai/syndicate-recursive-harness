#!/usr/bin/env node
/**
 * Nexus Supply-Chain & Attack-Surface Topology Graph Indexer (`nexus-graph.mjs`).
 *
 * Builds a deterministic knowledge graph of a workspace:
 *   Nodes: `module`, `function`, `route`, `dependency`, `syndicate_agent`
 *   Edges: `DEFINES`, `IMPORTS`, `EXPOSES`, `DEPENDS_ON`, `ROUTES_TO`
 *
 * Supports multi-hop reachability queries (`findPaths(graph, fromPredicate, toPredicate)`)
 * and reverse blast-radius impact queries (`impactOf(graph, nodeId)`).
 */
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { contentDigest } from "./evaluate-runs.mjs";

export function createGraph() {
  return { nodes: new Map(), edges: [] };
}

export function addNode(graph, id, kind, attrs = {}) {
  graph.nodes.set(id, { id, kind, ...attrs });
}

export function addEdge(graph, from, to, relation, attrs = {}) {
  graph.edges.push({ from, to, relation, ...attrs });
}

export function indexSourceText(graph, relPath, text) {
  const modId = `module:${relPath}`;
  addNode(graph, modId, "module", { file: relPath });

  // Extract Rust / JS / Python function definitions
  const fnRegex = /\b(?:pub\s+)?(?:async\s+)?(?:fn|function|def)\s+([A-Za-z_][A-Za-z0-9_]*)\b/g;
  for (const match of text.matchAll(fnRegex)) {
    const fnName = match[1];
    const fnId = `function:${relPath}::${fnName}`;
    addNode(graph, fnId, "function", { name: fnName, file: relPath });
    addEdge(graph, modId, fnId, "DEFINES");
  }

  // Extract HTTP route declarations (e.g., ("GET", "/api/machine") or "/v1/responses")
  const routeRegex = /\(\s*"(GET|POST|PUT|DELETE)"\s*,\s*"(\/[A-Za-z0-9_/-]*)"\s*\)/g;
  for (const match of text.matchAll(routeRegex)) {
    const routeId = `route:${match[1]} ${match[2]}`;
    addNode(graph, routeId, "route", { method: match[1], path: match[2], file: relPath });
    addEdge(graph, modId, routeId, "EXPOSES");
  }

  // Extract Rust `use crate::foo` / `mod foo` and JS `import ... from './foo'`
  for (const match of text.matchAll(/\b(?:use\s+crate::|mod\s+)([a-z_][a-z0-9_]*)\b/g)) {
    addEdge(graph, modId, `symbol:${match[1]}`, "IMPORTS");
  }
  for (const match of text.matchAll(/\bfrom\s+["']([^"']+)["']/g)) {
    addEdge(graph, modId, `import:${match[1]}`, "IMPORTS");
  }
}

export function indexCargoToml(graph, relPath, text) {
  const modId = `manifest:${relPath}`;
  addNode(graph, modId, "manifest", { file: relPath });
  let inDeps = false;
  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line.startsWith("[")) {
      inDeps = /^\[(?:workspace\.)?dependencies\]/.test(line);
      continue;
    }
    if (inDeps) {
      const m = /^([A-Za-z0-9_-]+)\s*=/.exec(line);
      if (m) {
        const depId = `dependency:cargo:${m[1]}`;
        addNode(graph, depId, "dependency", { ecosystem: "cargo", name: m[1] });
        addEdge(graph, modId, depId, "DEPENDS_ON");
      }
    }
  }
}

export function indexSyndicateJson(graph, relPath, data) {
  for (const id of data.agent_ids ?? []) {
    addNode(graph, `agent:${id}`, "syndicate_agent", {
      syndicate_id: data.syndicate_id,
      file: relPath,
    });
  }
  for (const edge of data.edges ?? []) {
    addEdge(graph, `agent:${edge.from}`, `agent:${edge.to}`, "ROUTES_TO", {
      relation: edge.relation,
      max_rounds: edge.constraints?.max_rounds ?? 1,
    });
  }
}

export function findPaths(graph, startId, targetId, maxDepth = 5) {
  const adj = new Map();
  for (const e of graph.edges) {
    if (!adj.has(e.from)) adj.set(e.from, []);
    adj.get(e.from).push(e);
  }
  const paths = [];
  const queue = [[startId]];
  while (queue.length > 0) {
    const currentPath = queue.shift();
    const last = currentPath[currentPath.length - 1];
    if (last === targetId && currentPath.length > 1) {
      paths.push(currentPath);
      continue;
    }
    if (currentPath.length > maxDepth) continue;
    for (const edge of adj.get(last) ?? []) {
      if (!currentPath.includes(edge.to)) {
        queue.push([...currentPath, edge.to]);
      }
    }
  }
  return paths;
}

export function impactOf(graph, targetId) {
  const callers = new Set();
  const queue = [targetId];
  while (queue.length > 0) {
    const cur = queue.shift();
    for (const e of graph.edges) {
      if (e.to === cur && !callers.has(e.from)) {
        callers.add(e.from);
        queue.push(e.from);
      }
    }
  }
  return [...callers].sort();
}

export async function buildWorkspaceGraph(rootDir) {
  const resolved = path.resolve(rootDir);
  const graph = createGraph();
  const ignore = new Set([".git", "target", "node_modules", "runtime", "models", "runs"]);

  async function walk(rel = "") {
    const abs = path.join(resolved, rel);
    for (const entry of await readdir(abs, { withFileTypes: true })) {
      if (ignore.has(entry.name)) continue;
      const childRel = path.join(rel, entry.name);
      if (entry.isDirectory()) {
        await walk(childRel);
      } else if ([".rs", ".mjs", ".py"].includes(path.extname(entry.name))) {
        indexSourceText(graph, childRel, await readFile(path.join(resolved, childRel), "utf8"));
      } else if (entry.name === "Cargo.toml") {
        indexCargoToml(graph, childRel, await readFile(path.join(resolved, childRel), "utf8"));
      } else if (entry.name === "syndicate.json") {
        const data = JSON.parse(await readFile(path.join(resolved, childRel), "utf8"));
        indexSyndicateJson(graph, childRel, data);
      }
    }
  }

  await walk();
  const nodes = [...graph.nodes.values()].sort((a, b) => a.id.localeCompare(b.id));
  const edges = [...graph.edges].sort((a, b) => `${a.from}->${a.to}`.localeCompare(`${b.from}->${b.to}`));
  return {
    schema_version: 1,
    digest: contentDigest({ nodes: nodes.length, edges: edges.length }),
    counts: {
      nodes: nodes.length,
      edges: edges.length,
      routes: nodes.filter((n) => n.kind === "route").length,
      functions: nodes.filter((n) => n.kind === "function").length,
      dependencies: nodes.filter((n) => n.kind === "dependency").length,
      agents: nodes.filter((n) => n.kind === "syndicate_agent").length,
    },
    graph,
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const summary = await buildWorkspaceGraph(process.argv[2] ?? ".");
  console.log(JSON.stringify({ schema_version: summary.schema_version, digest: summary.digest, counts: summary.counts }, null, 2));
}
