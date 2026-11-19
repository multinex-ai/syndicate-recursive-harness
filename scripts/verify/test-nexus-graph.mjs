#!/usr/bin/env node
import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  buildWorkspaceGraph,
  createGraph,
  findPaths,
  impactOf,
  indexCargoToml,
  indexSourceText,
  indexSyndicateJson,
} from "./nexus-graph.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

test("nexus-graph indexes routes, functions, dependencies, and syndicate multi-hop paths", () => {
  const g = createGraph();
  indexSourceText(g, "src/forge.rs", 'pub fn serve() {} ("GET", "/api/machine") => machine()');
  indexCargoToml(g, "Cargo.toml", "[dependencies]\nserde_json = \"1.0\"\nsha2 = \"0.10\"\n");
  indexSyndicateJson(g, "syndicates/isa-centra-spike/syndicate.json", {
    syndicate_id: "isa-centra-spike-v1",
    agent_ids: ["centra-actual", "orange-watch", "greyfox-recon", "timberwolf-red"],
    edges: [
      { from: "centra-actual", to: "orange-watch", relation: "commands" },
      { from: "orange-watch", to: "greyfox-recon", relation: "feeds" },
      { from: "greyfox-recon", to: "timberwolf-red", relation: "hands_off" },
    ],
  });

  assert.ok(g.nodes.has("route:GET /api/machine"));
  assert.ok(g.nodes.has("function:src/forge.rs::serve"));
  assert.ok(g.nodes.has("dependency:cargo:serde_json"));

  const paths = findPaths(g, "agent:centra-actual", "agent:timberwolf-red", 5);
  assert.equal(paths.length, 1);
  assert.deepEqual(paths[0], [
    "agent:centra-actual",
    "agent:orange-watch",
    "agent:greyfox-recon",
    "agent:timberwolf-red",
  ]);

  const upstream = impactOf(g, "agent:timberwolf-red");
  assert.deepEqual(upstream, ["agent:centra-actual", "agent:greyfox-recon", "agent:orange-watch"]);
});

test("buildWorkspaceGraph indexes the live syndicate-recursive-harness repository", async () => {
  const summary = await buildWorkspaceGraph(root);
  assert.ok(summary.counts.routes >= 10, "should discover Forge loopback API routes");
  assert.ok(summary.counts.functions >= 50, "should discover Rust/JS/Python functions");
  assert.ok(summary.counts.dependencies >= 3, "should discover Cargo dependencies");
  assert.ok(summary.counts.agents >= 7, "should discover Creative Relay and ISA Task Force Orange agents");
});
