# Repository rules

- Keep the public harness independent of proprietary downstream products and of any private organization/account identifier.
- Do not commit weights, credentials, runtime state, prompts, or generated user content.
- Use Rust or TypeScript for system tooling; Python remains limited to the established ML runtime.
- Keep agent/model handoffs automatic but bounded by configured routes, tool/host ceilings, time, dispatch count, and the one-heavy-model lock.
- Do not claim a clean install, live model interaction, mirror, release, or deployment without its evidence receipt.
- Keep source files below 700 lines and run the five applicable verification gates before completion.

