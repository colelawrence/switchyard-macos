// Decides, from the index Switchyard sends, whether a link leaves this browser.
// Mirrors `Pattern::matches` and `strip_tracking` in the app; the app's tests run this
// file under Node to check the two agree.

(function (root) {
  "use strict";

  function compile(rule) {
    if (!rule._re) {
      const re = (src, flags) => (src == null ? null : new RegExp(src, flags));
      rule._re = {
        host: re(rule.host, "i"),
        path: re(rule.path, ""),
        query: rule.query.map(([key, value]) => [key, re(value, "")]),
        fragment: re(rule.fragment, ""),
      };
    }
    return rule._re;
  }

  function stripTracking(url, tracking) {
    if (!/^https?:$/.test(url.protocol) || !url.search) return;
    const pairs = url.search.slice(1).split("&");
    const kept = pairs.filter((pair) => {
      const key = pair.split("=")[0];
      return !(key.startsWith("utm_") || tracking.includes(key));
    });
    if (kept.length !== pairs.length) url.search = kept.join("&");
  }

  function matches(rule, url) {
    const scheme = url.protocol.slice(0, -1);
    const schemeOk =
      rule.scheme === "web" ? scheme === "http" || scheme === "https"
        : rule.scheme === "*" || rule.scheme === scheme;
    const opaque = url.host === "" && !url.pathname.startsWith("/");
    if (!schemeOk || rule.opaque !== opaque) return false;

    const re = compile(rule);
    const host = url.hostname + (url.port ? ":" + url.port : "");
    if (re.host && !re.host.test(host)) return false;
    if (re.path && !re.path.test(url.pathname)) return false;
    if (re.query.length) {
      const pairs = url.search
        .slice(1)
        .split("&")
        .filter((p) => p)
        .map((p) => {
          const i = p.indexOf("=");
          return i < 0 ? [p, ""] : [p.slice(0, i), p.slice(i + 1)];
        });
      for (const [key, value] of re.query) {
        if (!pairs.some(([k, v]) => k === key && (!value || value.test(v)))) return false;
      }
    }
    if (re.fragment && !re.fragment.test(url.hash.slice(1))) return false;
    return true;
  }

  /// The first rule that matches `href`, as `{ name, leave }`, or null when none does.
  function decide(index, href) {
    if (!index) return null;
    let url;
    try {
      url = new URL(href);
    } catch {
      return null;
    }
    if (index.stripTracking) stripTracking(url, index.tracking);
    for (const rule of index.rules) {
      try {
        if (matches(rule, url)) return { name: rule.name, leave: rule.leave };
      } catch {
        // A regex this engine cannot run; the rule never claims links here.
      }
    }
    return null;
  }

  const api = { decide };
  root.SwitchyardMatch = api;
  if (typeof module === "object" && module.exports) module.exports = api;
})(globalThis);
