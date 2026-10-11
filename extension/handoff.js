// Shown in place of a page Switchyard took, so the tab is not left half-loaded.
const params = new URLSearchParams(location.search);
const url = params.get("url") || "";
document.getElementById("url").textContent = url;
document.getElementById("rule").textContent = `“${params.get("rule") || "A rule"}” opens this link in another app.`;
document.getElementById("back").onclick = () => history.back();
document.getElementById("here").onclick = async () => {
  await chrome.runtime.sendMessage({ type: "allow", url });
  location.replace(url);
};
