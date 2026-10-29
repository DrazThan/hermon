import datetime,json,pathlib,subprocess,shutil
r=pathlib.Path('/Users/taloz/code/hermon/docs/flow/evidence/115')
commands=[['hermes','--version'],['codex','--version'],['claude','--version'],['uname','-srm'],['sw_vers','-productVersion'],['git','-C','/Users/taloz/.hermes/hermes-agent','rev-parse','HEAD'],['git','-C','/Users/taloz/.hermes/hermes-agent','status','--porcelain','--untracked-files=no']]
out={'recorded_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'executables':{name:{'path_on_PATH':shutil.which(name),'resolved_path':str(pathlib.Path(shutil.which(name)).resolve())}for name in ['hermes','codex','claude']},'commands':[]}
for argv in commands:
 p=subprocess.run(argv,capture_output=True,text=True,timeout=15)
 out['commands'].append({'argv':argv,'returncode':p.returncode,'stdout':p.stdout,'stderr':p.stderr})
(r/'provenance.json').write_text(json.dumps(out,indent=2)+'\n');print(json.dumps(out,indent=2))
