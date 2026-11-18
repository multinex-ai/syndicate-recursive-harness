"""Local Metal server lifecycle and serialized heavy-model admission."""
from contextlib import contextmanager, nullcontext
from pathlib import Path
import fcntl,http.client,json,os,secrets,signal,socket,subprocess,time,urllib.request
ROOT=Path(__file__).resolve().parents[2]
MODEL=ROOT/'shared/models/qwen35-9b-uncensored/Qwen3.5-9B-Uncensored-HauhauCS-Aggressive-Q4_K_M.gguf'
PROJECTOR=MODEL.parent/'mmproj-Qwen3.5-9B-Uncensored-HauhauCS-Aggressive-BF16.gguf'
ENDPOINT='http://127.0.0.1:8087'
_HTTP_CONN=None
_API_KEY=secrets.token_hex(24)

def heavy_lock_path():
 configured=os.environ.get('SYNDICATE_HEAVY_LOCK','').strip()
 state=os.environ.get('XDG_STATE_HOME','').strip()
 if configured:
  candidate=Path(configured)
  if candidate.is_absolute():return candidate
 if state:
  candidate=Path(state)
  if candidate.is_absolute():return candidate/'local-syndicate-harness/heavy-model.lock'
 return Path.home()/'.local/state/local-syndicate-harness/heavy-model.lock'

HEAVY_LOCK_PATH=heavy_lock_path()
LOCK_CONTRACT_VERSION=1

def request_json(path,payload=None,timeout=180):
 global _HTTP_CONN
 data=json.dumps(payload).encode() if payload is not None else None
 method='POST' if data is not None else 'GET'
 headers={'Authorization':'Bearer '+_API_KEY,'Connection':'keep-alive'}
 if data is not None:headers['Content-Type']='application/json'
 for attempt in range(2):
  try:
   if _HTTP_CONN is None:
    _HTTP_CONN=http.client.HTTPConnection('127.0.0.1',8087,timeout=timeout)
   else:
    _HTTP_CONN.timeout=timeout
   _HTTP_CONN.request(method,path,body=data,headers=headers)
   resp=_HTTP_CONN.getresponse()
   raw=resp.read()
   if resp.status>=400:
    raise RuntimeError(f'HTTP {resp.status}: {raw.decode(errors="replace")[:400]}')
   return json.loads(raw)
  except (http.client.HTTPException,OSError,ConnectionError):
   if _HTTP_CONN is not None:
    try:_HTTP_CONN.close()
    except Exception:pass
    _HTTP_CONN=None
   if attempt==1:
    request=urllib.request.Request(ENDPOINT+path,data=data,headers=headers)
    with urllib.request.urlopen(request,timeout=timeout) as response:return json.load(response)

@contextmanager
def heavy_lock(timeout=0):
 lock=HEAVY_LOCK_PATH
 lock.parent.mkdir(parents=True,exist_ok=True)
 with lock.open('a+') as handle:
  deadline=time.monotonic()+timeout
  while True:
   try:
    fcntl.flock(handle,fcntl.LOCK_EX|fcntl.LOCK_NB);break
   except BlockingIOError:
    if time.monotonic()>=deadline:raise RuntimeError('Another local heavy-model worker is active.')
    time.sleep(.25)
  try:yield
  finally:fcntl.flock(handle,fcntl.LOCK_UN)

def terminate(process):
 if process.poll() is None:
  try:os.killpg(process.pid,signal.SIGTERM)
  except ProcessLookupError:pass
  try:process.wait(timeout=8)
  except subprocess.TimeoutExpired:
   try:os.killpg(process.pid,signal.SIGKILL)
   except ProcessLookupError:pass
   process.wait(timeout=8)

@contextmanager
def model_server(run_dir,vision=False,guard=None,lock_timeout=0,context_size=16384,low_memory=False):
 global _HTTP_CONN
 if guard is None:
  from media_policy import MediaGuard
  guard=MediaGuard(run_dir/'model.json',include_children=True,shutdown_grace_seconds=15)
 with heavy_lock(timeout=lock_timeout), (guard if guard is not None else nullcontext()):
  if not MODEL.is_file():raise FileNotFoundError('Model download is incomplete: '+str(MODEL))
  if vision and not PROJECTOR.is_file():raise FileNotFoundError('Vision projector download is incomplete')
  with socket.socket() as probe:
   if probe.connect_ex(('127.0.0.1',8087))==0:raise RuntimeError('Port 8087 is already occupied; refusing to reuse an unverified server.')
  args=['/opt/homebrew/bin/llama','serve','-m',str(MODEL),'--alias','local-agent','--host','127.0.0.1','--port','8087','--api-key',_API_KEY,'-c',str(context_size),'-np','1','-ngl','99','-fa','on','--cache-type-k','q8_0','--cache-type-v','q8_0','--jinja','--reasoning','off']
  if low_memory:args+=['--batch-size','256','--ubatch-size','64','--cache-ram','1024']
  if vision:args+=['--mmproj',str(PROJECTOR)]
  run_dir.mkdir(parents=True,exist_ok=True)
  with (run_dir/'server.log').open('w') as log:
   process=subprocess.Popen(args,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
   process_record={'pid':process.pid,'endpoint':ENDPOINT,'vision':vision,'state':'starting','context_size':context_size,'low_memory':low_memory,'heavy_lock_path':str(HEAVY_LOCK_PATH),'lock_contract_version':LOCK_CONTRACT_VERSION}
   process_path=run_dir/'server-process.json';process_path.write_text(json.dumps(process_record,indent=2))
   try:
    deadline=time.monotonic()+180
    while True:
     if process.poll() is not None:raise RuntimeError('Model server exited; inspect '+str(run_dir/'server.log'))
     try:
      if request_json('/health',timeout=2).get('status')=='ok':break
     except Exception:pass
     if time.monotonic()>deadline:raise TimeoutError('Model loading exceeded 180 seconds')
     time.sleep(.25)
    process_record['state']='healthy';process_path.write_text(json.dumps(process_record,indent=2))
    yield ENDPOINT
   finally:
    if _HTTP_CONN is not None:
     try:_HTTP_CONN.close()
     except Exception:pass
     _HTTP_CONN=None
    terminate(process)
    process_record.update(state='terminated',returncode=process.returncode)
    process_path.write_text(json.dumps(process_record,indent=2))
