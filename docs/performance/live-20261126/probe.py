import subprocess,tempfile,json,re,statistics,math,sys,time
local='--local' in sys.argv
out={'scope':'server loopback TLS' if local else 'public HTTPS from development host','at_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),'method':'HTTP/1.1 fresh TLS connections, identity encoding, 3 warmups then 20 sequential requests per URL; existing caches; no purge/config changes','pages':{}}
with tempfile.TemporaryDirectory() as d:
 for path in ['/','/hello-world/','/sample-page/']:
  rows=[]
  for i in range(23):
   cmd=['curl','-sS','--http1.1','--max-time','15','-A','Mozilla/5.0 Chrome/131.0.0.0 Safari/537.36','-H','Accept: text/html','-H','Accept-Encoding: identity','-D',d+'/headers','-o',d+'/body','-w','%{json}','https://rustpagespeed.com'+path]
   if local:cmd+=['--resolve','rustpagespeed.com:443:127.0.0.1']
   p=subprocess.run(cmd,capture_output=True,text=True)
   if p.returncode: rows.append({'error':p.stderr});continue
   m=json.loads(p.stdout);body=open(d+'/body','rb').read();h=open(d+'/headers').read();cache=re.findall(rb'PageSpeed Cache: (HIT|MISS|BYPASS)',body)
   rows.append({'warmup':i<3,'status':m['http_code'],'cache':cache[-1].decode() if cache else 'N/A','ttfb_ms':m['time_starttransfer']*1000,'total_ms':m['time_total']*1000,'connect_ms':m['time_connect']*1000,'tls_ready_ms':m['time_appconnect']*1000,'bytes':m['size_download'],'server':re.findall(r'(?im)^server: (.*)',h),'rum':b'pagespeed.addInstrumentationInit' in body})
  groups={}
  for state in sorted({r.get('cache','ERROR') for r in rows[3:]}):
   g=[r for r in rows[3:] if r.get('cache','ERROR')==state];good=[r for r in g if 'ttfb_ms' in r]
   groups[state]={'n':len(g)}
   for field in ['ttfb_ms','total_ms','tls_ready_ms']:
    v=sorted(r[field] for r in good)
    if v:groups[state][field]={'median':statistics.median(v),'p95':v[math.ceil(len(v)*.95)-1],'min':v[0],'max':v[-1]}
  out['pages'][path]={'summary':groups,'rows':rows}
print(json.dumps(out,indent=2))
