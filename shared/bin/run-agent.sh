#!/bin/bash
set -euo pipefail
AGENT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
exec "$AGENT_ROOT/target/release/local-agents-rs" agent "$@"
