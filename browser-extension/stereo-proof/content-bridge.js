(() => {
  "use strict";
  const marker = "tesktop-stereo-proof-v1";
  let lastMessageWarningAt = 0;

  // Support callback-only and Promise-returning extension APIs. Keep errors
  // visible in the Discord page console instead of silently dropping them.
  const sendRuntimeMessage = (message, onResponse = null) => {
    const warn = error => {
      if (Date.now() - lastMessageWarningAt < 10_000) return;
      lastMessageWarningAt = Date.now();
      console.warn("[AscendCord Stereo Proof] Extension message failed:",
        String(error?.message ?? error).slice(0, 180));
    };
    try {
      const pending = chrome.runtime.sendMessage(message, response => {
        const error = chrome.runtime.lastError;
        if (error) warn(error);
        else onResponse?.(response);
      });
      if (pending && typeof pending.catch === "function") pending.catch(warn);
    } catch (error) {
      warn(error);
    }
  };

  // This heartbeat proves the isolated content script reached the Discord tab,
  // independently of whether the main-world WebRTC observer is working.
  // The reply says whether AscendCord is running a lab test, which the page needs before
  // Discord opens the microphone.
  const heartbeat = () => sendRuntimeMessage({
    kind: "tesktop-content-bridge-heartbeat",
  }, response => {
    if (typeof response?.lab_armed !== "boolean") return;
    window.postMessage({
      source: "tesktop-stereo-proof-control",
      labArmed: response.lab_armed,
    }, location.origin);
  });
  heartbeat();
  setInterval(heartbeat, 3000);

  window.addEventListener("message", event => {
    if (event.source !== window || event.origin !== location.origin ||
        event.data?.source !== marker) return;

    if (event.data.observer && typeof event.data.observer === "object") {
      sendRuntimeMessage({
        kind: "tesktop-observer-status",
        observer: event.data.observer,
      });
    }

    const report = event.data.report;
    if (!report || report.protocol !== 1 || !Array.isArray(report.streams) ||
        report.streams.length > 16) return;

    sendRuntimeMessage({ kind: "tesktop-receiver-report", report });
  });

  // Developer requests from the page: { source: "tesktop-stereo-proof-dev", id, request }.
  // The reply comes back as { source: "tesktop-stereo-proof-dev-reply", id, response }.
  window.addEventListener("message", event => {
    if (event.source !== window || event.origin !== location.origin ||
        event.data?.source !== "tesktop-stereo-proof-dev" ||
        typeof event.data.request !== "string") return;
    const { id, request } = event.data;
    const reply = response => window.postMessage({
      source: "tesktop-stereo-proof-dev-reply", id, response,
    }, location.origin);
    try {
      chrome.runtime.sendMessage({ kind: "tesktop-dev", request: request.slice(0, 32) }, response => {
        const error = chrome.runtime.lastError;
        reply(error ? { ok: false, error: String(error.message ?? error) } : response);
      });
    } catch (error) {
      reply({ ok: false, error: String(error?.message ?? error) });
    }
  });

  chrome.runtime.onMessage.addListener(message => {
    if (message?.kind === "tesktop-lab-songs" && message.songs) {
      window.postMessage({ source: "tesktop-stereo-proof-control", songs: message.songs }, location.origin);
      return;
    }
    if (message?.kind === "tesktop-return-path") {
      window.postMessage({
        source: "tesktop-stereo-proof-control",
        returnPath: message.active === true,
      }, location.origin);
      return;
    }
    if (message?.kind !== "tesktop-curve-capture") return;
    window.postMessage({
      source: "tesktop-stereo-proof-control",
      captureCurve: message.active === true,
    }, location.origin);
  });
})();
