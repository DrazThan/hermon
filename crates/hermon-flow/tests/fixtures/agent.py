"""Synthetic agent process for Slice 4; no vendor CLI or credentials."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

mode, task_name, workdir, *extra = sys.argv[1:]
task = Path(task_name)
directory = task.parent
result = directory / "result.json"
done = {"schema": 1, "status": "done", "summary": "fixture finished", "artifacts": [],
        "block": None, "verdict": None, "questions": []}


def write_result(value=done):
    result.write_text(json.dumps(value))


print("fixture stdout", flush=True)
print("fixture stderr", file=sys.stderr, flush=True)
info = {"pid": os.getpid(), "pgrp": os.getpgrp(), "sid": os.getsid(0),
        "cwd": os.getcwd(), "task": str(task), "workdir": workdir, "extra": extra}
if mode == "stdin":
    info["stdin"] = sys.stdin.read()
if mode in ("timeout", "cancel"):
    heartbeat = directory / "heartbeat"
    child = subprocess.Popen([sys.executable, "-c", "import pathlib,signal,sys,time; "
                              "signal.signal(signal.SIGTERM,signal.SIG_IGN); "
                              "p=pathlib.Path(sys.argv[1]); "
                              "exec('while True:\\n p.write_text(str(time.time_ns()))\\n time.sleep(.03)')",
                              str(heartbeat)])
    info["descendant"] = child.pid

    def late_success(_signal, _frame):
        write_result()
        sys.exit(0)

    signal.signal(signal.SIGTERM, late_success)
(directory / "started.json").write_text(json.dumps(info))
with (directory / "launches").open("a") as output:
    output.write("launch\n")
if mode == "block":
    write_result({**done, "status": "block", "block": {"kind": "capability", "reason": "fixture lacks tool"}})
elif mode == "garbage":
    result.write_text("{not json")
elif mode == "missing":
    pass
elif mode in ("timeout", "cancel"):
    time.sleep(30)
elif mode == "sleep":
    time.sleep(.7)
    write_result()
elif mode == "codex":
    print(json.dumps({"type": "thread.started", "thread_id": "opaque-thread-119"}), flush=True)
    print(json.dumps({"type": "turn.completed", "usage": {}}), flush=True)
    write_result()
elif mode == "codex-no-completion":
    print(json.dumps({"type": "thread.started", "thread_id": "opaque-thread-119"}), flush=True)
    write_result()
elif mode == "claude-error":
    print(json.dumps({"type": "result", "subtype": "success", "session_id": "claude-119",
                      "is_error": True, "terminal_reason": "api_error"}), flush=True)
    write_result()
elif mode == "claude":
    print(json.dumps({"type": "result", "subtype": "success", "session_id": "claude-119",
                      "is_error": False, "terminal_reason": "completed"}), flush=True)
    write_result()
elif mode == "nonzero":
    write_result()
    sys.exit(7)
else:
    write_result()
