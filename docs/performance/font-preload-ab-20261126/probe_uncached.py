"""Run on the authorized live host. Restore exact original site config in finally."""
import http.client,ssl,socket,time,json,re,statistics,subprocess,pathlib,hashlib
config=pathlib.Path('/etc/nginx/sites-enabled/rustpagespeed.com').resolve()
original=config.read_bytes()
assert original.count(b'    pagespeed on;')==1
assert b'pagespeed DisableFilters preload_critical_fonts;' not in original
class LocalHTTPS(http.client.HTTPSConnection):
 def connect(self):self.sock=self._context.wrap_socket(socket.create_connection(('127.0.0.1',443),timeout=15),server_hostname='rustpagespeed.com')
def reload():
 subprocess.run(['nginx','-t'],check=True,stdout=subprocess.DEVNULL)
 subprocess.run(['nginx','-s','reload'],check=True,stdout=subprocess.DEVNULL)
 time.sleep(2)
def run(mode,round):
 directive=b'    pagespeed '+(b'EnableFilters' if mode=='on' else b'DisableFilters')+b' preload_critical_fonts;'
 uncached=original.replace(b'pagespeed HtmlCache on;',b'pagespeed HtmlCache off;').replace(b'pagespeed HtmlCacheEarlyLookup on;',b'pagespeed HtmlCacheEarlyLookup off;')
 assert uncached!=original
 config.write_bytes(uncached.replace(b'    pagespeed on;',b'    pagespeed on;\n'+directive))
 reload()
 result={'mode':mode,'round':round,'config_sha256':hashlib.sha256(config.read_bytes()).hexdigest(),'pages':{}}
 for path in ['/','/hello-world/','/sample-page/']:
  result['pages'][path]={}
  for phase in ['rewrite_config_cache_off']:
   c=LocalHTTPS('rustpagespeed.com',timeout=15,context=ssl.create_default_context());rows=[]
   for i in range(35):
    headers={'User-Agent':'Mozilla/5.0 Chrome/131.0.0.0 Safari/537.36','Accept':'text/html','Accept-Encoding':'identity'}
    # Both HTML cache paths are disabled in the temporary config.
    t=time.perf_counter();c.request('GET',path,headers=headers);r=c.getresponse();body=r.read();elapsed=(time.perf_counter()-t)*1000
    cache=re.findall(rb'PageSpeed Cache: (HIT|MISS|BYPASS)',body)
    links=re.findall(rb'<link\b[^>]*>',body,re.I)
    rows.append({'warmup':i<5,'total_ms':elapsed,'status':r.status,'cache':cache[-1].decode() if cache else 'N/A','bytes':len(body),'font_preloads':sum(bool(re.search(rb'\bas=[\x22\x27]?font\b',x,re.I)) for x in links)})
    assert r.status==200
    assert not cache or cache[-1]!=b'HIT', 'invalid rewrite benchmark: cache HIT'
   c.close();result['pages'][path][phase]=rows
 return result
out={'original_config_sha256':hashlib.sha256(original).hexdigest(),'method':'server loopback TLS persistent connection, identity, desktop UA; 5warmup+30measured per phase/page/round; OFF ON ON OFF; no cache purge; HtmlCache and HtmlCacheEarlyLookup disabled in config; resource-warm uncached rewrites, not true cold misses','rounds':[]}
try:
 for i,mode in enumerate(['off','on','on','off']):
  out['rounds'].append(run(mode,i));print('completed',i,mode,flush=True,file=__import__('sys').stderr)
finally:
 config.write_bytes(original);reload()
 out['restored']=config.read_bytes()==original
 print(json.dumps(out,indent=2),flush=True)
