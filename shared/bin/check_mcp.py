"""Exercise the real stdio server with the official MCP client; no inference."""
import asyncio
import datetime
import json
from pathlib import Path
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

ROOT=Path(__file__).resolve().parents[2]

async def main():
    params=StdioServerParameters(command='/bin/bash',args=[str(ROOT/'shared/bin/run-mcp.sh')])
    async with stdio_client(params) as (read,write):
        async with ClientSession(read,write) as client:
            initialized=await client.initialize()
            tools=await client.list_tools()
            names={t.name for t in tools.tools}
            expected={'local_agents_status','start_local_agent','local_agent_job','cancel_local_agent','local_agent_log'}
            assert names==expected,names
            start_schema=next(t.inputSchema for t in tools.tools if t.name=='start_local_agent')
            assert 'media_preset' in json.dumps(start_schema)
            assert 'reference_image' in json.dumps(start_schema)
            status=await client.call_tool('local_agents_status',{})
            assert not status.isError,status
            parsed=json.loads(status.content[0].text)
            assert len(parsed['roles'])==6
            invalid=await client.call_tool('start_local_agent',{'request':{'role':'cyber-lab','task':'fixture','request_id':'bad','command':'should-not-execute'}})
            assert invalid.isError
            incompatible=await client.call_tool('start_local_agent',{'request':{
                'role':'video-studio','task':'fixture','request_id':'invalid-reference-fixture',
                'reference_image':'/tmp/fixture.png'}})
            assert incompatible.isError
            traversal=await client.call_tool('local_agent_log',{'job_id':'../../secrets'})
            assert traversal.isError
            result={'passed':True,'verified_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
                    'server':initialized.serverInfo.model_dump(),'protocol_version':initialized.protocolVersion,
                    'tools':sorted(names),'checks':['stdio initialization','five tool schemas','media preset and reference schema exposure','status call','strict invalid request rejection','video reference rejection before worker launch','job path rejection'],
                    'model_inference_tested':False,'application_ui_connection_tested':False}
            (ROOT/'evidence/setup-20261102/mcp-protocol-tests.json').write_text(json.dumps(result,indent=2))
            print(json.dumps(result,indent=2))

asyncio.run(main())
