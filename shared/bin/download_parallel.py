"""Pinned, checksummed public downloads with resumable HTTP byte ranges."""
import concurrent.futures,hashlib,json,os,threading,time,urllib.request,urllib.parse
from pathlib import Path

class ScopedRedirect(urllib.request.HTTPRedirectHandler):
 def redirect_request(self,req,fp,code,msg,headers,newurl):
  result=super().redirect_request(req,fp,code,msg,headers,newurl)
  if result is not None and urllib.parse.urlparse(newurl).hostname!='huggingface.co':
   result.remove_header('Authorization')
  return result

def fetch(repo,revision,name,size,sha,destination,workers=8,token=None,url=None,chunk_bytes=32*1024*1024):
 workers=int(os.environ.get('LOCAL_MODEL_DOWNLOAD_WORKERS',workers))
 if not 1 <= workers <= 256:raise ValueError('Download concurrency must be 1–256')
 import resource
 soft,hard=resource.getrlimit(resource.RLIMIT_NOFILE)
 desired=2048 if hard==resource.RLIM_INFINITY else min(2048,hard)
 if soft<desired:resource.setrlimit(resource.RLIMIT_NOFILE,(desired,hard))
 path=Path(destination);path.parent.mkdir(parents=True,exist_ok=True)
 def digest(p):
  h=hashlib.sha256()
  with p.open('rb') as f:
   for b in iter(lambda:f.read(8*1024*1024),b''):h.update(b)
  return h.hexdigest()
 if path.exists() and path.stat().st_size==size and digest(path)==sha:return {'path':str(path),'sha256':sha,'verified':True}
 part=path.with_suffix(path.suffix+'.part');state=path.with_suffix(path.suffix+'.chunks.json');step=chunk_bytes
 if state.exists():
  saved=json.loads(state.read_text())
  if (saved['repo'],saved['revision'],saved['size'],saved['sha256'])!=(repo,revision,size,sha):raise ValueError('Resume metadata mismatch')
  step=saved['chunk_size'];done=set(saved['done']);offsets={int(k):v for k,v in saved.get('offsets',{}).items()}
 else:done=set();offsets={}
 fd=os.open(part,os.O_CREAT|os.O_RDWR,0o600);os.ftruncate(fd,size);lock=threading.Lock();start=time.monotonic();last=[start]
 def save():
  temp=state.with_suffix('.tmp');temp.write_text(json.dumps({'repo':repo,'revision':revision,'size':size,'sha256':sha,'chunk_size':step,'done':sorted(done),'offsets':offsets}));temp.replace(state)
 save()
 def chunk(i):
  lo=i*step;hi=min(size,lo+step)-1
  offset=offsets.get(i,lo)
  if not isinstance(offset,int) or not lo<=offset<=hi+1:raise ValueError('Invalid partial resume offset')
  failures=0
  while offset<=hi:
   before=offset
   try:
    # Shorter responses survive slow/interrupted links. Keep valid prefixes;
    # never infer received data from the sparse file's apparent length.
    end=min(hi,offset+4*1024*1024-1)
    source=url or f'https://huggingface.co/{repo}/resolve/{revision}/{name}?download=true'
    headers={'Range':f'bytes={offset}-{end}'}
    if token:headers['Authorization']='Bearer '+token
    req=urllib.request.Request(source,headers=headers)
    with urllib.request.build_opener(ScopedRedirect()).open(req,timeout=90) as response:
     if response.status!=206 or response.headers.get('Content-Range','')!=f'bytes {offset}-{end}/{size}':raise ValueError('Server range mismatch')
     while data:=response.read(1024*1024):
      if offset+len(data)>end+1:raise ValueError('Oversized range')
      n=os.pwrite(fd,data,offset)
      if n!=len(data):raise IOError('Short write')
      offset+=n
      with lock:offsets[i]=offset;save()
     if offset!=end+1:raise IOError('Truncated range; valid prefix retained')
    failures=0
   except Exception:
    failures=0 if offset>before else failures+1
    if failures>=8:raise
    time.sleep(min(8,1+failures))
  with lock:
   done.add(i);offsets.pop(i,None);save()
   if time.monotonic()-last[0]>15:
    print(name,round(sum(min(step,size-j*step) for j in done)/1e9,2),'/',round(size/1e9,2),'GB',flush=True);last[0]=time.monotonic()

 try:
  with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:list(pool.map(chunk,[i for i in range((size+step-1)//step) if i not in done]))
 finally:os.close(fd)
 assert digest(part)==sha,'Checksum mismatch'
 part.replace(path);state.unlink(missing_ok=True)
 print('Verified',name,flush=True)
 return {'path':str(path),'repo':repo,'revision':revision,'bytes':size,'sha256':sha,'verified':True,'seconds':round(time.monotonic()-start,2)}
if __name__=='__main__':
 from download_models import ROOT,REPO,REV,FILES
 with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
  results=list(pool.map(lambda f:fetch(REPO,REV,f[0],f[1],f[2],ROOT/'shared/models/qwen35-9b-uncensored'/f[0]),FILES))
 (ROOT/'evidence/setup-20261102/qwen-download.json').write_text(json.dumps({'repo':REPO,'revision':REV,'files':results},indent=2)+'\n')
