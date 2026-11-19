---
name: nexus-topology-recon
description: Map codebase routes, functions, dependencies, and syndicate routing graphs with the native nexus_topology tool for zero-day reachability triage, surgical context slicing, and blast-radius prediction.
---

# Nexus Supply-Chain & Attack-Surface Topology (`nexus-topology-recon`)

Use this skill inside `greyfox-recon`, `centra-actual`, `coordinator`, or `cyber-lab` whenever you need to map an attack surface, trace whether a vulnerable dependency is reachable from an exposed route, slice minimal context for a 9B–32B local model, or compute the blast radius of a planned code change.

## 1. Native Tool Execution

Call the workspace-confined native Rust tool directly (no shell required):

```json
{
  "name": "nexus_topology",
  "arguments": { "path": "." }
}
```

The tool returns a canonical SHA-256 `digest` and node/edge `counts` (`routes`, `functions`, `dependencies`, `agents`) indexed by `scripts/verify/nexus-graph.mjs`.

## 2. Five Core Cyber-Defense & Engineering Workflows

1. **Zero-Day & KEV Reachability Triage (`findPaths`):**
   - When `orange-watch` reports a CISA KEV or CVE in a dependency (`dependency:cargo:<crate>` or npm package), trace whether an exposed HTTP/CLI entry point (`route:<METHOD> <PATH>`) has a directed call/import path (`EXPOSES -> DEFINES -> IMPORTS -> DEPENDS_ON`) to that dependency.
   - Prioritize remediation on dependencies reachable from external routes; deprioritize unreferenced dev-only artifacts.
2. **Surgical Graph-RAG Context Slicing for Local Models (`9B`–`32B`):**
   - Never dump whole monoliths into the `16,384`-token local context window.
   - Index the workspace graph first, identify the target route or function node, and `read_file` only the 1–2 hop neighbor modules connected by `DEFINES` and `IMPORTS`.
3. **Pre-Edit Blast-Radius & Test Selection (`impactOf`):**
   - Before `timberwolf-red` or `code-builder` modifies a shared function, schema, or agent config, trace reverse edges (`impactOf(graph, nodeId)`) to identify every upstream caller, route, and syndicate detachment that must be re-verified.
4. **Shadow-Surface & Architecture Drift Detection:**
   - Compare the declared syndicate manifest (`syndicate.json` edges, `allowed_hosts_ceiling`, `allowed_tools_ceiling`) against the observed `routes`, `dependencies`, and `agents` in the Nexus graph to spot undeclared endpoints or unexpected dependencies before release.
5. **Privacy-Preserving Topology Receipts:**
   - Record the canonical `digest` and `counts` in mission receipts or MemQ checkpoints so peer nodes can verify structural parity without exposing raw proprietary source code.
