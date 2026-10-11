const $ = (id) => document.getElementById(id);
const VERSION = chrome.runtime.getManifest().version;

async function render() {
  const { state = {}, index } = await chrome.storage.local.get(["state", "index"]);
  $("dot").className = "dot " + (state.status || "");
  const leaving = index ? index.rules.filter((r) => r.leave).length : 0;
  switch (state.status) {
    case "connected":
      $("status").textContent = "Connected to Switchyard";
      $("detail").textContent =
        `${leaving} ${leaving === 1 ? "rule sends" : "rules send"} links out of ${state.browser?.name || "this browser"}.` +
        (state.app && state.app !== VERSION
          ? ` Switchyard ${state.app} is installed; reload this extension (${VERSION}) from the Extensions page to update it.`
          : "") +
        (state.error ? ` ${state.error}` : "");
      break;
    case "missing":
      $("status").textContent = "Switchyard isn't installed";
      $("detail").textContent =
        "Install the Switchyard app and open it once. It connects itself to this browser, and links here behave normally until it does.";
      break;
    case "error":
      $("status").textContent = "Can't reach Switchyard";
      $("detail").textContent = state.error || "";
      break;
    default:
      $("status").textContent = "Connecting…";
      $("detail").textContent = "";
  }

  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (state.status === "connected" && tab?.url && !/^(chrome|dia|arc|edge|brave)/.test(tab.url) && !tab.url.startsWith(location.origin)) {
    $("page").hidden = false;
    const decision = SwitchyardMatch.decide(index, tab.url);
    $("verdict").textContent = !decision
      ? "No rule matches this page."
      : decision.leave
        ? `“${decision.name}” sends this page to another app.`
        : `“${decision.name}” keeps this page here.`;
    $("send").onclick = async () => {
      await chrome.runtime.sendMessage({ type: "open", url: tab.url });
      window.close();
    };
  }
}

$("reconnect").onclick = async () => {
  await chrome.runtime.sendMessage({ type: "reconnect" });
  setTimeout(render, 500);
};
chrome.storage.onChanged.addListener(render);
render();
