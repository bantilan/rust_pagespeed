#!/usr/bin/env python3
"""Isolated worker/tier contention matrix. Never reload the installed NGINX."""
import argparse
import concurrent.futures
from datetime import datetime, timezone
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import random
import re
import signal
import statistics
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--nginx', required=True)
parser.add_argument('--module', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--port', type=int, default=18092)
parser.add_argument('--workers', type=int, nargs='+', default=[1, 2, 4])
parser.add_argument('--repeats', type=int, default=3)
parser.add_argument('--samples', type=int, default=100)
parser.add_argument('--clients', type=int, default=8)
parser.add_argument('--cold-requests', type=int, default=32)
parser.add_argument('--seed', type=int, default=20261124)
parser.add_argument('--shared-kb', type=int, default=16384)
args = parser.parse_args()
if min([*args.workers, args.repeats, args.samples, args.clients, args.cold_requests]) < 1:
    parser.error('counts must be positive')
if not 1024 <= args.shared_kb <= 262144:
    parser.error('shared-kb must be 1024..262144')
repo = Path(__file__).resolve().parent.parent
module = Path(args.module).resolve()
template = (repo / 'docs/performance/shared-memory/nginx-template.conf').read_text()
rng = random.Random(args.seed)
result = {'at': datetime.now(timezone.utc).isoformat(), 'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
          'module_sha256': hashlib.sha256(module.read_bytes()).hexdigest(),
          'nginx_version': subprocess.check_output([args.nginx, '-v'], stderr=subprocess.STDOUT, text=True).strip(),
          'arguments': vars(args), 'runs': [],
          'scope': 'Ordinary local client-observed latency; fresh application caches, warm OS caches. Fixed eight-client default; order randomized within worker/repetition pairs. Client scheduling limits throughput. Background counters are proxies, not resource fetch/encode counts.'}
expected = {}


def save():
    Path(args.output).write_text(json.dumps(result, indent=2) + '\n')


def normalize(body):
    return re.sub(rb'<!-- PageSpeed Cache:.*?-->', b'', body)


def request(path, unique=False):
    conn = http.client.HTTPConnection('127.0.0.1', args.port, timeout=30)
    headers = {'Host': 'localhost'}
    if unique:
        headers['X-PS-Benchmark'] = str(time.time_ns())
    started = time.perf_counter_ns()
    conn.request('GET', path, headers=headers)
    response = conn.getresponse()
    body = response.read()
    elapsed = (time.perf_counter_ns() - started) / 1e6
    status, worker = response.status, int(response.getheader('X-Worker', '0'))
    conn.close()
    assert status == 200 and worker, (status, worker)
    marker = 'absent'
    if b'PageSpeed Cache:' in body:
        marker = body.rsplit(b'PageSpeed Cache:', 1)[-1].split(b'|', 1)[0].strip().decode()
    return {'ms': elapsed, 'worker': worker, 'marker': marker}, body


def counters():
    sample, body = request('/pagespeed_admin/stats_json')
    variables = json.loads(body)['variables']
    return {'worker': sample['worker'], 'values': {k: v for k, v in variables.items()
            if k.startswith(('shared_cache_', 'background_jobs_', 'fetch_dedup'))}}


def burst(path, count, phase, digest=None):
    def one(_):
        sample, body = request(path, unique=phase == 'warm_miss')
        if phase == 'warm_hit':
            assert sample['marker'] == 'HIT', sample
        if phase == 'warm_miss':
            assert sample['marker'] != 'HIT', sample
        if digest:
            assert hashlib.sha256(normalize(body)).hexdigest() == digest, (phase, sample)
        return sample
    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.clients) as pool:
        samples = list(pool.map(one, range(count)))
    elapsed = time.perf_counter() - started
    values = sorted(s['ms'] for s in samples)
    return {'median_ms': statistics.median(values), 'p95_ms': values[math.ceil(.95 * len(values)) - 1],
            'elapsed_s': elapsed, 'requests_per_second': count / elapsed,
            'workers': sorted(set(s['worker'] for s in samples)), 'samples': samples}


for repetition in range(args.repeats):
    for workers in args.workers:
        tiers = [0, args.shared_kb]
        rng.shuffle(tiers)
        for shared_kb in tiers:
            prefix = Path(tempfile.mkdtemp(prefix='ps-worker-matrix-'))
            (prefix / 'conf').mkdir()
            (prefix / 'logs').mkdir()
            config = template.replace('@MODULE@', str(module)).replace('@PREFIX@', str(prefix)).replace('@PORT@', str(args.port))
            config = config.replace('master_process off;', 'master_process on;').replace('worker_processes 1;', f'worker_processes {workers};')
            config = config.replace('http {', 'http {\nadd_header X-Worker $pid always;')
            if shared_kb:
                config = config.replace(f'pagespeed FileCachePath {prefix}/cache;', f'pagespeed FileCachePath {prefix}/cache;\npagespeed CreateSharedMemoryMetadataCache {prefix}/cache {shared_kb};')
            (prefix / 'conf/nginx.conf').write_text(config)
            subprocess.run([args.nginx, '-t', '-p', str(prefix) + '/', '-c', 'conf/nginx.conf'], check=True, capture_output=True)
            run = {'repetition': repetition + 1, 'workers_configured': workers, 'shared_kb': shared_kb,
                   'prefix': str(prefix), 'config_sha256': hashlib.sha256(config.encode()).hexdigest(), 'pages': {}}
            result['runs'].append(run)
            with (prefix / 'runner.log').open('ab') as log:
                process = subprocess.Popen([args.nginx, '-p', str(prefix) + '/', '-c', 'conf/nginx.conf'], stdout=log, stderr=log, start_new_session=True)
                try:
                    deadline = time.monotonic() + 8
                    while True:
                        try:
                            counters()  # Readiness does not touch target HTML/resources.
                            break
                        except OSError:
                            assert time.monotonic() < deadline, 'startup timed out'
                            time.sleep(.05)
                    run['before'] = counters()
                    for name, slug in [('Moto', 'moto-business-html5-responsive-web-template'), ('Aroma', 'aroma-beauty-and-spa-responsive-bootstrap-template')]:
                        path = f'/test/{slug}/index.html'
                        page = {'cold_burst': burst(path, args.cold_requests, 'cold')}
                        run['pages'][name] = page
                        deadline = time.monotonic() + 30
                        hits = 0
                        while hits < 3:
                            sample, body = request(path)
                            hits = hits + 1 if sample['marker'] == 'HIT' else 0
                            assert time.monotonic() < deadline, 'HTML did not warm'
                            time.sleep(.02)
                        digest = hashlib.sha256(normalize(body)).hexdigest()
                        assert body.count(b'.pagespeed.') > 0
                        assert expected.setdefault(name, digest) == digest, (name, expected, digest)
                        page.update(normalized_html_sha256=digest, pagespeed_refs=body.count(b'.pagespeed.'))
                        page['warm_hit'] = burst(path, args.samples, 'warm_hit', digest)
                        page['warm_miss'] = burst(path, args.samples, 'warm_miss', digest)
                    run['after'] = counters()
                    values = run['after']['values']
                    if shared_kb:
                        assert values['shared_cache_domains'] == 1, values
                        assert 0 < values['shared_cache_byte_mapping_bytes'] <= shared_kb * 1024, values
                    # PSS apportions shared pages; RSS summed across workers double-counts them.
                    children = Path(f'/proc/{process.pid}/task/{process.pid}/children').read_text().split()
                    run['memory_kib'] = {}
                    for pid in children:
                        try:
                            text = Path(f'/proc/{pid}/smaps_rollup').read_text()
                            run['memory_kib'][pid] = {key: int(value) for key, value in re.findall(r'^(Rss|Pss|Private_Dirty):\s+(\d+) kB', text, re.M)}
                        except (OSError, PermissionError) as error:
                            run['memory_kib'][pid] = {'unavailable': str(error)}
                    run['passed'] = True
                    print(repetition + 1, workers, shared_kb, {p: {phase: round(data[phase]['median_ms'], 3) for phase in ['cold_burst', 'warm_hit', 'warm_miss']} for p, data in run['pages'].items()}, flush=True)
                finally:
                    try:
                        os.killpg(process.pid, signal.SIGTERM)
                        process.wait(timeout=8)
                    except (ProcessLookupError, subprocess.TimeoutExpired):
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        process.wait(timeout=5)
                    save()
assert hashlib.sha256(module.read_bytes()).hexdigest() == result['module_sha256'], 'module changed during matrix'
result['passed'] = True
save()
