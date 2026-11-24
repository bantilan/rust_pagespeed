#!/usr/bin/env python3
"""Pair interleaved warm HTML HIT requests across two isolated module builds."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import signal
import statistics
import subprocess
import tempfile
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--nginx', required=True)
p.add_argument('--before', required=True)
p.add_argument('--after', required=True)
p.add_argument('--output', required=True)
p.add_argument('--samples', type=int, default=500)
a = p.parse_args()
assert a.samples > 0
repo = Path(__file__).resolve().parent.parent
result = {'scope': 'Two simultaneous isolated single-worker instances, fresh independent caches, paired interleaved requests after confirmed warming. No live deployment change. Client/network scheduling remains in timing.', 'samples_per_page_and_build': a.samples, 'pages': {}, 'modules': {}}
processes, logs, ports = [], [], {}
last_bodies = {}


def get(mode, path):
    c = http.client.HTTPConnection(ports[mode][0], ports[mode][1], timeout=10)
    t = time.perf_counter_ns()
    try:
        c.request('GET', path, headers={'Host': 'localhost'})
        r = c.getresponse()
        body = r.read()
        ms = (time.perf_counter_ns() - t) / 1e6
        assert r.status == 200, r.status
        last_bodies[mode] = body
        marker = body.rsplit(b'PageSpeed Cache:', 1)[-1].split(b'|', 1)[0].strip().decode()
        return ms, marker, hashlib.sha256(re.sub(rb'<!-- PageSpeed Cache:.*?-->', b'', body)).hexdigest()
    finally:
        c.close()


try:
    for mode, module, address in [('before', a.before, '127.0.0.2'), ('after', a.after, '127.0.0.3')]:
        port = 18093
        prefix = Path(tempfile.mkdtemp(prefix='ps-hit-pair-'))
        (prefix / 'conf').mkdir()
        (prefix / 'logs').mkdir()
        text = (repo / 'docs/performance/shared-memory/nginx-template.conf').read_text().replace('@MODULE@', str(Path(module).resolve())).replace('@PREFIX@', str(prefix)).replace('@PORT@', str(port))
        text = text.replace(f'listen       {port};', f'listen       {address}:{port};')
        (prefix / 'conf/nginx.conf').write_text(text)
        result['modules'][mode] = {'prefix': str(prefix), 'sha256': hashlib.sha256(Path(module).read_bytes()).hexdigest()}
        subprocess.run([a.nginx, '-t', '-p', str(prefix) + '/', '-c', 'conf/nginx.conf'], check=True, capture_output=True)
        log = (prefix / 'runner.log').open('ab')
        logs.append(log)
        processes.append(subprocess.Popen([a.nginx, '-p', str(prefix) + '/', '-c', 'conf/nginx.conf'], stdout=log, stderr=log, start_new_session=True))
        ports[mode] = (address, port)
    for page, slug in [('Moto', 'moto-business-html5-responsive-web-template'), ('Aroma', 'aroma-beauty-and-spa-responsive-bootstrap-template')]:
        path = '/test/' + slug + '/index.html'
        expected = None
        for mode in ports:
            deadline, hits = time.monotonic() + 30, 0
            while hits < 3:
                try:
                    _, marker, digest = get(mode, path)
                    hits = hits + 1 if marker == 'HIT' else 0
                except OSError:
                    hits = 0
                assert time.monotonic() < deadline, (page, mode, 'warming timed out')
                time.sleep(.02)
            if expected is None:
                expected = digest
            if expected != digest:
                import difflib
                diff = list(difflib.unified_diff(last_bodies['before'].decode().splitlines(), last_bodies['after'].decode().splitlines()))
                print('\n'.join(diff)[:3000], flush=True)
            assert expected == digest, (page, mode, 'output mismatch')
        pairs = []
        for i in range(a.samples):
            pair = {}
            for mode in (['before', 'after'] if i % 2 else ['after', 'before']):
                ms, marker, digest = get(mode, path)
                assert marker == 'HIT' and digest == expected, (page, mode, marker)
                pair[mode] = ms
            pair['after_minus_before_ms'] = pair['after'] - pair['before']
            pairs.append(pair)
        result['pages'][page] = {'median_ms': {mode: statistics.median(x[mode] for x in pairs) for mode in ports}, 'median_paired_delta_ms': statistics.median(x['after_minus_before_ms'] for x in pairs), 'normalized_sha256': expected, 'pairs': pairs}
        print(page, result['pages'][page]['median_ms'], 'paired delta', result['pages'][page]['median_paired_delta_ms'], flush=True)
    result['passed'] = True
finally:
    for process in processes:
        try:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=8)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
    for log in logs:
        log.close()
    Path(a.output).write_text(json.dumps(result, indent=2) + '\n')
