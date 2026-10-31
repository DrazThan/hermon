#!/usr/bin/env python3
"""Synthetic ACP admission failures; never calls a provider or writes a result."""
import os
from pathlib import Path
original = Path(__file__).with_name('hermes-acp.py')
source = original.read_text()
anchor = "            if scenario in ('hold', 'late_end', 'lose'):"
assert source.count(anchor) == 1
injection = """            case = os.environ.get('ACP_FAILURE_CASE', 'known')
            error = {'code': -32603, 'message': 'Internal error', 'data': {'details': 'admitted_turn_failed'}}
            if case == 'wrong-code': error['code'] = -32000
            if case in ('generic', 'wrong-detail'): error['data'] = {'details': 'gateway_disconnected'}
            if case in ('notification', 'request-collision', 'wrong-id'):
                frame = {'jsonrpc': '2.0', 'error': error}
                if case == 'notification': frame['method'] = 'session/update'
                elif case == 'request-collision': frame.update(id=request, method='session/request_permission', params={'sessionId':session, 'options':[]})
                else: frame['id'] = request + 50
                send(frame)
                error = {'code': -32603, 'message': 'Internal error'}
            send({'jsonrpc':'2.0', 'id':request, 'error':error})
            continue
"""
source = source.replace(anchor, injection + anchor)
exec(compile(source, str(original), 'exec'))
