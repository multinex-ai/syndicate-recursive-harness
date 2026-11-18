#!/bin/bash
set -euo pipefail
AGENT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}"
exec "$AGENT_ROOT/shared/runtime/venv/bin/python" "$AGENT_ROOT/shared/harness/memq_mcp_server.py"
