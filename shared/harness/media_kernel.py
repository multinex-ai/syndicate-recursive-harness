"""MLX compute kernel for the Rust media workers (local-agents-rs image|video).

Only the model work lives here: load FLUX.2 klein (mflux) or the pinned Wan pipeline,
generate, save the file, print one JSON line of kernel facts. Validation, the heavy
lock, memory reclaim, admission, the resource watchdog and evidence records are all
in Rust (src/media.rs), which runs this as a killable child process.

Usage: media_kernel.py <request.json>
"""
import gc
import json
import os
import sys
import time
from pathlib import Path

GIB = 2**30


def configure(mx):
    # Allocation guideline, not a hard ceiling; the Rust watchdog enforces the limits.
    mx.set_memory_limit(7 * GIB)
    mx.set_cache_limit(GIB // 4)
    mx.reset_peak_memory()


def image(req):
    import mlx.core as mx
    from mflux.callbacks.instances.memory_saver import MemorySaver
    from mflux.models.common.config.model_config import ModelConfig
    from mflux.models.flux2.variants import Flux2Klein, Flux2KleinEdit
    configure(mx)
    started = time.monotonic()
    cls = Flux2KleinEdit if req.get('reference') else Flux2Klein
    model = cls(model_config=ModelConfig.flux2_klein_4b(), model_path=req['model_path'])
    model.callbacks.register(MemorySaver(model=model, keep_transformer=False, cache_limit_bytes=2**28, num_seeds=1))
    extra = {'image_paths': [req['reference']]} if req.get('reference') else {}
    result = model.generate_image(seed=req['seed'], prompt=req['prompt'], width=req['width'], height=req['height'],
                                  num_inference_steps=req['steps'], guidance=req['guidance'], **extra)
    Path(req['output']).parent.mkdir(parents=True, exist_ok=True)
    result.save(req['output'])
    return {'seconds': round(time.monotonic() - started, 2), 'mlx_peak_bytes': mx.get_peak_memory()}


def video(req):
    import mlx.core as mx
    import mlx.nn as nn
    import numpy as np
    models = Path(req['models'])
    sys.path.insert(0, req['vendor'])
    import wan.pipeline as pipeline_module
    import wan.utils as utils
    from wan.t5 import create_umt5_xxl_encoder, T5Encoder
    local_files = {'Wan2.1_VAE.pth': models / 'Wan2.1_VAE.pth', 'google/umt5-xxl/tokenizer.json': models / 'tokenizer.json'}

    def local_component(repo, filename):
        if repo != 'Wan-AI/Wan2.1-T2V-1.3B' or filename not in local_files:
            raise ValueError('Unconfigured video component: ' + filename)
        return str(local_files[filename])

    def load_quantized_t5(name):
        if name != 't2v-1.3B':
            raise ValueError('Only the qualified 1.3B architecture is configured')
        raw = mx.load(str(models / 't5-q8.safetensors'))
        # This MLX export serializes Sequential's linear gate as layers.0.
        remapped = {}
        for key, value in raw.items():
            target = key.replace('.ffn.gate.layers.0.', '.ffn.gate.')
            if target in remapped:
                raise ValueError('Duplicate UMT5 parameter after mapping: ' + target)
            remapped[target] = value
        weights = T5Encoder.sanitize(remapped)
        model = create_umt5_xxl_encoder()
        nn.quantize(model, group_size=64, bits=8, class_predicate=lambda path, module: path + '.scales' in weights)
        model.load_weights(list(weights.items()), strict=True)
        return model

    # Replace download resolution and text-encoder loading in the pinned vendored pipeline.
    utils._hf_download = local_component
    pipeline_module.load_t5 = load_quantized_t5
    mx.set_default_device(mx.gpu)
    configure(mx)
    mx.set_cache_limit(0)
    started = time.monotonic()
    pipeline = pipeline_module.WanPipeline('t2v-1.3B', checkpoint=str(models / req['checkpoint']))
    nn.quantize(pipeline.flow, bits=8)
    print('Loaded the custom Wan DiT and Q8 UMT5 encoder.', file=sys.stderr, flush=True)
    latents = pipeline.generate_latents(req['prompt'], negative_prompt='Text, watermarks, blurry, distorted, flickering',
                                        size=(req['width'], req['height']), frame_num=req['frames'], num_steps=req['steps'],
                                        guidance=req['guidance'], shift=5.0, seed=req['seed'])
    conditioning = next(latents)
    mx.eval(conditioning)
    peaks = {'conditioning': mx.get_peak_memory()}
    del pipeline.t5, conditioning
    gc.collect(); mx.clear_cache(); mx.reset_peak_memory()
    for step, latent in enumerate(latents, 1):
        mx.eval(latent)
        print(f'Denoising {step}/{req["steps"]}, elapsed {time.monotonic()-started:.1f}s', file=sys.stderr, flush=True)
    peaks['generation'] = mx.get_peak_memory()
    del pipeline.flow
    gc.collect(); mx.clear_cache(); mx.reset_peak_memory()
    frames = pipeline.decode(latent)
    mx.eval(frames)
    peaks['decoding'] = mx.get_peak_memory()
    Path(req['output']).parent.mkdir(parents=True, exist_ok=True)
    if not utils.save_video(frames, req['output'], fps=req['fps']):
        raise RuntimeError('FFmpeg did not save the video')
    pixels = np.array(frames)
    return {'seconds': round(time.monotonic() - started, 2), 'mlx_peak_bytes_by_phase': peaks,
            'finite_pixels': bool(np.isfinite(pixels).all()),
            'mean_absolute_frame_change': float(np.abs(np.diff(pixels.astype(np.float32), axis=0)).mean())}


if __name__ == '__main__':
    os.environ['HF_HUB_OFFLINE'] = '1'
    os.environ['HF_HUB_DISABLE_TELEMETRY'] = '1'
    request = json.loads(Path(sys.argv[1]).read_text())
    facts = {'image': image, 'video': video}[request['mode']](request)
    print(json.dumps({'kernel': facts}), flush=True)
