const BASE = "http://127.0.0.1:43721";
const MIN_INTERVAL_MS = 1500;
const lastSentByTab = new Map();

function isDiscordUrl(value) {
  try {
    const url = new URL(value);
    return url.protocol === "https:" &&
      ["discord.com", "ptb.discord.com", "canary.discord.com"].includes(url.hostname);
  } catch {
    return false;
  }
}

async function request(path, options = {}) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 1800);
  try {
    const response = await fetch(`${BASE}${path}`, {
      cache: "no-store",
      credentials: "omit",
      redirect: "error",
      ...options,
      signal: controller.signal,
    });
    if (!response.ok && response.status !== 204) {
      throw new Error(`Local verifier returned HTTP ${response.status}`);
    }
    return response.status === 204 ? null : await response.json();
  } finally {
    clearTimeout(timeout);
  }
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message?.kind === "tesktop-receiver-report") {
    if (!sender.tab?.id || !isDiscordUrl(sender.tab.url) ||
        !message.report || typeof message.report !== "object") {
      sendResponse({ ok: false, error: "Report did not come from a Discord tab." });
      return;
    }

    const now = Date.now();
    const last = lastSentByTab.get(sender.tab.id) ?? 0;
    if (now - last < MIN_INTERVAL_MS) {
      sendResponse({ ok: true, throttled: true });
      return;
    }
    lastSentByTab.set(sender.tab.id, now);

    request("/v1/receiver", {
      method: "POST",
      headers: { "Content-Type": "text/plain;charset=UTF-8" },
      body: JSON.stringify(message.report),
    }).then(() => sendResponse({ ok: true }))
      .catch(error => sendResponse({ ok: false, error: String(error?.message ?? error) }));
    return true;
  }

  if (message?.kind === "tesktop-read-status") {
    request("/v1/status")
      .then(status => sendResponse({ ok: true, status }))
      .catch(error => sendResponse({ ok: false, error: String(error?.message ?? error) }));
    return true;
  }

  sendResponse({ ok: false, error: "Unknown request." });
});

chrome.tabs.onRemoved.addListener(tabId => lastSentByTab.delete(tabId));
