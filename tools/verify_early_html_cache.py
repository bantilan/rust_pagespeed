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
    cold = get()
    assert cold['marker'] == 'MISS', cold
    before = counts.get('/', 0)
    warm = [get() for _ in range(20)]
    assert all(x['marker'] == 'HIT' for x in warm), warm
    assert all(x['body_hash'] == cold['body_hash'] for x in warm), warm
    if a.rum_only:
        assert cold['rum_browser_clock'] and all(x['rum_browser_clock'] for x in warm), warm
    assert counts.get('/', 0) == before, counts
    for headers in [{'Cookie': 'wordpress_logged_in_x=secret'}, {'Authorization': 'Basic test'}, {'Cache-Control': 'no-cache'}, {'Range': 'bytes=0-10'}]:
        before = counts.get('/', 0)
        get(headers=headers)
        assert counts.get('/', 0) == before + 1, (headers, counts)
    private = [get('/private') for _ in range(2)]
    assert counts['/private'] == 2, counts
    assert all(x['marker'] == 'BYPASS' for x in private), private
    result = {'synthetic_origin_delay_ms': 25, 'cold': cold, 'warm': warm,
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
