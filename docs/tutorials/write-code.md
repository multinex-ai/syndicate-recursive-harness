# Write code with the local model

Open `agents-chat` from the project you want to change and give Qwen a finish line:

> Add a `sum` function to this TypeScript package. Keep strict typing, add tests for positive, negative, and empty input, run the typechecker and tests, then list the files changed and exact results.

A useful coding request names four things:

1. The behavior to add.
2. Constraints such as strict typing or file-size limits.
3. The checks to run.
4. The evidence you want back.

The local model can read, edit, and run commands only through the configured workspace tools and sandbox. Review the diff before publishing anything. A local test pass proves the checkout behaved on this machine; it does not prove deployment or production readiness.

For a bounded multi-worker run:

```bash
./target/release/local-agents-rs agent conductor \
  "Implement the requested CLI change in the assigned workspace and have verifier rerun its checks" \
  --dispatch-limit 4 --timeout 1200
```

The conductor automatically queues the builder. The builder can automatically hand the result to the verifier. No user click is required between those internal stages.

