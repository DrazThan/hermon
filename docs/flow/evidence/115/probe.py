import json, os, pathlib, re, shlex, signal, subprocess, sys, time
ROOT = pathlib.Path('/tmp/hermon-115')
ROOT.mkdir(exist_ok=True)
TASK = ROOT / 'task.md'
TASK.write_text('This is a CLI contract probe, not a work assignment. Do not use tools, skills, delegation, commands, or edit files. Remember the nonce FLOW115_7C82 and reply with exactly FLOW115_OK.\n')
FOLLOW = ROOT / 'followup.md'
FOLLOW.write_text('CLI contract probe continued. Do not use any tools. Reply with only the nonce from my previous message.\n')
agent, case = sys.argv[1:3]
label = agent + '-' + case
sid = sys.argv[3] if len(sys.argv)>3 else None
if agent == 'hermes':
    argv = ['hermes', '-p', 'default', 'chat', '-Q', '--query-file', str(FOLLOW if sid else TASK)]
    if sid: argv += ['--resume', sid]
    stdin_file = None
elif agent == 'codex':
    argv = ['codex', 'exec', '--ignore-user-config', '--ignore-rules', '--skip-git-repo-check', '--json', '-c', 'approval_policy="never"']
    if sid: argv += ['resume', sid]
    else: argv += ['--sandbox', 'read-only']
    argv += ['-']
    stdin_file = FOLLOW if sid else TASK
elif agent == 'claude':
    argv = ['claude', '-p', '--output-format', 'json', '--safe-mode', '--tools', '', '--strict-mcp-config', '--permission-mode', 'dontAsk', '--permission-prompts', 'none']
    if sid: argv += ['--resume', sid]
    stdin_file = FOLLOW if sid else TASK
else: raise SystemExit('unknown agent')
if case == 'model-error':
    argv += ['--model', 'hermon-contract-model-does-not-exist-115']
if case == 'stream':
    if agent == 'hermes': argv += ['--format', 'stream-json']
    if agent == 'claude': argv[argv.index('json')] = 'stream-json'; argv += ['--verbose']
if case in ('interrupt', 'timeout'):
    # Ask for a long response so the process remains live at the signal deadline.
    long_task = ROOT / 'long-task.md'
    long_task.write_text('CLI contract signal probe. Do not use tools or edit files. Print the integers 1 through 2000 inclusive, separated by spaces. Begin immediately, without explanation.\n')
    if agent == 'hermes': argv[argv.index('--query-file')+1] = str(long_task)
    else: stdin_file = long_task
out_path, err_path = ROOT/(label+'.stdout'), ROOT/(label+'.stderr')
started=time.monotonic()
with out_path.open('wb') as out, err_path.open('wb') as err:
    with stdin_file.open('rb') if stdin_file else open(os.devnull,'rb') as inp:
        proc = subprocess.Popen(argv, cwd=ROOT, stdin=inp, stdout=out, stderr=err, start_new_session=True)
        action=None
        try:
            proc.wait(timeout=5 if case in ('interrupt','timeout') else 90)
        except subprocess.TimeoutExpired:
            action='SIGINT' if case=='interrupt' else 'SIGTERM'
            os.killpg(proc.pid, getattr(signal, action))
            try: proc.wait(timeout=8)
            except subprocess.TimeoutExpired:
                action += ',SIGKILL'
                os.killpg(proc.pid,signal.SIGKILL); proc.wait(timeout=5)
metadata={'argv':argv,'cwd':str(ROOT),'stdin':str(stdin_file) if stdin_file else '/dev/null','returncode':proc.returncode,'shell_equivalent':128-proc.returncode if proc.returncode<0 else proc.returncode,'supervisor_action':action,'elapsed_seconds':round(time.monotonic()-started,3)}
(ROOT/(label+'.json')).write_text(json.dumps(metadata,indent=2)+'\n')
print(json.dumps(metadata))
print('STDOUT:\n'+out_path.read_text(errors='replace')[-12000:])
print('STDERR:\n'+err_path.read_text(errors='replace')[-6000:])
