#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust/Cargo is required for the source installer. Install it from https://rustup.rs, then rerun ./install.sh" >&2
  exit 1
fi
exec cargo run --quiet --manifest-path "$ROOT/installer/Cargo.toml" -- install --root "$ROOT" "$@"

