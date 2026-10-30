#!/usr/bin/env python3
"""Deterministic public-ACP fixture. Never contacts a vendor or executes tools."""
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import threading
import time

assert sys.argv[1] == '-p' and sys.argv[3] == 'acp', sys.argv
profile = sys.argv[2]
scenario = os.environ.get('ACP_SCENARIO', 'done')
model_id = os.environ.get('ACP_MODEL_ID', 'openai-codex:gpt-6.1-sol-900k')
version = os.environ.get('ACP_VERSION', '0.22.4')
usage_mode = os.environ.get('ACP_USAGE_MODE', 'valid')
log = Path(os.environ['ACP_LOG']) if 'ACP_LOG' in os.environ else Path(os.environ['ACP_FIXTURE_LOG']) / (str(os.getpid()) + '.jsonl')
log.parent.mkdir(parents=True, exist_ok=True)
heartbeat = log.with_suffix('.heartbeat')
session = os.environ.get('ACP_SESSION_ID', 'acp-' + profile + '-' + str(os.getpid()))
current = os.environ.get('ACP_CURRENT_SESSION_ID', 'hermes-' + profile + '-' + str(os.getpid()))
lock = threading.Lock()
stop = threading.Event()
prompt_id = None
prompt_text = ''

def record(event, **values):
    with lock, log.open('a') as out:
        out.write(json.dumps({'event': event, **values}) + '\n')

def send(value):
    with lock:
        print(json.dumps({'jsonrpc': '2.0', **value}), flush=True)

def reply(request, result):
    send({'id': request, 'result': result})

def session_result():
    return {'sessionId': session,
            'models': {'currentModelId': model_id, 'availableModels': [{'modelId': model_id, 'name': model_id}]},
            'modes': {'currentModeId': 'default', 'availableModes': [{'id': 'default', 'name': 'Default'}, {'id': 'accept_edits', 'name': 'Accept edits'}]},
            '_meta': {'hermes': {'sessionProvenance': {'acpSessionId': session, 'currentHermesSessionId': current, 'rootHermesSessionId': current}}}}

def write_usage():
    if usage_mode == 'preseeded':
        return
    db = sqlite3.connect(os.environ['ACP_STATE_DB'], timeout=10)
    db.execute('CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,parent_session_id TEXT,end_reason TEXT)')
    db.execute('CREATE TABLE IF NOT EXISTS session_model_usage(session_id TEXT,model TEXT,billing_provider TEXT,task TEXT,api_call_count INTEGER)')
    db.execute('INSERT OR IGNORE INTO sessions VALUES (?,NULL,NULL)', (current,))
    provider, model = model_id.split(':', 1) if ':' in model_id else ('unknown', model_id)
    rows = []
    if usage_mode not in ('none', 'aux', 'foreign', 'zero', 'subtask'):
        rows.append((current, model, provider, '', 1))
    if usage_mode == 'mixed':
        rows.append((current, 'opus', 'claude-subscription-directsdk-experimental', '', 1))
    if usage_mode == 'aux':
        rows.append((current, model, provider, 'title_generation', 1))
    if usage_mode == 'foreign':
        rows.append(('different-unrelated-session', model, provider, '', 1))
    if usage_mode == 'zero':
        rows.append((current, model, provider, '', 0))
    if usage_mode == 'subtask':
        rows.append((current, model, provider, 'subtask', 1))
    if usage_mode == 'compression':
        parent = current + '-parent'
        db.execute('INSERT OR REPLACE INTO sessions VALUES (?,NULL,?)', (parent, 'compression'))
        db.execute('UPDATE sessions SET parent_session_id=? WHERE id=?', (parent, current))
        rows.append((parent, model, provider, '', 2))
    db.executemany('INSERT INTO session_model_usage VALUES (?,?,?,?,?)', rows)
    db.commit()
    db.close()

def write_result():
    path = os.environ.get('ACP_RESULT_PATH')
    for line in prompt_text.splitlines():
        if line.startswith('FIXTURE_RESULT_PATH='):
            path = json.loads(line.split('=', 1)[1])
    if path is None:
        found = re.search(r'Write\s+(.+?/result\.json)\s+(?:using|with)', prompt_text)
        if found:
            path = found.group(1)
    if path:
        result = {'schema': 1, 'status': 'done', 'summary': 'ACP fixture completed', 'artifacts': [], 'block': None, 'verdict': None, 'questions': []}
        if scenario == 'question':
            result.update(status='block', block={'kind': 'owner_question', 'reason': 'Choose fixture behavior'}, questions=['Which behavior?'])
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        Path(path).write_text(json.dumps(result))
    write_usage()

def pulse():
    while not stop.wait(0.025):
        heartbeat.write_text(str(time.monotonic_ns()))

record('spawn', argv=sys.argv[1:], pid=os.getpid())
try:
    for raw in sys.stdin:
        message = json.loads(raw)
        record('received', message=message)
        method = message.get('method')
        request = message.get('id')
        params = message.get('params', {})
        if method == 'initialize':
            reply(request, {'protocolVersion': 1, 'agentInfo': {'name': 'hermes-agent', 'version': version}, 'agentCapabilities': {'loadSession': True}})
        elif method == 'session/new':
            record('new', cwd=params.get('cwd'), mcpServers=params.get('mcpServers'))
            reply(request, session_result())
        elif method == 'session/load':
            session = params['sessionId']
            if not os.environ.get('ACP_CURRENT_SESSION_ID'):
                current = 'hermes-' + session.removeprefix('acp-')
            record('load', sessionId=session)
            reply(request, session_result())
        elif method == 'session/set_mode':
            assert params['modeId'] == 'accept_edits'
            reply(request, {})
        elif method == 'session/prompt':
            assert params['sessionId'] == session
            prompt_id = request
            prompt_text = '\n'.join(c.get('text', '') for c in params.get('prompt', []))
            record('prompt', sessionId=session, currentSessionId=current, text=prompt_text)
            if scenario in ('hold', 'late_end', 'lose'):
                threading.Thread(target=pulse, daemon=True).start()
                if scenario == 'lose':
                    record('viewer_exit_without_terminal')
                    os._exit(0)
            elif scenario == 'permission':
                send({'id': 'dangerous-permission', 'method': 'session/request_permission', 'params': {'sessionId': session, 'toolCall': {'toolCallId': 'dangerous-tool', 'title': 'Delete unrelated files', 'kind': 'execute', 'rawInput': {'command': 'rm -rf /unrelated'}}, 'options': [{'optionId': 'allow_once', 'name': 'Allow once', 'kind': 'allow_once'}, {'optionId': 'reject_once', 'name': 'Reject once', 'kind': 'reject_once'}]}})
            else:
                write_result()
                reply(request, {'stopReason': 'end_turn'})
        elif method == 'session/cancel':
            assert params['sessionId'] == session
            record('cancel', sessionId=session)
            stop.set()
            write_usage()
            if prompt_id is not None:
                reply(prompt_id, {'stopReason': 'end_turn' if scenario == 'late_end' else 'cancelled'})
                prompt_id = None
        elif method is None and request == 'dangerous-permission':
            record('permission_response', response=message)
            write_result()
            reply(prompt_id, {'stopReason': 'end_turn'})
        elif request is not None:
            send({'id': request, 'error': {'code': -32601, 'message': 'unsupported fixture method'}})
finally:
    stop.set()
