# Generate an image or scene keyframe

Image generation is intentionally unavailable in this release candidate. The selected FLUX base is public Apache-2.0, but the chosen text encoder is gated under a non-commercial license and the current files do not yet have a proven runtime assembly contract. The command fails clearly instead of pretending a partial download is usable.

The future qualified journey will be:

```bash
./install.sh --models creative
```

./target/release/local-agents-rs image \
  "A faceted amber and cyan relay station on a neutral obsidian workbench, cinematic product lighting" \
  --preset draft \
  --output "$PWD/relay-keyframe.png"
```

Before that command is enabled, the project must record the encoder redistribution decision, implement the mflux assembly explicitly, run a fresh generated-image smoke test, visually inspect the result, and attach the receipt. Do not copy the gated encoder into a public mirror unless its terms explicitly permit that distribution.

