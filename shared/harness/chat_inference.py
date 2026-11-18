"""One isolated CLI inference request; unload Qwen before returning tool calls."""
import base64
import json
import os
import signal
import sys
import uuid
import urllib.error
import urllib.request

from runtime import ROOT, ENDPOINT, model_server
from media_policy import MediaGuard


def main():
    # Ensure low-memory bounds, warning tolerance, and 3.0 GiB swap headroom during interactive chat inference on Apple Silicon
    os.environ.setdefault('LOCAL_AGENTS_LOW_MEMORY', '1')
    os.environ.setdefault('LOCAL_AGENTS_TOLERATE_WARNING_GIB', '2.0')
    os.environ.setdefault('LOCAL_AGENTS_MAX_SWAP_GROWTH_GIB', '3.0')
    run_dir = ROOT / 'runs' / ('terminal-' + uuid.uuid4().hex)
    run_dir.mkdir(parents=True)
    def cancelled(signum, frame):
        raise KeyboardInterrupt('Terminal inference cancelled or deadline exceeded')
    signal.signal(signal.SIGTERM, cancelled)
    signal.signal(signal.SIGALRM, cancelled)
    signal.alarm(1200)
    status, content_type, response = 503, 'application/json', b''
    guard = None
    failure = None
    try:
        payload = sys.stdin.buffer.read(4 * 1024 * 1024 + 1)
        if len(payload) > 4 * 1024 * 1024:
            raise ValueError('Inference request exceeds 4 MiB')
        body = json.loads(payload)
        if not isinstance(body, dict):
            raise ValueError('Expected an inference object')
        guard = MediaGuard(run_dir / 'inference.json', include_children=True, shutdown_grace_seconds=15)
        context_size = int(os.environ.get('LOCAL_AGENTS_CONTEXT_SIZE', '16384'))
        with model_server(run_dir, guard=guard, lock_timeout=900, context_size=context_size, low_memory=True):
            request = urllib.request.Request(ENDPOINT + '/v1/responses', data=payload,
                                             headers={'Content-Type': 'application/json'})
            try:
                upstream = urllib.request.urlopen(request, timeout=240)
            except urllib.error.HTTPError as error:
                upstream = error
            with upstream:
                status = upstream.status
                content_type = upstream.headers.get('Content-Type', 'application/json')
                response = upstream.read(32 * 1024 * 1024 + 1)
                if len(response) > 32 * 1024 * 1024:
                    raise ValueError('Inference response exceeds 32 MiB')
    except BaseException as error:
        status, content_type = 503, 'application/json'
        reason = guard.reason if guard is not None and guard.reason else str(error) or type(error).__name__
        resource_blocked = guard is not None and bool(guard.reason)
        message = ('Local Qwen stopped by the memory/resource guard: ' if resource_blocked else 'Local Qwen runtime error: ') + reason
        if resource_blocked and guard.samples:
            last = guard.failure_sample or guard.samples[-1]
            message += f". At the stop: {last['available_bytes']/2**30:.2f} GiB available; pressure level {last['pressure_level']} (1=normal, 2=warning). Free more memory before retrying; the 4 GiB start minimum does not guarantee enough memory during inference. Your saved chat is preserved."
        failure = {'message': message, 'type': 'local_resource_limit' if resource_blocked else 'local_runtime_error',
                   'evidence': str(run_dir)}
        response = json.dumps({'error': failure}).encode()
    finally:
        signal.alarm(0)
    # model_server has stopped its owned server and released the lock before
    # the client can see tool calls. Never log the prompt or credentials here.
    result = {'status': status, 'content_type': content_type,
              'body': base64.b64encode(response).decode(), 'run_directory': str(run_dir)}
    (run_dir / 'receipt.json').write_text(json.dumps({'http_status': status, 'run_directory': str(run_dir), 'error': failure}))
    print(json.dumps(result), flush=True)


if __name__ == '__main__':
    main()
