#!/usr/bin/env python3
"""Profile an isolated foreground Nginx; never changes the deployed server.

The cold field records the first request. Use a fresh prefix and empty cache
to interpret it as a cold-cache measurement.

Requires an existing config with daemon off, master_process off, one worker,
ShowCacheTimestamp on, and an unprivileged listen port. Use the pipeline-profile
Cargo feature for stage timings, strace for syscalls, or Callgrind for CPU costs.
"""
import argparse
import http.client
import json
import os
from pathlib import Path
import re
import signal
import statistics
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--nginx', required=True)
parser.add_argument('--prefix', required=True)
parser.add_argument('--config', default='conf/nginx.conf')
parser.add_argument('--port', type=int, required=True)
parser.add_argument('--host', default='localhost')
parser.add_argument('--path', required=True)
parser.add_argument('--samples', type=int, default=50)
parser.add_argument('--mode', choices=['plain', 'strace', 'callgrind'], default='plain')
parser.add_argument('--output', required=True)
parser.add_argument('--error-log', default='logs/error.log')
parser.add_argument('--cold-only', action='store_true')
parser.add_argument('--body-output', help='Optional warm-HIT body for output comparison')
args = parser.parse_args()
prefix = Path(args.prefix).resolve()
log = prefix / (args.mode + '-runner.log')
error_log = prefix / args.error_log
error_offset = error_log.stat().st_size if error_log.exists() else 0
command = [args.nginx, '-p', str(prefix) + '/', '-c', args.config]
if args.mode == 'strace':
    command = ['strace', '-f', '-tt', '-T', '-o', str(prefix / 'strace.log')] + command
elif args.mode == 'callgrind':
    command = ['valgrind', '--tool=callgrind', '--callgrind-out-file=' + str(prefix / 'callgrind.out')] + command
records = []
last_body = b''

def get(suffix='', unique_header=False):
    global last_body
    headers = {'Host': args.host}
    if unique_header:
        # Vary only the whole-HTML identity; keep resource URLs/options stable.
        headers['X-PS-Benchmark'] = str(time.time_ns())
    connection = http.client.HTTPConnection('127.0.0.1', args.port, timeout=60)
    started = time.perf_counter_ns()
    connection.request('GET', args.path + suffix, headers=headers)
    response = connection.getresponse()
    received_headers = time.perf_counter_ns()
    body = response.read()
    last_body = body
    finished = time.perf_counter_ns()
    connection.close()
    if response.status != 200:
        raise RuntimeError(f'Unexpected HTTP status {response.status}')
    marker = 'absent'
    if b'PageSpeed Cache:' in body:
        marker = body.rsplit(b'PageSpeed Cache:', 1)[-1].split(b'|', 1)[0].strip().decode()
    sample = {'ttfb_ms': (received_headers - started) / 1e6,
              'total_ms': (finished - started) / 1e6, 'marker': marker}
    if not suffix:
        sample['pipeline_index'] = len(records)
        records.append(sample)
    return sample

with log.open('w') as log_file:
    process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=log_file,
                               start_new_session=True)
    try:
        for _ in range(300):
            if process.poll() is not None:
                raise RuntimeError(f'Nginx exited; inspect {log}')
            try:
                cold = get()
                break
            except (ConnectionRefusedError, ConnectionResetError):
                time.sleep(.1)
        else:
            raise RuntimeError('Nginx did not start in 30 seconds')
        results = {'cold': cold}
        if not args.cold_only:
            # Warm asynchronously completed rewrites before measuring either
            # warm path. Require consecutive actual HITs rather than assuming.
            deadline = time.monotonic() + 30
            hits = 0
            while hits < 3 and time.monotonic() < deadline:
                sample = get()
                hits = hits + 1 if sample['marker'] == 'HIT' else 0
                time.sleep(.02)
            if hits < 3:
                raise RuntimeError('HTML did not reach stable HITs within 30s')
            for name, suffix, unique in [('warm_html_hit', '', False),
                                         ('warm_html_miss', '', True),
                                         ('pagespeed_off', '?PageSpeed=off', False)]:
                for _ in range(3):
                    get(suffix, unique)
                samples = [get(suffix, unique) for _ in range(args.samples)]
                if name == 'warm_html_hit' and args.body_output:
                    Path(args.body_output).write_bytes(last_body)
                results[name] = {
                    'median_ms': statistics.median(s['total_ms'] for s in samples),
                    'ttfb_median_ms': statistics.median(s['ttfb_ms'] for s in samples),
                    'markers': sorted(set(s['marker'] for s in samples)), 'samples': samples}
    finally:
        # Signal our foreground child only, allowing profiler parents to flush.
        if process.poll() is None:
            # Configurations must use a pid file beneath the isolated prefix.
            pid_file = prefix / 'nginx.pid'
            if pid_file.exists():
                try:
                    os.kill(int(pid_file.read_text()), signal.SIGQUIT)
                except ProcessLookupError:
                    pass
            else:
                os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=5)

pattern = re.compile(r'PS_PIPELINE_NS init=(\d+) lookup=(\d+) processing=(\d+) store=(\d+) hit=(true|false)')
logs = log.read_text()
if error_log.exists():
    logs += error_log.read_text()[error_offset:]
stages = [tuple(int(value) for value in match.groups()[:4])
          for match in pattern.finditer(logs)]
if stages:
    if len(stages) != len(records):
        raise RuntimeError(f'Cannot align stage timings: {len(stages)} rows for {len(records)} requests')
    for sample in records:
        sample['stages_us'] = dict(zip(['filter_init', 'html_cache_lookup', 'html_processing', 'html_cache_store'],
                                      (value / 1000 for value in stages[sample['pipeline_index']])))
    for name, result in results.items():
        if name.startswith('warm_'):
            result['stages_median_us'] = {
                key: statistics.median(s['stages_us'][key] for s in result['samples'])
                for key in records[0]['stages_us']}
fetches = [int(value) for value in re.findall(r'PS_RESOURCE_FETCH_NS elapsed=(\d+)', logs)]
results['resource_fetches'] = {'count': len(fetches), 'total_ms': sum(fetches) / 1e6,
    'background_count': len(re.findall(r'PS_RESOURCE_FETCH_NS elapsed=\d+ background=true', logs)),
    'foreground_count': len(re.findall(r'PS_RESOURCE_FETCH_NS elapsed=\d+ background=false', logs))}
Path(args.output).write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps({name: {key: value for key, value in result.items() if key != 'samples'}
                  for name, result in results.items()}, indent=2))
