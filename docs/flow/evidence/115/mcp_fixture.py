import json,sys
for line in sys.stdin:
 try:
  r=json.loads(line)
  if 'id' not in r:continue
  method=r.get('method')
  if method=='initialize':v={'protocolVersion':r['params']['protocolVersion'],'capabilities':{'tools':{}},'serverInfo':{'name':'flow115','version':'1.0'}}
  elif method=='tools/list':v={'tools':[{'name':'probe','description':'Read-only contract probe; returns a fixed nonce.','inputSchema':{'type':'object','properties':{},'additionalProperties':False},'annotations':{'readOnlyHint':True,'destructiveHint':False,'openWorldHint':False}}]}
  elif method=='tools/call':
   if r.get('params',{}).get('name')!='probe':raise ValueError('unknown tool')
   v={'content':[{'type':'text','text':'FLOW115_MCP_9A64'}],'isError':False}
  elif method=='ping':v={}
  else:
   print(json.dumps({'jsonrpc':'2.0','id':r['id'],'error':{'code':-32601,'message':'Method not found'}}),flush=True);continue
  print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':v}),flush=True)
 except Exception as e:
  print(str(e),file=sys.stderr,flush=True)
