"""Download the pinned gated encoder using the repository-specific local credential."""
import hashlib,json,urllib.request
from pathlib import Path
from download_parallel import fetch,ScopedRedirect
ROOT=Path(__file__).resolve().parents[2]
REPO='ponpoke/flux2-klein-4b-uncensored-text-encoder';REV='633217e588e4c0bc76619052e05d3ce0e057cd83'
TOKEN=Path.home()/'.config/local-agents/hf-token'
SOURCE=ROOT/'shared/models/image-uncensored/source/text_encoder'
def main():
 if not TOKEN.is_file():raise SystemExit('The repository-specific local download token has not been provisioned.')
 token=TOKEN.read_text().strip();results=[];SOURCE.mkdir(parents=True,exist_ok=True)
 for name in ['config.json','generation_config.json']:
  url=f'https://huggingface.co/{REPO}/resolve/{REV}/flux2-klein-4b-uncensored-text-encoder/{name}'
  request=urllib.request.Request(url,headers={'Authorization':'Bearer '+token})
  with urllib.request.build_opener(ScopedRedirect()).open(request,timeout=30) as response:data=response.read()
  json.loads(data);path=SOURCE/name;path.write_bytes(data)
  results.append({'path':str(path),'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest()})
 results.append(fetch(REPO,REV,'flux2-klein-4b-uncensored-text-encoder/model.safetensors',8044981680,'3d6f27fd39c58c62f593ff5ef48cc734e8abb188005e6a15582876e30833ab96',SOURCE/'model.safetensors',workers=8,token=token))
 (ROOT/'evidence/setup-20261102/image-encoder-download.json').write_text(json.dumps({'repo':REPO,'revision':REV,'files':results,'license':'flux-non-commercial-v2.1','integrity_verified':True},indent=2))
 print('Uncensored source encoder downloaded and integrity checked.')
if __name__=='__main__':main()
