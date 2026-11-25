#!/usr/bin/env python3
"""Isolated NGINX HTTPS-source image progression without a TLS origin server."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import tempfile
import time
from urllib.parse import urlsplit

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--nginx', required=True)
p.add_argument('--module', required=True)
p.add_argument('--image', required=True, help='Existing JPEG fixture')
p.add_argument('--output', required=True)
a = p.parse_args()
prefix = Path(tempfile.mkdtemp(prefix='ps-https-file-'))
(prefix / 'logs').mkdir()
shutil.copyfile(a.image, prefix / 'hero.jpg')
sock = socket.socket(); sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]; sock.close()
(prefix / 'page.html').write_text(f'<html><head><title>HTTPS image</title></head><body><img src="https://localhost:{port}/hero.jpg" width="800" height="560"></body></html>')
(prefix / 'nginx.conf').write_text(f'''load_module {Path(a.module).resolve()};
master_process off;
worker_processes 1;
pid {prefix}/nginx.pid;
error_log {prefix}/logs/error.log notice;
events {{ worker_connections 128; }}
http {{
 access_log off;
 pagespeed FileCachePath {prefix}/cache;
 server {{
  listen 127.0.0.1:{port};
  server_name localhost;
  root {prefix};
  pagespeed on;
  pagespeed RewriteLevel CoreFilters;
  pagespeed RespectXForwardedProto on;
  location / {{ }}
 }}
}}
''')

def get(path, accept):
    conn = http.client.HTTPConnection('127.0.0.1', port, timeout=15)
    try:
        conn.request('GET', path, headers={'Host': f'localhost:{port}', 'X-Forwarded-Proto': 'https', 'Accept': accept,
            'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/130.0.0.0 Safari/537.36'})
        r = conn.getresponse(); body = r.read()
        return r.status, dict(r.getheaders()), body
    finally:
        conn.close()

proc = None
try:
    subprocess.run([a.nginx, '-e', str(prefix / 'logs/error.log'), '-p', str(prefix), '-c', str(prefix / 'nginx.conf'), '-t'], check=True, capture_output=True)
    proc = subprocess.Popen([a.nginx, '-e', str(prefix / 'logs/error.log'), '-p', str(prefix), '-c', str(prefix / 'nginx.conf'), '-g', 'daemon off;'], start_new_session=True)
    for _ in range(100):
        try:
            with socket.create_connection(('127.0.0.1', port), timeout=.1): break
        except OSError: time.sleep(.05)
    rows = []
    optimized = None
    for _ in range(60):
        start = time.perf_counter()
        status, headers, body = get('/page.html', 'text/html,image/avif,image/webp,*/*')
        assert status == 200
        src = re.search(rb'<img[^>]+src="([^"]+)"', body).group(1).decode()
        rows.append({'ms': (time.perf_counter() - start) * 1000, 'src': src})
        if '.pagespeed.' in src:
            optimized = src
            break
        time.sleep(.2)
    assert optimized, rows
    status, headers, body = get(urlsplit(optimized).path, 'image/avif,image/webp,image/*,*/*')
    assert status == 200, status
    mime = next((v for k, v in headers.items() if k.lower() == 'content-type'), '')
    assert mime == 'image/avif', (mime, optimized)
    assert b'ftypavif' in body[:64], body[:64]
    assert len(body) < Path(a.image).stat().st_size
    result = {'source': f'https://localhost:{port}/hero.jpg', 'tls_origin_running': False,
        'requests': rows, 'optimized_url': optimized, 'mime': mime,
        'original_bytes': Path(a.image).stat().st_size, 'optimized_bytes': len(body),
        'optimized_sha256': hashlib.sha256(body).hexdigest(),
        'module_sha256': hashlib.sha256(Path(a.module).read_bytes()).hexdigest()}
    Path(a.output).write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))
finally:
    if proc:
        os.killpg(proc.pid, signal.SIGTERM); proc.wait(timeout=15)
