// Run: gjs tools/test_font_beacon_dedup.js
const GLib = imports.gi.GLib;
const ByteArray = imports.byteArray;
for (const asset of ['dbg', 'opt']) {
  let storage = {}, posts = 0, accept = true, blocked = false;
  globalThis.window = globalThis;
  globalThis.document = {readyState:'complete', fonts:{forEach: f => f({status:'loaded',family:'Karla',weight:'400',style:'normal'})}};
  globalThis.navigator = {sendBeacon: () => {posts++; return accept;}};
  globalThis.setTimeout = f => f();
  globalThis.sessionStorage = {getItem:k=>{if(blocked)throw Error('blocked');return storage[k];},setItem:(k,v)=>{if(blocked)throw Error('blocked');storage[k]=v;}};
  let source = ByteArray.toString(GLib.file_get_contents('crates/ps-html/assets/font_preload_beacon_'+asset+'.js')[1]);
  function load(){eval(source);}
  function run(n='nonce',p='https://site/page',o='options'){pagespeed.FontPreloads.Run('/beacon',p,o,n);}
  function check(n){if(posts!==n)throw Error(asset+': expected '+n+' posts, got '+posts);}
  load();run();run();check(1);
  load();run();check(1); // Cached HTML reload: same nonce, no duplicate POST.
  run('fresh');check(2); // Fresh learning nonce still sends.
  run('fresh','https://site/other');check(3);
  run('fresh','https://site/page','new-options');check(4);
  accept=false;run('retry');run('retry');check(6); // Rejected queue remains retryable.
  accept=true;run('retry');run('retry');check(7);
  blocked=true;load();run('blocked');run('blocked');check(8);
  print(asset+': PASS reload dedup, fresh nonce, page/options isolation, failed queue retry, blocked storage');
}
