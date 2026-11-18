---
license: apache-2.0
base_model: PandacatAI/umt5-xxl-enc-mlx-q8
library_name: mlx
tags:
  - text-encoder
  - quantized
  - apple-silicon
  - local-syndicate-harness
---

# Local Syndicate UMT5 XXL MLX Q8

This repository is a pinned redistribution mirror of the quantized UMT5 encoder selected from `PandacatAI/umt5-xxl-enc-mlx-q8` at revision `bffbdff244d3a91ac605640c7c72d695147b9901`.

Local Syndicate Harness² uses the encoder as one component of its optional Wan video route. The installer verifies the weights and configuration by byte count and SHA-256 before staging them.

This mirror preserves upstream attribution and license terms and does not claim authorship of UMT5 or the MLX conversion.
