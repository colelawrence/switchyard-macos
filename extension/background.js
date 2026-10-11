// Keeps a connection to the Switchyard app, stores the rule index it sends, and hands it
// links: clicked ones from content.js, and navigations that started some other way.

importScripts("match.js");

const HOST = "dev.switchyard";
const VERSION = chrome.runtime.getManifest().version;

let port = null;
let index = null;
let nextId = 1;
const pending = new Map();
/// URLs the user chose to open here anyway, until a time.
const allowed = new Map();

async function setState(patch) {
  const { state = {} } = await chrome.storage.local.get("state");
  const next = { ...state, ...patch, at: Date.now() };
  await chrome.storage.local.set({ state: next, connected: next.status === "connected" });
  badge(next);
}

function badge(state) {
  const problem = state.status === "missing" || state.status === "error";
  chrome.action.setBadgeText({ text: problem ? "!" : "" });
  chrome.action.setBadgeBackgroundColor({ color: state.status === "missing" ? "#d93025" : "#e37400" });
  chrome.action.setTitle({
    title:
      state.status === "connected" ? "Switchyard"
        : state.status === "missing" ? "Switchyard: install the Switchyard app to hand off links"
          : `Switchyard: ${state.error || "not connected"}`,
  });
}

function connect() {
  if (port) return;
  try {
    port = chrome.runtime.connectNative(HOST);
  } catch (e) {
    gone(String(e));
    return;
  }
  port.onMessage.addListener(receive);
  port.onDisconnect.addListener(() => {
    const error = chrome.runtime.lastError?.message || "Switchyard closed the connection";
    port = null;
    gone(error);
  });
  port.postMessage({ type: "hello", version: VERSION });
}

function gone(error) {
  const missing = /not found|forbidden|not allowed/i.test(error);
  setState({ status: missing ? "missing" : "error", error });
  for (const resolve of pending.values()) resolve(null);
  pending.clear();
  setTimeout(connect, missing ? 30_000 : 3_000);
}

function receive(message) {
  switch (message.type) {
    case "hello":
      index = message.index;
      chrome.storage.local.set({ index });
      setState({ status: "connected", error: message.error || null, app: message.version, browser: message.browser });
      break;
    case "index":
      index = message.index;
      chrome.storage.local.set({ index });
      setState({ error: message.error || null });
      break;
    case "check":
      pending.get(message.id)?.(message.leave);
      pending.delete(message.id);
      break;
  }
}

function send(url) {
  connect();
  if (!port) return false;
  port.postMessage({ type: "open", url });
  return true;
}

/// Asks the app whether a navigation should leave; it says no for links it just sent here.
function check(url) {
  if (!port) return Promise.resolve(null);
  const id = nextId++;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    port.postMessage({ type: "check", id, url });
    setTimeout(() => {
      if (pending.delete(id)) resolve(null);
    }, 2_000);
  });
}

function sameOrigin(a, b) {
  try {
    const origin = new URL(a).origin;
    return origin !== "null" && origin === new URL(b).origin;
  } catch {
    return false;
  }
}

// Catches links opened in a new tab from the context menu, by scripts, by other apps and
// from the address bar. Clicks are already handled by content.js and never get here.
chrome.webNavigation.onBeforeNavigate.addListener(async (details) => {
  if (details.frameId !== 0 || details.documentLifecycle === "prerender") return;
  const { url, tabId } = details;
  if (!index || !port || url.startsWith(chrome.runtime.getURL(""))) return;
  if ((allowed.get(url) || 0) > Date.now()) return;
  const decision = SwitchyardMatch.decide(index, url);
  if (!decision?.leave) return;
  let tab;
  try {
    tab = await chrome.tabs.get(tabId);
  } catch {
    return;
  }
  if (tab.url && sameOrigin(tab.url, url)) return;
  if ((await check(url)) !== true) return;
  if (!send(url)) return;
  const fresh = !tab.url || tab.url === url || /^(about:|chrome:\/\/new-?tab)/.test(tab.url);
  if (fresh) {
    chrome.tabs.remove(tabId).catch(() => {});
  } else {
    const page = chrome.runtime.getURL("handoff.html") + "?" + new URLSearchParams({ url, rule: decision.name });
    chrome.tabs.update(tabId, { url: page }).catch(() => {});
  }
});

chrome.runtime.onMessage.addListener((message, sender, reply) => {
  switch (message.type) {
    case "open":
      reply({ sent: send(message.url) });
      break;
    case "allow":
      allowed.set(message.url, Date.now() + 30_000);
      reply({});
      break;
    case "reconnect":
      if (port) port.disconnect();
      port = null;
      connect();
      reply({});
      break;
  }
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  const url = info.menuItemId === "open-link" ? info.linkUrl : tab?.url;
  if (url) send(url);
});

chrome.runtime.onInstalled.addListener(({ reason }) => {
  if (reason === "install") chrome.tabs.create({ url: chrome.runtime.getURL("welcome.html") });
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: "open-link", title: "Open Link through Switchyard", contexts: ["link"] });
    chrome.contextMenus.create({ id: "open-page", title: "Send Page to Switchyard", contexts: ["page"] });
  });
  chrome.alarms.create("reconnect", { periodInMinutes: 1 });
});

chrome.alarms.onAlarm.addListener(connect);
chrome.runtime.onStartup.addListener(connect);

chrome.storage.local.get("index").then((saved) => {
  index = index || saved.index || null;
});
// Until the app answers, links behave normally.
setState({ status: "connecting" }).then(connect);
