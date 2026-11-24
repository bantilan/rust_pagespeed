#!/usr/bin/env python3
"""Profile a matched isolated single-worker HTML workload; no installed server changes."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--nginx', required=True)
parser.add_argument('--module', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--mode', choices=['plain', 'strace', 'callgrind'], default='plain')
parser.add_argument('--path', default='/test/aerosky-real-estate-html-responsive-website-template/index.html')
parser.add_argument('--shared-kb', type=int, default=16384)
parser.add_argument('--samples', type=int, default=30)
parser.add_argument('--port', type=int, default=18090)
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
prefix = Path(tempfile.mkdtemp(prefix='ps-shared-profile-'))
(prefix / 'conf').mkdir()
(prefix / 'logs').mkdir()
module = Path(args.module).resolve()
config = (repo / 'docs/performance/shared-memory/nginx-template.conf').read_text()
config = config.replace('@MODULE@', str(module)).replace('@PREFIX@', str(prefix)).replace('@PORT@', str(args.port))
if args.shared_kb:
    config = config.replace(f'pagespeed FileCachePath {prefix}/cache;', f'pagespeed FileCachePath {prefix}/cache;\npagespeed CreateSharedMemoryMetadataCache {prefix}/cache {args.shared_kb};')
(prefix / 'conf/nginx.conf').write_text(config)
command = ['python3', str(repo / 'tools/profile_html.py'), '--nginx', args.nginx,
           '--prefix', str(prefix), '--port', str(args.port), '--path', args.path,
           '--samples', str(args.samples), '--mode', args.mode, '--output', str(prefix / 'measurement.json')]
with (prefix / 'driver.log').open('w') as output:
    subprocess.run(command, check=True, stdout=output, stderr=subprocess.STDOUT)
result = json.loads((prefix / 'measurement.json').read_text())
instrumented = b'PS_PIPELINE_NS' in module.read_bytes()
if not instrumented:
    result['resource_fetches'] = None
result['pipeline_instrumentation_available'] = instrumented
result.update({'prefix': str(prefix), 'mode': args.mode, 'path': args.path, 'shared_kb': args.shared_kb,
               'module_sha256': hashlib.sha256(module.read_bytes()).hexdigest(),
               'config_sha256': hashlib.sha256(config.encode()).hexdigest(),
               'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
               'scope': 'isolated single worker; profilers perturb timings; residual includes Nginx, uninstrumented Rust, sockets, client and scheduler'})
for phase in ['cold', 'warm_html_hit', 'warm_html_miss']:
    samples = result[phase].get('samples', [result[phase]])
    for sample in samples:
        if 'stages_us' in sample:
            sample['outside_instrumented_stages_ms'] = sample['total_ms'] - sum(sample['stages_us'].values()) / 1000
if args.mode == 'strace':
    trace = prefix / 'strace.log'
    summary = {}
    # This is a workload summary, not the sum of request critical paths. Thread
    # sleeps/epoll intervals overlap and must not be called request latency.
    for line in trace.read_text(errors='replace').splitlines():
        match = re.search(r'\b([a-z_]+)\(.*<([0-9.]+)>$', line)
        if match:
            name, duration = match.groups()
            value = summary.setdefault(name, {'calls': 0, 'summed_thread_seconds': 0})
            value['calls'] += 1
            value['summed_thread_seconds'] += float(duration)
    result['syscalls_workload_scope'] = summary
    compressed = Path(args.output).with_suffix('.strace.gz')
    with trace.open('rb') as source, gzip.open(compressed, 'wb') as output:
        output.write(source.read())
    result['compressed_trace'] = str(compressed)
if args.mode == 'callgrind':
    raw = prefix / 'callgrind.out'
    annotated = Path(args.output).with_suffix('.callgrind.txt')
    with annotated.open('w') as output:
        subprocess.run(['callgrind_annotate', '--inclusive=yes', '--threshold=95', str(raw)], check=True, stdout=output, stderr=subprocess.STDOUT)
    result['callgrind_annotation'] = str(annotated)
    compressed = Path(args.output).with_suffix('.callgrind.gz')
    with raw.open('rb') as source, gzip.open(compressed, 'wb') as output:
        output.write(source.read())
    result['compressed_callgrind'] = str(compressed)
Path(args.output).write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({phase: {key: value for key, value in result[phase].items() if key != 'samples'} for phase in ['cold', 'warm_html_hit', 'warm_html_miss', 'pagespeed_off']}, indent=2))
