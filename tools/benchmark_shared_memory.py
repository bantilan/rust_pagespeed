#!/usr/bin/env python3
"""Reproducible isolated shared-cache baseline/comparison; never reloads live Nginx."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess
import sys
import tempfile
from datetime import datetime, timezone

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--nginx', required=True)
parser.add_argument('--module', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--samples', type=int, default=100)
parser.add_argument('--cold-runs', type=int, default=5)
parser.add_argument('--port', type=int, default=18088)
args = parser.parse_args()
if args.samples < 1 or args.cold_runs < 1:
    parser.error('sample and cold-run counts must be positive')
repo = Path(__file__).resolve().parent.parent
module = Path(args.module).resolve()
template_path = repo / 'docs/performance/shared-memory/nginx-template.conf'
template = template_path.read_text()
result = {
    'at': datetime.now(timezone.utc).isoformat(),
    'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
    'module_sha256': hashlib.sha256(module.read_bytes()).hexdigest(),
    'nginx': str(Path(args.nginx).resolve()),
    'nginx_version': subprocess.check_output([args.nginx, '-v'], stderr=subprocess.STDOUT, text=True).strip(),
    'template_sha256': hashlib.sha256(template.encode()).hexdigest(),
    'method': 'Fresh isolated application caches/processes; OS caches warm; first full run has HIT/MISS/OFF phases; remaining runs cold-only. Request timings exclude process launch. HTTP client and sampling are tools/profile_html.py.',
    'pages': {},
}
for name, page in [('Moto', 'moto-business-html5-responsive-web-template'),
                   ('Aroma', 'aroma-beauty-and-spa-responsive-bootstrap-template')]:
    runs = []
    for run in range(args.cold_runs):
        prefix = Path(tempfile.mkdtemp(prefix='ps-shared-baseline-'))
        (prefix / 'conf').mkdir()
        (prefix / 'logs').mkdir()
        config = template.replace('@MODULE@', str(module)).replace('@PREFIX@', str(prefix)).replace('@PORT@', str(args.port))
        (prefix / 'conf/nginx.conf').write_text(config)
        output = prefix / 'measurements.json'
        body = prefix / 'warm.html'
        command = [sys.executable, str(repo / 'tools/profile_html.py'), '--nginx', args.nginx,
                   '--prefix', str(prefix), '--port', str(args.port), '--path', f'/test/{page}/index.html',
                   '--samples', str(args.samples), '--output', str(output), '--body-output', str(body)]
        if run:
            command.append('--cold-only')
        subprocess.run(command, check=True, stdout=subprocess.DEVNULL)
        data = json.loads(output.read_text())
        data['prefix'] = str(prefix)
        if body.exists():
            normalized = re.sub(rb'<!-- PageSpeed Cache:.*?-->', b'', body.read_bytes())
            data['normalized_html_sha256'] = hashlib.sha256(normalized).hexdigest()
            data['normalized_html_bytes'] = len(normalized)
            data['pagespeed_refs'] = normalized.count(b'.pagespeed.')
        runs.append(data)
    summary = {'cold_median_ms': statistics.median(r['cold']['total_ms'] for r in runs),
               'warm_hit_median_ms': runs[0]['warm_html_hit']['median_ms'],
               'warm_miss_median_ms': runs[0]['warm_html_miss']['median_ms'], 'runs': runs}
    result['pages'][name] = summary
    print(name, {key: value for key, value in summary.items() if key != 'runs'}, flush=True)
    Path(args.output).write_text(json.dumps(result, indent=2) + '\n')
