#!/usr/bin/env python3
"""Reuse the frozen ACP fake with independent numeric IDs in each RPC direction."""
from pathlib import Path

original = Path(__file__).with_name('hermes-acp.py')
source = original.read_text()
assert source.count("'id': 'dangerous-permission'") == 1
assert source.count("request == 'dangerous-permission'") == 1
source = source.replace("'id': 'dangerous-permission'", "'id': prompt_id")
source = source.replace("request == 'dangerous-permission'", "request == prompt_id")
exec(compile(source, str(original), 'exec'))
