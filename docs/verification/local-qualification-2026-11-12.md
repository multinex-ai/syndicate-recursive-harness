# Local qualification receipt — 2026-11-12

Status: local evidence only. This is not a hosted CI, clean external-machine, signed-release, or full any-to-any acceptance receipt.

## Type, tests, and build

- `cargo check --workspace --all-targets`: passed with zero errors and zero warnings.
- Rust tests: 105 passed, 0 failed (104 harness + 1 installer); harness suite 13.56 seconds.
- Node context/adapter tests: 9 passed, 0 failed; 117.85 milliseconds at the final checkpoint.
- `cargo build --workspace --release`: passed.
- Repository verifier: passed for required report sections, executable launchers, privacy scan, JSON, provenance, model-card plan, and the 700-line source ceiling.

## Installer

- No-model source install built the release workspace and created an isolated Python 3.12 environment.
- One initial PyPI connection failure stopped the first rehearsal.
- The installer gained a three-attempt bounded retry for the idempotent dependency sync.
- The resumed rehearsal reused partial work, installed 87 pinned packages, checksum-verified the pinned Codex CLI, and ended with doctor `READY`.

## Context and lock regressions

- A latest user task that fits remains byte-for-byte intact, including a middle requirement.
- A roughly 50,000-character latest task is rejected with a context-budget error instead of being silently sliced.
- Tool calls and results remain paired through transitive-tail compaction.
- Rust, Python, and the installer normalize whitespace around absolute heavy-lock overrides and reject relative overrides consistently.

## Qwen Metal smoke

- Both core model files matched the byte counts and SHA-256 values in `catalog/models/manifest.json`.
- The release binary started the local Qwen server with a 16,384-token context and a random loopback API key.
- Expected reply: `RUST-OK`; observed reply: `RUST-OK`.
- Load: 7.91 seconds. Total: 8.55 seconds.
- Server PID terminated and the heavy-model lock released.
- The llama.cpp log contained no unauthenticated-server warning after hardening.

## Qwen agent loop

- Command path: release Rust binary → `agent conductor` → authenticated loopback Qwen → deterministic result/reward receipt → teardown.
- Dispatch was intentionally disabled for this one-turn check, so it proves the agent loop rather than any-to-any specialist routing.
- Expected reply: `HARNESS-AGENT-OK`; observed reply: `HARNESS-AGENT-OK`.
- Status: `model_finished`; total: 21.07 seconds.
- Context receipt: 16,384 window, 14,300 hard stop, 2,048 output reserve, 3,241 estimated tokens, zero compacted exchanges, `private_reasoning_exposed: false`.

## Still open

- Interactive `agents-chat` journey, code/tool mutation journey, and full declared any-to-any relay.
- Extracted Wan video runtime and resource qualification.
- FLUX image encoder license/assembly approval.
- Clean external-Mac install, update, repair, rollback, and uninstall.
- Public GitHub issue/repository/CI/release and Hugging Face mirrors.
- macOS signing and notarization.
