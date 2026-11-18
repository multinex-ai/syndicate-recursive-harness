"""Download the selected Wan checkpoint and pinned local inference components."""
import concurrent.futures
import hashlib
import json
import urllib.request
from pathlib import Path

from download_parallel import fetch

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'shared/models/wan-uncensored'
SPECS = [
    ('blackshirt/NSFW_Wan_1.3b', '6199ecb457af237b143ca9cc2fccec6dcfbe4c22',
     {'wan_1.3B_exp_e14.safetensors': 'wan_1.3B_exp_e14.safetensors'}),
    ('Wan-AI/Wan2.1-T2V-1.3B', '37ec512624d61f7aa208f7ea8140a131f93afc9a',
     {'Wan2.1_VAE.pth': 'Wan2.1_VAE.pth', 'google/umt5-xxl/tokenizer.json': 'tokenizer.json'}),
    ('PandacatAI/umt5-xxl-enc-mlx-q8', 'bffbdff244d3a91ac605640c7c72d695147b9901',
     {'weights.safetensors': 't5-q8.safetensors', 'config.json': 't5-config.json'}),
]


def main():
    manifest = []
    for repo, revision, names in SPECS:
        url = f'https://huggingface.co/api/models/{repo}/revision/{revision}?blobs=true'
        with urllib.request.urlopen(url, timeout=60) as response:
            metadata = json.load(response)
        if metadata['sha'] != revision:
            raise ValueError('Repository revision mismatch')
        files = {f['rfilename']: f for f in metadata['siblings']}
        for remote, local in names.items():
            manifest.append(dict(files[remote], repo=repo, revision=revision, local=local))
    evidence = ROOT / 'evidence/setup-20261102'
    (evidence / 'video-download-manifest.json').write_text(json.dumps(manifest, indent=2))

    def download(item):
        target = OUT / item['local']
        target.parent.mkdir(parents=True, exist_ok=True)
        if item.get('lfs'):
            return fetch(item['repo'], item['revision'], item['rfilename'], item['size'],
                         item['lfs']['sha256'], target, workers=16)
        url = f"https://huggingface.co/{item['repo']}/resolve/{item['revision']}/{item['rfilename']}"
        with urllib.request.urlopen(url, timeout=60) as response:
            data = response.read()
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        if len(data) != item['size'] or blob != item['blobId']:
            raise ValueError('Metadata integrity mismatch')
        target.write_bytes(data)
        return {'path': str(target), 'sha256': hashlib.sha256(data).hexdigest(), 'verified': True}

    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        results = list(pool.map(download, manifest))
    (evidence / 'video-download.json').write_text(json.dumps({'files': results, 'verified': True}, indent=2))


if __name__ == '__main__':
    main()
