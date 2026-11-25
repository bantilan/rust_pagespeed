#!/usr/bin/env python3
"""Run the real collector in Chrome against a synthetic HTTP sink.
This verifies collection/transport only: no producer injection or learned HTML.
"""
import argparse
import hashlib
import html
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import json
import re
import subprocess
import tempfile
import threading
from urllib.parse import parse_qs
from PIL import Image
from io import BytesIO

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--chrome', default='/usr/bin/google-chrome')
p.add_argument('--output', required=True)
a = p.parse_args()
repo = Path(__file__).resolve().parent.parent
script = (repo / 'crates/ps-html/assets/automatic_sizes_collector.js').read_text()
root = Path(tempfile.mkdtemp(prefix='ps-auto-collector-'))
reports, image_hits = [], []
lock = threading.Lock()
image = BytesIO(); Image.new('RGB', (800, 600), 'blue').save(image, format='JPEG')
ids = [str(i) * 64 for i in range(1, 8)]
scope, nonce = 'a' * 64, 'AAAAAAAAAAA'

def fixture(path):
    def img(token, attrs='', css='width:160px;height:120px'):
        return f'<img src="/hero.jpg" srcset="/hero.jpg 800w" sizes="100vw" width="800" height="600" data-pagespeed-auto-size-id="{token}" style="{css}" {attrs}>'
    content = img(ids[0], 'fetchpriority="high"') + img(ids[1], css='width:260px;height:195px')
    content += img(ids[2]) + img(ids[2])  # Cloned identity must be omitted.
    content += img(ids[3], css='display:none')
    content += '<picture>' + img(ids[4]) + '</picture>'
    content += img(ids[5], 'data-pagespeed-no-transform')
    content += img('invalid')
    if path.startswith('/oversize'):
        content += '<img>' * 513
    if path.startswith('/too-many-slots'):
        content = ''.join(img(f'{i:064x}') for i in range(65))
    if path.startswith('/empty'):
        content = img(ids[3], css='display:none')
    config = f'pagespeed.collectAutomaticSizes("/beacon", "{scope}", "{nonce}");'
    if path.startswith('/fallback'):
        config = 'Object.defineProperty(navigator,"sendBeacon",{value:function(){return false;}});' + config
    if path.startswith('/throw-beacon'):
        config = 'Object.defineProperty(navigator,"sendBeacon",{value:function(){throw new Error("test blocked beacon");}});' + config
    if path.startswith('/cross-origin'):
        config = config.replace('"/beacon"', '"http://127.0.0.1:1/beacon"')
    return ('<!doctype html><html><head><title>Collector</title></head><body>' + content + '<pre id="result"></pre><script>' + script + config + '''
window.addEventListener('load',function(){setTimeout(function(){
document.getElementById('result').textContent=JSON.stringify({viewport:innerWidth,images:Array.from(document.images).map(i=>({src:i.getAttribute('src'),srcset:i.getAttribute('srcset'),sizes:i.getAttribute('sizes')}))});},100)});
</script></body></html>''').encode()

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self):
        if self.path == '/hero.jpg':
            with lock: image_hits.append(self.path)
            body, mime = image.getvalue(), 'image/jpeg'
        else:
            body, mime = fixture(self.path), 'text/html'
        self.send_response(200); self.send_header('Content-Type', mime)
        self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_POST(self):
        size = int(self.headers['Content-Length']); assert size <= 65536
        raw = self.rfile.read(size).decode()
        with lock: reports.append({'raw': raw, 'fields': parse_qs(raw, strict_parsing=True)})
        self.send_response(204); self.end_headers()

server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
result = {'passed': False, 'scope': 'real collector JS, synthetic HTTP transport, no module producer/learned output',
          'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
          'collector_sha256': hashlib.sha256(script.encode()).hexdigest(), 'cases': [], 'prefix': str(root)}
try:
    result['browser'] = subprocess.check_output([a.chrome, '--version'], text=True).strip()
    for path, viewport, expected in [('/page', 640, 1), ('/page-desktop', 1024, 1),
                                     ('/fallback', 640, 1), ('/throw-beacon', 640, 1), ('/cross-origin', 640, 0),
                                     ('/oversize', 640, 0), ('/too-many-slots', 640, 0), ('/empty', 640, 0)]:
        before = len(reports); hits_before = len(image_hits)
        command = [a.chrome, '--headless', '--no-sandbox', '--disable-gpu', '--disable-background-networking',
                   '--no-first-run', '--no-default-browser-check', '--hide-scrollbars', '--force-device-scale-factor=1',
                   f'--window-size={viewport},900', '--virtual-time-budget=2500', '--dump-dom',
                   f'--user-data-dir={root}/{path[1:]}-profile', f'http://127.0.0.1:{server.server_port}{path}']
        with (root / f'{path[1:]}.log').open('w') as log:
            dom = subprocess.check_output(command, stderr=log, text=True, timeout=20)
        (root / f'{path[1:]}.html').write_text(dom)
        summary_match = re.search(r'<pre id="result">(.*?)</pre>', dom, re.S)
        assert summary_match, 'browser load instrumentation did not finish'
        summary = json.loads(html.unescape(summary_match.group(1)))
        assert summary['viewport'] == viewport, summary['viewport']
        received = reports[before:]
        assert len(received) == expected, (path, received)
        if expected:
            fields = received[0]['fields']
            assert set(fields) == {'url', 'asi', 'asn', 'asm'}, fields
            assert fields['asi'] == [scope] and fields['asn'] == [nonce], fields
            lines = fields['asm'][0].splitlines()
            assert lines[0] == f'automatic-sizes/1 {viewport}', lines
            measurements = dict(line.split(' ') for line in lines[1:])
            assert measurements == {ids[0]: str(160 * 64), ids[1]: str(260 * 64)}, measurements
            assert len(image_hits) - hits_before == 1, 'same resource loaded more than once'
        for img in summary['images']:
            if img['src']:
                assert img == {'src': '/hero.jpg', 'srcset': '/hero.jpg 800w', 'sizes': '100vw'}, img
        result['cases'].append({'path': path, 'viewport': viewport, 'reports': received,
                                'image_requests': len(image_hits) - hits_before,
                                'image_attributes_unchanged': True})
    result['passed'] = True
finally:
    server.shutdown(); server.server_close()
    Path(a.output).write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({'passed': result['passed'], 'cases': len(result['cases']), 'prefix': str(root)}))
