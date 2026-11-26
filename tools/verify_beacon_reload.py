#!/usr/bin/env python3
"""Verify explicit beacon evidence across a real isolated NGINX graceful reload."""
import argparse
import base64
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
from urllib.parse import urlencode
import zlib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--nginx', required=True)
parser.add_argument('--module', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--port', type=int, default=18091)
parser.add_argument('--zstd', action='store_true', help='Enable/accept zstd and decode HTML before checking nonce behavior')
parser.add_argument('--accept-before-reload', action='store_true', help='Accept before reload, then ensure new workers preserve evidence and reject replay')
parser.add_argument('--early-html-cache', action='store_true', help='Exercise opt-in pre-origin HTML cache lookup')
parser.add_argument('--font-query-post', action='store_true', help='Exercise real font collector empty POST/query transport with evidence/replay checks')
parser.add_argument('--image-free', action='store_true', help='Verify critical beacon nonce acceptance on a text-only page')
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
prefix = Path(tempfile.mkdtemp(prefix='ps-beacon-reload-'))
(prefix / 'conf').mkdir()
(prefix / 'logs').mkdir()
(prefix / 'hold.bin').write_bytes(b'x' * 65536)
(prefix / 'page.html').write_text('<html><head><title>Beacon reload</title></head><body><img src="/hero.jpg"></body></html>')
if args.font_query_post:
    (prefix / 'page.html').write_text('<html><head><style>@font-face{font-family:Manrope;src:url(/manrope.woff2)}body{font-family:Manrope}</style></head><body><img src="/hero.jpg">text</body></html>')
if args.image_free:
    (prefix / 'page.html').write_text('<html><head><title>Text page</title></head><body><h1>Text LCP</h1></body></html>')
config = (repo / 'docs/performance/shared-memory/nginx-template.conf').read_text()
config = config.replace('@MODULE@', str(Path(args.module).resolve())).replace('@PREFIX@', str(prefix)).replace('@PORT@', str(args.port))
config = config.replace('master_process off;', 'master_process on;').replace('worker_processes 1;', 'worker_processes 2;')
config = config.replace('http {', 'http {\nadd_header X-Worker $pid always;')
config = config.replace(f'pagespeed FileCachePath {prefix}/cache;', f'pagespeed FileCachePath {prefix}/cache;\npagespeed CreateSharedMemoryMetadataCache {prefix}/cache 1024;')
config = config.replace('    location / {', f'    location = /beacon-reload.html {{ alias {prefix}/page.html; }}\n    location = /hold.bin {{ alias {prefix}/hold.bin; limit_rate 8k; }}\n    location / {{')
if args.image_free:
    config = config.replace(f'alias {prefix}/page.html;', f'alias {prefix}/page.html; expires 300s;')
if args.early_html_cache:
    config = config.replace('pagespeed RewriteLevel CoreFilters;', 'pagespeed RewriteLevel CoreFilters;\npagespeed HtmlCacheEarlyLookup on;')
if args.zstd:
    config = config.replace('pagespeed RewriteLevel CoreFilters;', 'pagespeed RewriteLevel CoreFilters;\npagespeed EnableFilters compress_zstd;')
(prefix / 'conf/nginx.conf').write_text(config)
subprocess.run([args.nginx, '-t', '-p', str(prefix) + '/', '-c', 'conf/nginx.conf'], check=True, capture_output=True)
result = {'passed': False, 'zstd': args.zstd, 'prefix': str(prefix), 'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
          'module_sha256': hashlib.sha256(Path(args.module).read_bytes()).hexdigest(), 'config_sha256': hashlib.sha256(config.encode()).hexdigest()}


def connection():
    return http.client.HTTPConnection('127.0.0.1', args.port, timeout=5)


def request(path, params=None, query_post=False):
    conn = connection()
    if query_post:
        path += '?' + urlencode(params)
    conn.request('POST' if params else 'GET', path, body=b'' if query_post else (urlencode(params) if params else None),
                 headers={'Host': 'localhost', 'Content-Type': 'application/x-www-form-urlencoded', 'Accept-Encoding': 'zstd' if args.zstd else 'identity', 'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'})
    response = conn.getresponse()
    body = response.read()
    if response.getheader('Content-Encoding') == 'zstd':
        import ctypes
        lib = ctypes.CDLL('libzstd.so.1')
        lib.ZSTD_decompress.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t]
        lib.ZSTD_decompress.restype = ctypes.c_size_t
        lib.ZSTD_isError.argtypes = [ctypes.c_size_t]
        lib.ZSTD_isError.restype = ctypes.c_uint
        decoded = ctypes.create_string_buffer(2 * 1024 * 1024)
        size = lib.ZSTD_decompress(decoded, len(decoded), body, len(body))
        assert not lib.ZSTD_isError(size), 'invalid zstd beacon response'
        body = decoded.raw[:size]
        result['decoded_zstd_responses'] = result.get('decoded_zstd_responses', 0) + 1
    status, pid = response.status, int(response.getheader('X-Worker', '0'))
    conn.close()
    assert status in (200, 204), (status, body[:200])
    if query_post:
        assert status == 204, (status, body[:200])
    assert pid, 'missing worker identity'
    return pid, body


def evidence():
    # Read-only oracle for the persisted versioned evidence codec; never write
    # cache files or infer acceptance solely from the beacon HTTP status.
    records = []
    for root, directories, files in os.walk(prefix / 'cache'):
        directories[:] = [d for d in directories if d != '.pagespeed-lru']
        for name in files:
            try:
                blob = zlib.decompress((Path(root) / name).read_bytes(), -15)
            except (OSError, zlib.error):
                continue
            if blob.startswith(b'beacon-evidence/1\n'):
                lines = blob.decode().splitlines()
                count = int(lines[8])
                nonces = [base64.urlsafe_b64decode(line.split('\t')[0] + '===').decode() for line in lines[9:9 + count]]
                records.append({'valid': int(lines[2]), 'generation': int(lines[5]), 'nonces': nonces})
    assert len(records) == 1, records
    return records[0]


def font_evidence():
    records = []
    for path in (prefix / 'cache').rglob('*'):
        if not path.is_file() or '.pagespeed-lru' in path.parts:
            continue
        try:
            blob = zlib.decompress(path.read_bytes(), -15)
        except (OSError, zlib.error):
            continue
        if blob.startswith(b'font-confirmed/1\n'):
            records.append(blob.decode())
    assert len(records) <= 1, records
    return records[0] if records else None


process = hold = log = None
try:
    log = (prefix / 'runner.log').open('ab')
    process = subprocess.Popen([args.nginx, '-p', str(prefix) + '/', '-c', 'conf/nginx.conf'], stdout=log, stderr=log, start_new_session=True)
    deadline = time.monotonic() + 8
    while True:
        try:
            pid, body = request('/beacon-reload.html')
            break
        except OSError:
            assert time.monotonic() < deadline, 'NGINX startup timed out'
            time.sleep(.05)
    match = re.search(rb"pagespeed\.CriticalImages\.Run\('([^']*)','([^']*)','([^']*)',(?:true|false),(?:true|false),'([^']*)'\)", body)
    assert match, body[-2000:]
    endpoint, page_url, options_hash, nonce = [v.decode() for v in match.groups()]
    if args.zstd:
        assert result.get('decoded_zstd_responses', 0) > 0, 'HTML was not zstd encoded'
    if args.font_query_post:
        font_match = re.search(rb"pagespeed\.FontPreloads\.Run\('([^']*)','([^']*)','([^']*)','([^']*)'\)", body)
        assert font_match, body[-2000:]
        fe, fp, fo, fn = [value.decode() for value in font_match.groups()]
        assert font_evidence() is None
        font_params = {'url': fp, 'oh': fo, 'n': 'not-issued', 'ff': 'manrope|400|n'}
        request(fe, font_params, query_post=True)
        assert font_evidence() is None, 'unissued nonce accepted'
        font_params['n'] = fn
        request(fe, font_params, query_post=True)
        learned = font_evidence()
        assert learned == 'font-confirmed/1\n1\nmanrope\t400\tn\n', learned
        font_params['ff'] = 'unexpected|700|n'
        request(fe, font_params, query_post=True)
        assert font_evidence() == learned, 'replay mutated evidence'
        result['font_empty_post'] = {'accepted': True, 'unissued_rejected': True, 'replay_rejected': True, 'evidence': learned}
    initial = evidence()
    assert nonce in initial['nonces'] and initial['valid'] == 0, initial
    params = {'url': page_url, 'oh': options_hash, 'n': nonce, 'lcpk': 'unknown' if args.image_free else 'text'}
    accepted_before = None
    if args.accept_before_reload:
        request(endpoint, params)
        accepted_before = evidence()
        assert accepted_before['valid'] == 1 and nonce not in accepted_before['nonces'], accepted_before
    hold = connection()
    hold.request('GET', '/hold.bin', headers={'Host': 'localhost'})
    held = hold.getresponse()
    old_pid = int(held.getheader('X-Worker'))
    assert held.status == 200
    old_workers = set(Path(f'/proc/{process.pid}/task/{process.pid}/children').read_text().split())
    os.kill(process.pid, signal.SIGHUP)
    deadline = time.monotonic() + 5
    while True:
        new_pid, _ = request('/pagespeed_admin/stats_json')
        if str(new_pid) not in old_workers:
            break
        assert time.monotonic() < deadline, 'new worker never answered'
        time.sleep(.05)
    assert Path(f'/proc/{old_pid}/stat').exists(), 'old worker did not overlap'
    accepted_pid, _ = request(endpoint, params)
    assert str(accepted_pid) not in old_workers, (accepted_pid, old_workers)
    accepted = evidence()
    assert accepted['valid'] == 1 and accepted['generation'] == initial['generation'] + 1, (initial, accepted)
    assert nonce not in accepted['nonces'], accepted
    if accepted_before is not None:
        assert accepted == accepted_before, (accepted_before, accepted)
    # Same browser report must not count twice after crossing the reload.
    replay_pid, _ = request(endpoint, params)
    replay = evidence()
    assert replay == accepted, (accepted, replay)
    assert Path(f'/proc/{old_pid}/stat').exists(), 'old worker exited before beacon checks'
    if args.font_query_post:
        request(fe, font_params, query_post=True)
        assert font_evidence() == learned, 'font evidence/replay changed across reload'
        result['font_empty_post']['reload_preserved'] = True
    result.update(passed=True, accept_before_reload=args.accept_before_reload, old_worker=old_pid, accepted_worker=accepted_pid, replay_worker=replay_pid,
                  initial=initial, accepted=accepted, after_replay=replay, old_worker_alive_during_accept=Path(f'/proc/{old_pid}/stat').exists())
finally:
    if hold:
        hold.close()
    if process:
        try:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=8)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
    if log:
        log.close()
    Path(args.output).write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
