---
license: apache-2.0
base_model: HauhauCS/Qwen3.5-9B-Uncensored-HauhauCS-Aggressive
library_name: llama.cpp
tags:
  - gguf
  - apple-silicon
  - local-syndicate-harness
---

# Local Syndicate Qwen 3.5 9B

This repository is a pinned redistribution mirror for Local Syndicate Harness². It preserves the selected Q4_K_M GGUF and BF16 vision projector from `HauhauCS/Qwen3.5-9B-Uncensored-HauhauCS-Aggressive` at revision `0a41c68809d375475f954be12ba7c40efa56c2a9`.

The harness uses these files for local chat, coding, orchestration, and vision reasoning on Apple Silicon. The installer verifies exact byte counts and SHA-256 digests before activation. This mirror does not claim authorship of the model and does not alter the upstream license or acceptable-use obligations.

Source, file hashes, and the runnable manifest live in `multinex-ai/local-syndicate-harness-squared` under `catalog/models/manifest.json`.
