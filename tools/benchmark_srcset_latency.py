#!/usr/bin/env python3
"""Sequential same-build off/on cache benchmark with raw correctness oracles.
Each round starts a fresh isolated NGINX process/cache for each mode. Startup
is outside the request timer. Do not run builds or other benchmarks alongside.
"""
import argparse
import json
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--nginx', required=True)
p.add_argument('--module', required=True)
p.add_argument('--output-dir', required=True)
p.add_argument('--rounds', type=int, default=5)
p.add_argument('--samples', type=int, default=20)
a = p.parse_args()
assert a.rounds > 0 and a.samples > 0
out = Path(a.output_dir)
out.mkdir(parents=True, exist_ok=True)
base = [sys.executable, str(Path(__file__).with_name('verify_early_html_cache.py')),
        '--nginx', a.nginx, '--module', a.module, '--rum-only', '--samples', str(a.samples)]
runs = {'off': [], 'on': []}
for batch in range(a.rounds):
    for mode in (['off', 'on'] if batch % 2 == 0 else ['on', 'off']):
        path = out / f'{mode}-{batch}.json'
        with path.with_suffix('.log').open('w') as log:
            subprocess.run(base + ['--srcset-policy', mode, '--output', str(path)],
                           stdout=log, stderr=log, check=True)
        sample = json.loads(path.read_text())
        assert sample['canonical_host'] == 'localhost'
        runs[mode].append(sample)
        print(f'batch{batch} {mode} passed', flush=True)
# Persistent disk-cache key separation: exact same URL/host/options except policy.
cache = tempfile.mkdtemp(prefix='ps-srcset-key-cache-')
for mode in ['off', 'on']:
    path = out / f'key-separation-{mode}.json'
    with path.with_suffix('.log').open('w') as log:
        subprocess.run(base + ['--cache-dir', cache, '--srcset-policy', mode,
                               '--output', str(path)], stdout=log, stderr=log, check=True)
# Every verifier asserts expected policy bytes and first MISS; it never clears
# the shared directory between these two processes. No timing claim for this pair.
summary = {'scope': 'loopback fresh HTTP connections, PassThrough+RUM,25ms synthetic origin; no image processing/network resources',
           'cold_definition': 'first measured request in fresh isolated process+disk cache; process startup excluded',
           'warm_miss_definition': 'unique HTML paths in same warm process after HIT oracle; no resource optimizers in fixture',
           'cache_key_policy_separation_verified': True, 'cases': {}}
for mode, values in runs.items():
    summary['cases'][mode] = {}
    for name in ['cold', 'warm_html_misses', 'warm']:
        groups = [[r[name]] if name == 'cold' else r[name] for r in values]
        samples = [s['ms'] for group in groups for s in group]
        batch_medians = [statistics.median(s['ms'] for s in group) for group in groups]
        summary['cases'][mode][name] = {
            'count': len(samples), 'median_ms': statistics.median(samples),
            'p95_ms': sorted(samples)[min(len(samples)-1, int(len(samples)*.95))],
            'batch_medians_ms': batch_medians,
            'batch_median_range_ms': [min(batch_medians), max(batch_medians)]}
summary['module_sha256'] = runs['on'][0]['module_sha256']
summary['source_commit'] = runs['on'][0]['source_commit']
(out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary))
