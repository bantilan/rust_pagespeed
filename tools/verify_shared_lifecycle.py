#!/usr/bin/env python3
"""Exercise an isolated two-worker shared-cache reload and restart; never touch the installed service."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--nginx', required=True)
parser.add_argument('--module', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--port', type=int, default=18089)
parser.add_argument('--check-metrics', action='store_true')
parser.add_argument('--benchmark-requests', type=int, default=0, help='Optional eight-client warm HIT comparison after lifecycle checks')
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
prefix = Path(tempfile.mkdtemp(prefix='ps-shared-lifecycle-'))
(prefix / 'conf').mkdir()
(prefix / 'logs').mkdir()
(prefix / 'hold.bin').write_bytes(b'x' * 65536)
module = Path(args.module).resolve()
template = (repo / 'docs/performance/shared-memory/nginx-template.conf').read_text()
template = template.replace('@MODULE@', str(module)).replace('@PREFIX@', str(prefix)).replace('@PORT@', str(args.port))
template = template.replace('master_process off;', 'master_process on;').replace('worker_processes 1;', 'worker_processes 2;')
template = template.replace('http {', 'http {\nadd_header X-Worker $pid always;')
template = template.replace('    location / {', f'    location = /hold.bin {{ alias {prefix}/hold.bin; limit_rate 8k; }}\n    location / {{')
path = '/test/moto-business-html5-responsive-web-template/index.html'


def configure(kb):
    text = template.replace(f'pagespeed FileCachePath {prefix}/cache;',
                            f'pagespeed FileCachePath {prefix}/cache;\npagespeed CreateSharedMemoryMetadataCache {prefix}/cache {kb};')
    (prefix / 'conf/nginx.conf').write_text(text)
    subprocess.run([args.nginx, '-t', '-p', str(prefix) + '/', '-c', 'conf/nginx.conf'], check=True, capture_output=True)


def request(target=path):
    connection = http.client.HTTPConnection('127.0.0.1', args.port, timeout=5)
    connection.request('GET', target, headers={'Host': 'localhost'})
    response = connection.getresponse()
    body = response.read()
    worker = int(response.getheader('X-Worker', '0'))
    status = response.status
    connection.close()
    if status != 200 or worker == 0:
        raise RuntimeError(f'invalid response {status}, worker {worker}')
    return worker, body


def warm(exclude=0):
    deadline = time.monotonic() + 15
    last = None
    while time.monotonic() < deadline:
        try:
            worker, body = request()
            marker = body.rsplit(b'PageSpeed Cache:', 1)[-1].split(b'|', 1)[0].strip()
            if worker != exclude and b'.pagespeed.' in body and marker == b'HIT':
                normalized = re.sub(rb'<!-- PageSpeed Cache:.*?-->', b'', body)
                digest = hashlib.sha256(normalized).hexdigest()
                if last == digest:
                    return worker, digest, len(normalized), body.count(b'.pagespeed.')
                last = digest
        except (OSError, http.client.HTTPException):
            pass
        time.sleep(0.05)
    raise RuntimeError('optimized HTML never stabilized')


log = (prefix / 'runner.log').open('ab')
process = None
hold = None


def start():
    return subprocess.Popen([args.nginx, '-p', str(prefix) + '/', '-c', 'conf/nginx.conf'],
                            stdout=log, stderr=log, start_new_session=True)


def stop():
    global process
    if process is not None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
        process = None


result = {'prefix': str(prefix), 'module_sha256': hashlib.sha256(module.read_bytes()).hexdigest(),
          'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()}
try:
    configure(1024)
    process = start()
    before = warm()
    if args.check_metrics:
        samples = {}
        deadline = time.monotonic() + 5
        while len(samples) < 2 and time.monotonic() < deadline:
            pid, body = request('/pagespeed_admin/stats_json')
            values = json.loads(body)['variables']
            assert values['shared_cache_domains'] == 1, values
            assert 0 < values['shared_cache_byte_mapping_bytes'] <= 1024 * 1024, values
            assert values['shared_cache_owner_claims'] > 0, values
            assert values['shared_cache_resident_entries'] > 0, values
            samples[pid] = {key: value for key, value in values.items() if key.startswith('shared_cache_')}
        assert len(samples) == 2, samples
        capacities = {value['shared_cache_byte_mapping_bytes'] for value in samples.values()}
        assert len(capacities) == 1
        result['metrics_workers'] = samples
    hold = http.client.HTTPConnection('127.0.0.1', args.port, timeout=5)
    hold.request('GET', '/hold.bin', headers={'Host': 'localhost'})
    held = hold.getresponse()
    old_worker = int(held.getheader('X-Worker'))
    assert held.status == 200
    original_workers = set(Path(f'/proc/{process.pid}/task/{process.pid}/children').read_text().split())
    configure(2048)
    os.kill(process.pid, signal.SIGHUP)
    # Wait for a genuinely new worker, not the other worker in the old pair.
    deadline = time.monotonic() + 5
    after = None
    while time.monotonic() < deadline:
        candidate = warm(exclude=old_worker)
        if str(candidate[0]) not in original_workers and Path(f'/proc/{old_worker}/stat').exists():
            # Old response is throttled and remains active through this check.
            children = Path(f'/proc/{process.pid}/task/{process.pid}/children').read_text().split()
            if len(children) >= 3:
                after = candidate
                break
        time.sleep(0.05)
    if after is None:
        raise RuntimeError('did not observe overlapping worker generations')
    assert before[1:] == after[1:], (before, after)
    hold.close()
    hold = None
    mappings = sorted(str(p.relative_to(prefix)) for p in (prefix / 'cache/.pagespeed-lru').glob('shared-v2/*/bytes-v2-*.dat'))
    assert len(mappings) == 2, mappings
    stop()
    process = start()
    restarted = warm()
    assert before[1:] == restarted[1:], (before, restarted)
    if args.check_metrics:
        _, body = request('/pagespeed_admin/stats_json')
        values = json.loads(body)['variables']
        assert values['shared_cache_domains'] == 1
        assert 1024 * 1024 < values['shared_cache_byte_mapping_bytes'] <= 2048 * 1024
        result['metrics_after_restart'] = {key: value for key, value in values.items() if key.startswith('shared_cache_')}
    if args.benchmark_requests:
        import concurrent.futures
        import statistics
        def concurrent_run():
            def timed(_):
                started = time.perf_counter_ns()
                pid, body = request()
                elapsed = (time.perf_counter_ns() - started) / 1e6
                marker = body.rsplit(b'PageSpeed Cache:', 1)[-1].split(b'|', 1)[0].strip()
                assert marker == b'HIT', marker
                normalized = re.sub(rb'<!-- PageSpeed Cache:.*?-->', b'', body)
                assert hashlib.sha256(normalized).hexdigest() == before[1]
                return {'ms': elapsed, 'worker': pid}
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
                samples = list(executor.map(timed, range(args.benchmark_requests)))
            return {'clients': 8, 'requests': len(samples), 'median_ms': statistics.median(sample['ms'] for sample in samples), 'workers': sorted(set(sample['worker'] for sample in samples)), 'samples': samples}
        result['concurrent_shared'] = concurrent_run()
        stop()
        config_path = prefix / 'conf/nginx.conf'
        config_path.write_text(re.sub(r'^pagespeed CreateSharedMemoryMetadataCache .*?;\n', '', config_path.read_text(), flags=re.MULTILINE))
        process = start()
        warm()
        result['concurrent_default'] = concurrent_run()
    result.update({'passed': True, 'before': before, 'after_reload': after, 'after_restart': restarted,
                   'held_old_worker': old_worker, 'byte_mappings': mappings,
                   'checkpoint_policy': 'validated existing write-through disk cache; no native mutex checkpoint'})
finally:
    if hold is not None:
        hold.close()
    stop()
    log.close()
Path(args.output).write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
