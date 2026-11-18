# Your first local chat

After `./install.sh --models core`, add this checkout's `bin` folder to `PATH` and run:

```bash
agents-chat
```

Ask a small, checkable question first:

> Inspect this folder. Name the language and test command you can prove from its files. Do not edit anything.

Qwen is started only for the response and then unloaded. This matters on a shared-memory Mac: the local worker bridge can run after the language model gives memory back.

Useful commands inside the chat:

- `/quit` exits without leaving Qwen resident.
- Start the launcher with `resume --last` to continue the most recent isolated local chat.
- Set `SYNDICATE_WORKSPACE=/absolute/project/path` before `agents-chat` to choose a workspace.

If the model cannot load, run `cargo run -p syndicate-installer -- doctor --models core`. The evidence for each inference attempt is written under `runs/terminal-*`; prompts are not printed into normal adapter logs.

