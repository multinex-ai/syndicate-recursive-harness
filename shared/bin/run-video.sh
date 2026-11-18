#!/bin/bash
# video worker: Rust owns validation, the heavy lock, memory reclaim and the resource guard;
# only the MLX compute runs in shared/harness/media_kernel.py.
set -euo pipefail
AGENT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}"
exec "$AGENT_ROOT/target/release/local-agents-rs" video "$@"
