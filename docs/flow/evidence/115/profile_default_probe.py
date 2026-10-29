import json, pathlib, subprocess, signal, os, time
r=pathlib.Path('/tmp/hermon-115'); argv=['hermes','-p','default','chat','-Q','--query-file',str(r/'profile-task.md')]
t=time.monotonic(); action=None
with (r/'hermes-profile-default.stdout').open('wb') as out,(r/'hermes-profile-default.stderr').open('wb') as err:
 p=subprocess.Popen(argv,cwd=r,stdin=subprocess.DEVNULL,stdout=out,stderr=err,start_new_session=True)
 try:p.wait(timeout=90)
 except subprocess.TimeoutExpired:
  action='SIGTERM';os.killpg(p.pid,signal.SIGTERM)
  try:p.wait(timeout=8)
  except subprocess.TimeoutExpired:action+=',SIGKILL';os.killpg(p.pid,signal.SIGKILL);p.wait()
m={'argv':argv,'cwd':str(r),'stdin':'/dev/null','returncode':p.returncode,'supervisor_action':action,'elapsed_seconds':round(time.monotonic()-t,3)}
(r/'hermes-profile-default.json').write_text(json.dumps(m,indent=2)+'\n'); print(json.dumps(m)); print((r/'hermes-profile-default.stdout').read_text()); print((r/'hermes-profile-default.stderr').read_text())
