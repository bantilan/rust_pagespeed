/* Automatic sizing measurements only. Does not change image URLs or sizes. */
(function (w, d) {
  "use strict";
  w.pagespeed = w.pagespeed || {};
  w.pagespeed.collectAutomaticSizes = function (endpoint, scope, nonce) {
    var sent = false;
    if (!Number.isInteger || !Number.isFinite || !w.Element || !w.Element.prototype.closest) return;
    if (!/^[a-f0-9]{64}$/.test(scope) || !/^[A-Za-z0-9_-]{11}$/.test(nonce)) return;
    var target;
    try {
      target = new URL(endpoint, w.location.href);
      if (target.origin !== w.location.origin) return;
    } catch (_) { return; }
    function collect() {
      if (sent) return;
      sent = true;
      var viewport = w.innerWidth;
      if (!Number.isInteger(viewport) || viewport < 1 || viewport > 16384 ||
          !w.matchMedia("(width: " + viewport + "px)").matches) return;
      var images = d.images;
      if (images.length > 512) return;
      var widths = Object.create(null), seen = Object.create(null), count = 0;
      for (var i = 0; i < images.length; i++) {
        var image = images[i], id = image.getAttribute("data-pagespeed-auto-size-id");
        if (!id || !/^[a-f0-9]{64}$/.test(id)) continue;
        if (seen[id]) { if (widths[id]) { delete widths[id]; count--; } continue; }
        seen[id] = true;
        if (image.closest("picture,noscript,template,svg") ||
            image.hasAttribute("data-pagespeed-no-transform") ||
            image.hasAttribute("pagespeed_no_transform")) continue;
        var style = w.getComputedStyle(image), rect = image.getBoundingClientRect();
        if (style.display === "none" || style.visibility !== "visible" || rect.width <= 0 || rect.height <= 0) continue;
        // Avoid underestimating when transforms shrink the visual box.
        var width = Math.max(rect.width, image.clientWidth), fixed = Math.ceil(width * 64);
        if (!Number.isFinite(fixed) || fixed < 1 || fixed > 1048576) continue;
        widths[id] = fixed;
        if (++count > 64) return;
      }
      if (!count || viewport !== w.innerWidth) return;
      var wire = "automatic-sizes/1 " + viewport + "\n";
      Object.keys(widths).sort().forEach(function (id) { wire += id + " " + widths[id] + "\n"; });
      if (wire.length > 8192 || w.location.href.length > 8192) return;
      var body = "url=" + encodeURIComponent(w.location.href) + "&asi=" + scope + "&asn=" + nonce + "&asm=" + encodeURIComponent(wire);
      if (body.length > 65536) return;
      try {
        if (w.navigator.sendBeacon && w.navigator.sendBeacon(target.href, new Blob([body], {type: "application/x-www-form-urlencoded;charset=UTF-8"}))) return;
      } catch (_) {}
      if (w.fetch) w.fetch(target.href, {method: "POST", body: body, credentials: "same-origin", mode: "same-origin", keepalive: true,
        headers: {"Content-Type": "application/x-www-form-urlencoded;charset=UTF-8"}}).catch(function () {});
    }
    if (d.readyState === "complete") w.setTimeout(collect, 0);
    else w.addEventListener("load", collect, {once: true});
  };
})(window, document);
