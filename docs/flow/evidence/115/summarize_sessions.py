import asyncio,hashlib,json,os,pathlib,sys
sys.path.insert(0,'/Users/taloz/.hermes/hermes-agent')
os.environ['HERMES_HOME']='/Users/taloz/.hermes'
from hermes_cli.gateway_client import connect_gateway
ids=['local-dddff2885ca92d131b1c1937463d6b22b26712206512dd89ea56178019ef636c','local-cbe9c2b8850d8fe5d86a106adfad9251577390b9ae97bf08b9bb49fd4d7941a6','local-63091769f2dd01123262cc5be660291f5a2d1e347b23941aeebb4ad959206248','local-e5b06343dd8d158f6f9b3f9bb5c8d5011f87c98bff12cd07f008e84447173996','local-4ee90923d7bbc41d47da5766557cf05526d571ad270a0dff54e86cf84f4d26e8']
async def main():
 out=[]
 async with connect_gateway() as c:
  for sid in ids:
   s=await c.rpc('session.resume',session_id=sid,_timeout=5);i=s.get('info')or{}
   r={k:s.get(k)for k in ['session_id','running','execution_generation']};r['profile']=i.get('profile_name');r['cwd']=i.get('cwd');r['mcp_servers']=[{k:x.get(k)for k in ['name','connected','tools']}for x in i.get('mcp_servers',[])];r['tool_evidence']=[]; calls={}
   for m in s.get('messages',[]):
    for t in m.get('tool_calls')or[]:
     f=t.get('function')or{};name=f.get('name');calls[t.get('id')]=name
     if name:
      r['tool_evidence'].append({'kind':'call','name':name,'arguments':f.get('arguments')})
    if m.get('role')=='tool':
     name=calls.get(m.get('tool_call_id'));content=m.get('content')or''
     if not name:continue
     try:v=json.loads(content)
     except Exception:v={}
     e={'kind':'result','name':name,'content_sha256':hashlib.sha256(content.encode()).hexdigest(),'top_level_keys':list(v)if isinstance(v,dict)else[]}
     if isinstance(v,dict):
      for k in ['success','isError','is_error','has_more']:
       if k in v:e[k]=v[k]
      if name=='skill_view':e['skill_name']=v.get('name');e['readiness_status']=v.get('readiness_status');e['first_heading']=next((x for x in v.get('content','').splitlines()if x.startswith('# ')),None)
      for k in ['results','data','content']:
       if isinstance(v.get(k),list):e[k+'_count']=len(v[k])
     r['tool_evidence'].append(e)
   out.append(r)
 pathlib.Path('/tmp/hermon-115/hermes-session-evidence.json').write_text(json.dumps(out,indent=2)+'\n');print(json.dumps(out,indent=2))
asyncio.run(main())
