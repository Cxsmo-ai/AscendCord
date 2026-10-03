(() => {
  "use strict";
  const marker = "tesktop-stereo-proof-v1";

  window.addEventListener("message", event => {
    if (event.source !== window || event.origin !== location.origin ||
        event.data?.source !== marker) return;

    const report = event.data.report;
    if (!report || report.protocol !== 1 || !Array.isArray(report.streams) ||
        report.streams.length > 16) return;

    chrome.runtime.sendMessage({ kind: "tesktop-receiver-report", report });
  });
})();
