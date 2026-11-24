#!/usr/bin/env python3
"""Read-only installed localhost latency check; no configuration or cache purge."""
import argparse
import hashlib
import http.client
import json
from pathlib import Path
import random
import re
import statistics
import time
from datetime import datetime, timezone

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--output', required=True)
p.add_argument('--samples', type=int, default=100)
a = p.parse_args()
assert a.samples > 0
result = {'at': datetime.now(timezone.utc).isoformat(), 'scope': 'Installed localhost, existing caches, new connection unless keepalive. No purge/reload; header-forced MISS only if marker confirms. On-disk module hash does not prove loaded image/source.',
          'installed_module_sha256': hashlib.sha256(Path('/usr/lib/nginx/modules/ngx_pagespeed_rust.so').read_bytes()).hexdigest(), 'pages': {}}
rng = random.Random(20261124)


def get(path, conn=None, unique=False):
    owned = conn is None
    conn = conn or http.client.HTTPConnection('localhost', 80, timeout=5)
    headers = {'Host': 'localhost'}
    if unique:
        headers['X-PS-Benchmark'] = str(time.time_ns())
    start = time.perf_counter_ns()
    try:
        conn.request('GET', path, headers=headers)
        response = conn.getresponse()
        ttfb = (time.perf_counter_ns() - start) / 1e6
        body = response.read()
        sample = {'ms': (time.perf_counter_ns() - start) / 1e6, 'ttfb_ms': ttfb, 'status': response.status, 'bytes': len(body), 'connection_closed_by_server': response.will_close}
        sample['marker'] = body.rsplit(b'PageSpeed Cache:', 1)[-1].split(b'|', 1)[0].strip().decode() if b'PageSpeed Cache:' in body else 'absent'
        return sample, body
    finally:
        if owned:
            conn.close()


def summary(samples):
    values = sorted(s['ms'] for s in samples)
    return {'median_ms': statistics.median(values), 'p95_ms': values[max(0, int(.95 * len(values) + .999) - 1)], 'markers': sorted(set(s['marker'] for s in samples)), 'statuses': sorted(set(s['status'] for s in samples)), 'samples': samples}


try:
    stat, body = get('/pagespeed_admin/stats_json')
    result['admin_before'] = json.loads(body) if stat['status'] == 200 else {'status': stat['status']}
    for name, slug in [('Moto','moto-business-html5-responsive-web-template'),('Aroma','aroma-beauty-and-spa-responsive-bootstrap-template'),('Aerosky','aerosky-real-estate-html-responsive-website-template')]:
        path = '/test/' + slug + '/index.html'
        page = {'first_observation': get(path)[0]}
        result['pages'][name] = page
        for _ in range(10):
            _, body = get(path)
        normalized = re.sub(rb'<!-- PageSpeed Cache:.*?-->', b'', body)
        page['normalized_html_sha256'] = hashlib.sha256(normalized).hexdigest()
        page['pagespeed_refs'] = body.count(b'.pagespeed.')
        samples = {phase: [] for phase in ['enabled','forced_miss','off']}
        # Interleave phase order to reduce load drift; resource options unchanged.
        for _ in range(a.samples):
            phases = list(samples)
            rng.shuffle(phases)
            for phase in phases:
                sample, _ = get(path + ('?PageSpeed=off' if phase == 'off' else ''), unique=phase == 'forced_miss')
                samples[phase].append(sample)
        page.update({phase: summary(rows) for phase, rows in samples.items()})
        conn = http.client.HTTPConnection('localhost', 80, timeout=5)
        try:
            page['keepalive_enabled'] = summary([get(path, conn)[0] for _ in range(a.samples)])
        finally:
            conn.close()
        # Existing optimized resource URLs only; no artificial invalidation.
        urls = list(dict.fromkeys(re.findall(rb'''(?:src|href)=["']([^"']+\.pagespeed\.[^"']+)["']''', body)))[:5]
        page['resources'] = []
        for raw in urls:
            url = raw.decode().replace('&amp;', '&')
            if url.startswith('http://localhost/'):
                url = url[len('http://localhost'):]
            if not url.startswith('/'):
                url = path.rsplit('/',1)[0] + '/' + url
            if url.startswith('//') or '://' in url:
                continue
            page['resources'].append({'url': url, 'timing': summary([get(url)[0] for _ in range(5)])})
        print(name, {phase: round(page[phase]['median_ms'], 3) for phase in ['enabled','forced_miss','off','keepalive_enabled']}, flush=True)
    stat, body = get('/pagespeed_admin/stats_json')
    result['admin_after'] = json.loads(body) if stat['status'] == 200 else {'status': stat['status']}
    result['completed'] = True
finally:
    Path(a.output).write_text(json.dumps(result, indent=2) + '\n')
