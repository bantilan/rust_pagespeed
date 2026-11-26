"""Read-only live request audit; no config edits, reload, purge or deployment."""
import http.client,ssl,socket,time,json,re,statistics,random,collections
class LocalHTTPS(http.client.HTTPSConnection):
 def connect(self):self.sock=self._context.wrap_socket(socket.create_connection(('127.0.0.1',443),timeout=15),server_hostname='rustpagespeed.com')
rng=random.Random(20261126);out={}
for page in ['/','/hello-world/','/sample-page/']:
 c=LocalHTTPS('rustpagespeed.com',timeout=15,context=ssl.create_default_context());rows=[]
 for i in range(25):
  phases=['normal','analytics_cookie'];rng.shuffle(phases)
  for phase in phases:
   headers={'Accept-Encoding':'identity','Accept':'text/html','User-Agent':'Mozilla/5.0 Chrome/131.0.0.0 Safari/537.36'}
   path=page
   if phase=='analytics_cookie':headers['Cookie']='_ga=GA1.1.123.456'
   if phase=='unique_header':headers['X-PS-Latency-Audit']=str(time.time_ns())
   if phase=='pagespeed_off':path+='?PageSpeed=off'
   t=time.perf_counter();c.request('GET',path,headers=headers);r=c.getresponse();body=r.read();elapsed=(time.perf_counter()-t)*1000;marks=re.findall(rb'PageSpeed Cache: (HIT|MISS|BYPASS)',body)
   rows.append({'phase':phase,'warmup':i<5,'ms':elapsed,'status':r.status,'cache':marks[-1].decode() if marks else 'absent','bytes':len(body),'page_speed_header':r.getheader('X-Page-Speed'),'server':r.getheader('Server')})
 c.close();summary={}
 for phase in ['normal','analytics_cookie']:
  g=[r for r in rows if r['phase']==phase and not r['warmup']];v=sorted(r['ms'] for r in g)
  summary[phase]={'median_ms':statistics.median(v),'p95_ms':v[18],'cache':dict(collections.Counter(r['cache'] for r in g)),'statuses':dict(collections.Counter(r['status'] for r in g))}
 out[page]={'summary':summary,'rows':rows}
print(json.dumps(out,indent=2))
