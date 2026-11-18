# Contributing

Start with a GitHub issue that names the behavior and acceptance evidence. Keep Rust and TypeScript strict, exported boundaries explicit, and source files below roughly 700 lines. Do not commit model weights, credentials, generated private content, or local runtime state.

Before a pull request:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo build --workspace --release
node --test shared/chat/test-*.mjs
node scripts/verify/repository.mjs
```

For UI or media changes, attach a reproducible runtime receipt and the smallest useful visual evidence. State what was locally tested separately from what was deployed or tested on a clean machine.

