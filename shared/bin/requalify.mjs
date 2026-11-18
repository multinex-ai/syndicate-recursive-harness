import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawn} from 'node:child_process';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..'),out=path.join(root,'evidence/requalification-20261103'),python=path.join(root,'shared/runtime/venv/bin/python');
const commands=[
 ['infrastructure',['-m','unittest','discover','-s','shared/harness','-p','test_*.py']],
 ['cyber',['shared/harness/acceptance.py','cyber']],['browser',['shared/harness/acceptance.py','browser']],
 ['vision',['shared/harness/acceptance_extended.py','vision']],['delegation',['shared/harness/acceptance_extended.py','communication']],
 ['cancellation',['shared/harness/acceptance_extended.py','lifecycle']],['memory',['shared/harness/acceptance_memory.py']],
 ['desktop',['shared/harness/acceptance_desktop.py']],['image-routing',['shared/harness/acceptance_media_routing.py']],
 ['mcp-protocol',['shared/bin/check_mcp.py']]
];
const selected=process.argv.slice(2);
if(selected.some(name=>!commands.some(([key])=>key===name)))throw new Error('Unknown qualification step');
const state=selected.length?JSON.parse(fs.readFileSync(path.join(out,'suite.json'),'utf8')):{started:new Date().toISOString(),state:'running',steps:[]};
if(selected.length&&state.state==='running')throw new Error('Wait for the active suite before rerunning a step');
state.state='running';
const save=()=>fs.writeFileSync(path.join(out,'suite.json'),JSON.stringify(state,null,2));
for(const [name,args] of commands){
 if(selected.length&&!selected.includes(name))continue;
 const prior=state.steps.find(s=>s.name===name);
 if(prior){
  state.history??=[];state.history.push({...prior});state.steps=state.steps.filter(s=>s.name!==name);
  const log=path.join(out,name+'.log');if(fs.existsSync(log))fs.copyFileSync(log,path.join(out,`${name}.attempt-${state.history.filter(s=>s.name===name).length}.log`));
 }
 const row={name,state:'running',started:new Date().toISOString()};state.steps.push(row);save();console.log('Starting '+name);
 const fd=fs.openSync(path.join(out,name+'.log'),'w');
 const exitCode=await new Promise((resolve,reject)=>{const child=spawn(python,args,{cwd:root,env:{...process.env,PYTHONPATH:path.join(root,'shared/harness')},stdio:['ignore',fd,fd]});child.on('error',reject);child.on('exit',resolve)});
 fs.closeSync(fd);Object.assign(row,{state:exitCode===0?'passed':'failed',exitCode,finished:new Date().toISOString()});save();console.log(name+': '+row.state);
}
state.state=commands.every(([name])=>state.steps.some(s=>s.name===name&&s.state==='passed'))?'passed':'requires_repairs';state.finished=new Date().toISOString();save();console.log(JSON.stringify(state));
