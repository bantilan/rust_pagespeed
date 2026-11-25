import json,statistics,math,collections
from pathlib import Path
base=Path(__file__).parent
cached=json.loads((base/'raw.json').read_text());uncached=json.loads((base/'uncached.json').read_text())
assert cached['restored'] and uncached['restored']
assert cached['original_config_sha256']==uncached['original_config_sha256']
out={}
for phase,data,label in [('hit',cached,'HTML HIT'),('rewrite_config_cache_off',uncached,'Uncached rewrite')]:
 for page in ['/','/hello-world/','/sample-page/']:
  key=label+' '+page;out[key]={}
  for mode in ['off','on']:
   rounds=[r for r in data['rounds'] if r['mode']==mode];rows=[x for r in rounds for x in r['pages'][page][phase] if not x['warmup']];v=sorted(x['total_ms'] for x in rows)
   if phase=='hit':assert all(x['cache']=='HIT' for x in rows)
   else:assert all(x['cache']!='HIT' for x in rows)
   assert all(x['status']==200 for x in rows)
   out[key][mode]={'n':len(v),'median_ms':statistics.median(v),'p95_ms':v[math.ceil(.95*len(v))-1],'round_medians_ms':[statistics.median(x['total_ms'] for x in r['pages'][page][phase] if not x['warmup']) for r in rounds],'cache':dict(collections.Counter(x['cache'] for x in rows)),'font_preloads':sorted(set(x['font_preloads'] for x in rows))}
  out[key]['delta_ms']=out[key]['on']['median_ms']-out[key]['off']['median_ms']
(base/'summary.json').write_text(json.dumps(out,indent=2)+'\n')
for k,v in out.items():print(k,round(v['off']['median_ms'],3),round(v['on']['median_ms'],3),'delta',round(v['delta_ms'],3),'fonts',v['on']['font_preloads'])
