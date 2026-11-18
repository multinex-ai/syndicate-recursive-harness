# models/

Model profile definitions. These are small JSON files that describe how to reach a model. The weights themselves live in `../shared/models/` and are not tracked.

| Profile | Provider | Model | Endpoint |
| --- | --- | --- | --- |
| `local-qwen35-9b-uncensored` | local GGUF via llama.cpp | Qwen 3.5 9B (Q4_K_M) with vision projector | `127.0.0.1:8087` |
| `cloudflare-qwen-orchestrator` | Cloudflare Workers AI (hosted) | Qwen2.5 Coder 32B | Cloudflare API |
| `cloudflare-llama-osint-fast` | Cloudflare Workers AI (hosted) | Llama 3.3 70B fp8 fast | Cloudflare API |

## Fields

`schema_version`, `profile_id`, `provider_kind`, `model_ref`, `mmproj_ref`, `endpoint`, `context_window`, `kv_cache_quant`, `requires_heavy_lock`.

`requires_heavy_lock` is `true` for models that must hold the single heavy-model slot, which is why only one local model runs at a time.

## Before you use a hosted profile

The hosted profiles send prompts to a third party and need `CLOUDFLARE_ACCOUNT_ID` plus a token in the environment. Neither belongs in this folder. The workspace's "local only" statement describes the local profile. Confirm which profile a role or syndicate agent selects in its own config before assuming where inference runs.
