#!/usr/bin/env python3
"""Run on rustpagespeed.com host: sequential loopback TLS/keepalive probes; no mutations."""
import http.client,ssl,socket,time,json,statistics,math,re,subprocess,hashlib
class LocalHTTPS(http.client.HTTPSConnection):
 def connect(self):
  self.sock=self._context.wrap_socket(socket.create_connection(('127.0.0.1',443),timeout=5),server_hostname='rustpagespeed.com')
for enc in ['identity','zstd']:
 rows=[]
 for i in range(33):
  p=subprocess.run(['curl','-sS','--compressed','--max-time','5','--resolve','rustpagespeed.com:443:127.0.0.1','-H','Accept-Encoding: '+enc,'-D','/tmp/rps-early-headers','-w','\nMETRIC %{http_code} %{time_starttransfer} %{size_download}\n','https://rustpagespeed.com/'],capture_output=True)
  m=re.search(rb'METRIC (\d+) ([\d.]+) ([\d.]+)',p.stdout);c=re.findall(rb'PageSpeed Cache: (HIT|MISS|BYPASS)',p.stdout)
  assert p.returncode==0 and m and m[1]==b'200',(p.returncode,p.stderr)
  headers=open('/tmp/rps-early-headers').read().lower();assert ('content-encoding: zstd' in headers)==(enc=='zstd')
  body=p.stdout[:m.start()];body=re.sub(rb'<!-- PageSpeed Cache:.*?-->',b'',body)
  rows.append({'ms':float(m[2])*1000,'bytes':int(m[3]),'cache':c[-1].decode() if c else None,'hash':hashlib.sha256(body).hexdigest()})
 warmup,rows=rows[:3],rows[3:]
 assert all(x['cache']=='HIT' for x in rows),rows
 v=sorted(x['ms'] for x in rows);print(json.dumps({'mode':enc,'warmup':warmup,'median_hit_ms':statistics.median(v),'p95_hit_ms':v[math.ceil(.95*len(v))-1],'rows':rows}),flush=True)
c=LocalHTTPS('rustpagespeed.com',timeout=5,context=ssl.create_default_context());rows=[]
for i in range(31):
 t=time.perf_counter();c.request('GET','/',headers={'Accept-Encoding':'identity'});r=c.getresponse();body=r.read();elapsed=(time.perf_counter()-t)*1000;assert r.status==200
 assert b'PageSpeed Cache: HIT' in body
 if i:rows.append(elapsed)
c.close();print(json.dumps({'mode':'keepalive-identity','median_ms':statistics.median(rows),'samples_ms':rows}),flush=True)
