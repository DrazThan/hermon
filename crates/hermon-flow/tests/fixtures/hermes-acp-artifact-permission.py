#!/usr/bin/env python3
"""Ask for one named artifact write without touching the requested path."""
import os
from pathlib import Path

permission_path = os.environ['ACP_PERMISSION_PATH']
original = Path(__file__).with_name('hermes-acp.py')
source = original.read_text()
old = "'rawInput': {'command': 'rm -rf /unrelated'}"
assert source.count(old) == 1
source = source.replace(old, "'rawInput': {'tool': 'write_file', 'arguments': {'path': permission_path, 'content': 'fixture artifact'}}")
source = source.replace("'title': 'Delete unrelated files', 'kind': 'execute'", "'title': 'Write assigned artifact', 'kind': 'edit'")
old_options = "'options': [{'optionId': 'allow_once'"
assert source.count(old_options) == 1
source = source.replace(old_options, "'options': [{'optionId': 'allow_always', 'name': 'Always allow', 'kind': 'allow_always'}, {'optionId': 'allow_once'")
exec(compile(source, str(original), 'exec'))
