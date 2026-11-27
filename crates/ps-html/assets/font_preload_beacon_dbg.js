// Font preload beacon (debug): reports loaded font faces so the
// server confirms critical families over visits. Never throws:
// measurement must not break pages.
window.pagespeed = window.pagespeed || {};
window.pagespeed.FontPreloads = (function() {
  function normalizeWeight(weight) {
    if (weight === 'normal') {
      return 400;
    }
    if (weight === 'bold') {
      return 700;
    }
    var parsed = parseInt(weight, 10);
    if (isNaN(parsed)) {
      return 400;
    }
    return parsed;
  }
  function normalizeStyle(style) {
    var first = (style || 'normal').toLowerCase().split(' ')[0];
    if (first === 'italic') {
      return 'i';
    }
    if (first === 'oblique') {
      return 'o';
    }
    return 'n';
  }
  function collect() {
    var out = [];
    var seen = {};
    if (!document.fonts || !document.fonts.forEach) {
      return out;
    }
    try {
      document.fonts.forEach(function(face) {
        if (face.status !== 'loaded' || out.length >= 8) {
          return;
        }
        var family = (face.family || '').trim().toLowerCase();
        if (!family) {
          return;
        }
        var key = family + '|' + normalizeWeight(face.weight) + '|' + normalizeStyle(face.style);
        if (!seen[key]) {
          seen[key] = 1;
          out.push(key);
        }
      });
    } catch (ignored) {
      return [];
    }
    return out;
  }
  var sent = {};
  function send(beaconUrl, pageUrl, oh, nonce) {
    var storageKey = 'pagespeed.font-report:' + JSON.stringify([pageUrl, oh]);
    if (sent[storageKey] === nonce) { return; }
    try {
      if (window.sessionStorage.getItem(storageKey) === nonce) { return; }
    } catch (ignored) {}

    var faces;
    try {
      faces = collect();
    } catch (ignored) {
      return;
    }
    if (!faces.length) {
      return;
    }
    var url = beaconUrl + '?url=' + encodeURIComponent(pageUrl) +
        '&oh=' + encodeURIComponent(oh) +
        '&n=' + encodeURIComponent(nonce) +
        '&ff=' + encodeURIComponent(faces.join(','));
    try {
      var queued = false;
      if (navigator.sendBeacon) {
        queued = navigator.sendBeacon(url);
      } else {
        new Image().src = url;
        queued = true;
      }
      if (queued) {
        sent[storageKey] = nonce;
        try { window.sessionStorage.setItem(storageKey, nonce); } catch (ignored) {}
      }
    } catch (ignored) {}
  }
  function run(beaconUrl, pageUrl, oh, nonce) {
    function go() {
      function fire() {
        setTimeout(function() {
          send(beaconUrl, pageUrl, oh, nonce);
        }, 500);
      }
      if (document.fonts && document.fonts.ready && document.fonts.ready.then) {
        document.fonts.ready.then(fire, fire);
      } else {
        fire();
      }
    }
    if (document.readyState === 'complete') {
      go();
    } else {
      window.addEventListener('load', go);
    }
  }
  return {Run: run};
})();
