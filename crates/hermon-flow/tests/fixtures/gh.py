#!/usr/bin/python3
"""Synthetic gh peer; no network or real GitHub credentials."""
import json
import os
import pathlib
import sys
import subprocess

root = pathlib.Path(os.environ['GH_FIXTURE'])
args = sys.argv[1:]
with (root / 'calls.jsonl').open('a') as f:
    f.write(json.dumps(args) + '\n')

def option(name):
    return args[args.index(name) + 1]

def read(name, default):
    p = root / name
    return json.loads(p.read_text()) if p.exists() else default

if args[:2] == ['pr', 'list']:
    assert option('--repo') == 'fixture/project'
    assert option('--base') == 'qa'
    assert option('--head') == 'flow/run-122'
    assert option('--state') == 'all'
    if (root / 'list-fail').exists():
        sys.exit(1)
    print(json.dumps(read('prs.json', [])))
elif args[:2] == ['pr', 'create']:
    assert option('--repo') == 'fixture/project'
    assert option('--base') == 'qa'
    assert option('--head') == 'flow/run-122'
    subprocess.run(['/usr/bin/git', '--git-dir=' + os.environ['GH_REMOTE'], 'rev-parse', '--verify', 'refs/heads/flow/run-122'], check=True, stdout=subprocess.DEVNULL)
    body = pathlib.Path(option('--body-file')).read_text()
    (root / 'created-body.txt').write_text(body)
    pr = {'number': 42, 'url': 'https://github.com/fixture/project/pull/42',
          'state': 'OPEN', 'title': option('--title'), 'baseRefName': 'qa', 'headRefName': 'flow/run-122',
          'body': body, 'headRepositoryOwner': {'login': 'fixture'}}
    (root / 'prs.json').write_text(json.dumps([pr]))
    if (root / 'drop-create-response').exists():
        sys.exit(1)
    print(pr['url'])
elif args[:2] == ['pr', 'view']:
    assert option('--repo') == 'fixture/project'
    print(json.dumps(read('view.json', {
        'number': 42, 'url': 'https://github.com/fixture/project/pull/42',
        'state': 'OPEN', 'mergeCommit': None,
        'baseRefName': 'qa', 'headRefName': 'flow/run-122',
        'headRepositoryOwner': {'login': 'fixture'},
        'body': read('prs.json', [{}])[0].get('body', ''),
    })))
else:
    raise AssertionError('unexpected gh operation: ' + repr(args))
