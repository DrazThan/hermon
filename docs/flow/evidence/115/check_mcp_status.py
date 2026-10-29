exec(open('/tmp/hermon-115/cleanup.py').read().split('async def main():')[0])
os.environ['HERMES_HOME']='/Users/taloz/.hermes'
async def check():
 async with connect_gateway() as c:
  s=await c.rpc('session.resume',session_id='local-4ee90923d7bbc41d47da5766557cf05526d571ad270a0dff54e86cf84f4d26e8',_timeout=5)
  ids={t['id']:t['function']['name']for m in s['messages']for t in m.get('tool_calls')or[]}
  for m in s['messages']:
   if m.get('role')=='tool'and ids.get(m.get('tool_call_id'))=='tool_call':
    raw=m['content']; raw=raw.split('\n\n',1)[1].rsplit('\n</untrusted_tool_result>',1)[0] if raw.startswith('<untrusted_tool_result') else raw; v=json.loads(raw)
    def shape(x):
     if isinstance(x,dict):return {k:shape(v)for k,v in x.items()}
     if isinstance(x,list):return [shape(v)for v in x[:1]]
     if isinstance(x,str):
      try:return shape(json.loads(x))
      except (ValueError,TypeError):return '<text>'
     return x
    import collections,re; value=v.get('result',''); out={'decoded_response_shape':shape(v),'outer_keys':list(v),'result_text_length':len(value),'xml_tag_counts':dict(collections.Counter(re.findall(r'</?([A-Za-z_][\w:-]*)\b',value))),'starts_with_error':value.lstrip().lower().startswith(('error:', 'error ', '{"error"'))};__import__('pathlib').Path('/tmp/hermon-115/hermes-mcp-result-shape.json').write_text(json.dumps(out,indent=2)+'\n');print(json.dumps(out))
asyncio.run(check())
