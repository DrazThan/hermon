import json,os,pathlib,signal,subprocess,sys,time
r=pathlib.Path('/tmp/hermon-115'); agent=sys.argv[1]
if agent=='codex':
 argv=['codex','exec','--ignore-user-config','--ignore-rules','--skip-git-repo-check','--json','--sandbox','read-only','-c','approval_policy="never"','-c','mcp_servers.flow115.command="python3"','-c','mcp_servers.flow115.args=["/tmp/hermon-115/mcp_fixture.py"]','-']
else:
 argv=['claude','-p','--output-format','stream-json','--verbose','--setting-sources','project','--tools','Read','--allowedTools','Read,mcp__flow115__probe','--strict-mcp-config','--mcp-config',str(r/'mcp-config.json'),'--permission-mode','dontAsk','--permission-prompts','none']
label=agent+'-config'; t=time.monotonic();action=None
with (r/('config-task-'+agent+'.md')).open('rb') as inp,(r/(label+'.stdout')).open('wb') as out,(r/(label+'.stderr')).open('wb') as err:
 p=subprocess.Popen(argv,cwd=r,stdin=inp,stdout=out,stderr=err,start_new_session=True)
 try:p.wait(timeout=90)
 except subprocess.TimeoutExpired:
  action='SIGTERM';os.killpg(p.pid,signal.SIGTERM)
  try:p.wait(timeout=8)
  except subprocess.TimeoutExpired:action+=',SIGKILL';os.killpg(p.pid,signal.SIGKILL);p.wait()
m={'argv':argv,'cwd':str(r),'stdin':str(r/('config-task-'+agent+'.md')),'returncode':p.returncode,'supervisor_action':action,'elapsed_seconds':round(time.monotonic()-t,3)}
(r/(label+'.json')).write_text(json.dumps(m,indent=2)+'\n');print(json.dumps(m));print((r/(label+'.stdout')).read_text());print((r/(label+'.stderr')).read_text())
