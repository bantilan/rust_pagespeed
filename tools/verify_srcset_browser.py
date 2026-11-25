#!/usr/bin/env python3
"""Isolated Chrome selection test using actual NGINX-rewritten HTML and fixture JPEGs.
Requires installed Chrome and Pillow; uses DevTools pipe, no browser Python SDK.
"""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import tempfile
import time
from PIL import Image

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--nginx', required=True)
p.add_argument('--module', required=True)
p.add_argument('--chrome', default='/usr/bin/google-chrome')
p.add_argument('--output', required=True)
a = p.parse_args()
root = Path(tempfile.mkdtemp(prefix='ps-srcset-browser-'))
(root / 'logs').mkdir()
for width in [300, 768, 800]:
    image = Image.new('RGB', (width, width * 3 // 4))
    image.putdata([((x * 7 + y) % 256, (y * 3 + x) % 256, (x + y * 9) % 256)
                   for y in range(image.height) for x in range(width)])
    image.save(root / f'{width}.jpg', quality=85)
page = '''<html><head><style>body{margin:0}.slot{width:calc(100vw - 60px);max-width:645px}img{width:100%;height:auto}</style></head>
<body><div class="slot"><img class="hero" width="800" height="600" src="/800.jpg" srcset="/300.jpg 300w, /768.jpg 768w, /800.jpg 800w" sizes="(max-width: 800px) 100vw, 800px"></div></body></html>'''
for mode in ['off', 'on']:
    (root / mode).mkdir()
    (root / mode / 'index.html').write_text(page)
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
  root {root};
  pagespeed on;
  pagespeed RewriteLevel PassThrough;
  pagespeed SrcsetSizesClass hero;
  pagespeed SrcsetSizes "(max-width: 800px) calc(100vw - 60px), 645px";
  location /off/ {{ pagespeed SrcsetSizesEnabled off; }}
  location /on/ {{ pagespeed SrcsetSizesEnabled on; }}
  location ~ \\.jpg$ {{ types {{ image/jpeg jpg; }} }}
 }}
}}
'''
(root / 'nginx.conf').write_text(conf)

class Chrome:
    def __init__(self):
        browser_read, self.writer = os.pipe()
        self.reader, browser_write = os.pipe()
        self.buffer = b''
        self.seq = 0
        self.log = (root / 'chrome.log').open('wb')
        self.proc = subprocess.Popen([
            'bash', '-c', 'exec 3<&"$1" 4>&"$2"; shift 2; exec "$@"', '--',
            str(browser_read), str(browser_write), a.chrome,
            '--headless', '--no-sandbox', '--disable-gpu', '--no-first-run',
            '--disable-background-networking', '--disable-component-update',
            '--no-default-browser-check', '--remote-debugging-pipe',
            f'--user-data-dir={root}/chrome-profile', 'about:blank'],
            pass_fds=(browser_read, browser_write), stdout=self.log, stderr=self.log,
            start_new_session=True)
        os.close(browser_read); os.close(browser_write)

    def call(self, method, params=None, session=None):
        self.seq += 1
        command = {'id': self.seq, 'method': method, 'params': params or {}}
        if session:
            command['sessionId'] = session
        os.write(self.writer, json.dumps(command).encode() + b'\0')
        deadline = time.monotonic() + 20
        while True:
            while b'\0' not in self.buffer:
                remaining = deadline - time.monotonic()
                assert remaining > 0 and select.select([self.reader], [], [], remaining)[0], (method, 'timeout')
                chunk = os.read(self.reader, 65536)
                assert chunk, (method, 'Chrome pipe closed', (root / 'chrome.log').read_text()[-2000:])
                self.buffer += chunk
            raw, self.buffer = self.buffer.split(b'\0', 1)
            if not raw:
                continue
            reply = json.loads(raw)
            if reply.get('id') == self.seq:
                assert 'error' not in reply, reply
                return reply.get('result', {})

    def close(self):
        os.close(self.writer); os.close(self.reader)
        os.killpg(self.proc.pid, signal.SIGTERM)
        self.proc.wait(timeout=10)
        self.log.close()

proc = browser = None
result = {'passed': False, 'prefix': str(root), 'cases': [],
          'module_sha256': hashlib.sha256(Path(a.module).read_bytes()).hexdigest(),
          'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
          'fixture': 'Only supplied300/768/800JPEGs; PassThrough+sizes policy, no image conversion or variants',
          'jpeg_bytes': {str(w): (root / f'{w}.jpg').stat().st_size for w in [300, 768, 800]}}
try:
    subprocess.run([a.nginx, '-t', '-p', str(root), '-c', str(root / 'nginx.conf')], check=True, capture_output=True)
    proc = subprocess.Popen([a.nginx, '-e', str(root / 'logs/error.log'), '-p', str(root), '-c', str(root / 'nginx.conf'), '-g', 'daemon off;'], start_new_session=True)
    deadline = time.monotonic() + 10
    while True:
        try:
            with socket.create_connection(('127.0.0.1', port), timeout=.1):
                break
        except OSError:
            assert time.monotonic() < deadline
            time.sleep(.05)
    browser = Chrome()
    result['browser'] = browser.call('Browser.getVersion')
    for viewport in [352, 412, 1024]:
        for dpr in [1, 1.75, 2, 3]:
            for mode in ['off', 'on']:
                target = browser.call('Target.createTarget', {'url': 'about:blank'})['targetId']
                session = browser.call('Target.attachToTarget', {'targetId': target, 'flatten': True})['sessionId']
                browser.call('Emulation.setDeviceMetricsOverride', {'width': viewport, 'height': 900, 'deviceScaleFactor': dpr, 'mobile': False}, session)
                browser.call('Network.enable', session=session)
                browser.call('Network.setCacheDisabled', {'cacheDisabled': True}, session)
                browser.call('Page.enable', session=session)
                browser.call('Page.navigate', {'url': f'http://127.0.0.1:{port}/{mode}/index.html'}, session)
                expression = '''new Promise((resolve,reject)=>{let tries=0;const timer=setInterval(()=>{const i=document.querySelector('img');if(i&&i.complete&&i.naturalWidth){clearInterval(timer);const r=performance.getEntriesByName(i.currentSrc).at(-1);resolve({viewport:innerWidth,dpr:devicePixelRatio,currentSrc:i.currentSrc,sizes:i.sizes,srcset:i.srcset,cssWidth:i.getBoundingClientRect().width,naturalWidth:i.naturalWidth,naturalHeight:i.naturalHeight,transferSize:r?.transferSize,encodedBodySize:r?.encodedBodySize});}else if(++tries>200){clearInterval(timer);reject('image load timeout');}},25)})'''
                evaluated = browser.call('Runtime.evaluate', {'expression': expression, 'awaitPromise': True, 'returnByValue': True}, session)
                assert 'exceptionDetails' not in evaluated, evaluated
                sample = evaluated['result']['value']
                assert sample['viewport'] == viewport and sample['dpr'] == dpr, sample
                selected = int(Path(sample['currentSrc']).stem)
                assert selected in [300, 768, 800], sample
                sample.update(mode=mode, fileWidth=selected, fileHeight=selected * 3 // 4)
                assert sample['encodedBodySize'] == result['jpeg_bytes'][str(selected)], sample
                if mode == 'on':
                    assert sample['sizes'] == '(max-width: 800px) calc(100vw - 60px), 645px', sample
                else:
                    assert sample['sizes'] == '(max-width: 800px) 100vw, 800px', sample
                result['cases'].append(sample)
                browser.call('Target.closeTarget', {'targetId': target})
    for i in range(0, len(result['cases']), 2):
        off, on = result['cases'][i:i + 2]
        assert off['cssWidth'] == on['cssWidth'], (off, on)
        assert on['fileWidth'] >= min(800, on['cssWidth'] * on['dpr']), on
        assert on['encodedBodySize'] <= off['encodedBodySize'], (off, on)
    result['passed'] = True
finally:
    if browser:
        browser.close()
    if proc:
        os.killpg(proc.pid, signal.SIGTERM); proc.wait(timeout=10)
    Path(a.output).write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({'passed': result['passed'], 'cases': len(result['cases']), 'prefix': str(root)}))
