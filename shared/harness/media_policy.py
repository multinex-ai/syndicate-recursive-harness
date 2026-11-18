"""Conservative media presets and process-local resource monitoring for this Mac."""
from contextlib import AbstractContextManager
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import threading
import time

import psutil
from runtime import ROOT

GIB = 2**30
PRESETS = {
    'image': {'draft': {'width': 512, 'height': 512},
              'quality': {'width': 768, 'height': 768}},
    'video': {'draft': {'width': 320, 'height': 192, 'frames': 33, 'steps': 24},
              'quality': {'width': 384, 'height': 224, 'frames': 33, 'steps': 32},
              'detail-short': {'width': 512, 'height': 288, 'frames': 17, 'steps': 32}},
}


def media_arguments(role, preset='draft', reference_image=None):
    kind = role.removesuffix('-studio')
    if kind not in PRESETS or preset not in PRESETS[kind]:
        raise ValueError('Unsupported media role/preset')
    args = ['--preset', preset]
    if reference_image:
        if kind != 'image':
            raise ValueError('Reference editing is supported only by the image worker')
        path = Path(reference_image)
        if not path.is_absolute():
            raise ValueError('Reference image must be an absolute local path')
        args += ['--reference-image', str(path)]
    return args


def snapshot():
    memory = psutil.virtual_memory()
    pressure = int(subprocess.check_output(
        ['/usr/sbin/sysctl', '-n', 'kern.memorystatus_vm_pressure_level'], text=True, timeout=3))
    return {'time': time.time(), 'physical_bytes': memory.total,
            'available_bytes': memory.available, 'pressure_level': pressure,
            'swap_used_bytes': psutil.swap_memory().used,
            'disk_free_bytes': shutil.disk_usage(ROOT).free,
            'worker_rss_bytes': psutil.Process().memory_info().rss}


def warning_tolerance():
    val = os.environ.get('LOCAL_AGENTS_TOLERATE_WARNING_GIB')
    if val:
        try:
            g = float(val)
            if g > 0:
                return max(1.5, g) * GIB
        except (ValueError, TypeError):
            pass
    return None


def max_swap_growth():
    val = os.environ.get('LOCAL_AGENTS_MAX_SWAP_GROWTH_GIB')
    if val:
        try:
            g = float(val)
            if g > 0:
                return g * GIB
        except (ValueError, TypeError):
            pass
    if os.environ.get('LOCAL_AGENTS_TOLERATE_WARNING_GIB') or os.environ.get('LOCAL_AGENTS_LOW_MEMORY') == '1':
        return 3.0 * GIB
    return 0.75 * GIB


def admission_reason(sample):
    tol = warning_tolerance()
    if sample['pressure_level'] >= 4:
        return 'Critical macOS memory pressure'
    if sample['pressure_level'] != 1:
        if not (tol and sample['pressure_level'] == 2 and sample['available_bytes'] >= tol):
            return 'macOS memory pressure is not normal'
    min_avail = tol if tol else 4 * GIB
    if sample['available_bytes'] < min_avail:
        return 'Less than 4 GiB available before model loading'
    if sample['disk_free_bytes'] < 12 * GIB:
        return 'Less than 12 GiB disk reserve'
    return None


def runtime_reason(sample, initial):
    if sample['pressure_level'] >= 4:
        return 'Critical macOS memory pressure'
    if sample['worker_rss_bytes'] > 8 * GIB:
        return 'Worker resident memory exceeded 8 GiB'
    max_swap = max_swap_growth()
    if sample['swap_used_bytes'] - initial['swap_used_bytes'] > max_swap:
        gib_val = max_swap / GIB
        if gib_val >= 1.0:
            return f'System swap grew by more than {gib_val:.1f} GiB during this job'
        return f'System swap grew by more than {int(max_swap / (2**20))} MiB during this job'
    if sample['disk_free_bytes'] < 12 * GIB:
        return 'Disk reserve fell below 12 GiB'
    return None


class MediaGuard(AbstractContextManager):
    """Monitor only; terminate this worker on resource pressure, never other apps.

    MLX's memory limit is an allocation guideline, not a hard OS quota. This
    sampled watchdog can overshoot between samples and cannot reserve RAM.
    """
    def __init__(self, output, include_children=False, shutdown_grace_seconds=5):
        self.path = Path(output).with_suffix('.resources.json')
        self.stop = threading.Event()
        self.samples = []
        self.reason = None
        self.thread = None
        self.include_children = include_children
        self.shutdown_grace_seconds = shutdown_grace_seconds
        self.failure_sample = None

    def take_snapshot(self):
        sample = snapshot()
        if self.include_children:
            child_rss = 0
            for child in psutil.Process().children(recursive=True):
                try:
                    child_rss += child.memory_info().rss
                except psutil.NoSuchProcess:
                    pass
            sample['child_rss_bytes'] = child_rss
            sample['worker_rss_bytes'] += child_rss
        return sample

    def save(self, status):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text(json.dumps({'status': status, 'reason': self.reason,
            'trigger_sample': self.failure_sample,
            'limits': {'min_start_available_gib': 4, 'min_disk_free_gib': 12,
                       'max_worker_rss_gib': 8, 'max_swap_growth_gib': .75,
                       'mlx_allocation_guideline_gib': 7, 'mlx_cache_gib': .25,
                       'sample_seconds': 2, 'warning_grace_seconds': 6},
            'limitation': 'Sampled safeguards; not an OS memory reservation or hard allocation ceiling. System swap includes other apps.',
            'samples': self.samples}, indent=2))

    def __enter__(self):
        self.initial = self.take_snapshot()
        self.samples.append(self.initial)
        self.reason = admission_reason(self.initial)
        if self.reason:
            self.failure_sample = self.initial
            self.save('admission_blocked')
            raise RuntimeError(self.reason)
        self.save('running')
        self.thread = threading.Thread(target=self.watch, daemon=True)
        self.thread.start()
        return self

    def watch(self):
        warning_since = None
        tol = warning_tolerance()
        while not self.stop.wait(2):
            try:
                sample = self.take_snapshot()
                self.samples.append(sample)
                self.reason = runtime_reason(sample, self.initial)
                if tol and sample['pressure_level'] == 2 and sample['available_bytes'] >= tol:
                    warning = sample['available_bytes'] < 1.5 * GIB
                else:
                    warning = sample['pressure_level'] != 1 or sample['available_bytes'] < 1.5 * GIB
                warning_since = (warning_since or time.monotonic()) if warning else None
                if warning_since and time.monotonic() - warning_since >= 6:
                    self.reason = self.reason or 'Sustained memory warning or less than 1.5 GiB available'
            except Exception as error:
                self.reason = 'Resource monitoring failed: ' + str(error)
            if self.reason:
                self.failure_sample = self.samples[-1]
                self.save('resource_stopped')
                os.kill(os.getpid(), signal.SIGINT)
                # Native GPU calls may defer Python's interrupt. Only this worker
                # exits if it has not unwound within five seconds.
                if not self.stop.wait(self.shutdown_grace_seconds):
                    os._exit(130)
                return
            self.save('running')

    def configure_mlx(self):
        import mlx.core as mx
        mx.set_memory_limit(7 * GIB)
        mx.set_cache_limit(GIB // 4)
        mx.reset_peak_memory()

    def __exit__(self, kind, value, traceback):
        self.stop.set()
        if self.thread:
            self.thread.join(timeout=4)
        try:
            self.samples.append(self.take_snapshot())
        except Exception:
            pass
        self.save('resource_stopped' if self.reason else 'completed' if kind is None else 'failed')
        return False
