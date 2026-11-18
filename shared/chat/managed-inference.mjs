import {spawn} from 'node:child_process';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
const packagedRoot=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..');
const root=path.resolve(process.env.LOCAL_AGENTS_ROOT || packagedRoot);
export function managedInference(body, signal, spawnChild=spawn) {
  return new Promise((resolve,reject)=>{
    const child=spawnChild(`${root}/shared/runtime/venv/bin/python`,[`${root}/shared/harness/chat_inference.py`],{
      stdio:['pipe','pipe','pipe'],
      env: {
        ...process.env,
        LOCAL_AGENTS_LOW_MEMORY: '1',
        LOCAL_AGENTS_TOLERATE_WARNING_GIB: '2.0',
        LOCAL_AGENTS_CONTEXT_SIZE: process.env.LOCAL_AGENTS_CONTEXT_SIZE || process.env.SYNDICATE_CONTEXT_WINDOW || '16384',
        LOCAL_AGENTS_MAX_SWAP_GROWTH_GIB: process.env.LOCAL_AGENTS_MAX_SWAP_GROWTH_GIB || '3.0',
      }
    });
    let output='',errorText='',stopped=false,killTimer;
    const cancel=()=>{stopped=true;child.kill('SIGTERM');killTimer=setTimeout(()=>child.kill('SIGKILL'),20000);killTimer.unref();};
    signal?.addEventListener('abort',cancel,{once:true});
    if(signal?.aborted)cancel();
    child.stdout.on('data',data=>{output+=data;if(output.length>48*1024*1024)cancel();});
    child.stderr.on('data',data=>{errorText=(errorText+data).slice(-8192);});
    child.stdin.on('error',()=>{});
    child.on('error',reject);
    child.on('close',code=>{
      clearTimeout(killTimer);signal?.removeEventListener('abort',cancel);
      if(stopped)return reject(new Error('Local inference cancelled'));
      if(code!==0)return reject(new Error(`Local inference exited ${code}: ${errorText}`));
      try{const value=JSON.parse(output);resolve({status:value.status,contentType:value.content_type,text:Buffer.from(value.body,'base64').toString('utf8')});}catch(error){reject(error);}
    });
    child.stdin.end(JSON.stringify(body));
  });
}
