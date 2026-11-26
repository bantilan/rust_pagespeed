import http.client,ssl,socket,time,json,re,statistics,math,sys
local='--local' in sys.argv
class LocalHTTPS(http.client.HTTPSConnection):
 def connect(self):self.sock=self._context.wrap_socket(socket.create_connection(('127.0.0.1',443),timeout=15),server_hostname='rustpagespeed.com')
out={}
for path in ['/','/hello-world/','/sample-page/']:
 c=(LocalHTTPS if local else http.client.HTTPSConnection)('rustpagespeed.com',timeout=15,context=ssl.create_default_context());rows=[]
 for i in range(23):
  t=time.perf_counter();c.request('GET',path,headers={'User-Agent':'Mozilla/5.0 Chrome/131.0.0.0 Safari/537.36','Accept':'text/html','Accept-Encoding':'identity'});r=c.getresponse();first=(time.perf_counter()-t)*1000;body=r.read();total=(time.perf_counter()-t)*1000;marks=re.findall(rb'PageSpeed Cache: (HIT|MISS|BYPASS)',body)
  rows.append({'warmup':i<3,'status':r.status,'cache':marks[-1].decode() if marks else 'N/A','ttfb_headers_ms':first,'total_ms':total,'bytes':len(body),'server':r.getheader('Server'),'detail':re.findall(rb'PageSpeed Cache Detail: ([a-z-]+)',body)[-1:].pop().decode() if re.findall(rb'PageSpeed Cache Detail: ([a-z-]+)',body) else None})
 c.close();v=sorted(x['total_ms'] for x in rows[3:]);out[path]={'median_total_ms':statistics.median(v),'p95_total_ms':v[math.ceil(.95*len(v))-1],'rows':rows}
print(json.dumps({'scope':'loopback' if local else 'public','method':'HTTP/1.1 persistent TLS connection per page, 3 warmups +20 requests, identity encoding; headers timing includes header parsing','pages':out},indent=2))
