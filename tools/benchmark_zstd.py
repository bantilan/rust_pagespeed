#!/usr/bin/env python3
"""Isolated identity/zstd HTML HIT/MISS and warm-resource wire latency."""
import argparse
import ctypes
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import signal
import statistics
import subprocess
import tempfile
import time

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--nginx',required=True);p.add_argument('--module',required=True);p.add_argument('--output',required=True)
p.add_argument('--samples',type=int,default=100);p.add_argument('--port',type=int,default=18095)
a=p.parse_args();assert a.samples>0
repo=Path(__file__).resolve().parent.parent
lib=ctypes.CDLL('libzstd.so.1');lib.ZSTD_decompress.restype=ctypes.c_size_t
lib.ZSTD_decompress.argtypes=[ctypes.c_void_p,ctypes.c_size_t,ctypes.c_void_p,ctypes.c_size_t]
lib.ZSTD_isError.argtypes=[ctypes.c_size_t];lib.ZSTD_isError.restype=ctypes.c_uint
scratch=ctypes.create_string_buffer(2*1024*1024)
result={'module_sha256':hashlib.sha256(Path(a.module).read_bytes()).hexdigest(),'scope':'Ordinary single-worker private NGINX; client wire timing excludes decompression; no deployment/cache purge of installed server','runs':[]}
path='/test/moto-business-html5-responsive-web-template/index.html'
def get(encoding='identity',unique=False,url=path):
 c=http.client.HTTPConnection('127.0.0.1',a.port,timeout=10)
 headers={'Host':'localhost','Accept-Encoding':encoding}
 if unique:headers['X-PS-Benchmark']=str(time.time_ns())
 t=time.perf_counter_ns()
 try:
  c.request('GET',url,headers=headers);r=c.getresponse();wire=r.read();elapsed=(time.perf_counter_ns()-t)/1e6
  assert r.status==200,r.status
  coded=r.getheader('Content-Encoding');body=wire
  if coded=='zstd':
   size=lib.ZSTD_decompress(scratch,len(scratch),wire,len(wire));assert not lib.ZSTD_isError(size),'invalid frame';body=scratch.raw[:size]
  else:assert coded is None,coded
  marker=body.rsplit(b'PageSpeed Cache:',1)[-1].split(b'|',1)[0].strip().decode() if b'PageSpeed Cache:' in body else 'absent'
  return {'ms':elapsed,'wire_bytes':len(wire),'encoding':coded,'marker':marker},body
 finally:c.close()
def measure(encoding,unique=False,url=path):
 rows=[]
 for _ in range(a.samples):
  sample,body=get(encoding,unique,url);rows.append(sample)
  if enabled and encoding=='zstd':assert sample['encoding']=='zstd',sample
  else:assert sample['encoding'] is None,sample
  if url==path:assert (sample['marker']!='HIT' if unique else sample['marker']=='HIT'),sample
 return {'median_ms':statistics.median(s['ms'] for s in rows),'samples':rows}
try:
 for enabled in [False,True]:
  prefix=Path(tempfile.mkdtemp(prefix='ps-zstd-http-'));(prefix/'conf').mkdir();(prefix/'logs').mkdir()
  conf=(repo/'docs/performance/shared-memory/nginx-template.conf').read_text().replace('@MODULE@',str(Path(a.module).resolve())).replace('@PREFIX@',str(prefix)).replace('@PORT@',str(a.port))
  conf=conf.replace('pagespeed RewriteLevel CoreFilters;', 'pagespeed RewriteLevel CoreFilters;\npagespeed '+('Enable' if enabled else 'Disable')+'Filters compress_zstd;')
  (prefix/'conf/nginx.conf').write_text(conf)
  subprocess.run([a.nginx,'-t','-p',str(prefix)+'/', '-c','conf/nginx.conf'],check=True,capture_output=True)
  run={'enabled':enabled,'prefix':str(prefix),'config_sha256':hashlib.sha256(conf.encode()).hexdigest()};result['runs'].append(run)
  with (prefix/'runner.log').open('ab') as log:
   process=subprocess.Popen([a.nginx,'-p',str(prefix)+'/', '-c','conf/nginx.conf'],stdout=log,stderr=log,start_new_session=True)
   try:
    for encoding in ['identity','zstd']:
     deadline=time.monotonic()+30;hits=0
     while hits<3:
      try:
       sample,body=get(encoding);hits=hits+1 if sample['marker']=='HIT' else 0
      except OSError:hits=0
      assert time.monotonic()<deadline,'never warmed';time.sleep(.02)
    run['identity_hit']=measure('identity');run['zstd_hit']=measure('zstd')
    run['identity_miss']=measure('identity',True);run['zstd_miss']=measure('zstd',True)
    _,identity=get('identity');_,compressed=get('zstd')
    normalize=lambda body:re.sub(rb'<!-- PageSpeed Cache:.*?-->',b'',body)
    assert normalize(identity)==normalize(compressed),'HTML changed by encoding'
    run['normalized_html_sha256']=hashlib.sha256(normalize(identity)).hexdigest()
    matches=re.findall(rb'''href=["']([^"']+\.pagespeed\.[^"']+\.css)["']''',identity);assert matches,'no optimized CSS'
    css=matches[0].decode();css=css if css.startswith('/') else path.rsplit('/',1)[0]+'/'+css
    _,plain=get('identity',url=css);_,decoded=get('zstd',url=css);assert plain==decoded,'resource changed'
    run['resource_identity']=measure('identity',url=css);run['resource_zstd']=measure('zstd',url=css)
    text=Path(f'/proc/{process.pid}/smaps_rollup').read_text()
    run['memory_kib']={k:int(v) for k,v in re.findall(r'^(Rss|Pss):\s+(\d+) kB',text,re.M)}
    run['passed']=True;print(enabled,{k:round(v['median_ms'],3) for k,v in run.items() if isinstance(v,dict) and 'median_ms' in v},flush=True)
   finally:
    try:os.killpg(process.pid,signal.SIGTERM);process.wait(timeout=8)
    except (ProcessLookupError,subprocess.TimeoutExpired):
     try:os.killpg(process.pid,signal.SIGKILL)
     except ProcessLookupError:pass
     process.wait(timeout=5)
 result['passed']=True
finally:Path(a.output).write_text(json.dumps(result,indent=2)+'\n')
