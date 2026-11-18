# agents/

Worker roles for the local harness. Each folder defines one role: what it may do, where it works, and where its output goes. The runtime that executes them is in [`../shared/`](../shared/README.md).

`agents/` is not `.agents/`. This folder holds roles. `.agents/` at the workspace root holds host integration (MCP config and skill packages).

| Role | Runnable | Purpose |
| --- | --- | --- |
| `coordinator` | yes | Plans a task and dispatches it to specialists |
| `cyber-lab` | yes | Authorized coding and security work inside its workspace |
| `browser` | yes | Browser tasks against explicitly allowed hosts |
| `desktop` | yes | Observes and controls native macOS windows |
| `image-studio` | yes | Image generation (FLUX.2 Klein via MFLUX) |
| `video-studio` | yes | Video generation (Wan 1.3B via MLX) |
| `browser-fara` | no | Excluded candidate record, kept for provenance |
| `voice` | no | Architecture-only plan for a speech worker |
| `premium-editor` | no | Review-only proposal; not registered with the runtime |

"Runnable" means a `config.json` exists. It does not mean the role is running or fully qualified; see [qualification status](../docs/qualification-status.md).

## Quick CLI Aliases

The local agent system has shell aliases configured in `~/.zshrc` (see [Cheat Sheet](../docs/cli-aliases.md)):

| Command | Action |
| :--- | :--- |
| `agent-run <role> '<task>'` | Run any runnable role directly with a prompt |
| `agent-image '<prompt>'` | Dispatch image generation to `image-studio` |
| `agent-video '<prompt>'` | Dispatch video generation to `video-studio` |
| `agents-status` | Print live agent roles, jobs, and capabilities |
| `agents-resources` | Check RAM, disk, swap, and model admission status |
| `agents-memory` | See candidate apps recommended to close |
| `agents-reclaim` | Safely close background apps to free RAM |
| `agents-mcp` | Start stdio MCP bridge for IDE integration |
| `agents-chat` | Start interactive terminal chat |

## Role folder layout

| Path | Purpose | Kind |
| --- | --- | --- |
| `config.json` | Executable role config: model, tools, limits | source |
| `candidate.json` | Why the role exists and what was recorded about it | source |
| `workspace/` | The only directory the role works in | runtime, ignored |
| `inbox/` | Structured messages from other roles | runtime, ignored |
| `outputs/` | Generated files and evidence sidecars | runtime, ignored |
| `tests/` | Acceptance fixtures (cyber-lab, desktop) | source |

Not every role has every folder. Ignore rules are in the workspace [`.gitignore`](../.gitignore); the Agents root itself is not a Git repository.

## Adding or changing a role

1. Copy a similar role folder and edit `config.json`.
2. Record scope and exclusions in `candidate.json`.
3. Keep the workspace inside the role folder. Path checks are not an OS sandbox.
4. Check it from the harness: `bash shared/bin/run-agent.sh <role> 'List what you can do.'`
5. Update this table and [folder hierarchy](../docs/folder-hierarchy.md).
