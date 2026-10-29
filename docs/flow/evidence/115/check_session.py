exec(open('/tmp/hermon-115/cleanup.py').read().split('async def main():')[0])
async def check():
 async with connect_gateway() as c:
  s=await c.rpc('session.resume',session_id=SID,_timeout=5)
  print(json.dumps({k:s.get(k) for k in ['session_id','execution_generation','running','revision']})); print('info_keys',list((s.get('info') or {}).keys())); print('message_roles',[m.get('role') for m in s.get('messages',[])]); print('pending_count',len(s.get('pending',[])))
asyncio.run(check())
