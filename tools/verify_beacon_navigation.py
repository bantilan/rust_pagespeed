"""Run collector lifecycle regressions in Chrome, with no external requests."""
import html,json,pathlib,re,subprocess,tempfile
repo=pathlib.Path(__file__).resolve().parent.parent
results=[]
with tempfile.TemporaryDirectory(prefix='ps-beacon-navigation-') as tmp:
 for asset in ['critical_images_beacon_opt.js','critical_images_beacon_dbg.js']:
  for mode in ['lazy-empty','lazy-delayed','lazy-images','hidden-native','hidden-fallback','hidden-throw']:
   js=(repo/'crates/ps-html/assets'/asset).read_text()
   setup='''window.sent=[];window.XMLHttpRequest=function(){this.open=function(){};this.setRequestHeader=function(){};this.send=function(body){sent.push({transport:'xhr',body:body});show()}};function show(){document.getElementById('result').textContent=JSON.stringify(sent)};'''
   if mode in ['lazy-empty','lazy-delayed','lazy-images']:
    setup+='''window.PerformanceObserver=function(callback){this.observe=function(){setTimeout(function(){callback({getEntries:function(){return [{element:document.querySelector('h1')}]}})},DELAY)};this.takeRecords=function(){return []};this.disconnect=function(){}};'''
    setup=setup.replace('DELAY','100' if mode=='lazy-delayed' else '0')
   else:
    setup+='''window.PerformanceObserver=function(){this.observe=function(){};this.takeRecords=function(){return []};this.disconnect=function(){}};'''
   setup+="Object.defineProperty(navigator,'sendBeacon',{value:function(url,body){"
   if mode=='hidden-fallback':setup+='return false;'
   elif mode=='hidden-throw':setup+="throw new Error('queue unavailable');"
   else:setup+="body.text().then(function(text){sent.push({transport:'beacon',body:text});show()});return true;"
   setup+='}});'
   ending="pagespeed.CriticalImages.Run('/beacon','https://test/page','options',false,false,'testnonce');"
   if mode not in ['lazy-empty','lazy-delayed','lazy-images']:ending+="window.addEventListener('load',function(){window.dispatchEvent(new Event('pagehide'))});"
   path=pathlib.Path(tmp)/(asset+mode+'.html')
   image='<img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==">' if mode=='lazy-images' else ''
   path.write_text('<html><head></head><body>'+image+'<h1>Text</h1><pre id="result"></pre><script>'+setup+js+ending+'</script></body></html>')
   r=subprocess.run(['google-chrome','--headless','--no-sandbox','--disable-gpu','--virtual-time-budget=1500','--dump-dom',path.as_uri()],capture_output=True,text=True,timeout=25,check=True)
   m=re.search(r'<pre id="result">(.*?)</pre>',r.stdout)
   reports=json.loads(html.unescape(m[1])) if m and m[1] else []
   expected='xhr' if mode in ['hidden-fallback','hidden-throw'] else 'beacon'
   ok=len(reports)==1 and reports[0]['transport']==expected and 'n=testnonce' in reports[0]['body'] and ('lcpk=text' if mode in ['lazy-empty','lazy-delayed','lazy-images'] else 'lcpk=unknown') in reports[0]['body']
   if mode=='lazy-images':ok=not reports  # Image pages retain lazyload-driven timing.
   results.append({'asset':asset,'mode':mode,'reports':reports,'passed':ok})
print(json.dumps(results,indent=2))
assert all(x['passed'] for x in results)
