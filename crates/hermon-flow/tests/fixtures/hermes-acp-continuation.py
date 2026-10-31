#!/usr/bin/env python3
"""Derive finite same-session interim turns from the frozen public ACP fake."""
import os
import time
from pathlib import Path

interim_turns = int(os.environ.get('ACP_INTERIM_TURNS', '1'))
prompt_turns = 0
original = Path(__file__).with_name('hermes-acp.py')
source = original.read_text()
anchor = "            record('prompt', sessionId=session, currentSessionId=current, text=prompt_text)\n"
assert source.count(anchor) == 1
source = source.replace(anchor, """            prompt_turns += 1
            if prompt_turns == 1:
                for line in prompt_text.splitlines():
                    if line.startswith('FIXTURE_RESULT_PATH='):
                        os.environ['ACP_RESULT_PATH'] = json.loads(line.split('=', 1)[1])
            if os.environ.get('ACP_HOLD_FOLLOWUP') == 'yes' and prompt_turns > 1:
                scenario = 'hold'
""" + anchor)
anchor = 'def write_result():\n'
assert source.count(anchor) == 1
source = source.replace(anchor, """def write_result():
    if prompt_turns <= interim_turns:
        if prompt_turns == 1:
            time.sleep(float(os.environ.get('ACP_INTERIM_DELAY_SECONDS', '0')))
        send({'method': 'session/update', 'params': {'sessionId': session, 'update': {'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'Fixture background check is still running.'}}}})
        write_usage()
        return
""")
anchor = 'Path(path).write_text(json.dumps(result))'
assert source.count(anchor) == 1
source = source.replace(anchor, "Path(path).write_text('invalid fixture result JSON' if os.environ.get('ACP_INVALID_RESULT') == 'yes' else json.dumps(result))")
exec(compile(source, str(original), 'exec'))
