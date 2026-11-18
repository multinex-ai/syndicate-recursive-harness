#!/bin/bash
# Local Agents MCP bridge: the native Rust control plane (jobs, supervisor, agent loop, tools).
set -euo pipefail
AGENT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}"
export HF_HUB_OFFLINE=1 HF_HUB_DISABLE_TELEMETRY=1
exec "$AGENT_ROOT/target/release/local-agents-rs" mcp
