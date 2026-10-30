#!/usr/bin/env python3
"""Ask for a patch permission; never execute the patch or alter its targets."""
import json
import os
from pathlib import Path

patch_arguments = json.loads(os.environ['ACP_PATCH_ARGUMENTS'])
display_path = os.environ['ACP_DISPLAY_PATH']
original = Path(__file__).with_name('hermes-acp.py')
source = original.read_text()
anchor = "'rawInput': {'command': 'rm -rf /unrelated'}"
assert source.count(anchor) == 1
source = source.replace(anchor, "'rawInput': {'tool': 'patch', 'arguments': patch_arguments}")
anchor = "'title': 'Delete unrelated files', 'kind': 'execute'"
assert source.count(anchor) == 1
source = source.replace(anchor, "'title': 'Update the safe assigned artifact', 'kind': 'edit', 'content': [{'type': 'diff', 'path': display_path, 'oldText': 'old', 'newText': 'new'}]")
anchor = "'params': {'sessionId': session, 'toolCall':"
assert source.count(anchor) == 1
source = source.replace(anchor, "'params': {'sessionId': os.environ.get('ACP_PERMISSION_SESSION', session), 'toolCall':")
anchor = "'options': [{'optionId': 'allow_once'"
assert source.count(anchor) == 1
source = source.replace(anchor, "'options': [{'optionId': 'allow_always', 'name': 'Always allow', 'kind': 'allow_always'}, {'optionId': 'allow_once'")
exec(compile(source, str(original), 'exec'))
