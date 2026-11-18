---
license: apache-2.0
base_model: Runpod/FLUX.2-klein-4B-mflux-4bit
library_name: mlx
tags:
  - image-generation
  - apple-silicon
  - local-syndicate-harness
---

# Local Syndicate FLUX.2 Klein 4B Base

This repository is a pinned redistribution mirror of the Apple-Silicon FLUX.2 Klein 4B base files selected from `Runpod/FLUX.2-klein-4B-mflux-4bit` at revision `7ee1b3aa8178a1240050490072196a57da2bf2a9`.

It is only the base component. Local Syndicate Harness² keeps image generation disabled until a compatible text encoder, its license, and the complete assembly pass end-to-end qualification. The gated FLUX Non-Commercial encoder is not included in this mirror.

The installer checks exact byte counts and SHA-256 digests from `catalog/models/manifest.json` before staging any file.
