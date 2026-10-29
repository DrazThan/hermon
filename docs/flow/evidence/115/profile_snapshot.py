exec(open('/tmp/hermon-115/cleanup.py').read().split('async def main():')[0])
async def check():
 async with connect_gateway() as c:
  s=await c.rpc('session.resume',session_id=SID,_timeout=5)
  i=s.get('info') or {}
  out={k:s.get(k) for k in ['session_id','execution_generation','running','revision']}
  out['info']={k:i.get(k) for k in ['profile_name','cwd','skills','mcp_servers','enabled_toolsets']}
  out['tool_results']=[{k:m.get(k) for k in ['name','tool_call_id','content']} for m in s.get('messages',[]) if m.get('role')=='tool']
  pathlib=__import__('pathlib');pathlib.Path('/tmp/hermon-115/profile-snapshot.json').write_text(json.dumps(out,indent=2)+'\n')
  print(json.dumps(out))
asyncio.run(check())
