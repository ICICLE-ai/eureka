#!/usr/bin/env python3
"""Summarize live or finished harness JSONL traces without changing them."""
import argparse
from collections import Counter
import json
from pathlib import Path
import time


def summarize(path):
    attempts = {}
    malformed = 0
    retries = Counter()
    tools = Counter()
    for line in path.open(encoding="utf-8", errors="replace"):
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            malformed += 1  # A live writer can leave an incomplete last line.
            continue
        name = event.get('attempt', '?')
        if name == 'harness':
            continue
        row = attempts.setdefault(name, dict(id=name, label=name, status='unfinished', turns=0,
                                             input=0, cached=0, output=0, errors=0, incomplete=0))
        kind = event.get('event')
        row['last_ms'] = event.get('ts_ms', 0)
        if kind == 'attempt_start':
            row['label'] = event.get('label', name)
            row['artifacts'] = event.get('artifacts')
        elif kind in ('model_start', 'tool_start'):
            row['active'] = event.get('name', 'model')
        elif kind == 'model':
            row['active'] = None
            row['turns'] = event.get('turn', row['turns'] + 1)
            row['incomplete'] += bool(event.get('incomplete'))
            usage = event.get('usage', {})
            for column, field in [('input', 'input_tokens'), ('cached', 'cached_input_tokens'), ('output', 'output_tokens')]:
                row[column] += usage.get(field, 0)
        elif kind == 'tool':
            row['active'] = None
            tools[event.get('name', '?')] += 1
            row['errors'] += bool(event.get('error') or str(event.get('output', '')).startswith('ERROR:'))
        elif kind == 'request_retry':
            retries[event.get('reason', '?')[:160]] += 1
        elif kind == 'attempt_end':
            row['status'] = event.get('status', event.get('verdict', 'finished'))
            row['active'] = None
            row['candidate'] = event.get('candidate')
            row['note'] = event.get('note')
            row['termination'] = event.get('termination')
            row['detail'] = event.get('detail')
    now = time.time() * 1000
    for row in attempts.values():
        row['seconds_since_event'] = round(max(0, now - row.pop('last_ms')) / 1000)
    return dict(trace=str(path), attempts=list(attempts.values()), retries=dict(retries),
                tools=dict(tools), malformed_lines=malformed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('traces', type=Path, nargs='+')
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args()
    reports = [summarize(path) for path in args.traces]
    if args.json:
        print(json.dumps(reports, indent=2))
        return
    for report in reports:
        print(report['trace'])
        for row in report['attempts']:
            active = f"; waiting on {row['active']}" if row.get('active') else ''
            print(f"  {row['id']} ({row['label']}): {row['status']}; turns={row['turns']}; "
                  f"tokens={row['input']}/{row['cached']}/{row['output']} in/cached/out; "
                  f"tool errors={row['errors']}; incomplete={row['incomplete']}{active}; "
                  f"last event {row['seconds_since_event']}s ago")
            if row.get('candidate'):
                print(f"    candidate: {row['candidate']}")
        print(f"  retries: {sum(report['retries'].values())}; malformed/partial lines: {report['malformed_lines']}")


if __name__ == '__main__':
    main()
