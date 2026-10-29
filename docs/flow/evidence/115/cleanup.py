import asyncio,json,os,sys
sys.path.insert(0,'/Users/taloz/.hermes/hermes-agent')
os.environ['HERMES_HOME']='/Users/taloz/.hermes/profiles/sdlc-docs'
from hermes_cli.gateway_client import connect_gateway
SID='local-92c92592d1e610fe61ebdeebe71dd84526ec6d9c89666520b79b74c63b0497f4'
async def main():
 async with connect_gateway() as c:
  state=await c.rpc('session.resume',session_id=SID,_timeout=5)
  print('snapshot',json.dumps({k:state.get(k) for k in ['session_id','execution_generation','status','active_turn']}))
  result=await c.rpc('session.interrupt',session_id=SID,execution_generation=state['execution_generation'],_timeout=5)
  print('interrupt',json.dumps(result))
asyncio.run(main())
