"""Fetch only the quantized diffusion/VAE/tokenizer components; exclude stock encoder."""
import concurrent.futures,hashlib,json,urllib.request
from pathlib import Path
from download_parallel import fetch
ROOT=Path(__file__).resolve().parents[2]
manifest=json.loads((ROOT/'evidence/setup-20261102/flux-quantized-base-manifest.json').read_text())
out=ROOT/'shared/models/image-uncensored/flux2-klein-4b-mflux-q4'
def get(f):
 name=f['rfilename']
 if name in ('.gitattributes','README.md'):return None
 target=out/name;target.parent.mkdir(parents=True,exist_ok=True)
 if f.get('lfs'):
  return fetch(manifest['repo'],manifest['revision'],name,f['size'],f['lfs']['sha256'],target,workers=4)
 data=urllib.request.urlopen(f"https://huggingface.co/{manifest['repo']}/resolve/{manifest['revision']}/{name}",timeout=60).read()
 if len(data)!=f['size']:raise ValueError('Size mismatch: '+name)
 target.write_bytes(data);return {'path':str(target),'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),'size_verified':True}
if __name__=='__main__':
 with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:results=[x for x in pool.map(get,manifest['files']) if x]
 (ROOT/'evidence/setup-20261102/image-base-download.json').write_text(json.dumps({'repo':manifest['repo'],'revision':manifest['revision'],'files':results,'stock_text_encoder_downloaded':False},indent=2))
