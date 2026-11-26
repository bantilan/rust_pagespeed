"""Execute both shipped collectors in Chrome with LCP unavailable; no live traffic."""
import tempfile,subprocess,pathlib,json,re
repo=pathlib.Path(__file__).resolve().parent.parent
results=[]
with tempfile.TemporaryDirectory(prefix='ps-empty-beacon-js-') as tmp:
 for asset in ['critical_images_beacon_opt.js','critical_images_beacon_dbg.js']:
  js=(repo/'crates/ps-html/assets'/asset).read_text()
  html='''<html><head><script>window.PerformanceObserver=undefined;window.sent=[];window.XMLHttpRequest=function(){this.open=function(){};this.setRequestHeader=function(){};this.send=function(body){sent.push(body);document.getElementById('result').textContent=JSON.stringify(sent)}};</script></head><body><h1>Text</h1><pre id="result"></pre><script>'''+js+'''\npagespeed.CriticalImages.Run('/beacon','https://test/page','options',true,false,'testnonce');</script></body></html>'''
  path=pathlib.Path(tmp)/asset.replace('.js','.html');path.write_text(html)
  r=subprocess.run(['google-chrome','--headless','--no-sandbox','--disable-gpu','--virtual-time-budget=1500','--dump-dom',path.as_uri()],capture_output=True,text=True,timeout=25,check=True)
  m=re.search(r'<pre id="result">(.*?)</pre>',r.stdout)
  import html as h
  reports=json.loads(h.unescape(m[1])) if m and m[1] else []
  assert len(reports)==1 and 'lcpk=unknown' in reports[0] and 'n=testnonce' in reports[0], (asset,reports)
  results.append({'asset':asset,'reports':reports,'passed':True})
print(json.dumps(results,indent=2))
