// Catches clicks on links that a Switchyard rule sends elsewhere, before the page or the
// browser acts on them. The decision is synchronous, from the index kept in storage.

(() => {
  "use strict";

  let index = null;
  let connected = false;

  chrome.storage.local.get(["index", "connected"], (saved) => {
    index = saved.index || null;
    connected = !!saved.connected;
  });
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area !== "local") return;
    if ("index" in changes) index = changes.index.newValue || null;
    if ("connected" in changes) connected = !!changes.connected.newValue;
  });

  function linkOf(event) {
    for (const el of event.composedPath()) {
      if (el instanceof Element && (el.localName === "a" || el.localName === "area") && el.hasAttribute("href")) {
        return el;
      }
    }
    return null;
  }

  function onClick(event) {
    // Without the app, or after the extension reloads under this page, links behave normally.
    if (!connected || !index || !chrome.runtime?.id) return;
    if (event.type === "click" ? event.button !== 0 : event.button !== 1) return;
    const link = linkOf(event);
    if (!link || link.hasAttribute("download")) return;
    const href = link.href;
    if (typeof href !== "string" || /^javascript:/i.test(href)) return;
    let url;
    try {
      url = new URL(href);
    } catch {
      return;
    }
    // Links within the site you are already on stay here.
    if (url.origin !== "null" && url.origin === location.origin) return;
    const decision = SwitchyardMatch.decide(index, href);
    if (!decision || !decision.leave) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    chrome.runtime.sendMessage({ type: "open", url: href }).catch(() => {});
  }

  window.addEventListener("click", onClick, true);
  window.addEventListener("auxclick", onClick, true);
})();
