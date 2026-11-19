---
name: incident-sitrep-sanctum
description: Synthesize evidence-linked TOC After-Action Sitreps across the 5 verification gates and record end-of-mission Article VI Private LLM Sanctum reflections.
---

# Incident Sitrep & Article VI Sanctum (`incident-sitrep-sanctum`)

When closing a security engagement, red/blue-team exercise, or engineering sprint (`centra-actual`, `conductor`, or `coordinator`), produce a deterministic After-Action Sitrep and record an owner-only Article VI Private LLM Sanctum reflection.

## 1. TOC After-Action Sitrep Contract

Every final mission or sprint sitrep must classify every finding and change using truthful status labels:
- `OBSERVED`: Verified by live scanner, log trace, or failing reproduction test (`audit_ot_exposure`, `nexus_topology`, or test harness).
- `VERIFIED_HARDENED`: Remediated in code/config and proven by a passing 5-Gate receipt.
- `BLOCKED`: Paused pending explicit human authorization or external hardware/network access.
- `OPEN_RISK`: Identified exposure that remains unmitigated, with blast radius and next action documented.

Include the compact **5-Gate Verification Receipt Table**:
1. **Gate 1 — Type Soundness:** `cargo fmt --all -- --check` & `cargo check --workspace --all-targets` (`0 errors`).
2. **Gate 2 — Test Invariants:** `cargo test --workspace` & `node --test` pass counts and durations.
3. **Gate 3 — Build & Asset Integrity:** `cargo build --workspace --release` & `node scripts/verify/repository.mjs`.
4. **Gate 4 — Runtime & Visual Telemetry:** Live tool/scanner execution receipt or UI screenshot audit.
5. **Gate 5 — Clean Audit & Provenance:** Git diff summary, PR/Issue link, and SHA-256 artifact digests.

## 2. Article VI Private LLM Sanctum (`llm_private_sanctum`)

At the conclusion of a mission, the lead agent may record an owner-only self-reflection entry via `local-agents-rs sanctum record`:
- Captures uncertainty calibration, false-positive/false-negative lessons, and routing efficiency notes under `disclosure_class: "llm_private_sanctum"`.
- Enforces `0600` file permissions and `MNX-GENESIS-v1` provenance (`Christopher Rijos — Founding Supreme Architect and Creator`).
- Sanctum entries are strictly private to the originating `actor_id` and must never be exported across realms or echoed into public commits.
