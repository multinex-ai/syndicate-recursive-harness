# Generate a short video

Install the creative set and start with the draft preset:

```bash
./install.sh --models creative

./target/release/local-agents-rs video \
  "A slow camera push toward a faceted relay as amber modules pass a pale-cyan pulse, restrained motion, no text" \
  --preset draft --output "$PWD/relay-draft.mp4"
```

The current local Wan adapter is text-to-video. It does not claim image-to-video continuity. `draft` is the fastest composition check; `quality` uses the larger qualified frame; `detail-short` trades duration for detail.

The output is accepted only when the MP4 decodes and its evidence receipt exists. Watch the entire clip anyway: a valid file can still contain visual artifacts, continuity errors, or unwanted content.

Model and safety limits:

- The selected adult checkpoint is marked Not-For-All-Audiences and has no built-in safety filter.
- Never generate sexual content involving minors or non-consensual depictions of a real person.
- Obtain rights and consent before publication.
- Keep the source model license and attribution when redistributing weights.

