---
name: visual-gui-grounding
description: Execute deterministic 4-step macOS native GUI interactions in normalized_1000 coordinates and verify Wan 2.2 / FLUX.2 media generation telemetry receipts.
---

# Visual GUI Grounding & Media Telemetry (`visual-gui-grounding`)

When operating native macOS windows (`desktop`), visual web flows (`browser`), or local diffusion/video generation kernels (`image-studio`, `video-studio`), ground every action in verifiable pixel and tensor telemetry.

## 1. Mandatory 4-Step GUI Grounding Loop (`normalized_1000`)

Never guess screen coordinates or click blind. Every native UI interaction must execute the four-stage verification sequence:
1. **Observe (`desktop_observe`):** Enumerate visible application windows, window bounds, and accessibility tree elements.
2. **Focus (`desktop_focus`):** Bring the target application window to the foreground before capturing or clicking.
3. **Screenshot (`desktop_screenshot`):** Capture the live window state and locate target controls in `normalized_1000` coordinates (`x, y in [0, 1000]` scaled to the active target bounds).
4. **Act & Re-Verify (`desktop_click` / `desktop_type` -> `desktop_screenshot`):** Perform the bounded click or keystroke and capture a post-action screenshot confirming the expected state transition.

## 2. Media Kernel Telemetry Receipts (Wan 2.2 & FLUX.2-klein)

Every local image (`generate_local_image`) or video (`generate_local_video`) generation call must validate its returned JSON receipt before declaring success:
- **Numerical Sanity:** `finite_pixels == true` (zero `NaN` or `Inf` tensor outputs) and non-degenerate luminance variance (not an all-black or all-white frame).
- **Temporal Motion (Video):** Verify `mean_absolute_frame_change > 0.0` across synthesized frames to confirm active temporal denoising rather than frozen frame duplication.
- **Unified Memory Budget:** Inspect `mlx_peak_bytes_by_phase` / `mps_allocated_bytes` to confirm the heavy-model lock released peak VRAM cleanly upon completion.
