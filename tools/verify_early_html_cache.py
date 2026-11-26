#!/usr/bin/env python3
"""Isolated FastCGI counter oracle for pre-origin HTML-cache lookup."""
import argparse
import hashlib
import http.client
import json
import os
import re
from pathlib import Path
import signal
import socket
import socketserver
import statistics
import struct
import subprocess
import tempfile
import threading
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--nginx', required=True)
p.add_argument('--module', required=True)
p.add_argument('--output', required=True)
p.add_argument('--rum-only', action='store_true', help='Isolate RUM clock retention with add_instrumentation; core beacon sampling tested separately')
p.add_argument('--analytics-early', choices=['baseline', 'fixed'], help='Verify analytics origin counter before/after allowlist parity fix')
p.add_argument('--sampled-image-free', action='store_true', help='Real Chrome image-free lazyload sampling through stable cache HITs')
p.add_argument('--browser-groups', action='store_true', help='Replay Chrome/Firefox navigation header differences against one public entry')
a = p.parse_args()
root = Path(tempfile.mkdtemp(prefix='ps-early-html-'))
(root / 'logs').mkdir()
counts = {}
lock = threading.Lock()

def read_exact(sock, size):
    data = b''
    while len(data) < size:
        chunk = sock.recv(size - len(data))
        if not chunk:
            raise EOFError()
        data += chunk
    return data

def params(data):
    out = {}
    while data:
        lengths = []
        for _ in range(2):
            if data[0] & 128:
                lengths.append(struct.unpack('!I', data[:4])[0] & 0x7fffffff)
                data = data[4:]
            else:
                lengths.append(data[0])
                data = data[1:]
        n, v = lengths
        out[data[:n].decode()] = data[n:n + v].decode()
        data = data[n + v:]
    return out

class Origin(socketserver.BaseRequestHandler):
    def handle(self):
        fields = b''
        while True:
            version, kind, req, length, padding, _ = struct.unpack('!BBHHBB', read_exact(self.request, 8))
            data = read_exact(self.request, length)
            read_exact(self.request, padding)
            if kind == 4:
                fields += data
            if kind == 5 and not data:
                break
        request = params(fields)
        uri = request.get('REQUEST_URI', '/')
        with lock:
            counts[uri] = counts.get(uri, 0) + 1
        time.sleep(.025)  # Explicit synthetic origin cost, not production timing.
        cookie = 'Set-Cookie: session=private\r\n' if uri.startswith('/private') else ''
        body = b'<html><head><title>Origin counter</title></head><body>' + b'hello world ' * 4000 + b'</body></html>'
        response = ('Status: 200 OK\r\nContent-Type: text/html\r\nCache-Control: public, max-age=60\r\n' + cookie + '\r\n').encode() + body
        self.request.sendall(struct.pack('!BBHHBB', 1, 6, req, len(response), 0, 0) + response)
        self.request.sendall(struct.pack('!BBHHBB', 1, 6, req, 0, 0, 0))
        self.request.sendall(struct.pack('!BBHHBB', 1, 3, req, 8, 0, 0) + b'\0' * 8)

origin = socketserver.ThreadingTCPServer(('127.0.0.1', 0), Origin)
origin.daemon_threads = True
threading.Thread(target=origin.serve_forever, daemon=True).start()
s = socket.socket(); s.bind(('127.0.0.1', 0)); port = s.getsockname()[1]; s.close()
conf = f'''load_module {Path(a.module).resolve()};
worker_processes 1;
master_process off;
pid {root}/nginx.pid;
error_log {root}/logs/error.log notice;
events {{ worker_connections 128; }}
http {{
 access_log off;
 pagespeed FileCachePath {root}/cache;
 server {{
  listen 127.0.0.1:{port};
  server_name localhost;
  pagespeed on;
  pagespeed HtmlCache on;
  pagespeed HtmlCacheEarlyLookup on;
  pagespeed ShowCacheTimestamp on;
  location / {{
   fastcgi_pass 127.0.0.1:{origin.server_address[1]};
   fastcgi_param REQUEST_METHOD $request_method;
   fastcgi_param REQUEST_URI $request_uri;
   fastcgi_param SCRIPT_FILENAME /synthetic/index.php;
  }}
 }}
}}
'''
if a.sampled_image_free:
    conf = conf.replace('  pagespeed on;', '  pagespeed on;\n  pagespeed EnableFilters lazyload_images;')
if a.rum_only:
    conf = conf.replace('  pagespeed on;', '  pagespeed on;\n  pagespeed RewriteLevel PassThrough;\n  pagespeed EnableFilters add_instrumentation;')
(root / 'nginx.conf').write_text(conf)
proc = None

def get(path='/', headers=None):
    c = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
    start = time.perf_counter()
    try:
        browser = {'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/130.0.0.0 Safari/537.36'} if a.rum_only else {}
        c.request('GET', path, headers={**browser, **(headers or {})})
        r = c.getresponse(); body = r.read()
        assert r.status == 200, (r.status, body[:100])
        marker = body.rsplit(b'PageSpeed Cache:', 1)[-1].split(b'|', 1)[0].strip().decode()
        return {'ms': (time.perf_counter() - start) * 1000, 'marker': marker, 'bytes': len(body),
                'rum_browser_clock': b'window.mod_pagespeed_start = Number(new Date())' in body,
                'body_hash': hashlib.sha256(re.sub(rb'<!-- PageSpeed Cache:.*?-->', b'', body)).hexdigest()}
    finally:
        c.close()

try:
    subprocess.run([a.nginx, '-p', str(root), '-c', str(root / 'nginx.conf'), '-t'], check=True, capture_output=True)
    proc = subprocess.Popen([a.nginx, '-p', str(root), '-c', str(root / 'nginx.conf'), '-g', 'daemon off;'], start_new_session=True)
    for _ in range(100):
        try:
            with socket.create_connection(('127.0.0.1', port), timeout=.1):
                break
        except OSError:
            time.sleep(.05)
    if a.sampled_image_free:
        # Real browser + actual POST receiver, no forged sampling completion.
        # The fourth issuance observes three accepted samples and selects low frequency.
        import zlib
        def sample_record():
            records = []
            for path in (root / 'cache').rglob('*'):
                if not path.is_file() or '.pagespeed-lru' in path.parts:
                    continue
                try:
                    blob = zlib.decompress(path.read_bytes(), -15)
                except (OSError, zlib.error):
                    continue
                if blob.startswith(b'beacon-evidence/1\n'):
                    records.append(blob.decode().splitlines())
            assert len(records) <= 1, records
            return records[0] if records else None
        def chrome_visit():
            run = subprocess.run(['google-chrome', '--headless', '--no-sandbox', '--disable-gpu',
                '--no-proxy-server', '--virtual-time-budget=2000', '--dump-dom',
                f'http://127.0.0.1:{port}/'], capture_output=True, text=True, timeout=25, check=True)
            markers = re.findall(r'<!-- PageSpeed Cache: (HIT|MISS|BYPASS)', run.stdout)
            return markers[-1] if markers else 'DOM_NO_MARKER'
        visits = []
        for count in range(1, 5):
            visits.append(chrome_visit())
            record = sample_record()
            assert record and int(record[2]) >= count, (visits, record)
            if count < 4:
                time.sleep(5.1)
        visits.append(chrome_visit())
        # Fresh probe identity, same learned page/device evidence.
        headers = {'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/130.0.0.0 Safari/537.36'}
        publication = get(headers=headers)
        before = counts.get('/', 0)
        stable = [get(headers=headers)['marker'] for _ in range(3)]
        time.sleep(5.1)  # Cross the former high-frequency boundary.
        stable.append(get(headers=headers)['marker'])
        assert all(marker == 'HIT' for marker in stable), (visits, stable)
        assert counts.get('/', 0) == before, counts
        result = {'sampling_visits': visits, 'publication': publication, 'stable_visits': stable,
                  'valid_beacons': int(sample_record()[2]), 'origin_counts': counts,
                  'warm_hits_skipped_origin': True, 'prefix': str(root)}
        Path(a.output).write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
        raise SystemExit(0)
    cold = get()
    assert cold['marker'] == 'MISS', cold
    before = counts.get('/', 0)
    warm = [get() for _ in range(20)]
    assert all(x['marker'] == 'HIT' for x in warm), warm
    assert all(x['body_hash'] == cold['body_hash'] for x in warm), warm
    if a.rum_only:
        assert cold['rum_browser_clock'] and all(x['rum_browser_clock'] for x in warm), warm
    assert counts.get('/', 0) == before, counts
    # Returning from another public page must not invoke the origin again.
    before_navigation = counts.get('/', 0)
    navigation = get(headers={'Referer': 'http://localhost/sample-page/', 'Sec-Fetch-Site': 'same-origin', 'Priority': 'u=1'})
    assert navigation['marker'] == 'HIT' and navigation['body_hash'] == cold['body_hash'], navigation
    assert counts.get('/', 0) == before_navigation, counts
    browser_groups = []
    if a.browser_groups:
        common = {'Accept-Language': 'en-US,en;q=0.9', 'Accept-Encoding': 'identity',
            'Sec-Fetch-Dest': 'document', 'Sec-Fetch-Mode': 'navigate',
            'Sec-Fetch-Site': 'same-origin', 'Sec-Fetch-User': '?1',
            'Upgrade-Insecure-Requests': '1', 'Referer': 'http://localhost/another-page/',
            'Priority': 'u=0, i'}
        chrome = {**common, 'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/158.0.0.0 Safari/537.36',
            'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,image/jxl,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7',
            'Cache-Control': 'max-age=0', 'Sec-CH-UA': '"Chromium";v="158", "Not.A/Brand";v="24", "Google Chrome";v="158"',
            'Sec-CH-UA-Mobile': '?0', 'Sec-CH-UA-Platform': '"Linux"'}
        firefox = {**common, 'User-Agent': 'Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0',
            'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
            'Sec-GPC': '1', 'Connection': 'keep-alive', 'TE': 'trailers'}
        prime = get('/browser-groups', chrome)
        assert prime['marker'] == 'MISS', prime
        before = counts['/browser-groups']
        for headers in [firefox, chrome] * 5:
            hit = get('/browser-groups', headers)
            assert hit['marker'] == 'HIT' and hit['body_hash'] == prime['body_hash'], hit
            browser_groups.append(hit)
        assert counts['/browser-groups'] == before, counts
    analytics = []
    if a.analytics_early:
        before = counts.get('/', 0)
        for i in range(20):
            sample = get(headers={'Cookie': f'_ga=GA1.1.{i}; _gid=test'})
            assert sample['marker'] == 'HIT', sample
            assert sample['body_hash'] == cold['body_hash'], sample
            if a.rum_only:
                assert sample['rum_browser_clock'], sample
            analytics.append(sample)
        expected = 0 if a.analytics_early == 'fixed' else 20
        assert counts.get('/', 0) == before + expected, (counts, before, expected)
    for headers in [{'Cookie': '_ga=123; session=secret'}, {'Cookie': 'unknown=secret'}, {'Cookie': '_ga'}, {'Cookie2': '_ga=123'}, {'Cookie': 'wordpress_logged_in_x=secret'}, {'Authorization': 'Basic test'}, {'Cache-Control': 'no-cache'}, {'Range': 'bytes=0-10'}]:
        before = counts.get('/', 0)
        get(headers=headers)
        assert counts.get('/', 0) == before + 1, (headers, counts)
    private = [get('/private') for _ in range(2)]
    private.extend(get('/private', headers={'Cookie': '_ga=123'}) for _ in range(2))
    assert counts['/private'] == 4, counts
    assert all(x['marker'] == 'BYPASS' for x in private), private
    result = {'cross_browser_hits': browser_groups, 'navigation_hit_skipped_origin': True, 'navigation': navigation, 'analytics': analytics, 'analytics_median_ms': statistics.median(x['ms'] for x in analytics) if analytics else None, 'analytics_expected': a.analytics_early, 'synthetic_origin_delay_ms': 25, 'cold': cold, 'warm': warm,
              'warm_median_ms': statistics.median(x['ms'] for x in warm),
              'origin_counts': counts, 'warm_hits_skipped_origin': True,
              'cookie_auth_cachecontrol_range_bypass_verified': True,
              'set_cookie_response_not_cached': True, 'prefix': str(root)}
    Path(a.output).write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))
finally:
    if proc:
        os.killpg(proc.pid, signal.SIGTERM)
        proc.wait(timeout=10)
    origin.shutdown(); origin.server_close()
