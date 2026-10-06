const BASE = "http://127.0.0.1:43721";
const MIN_INTERVAL_MS = 1500;
const MAX_TEST_SAMPLES = 1800;
// The synthetic sender cycles through 48 bands at 700 ms each (33.6 s).
// Capture a full pass plus a margin after the first exact SSRC match.
const AUTO_CAPTURE_DURATION_MS = 40_000;
const lastSentByTab = new Map();
let latestSpectrum = null;
let curveCaptureActive = false;
const capturedCurves = new Map();
let diagnosticsPostActive = false;
let lastDiagnosticsPostAt = 0;
const diagnostics = {
  contentBridgeAt: 0,
  observerReportAt: 0,
  reportCount: 0,
  peerConnections: 0,
  inboundStreams: 0,
  forwardError: "",
  observerState: "not-seen",
  observerApi: false,
  observerError: "",
  observerAt: 0,
  healthForwardError: "",
  senderStatusError: "",
};
let latestSenderSsrc = null;
let latestSender = null;
let activeTest = null;
let completedTest = null;
let completedExport = null;
let autoSweepActive = false;
let autoSweepRunStarted = false;
let autoCaptureStartedAt = 0;
let senderStatusRequest = null;
let lastSenderStatusAt = 0;

function beginTest(automatic = false) {
  activeTest = {
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    started_at_ms: Date.now(),
    sender_ssrc: latestSenderSsrc,
    automatic,
    samples: [],
  };
  completedTest = null;
  completedExport = null;
}

function finishTest() {
  if (!activeTest) return;
  activeTest.ended_at_ms = Date.now();
  completedTest = activeTest;
  activeTest = null;
  const curve = [...capturedCurves.values()].find(item =>
    item.ssrc === completedTest.sender_ssrc);
  const curveSummary = curve ? summarizeCurve(curve.spectrum_dbfs) : null;
  completedExport = {
    format: "AscendCord Stereo Proof receiver test v1",
    exported_at: new Date().toISOString(),
    privacy: "Numeric WebRTC diagnostics only; no audio samples, credentials, or messages.",
    diagnostics: {
      ...diagnostics,
      test: {
        running: false,
        automatic: completedTest.automatic === true,
        id: completedTest.id,
        started_at_ms: completedTest.started_at_ms,
        ended_at_ms: completedTest.ended_at_ms,
        sender_ssrc: completedTest.sender_ssrc,
        sample_count: completedTest.samples.length,
      },
      auto_sweep_active: autoSweepActive,
      auto_capture_waiting: false,
      curve_capture_active: false,
      auto_export_ready: true,
      test_summary: summarizeSamples(completedTest.samples),
      sweep_curve: curve ? {
        ssrc: curve.ssrc,
        spectrum_dbfs: [...curve.spectrum_dbfs],
        ...curveSummary,
      } : null,
      test_samples: completedTest.samples.slice(-300),
    },
    test: { ...completedTest, samples: [...completedTest.samples] },
  };
}

function summarizeCurve(values) {
  if (!Array.isArray(values) || values.length !== 48 ||
      !values.every(value => Number.isFinite(value) && value >= -120 && value <= 12)) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const median_dbfs = (sorted[23] + sorted[24]) / 2;
  const relative_db = values.map(value => value - median_dbfs);
  const minimum_dbfs = Math.min(...values);
  const maximum_dbfs = Math.max(...values);
  return {
    median_dbfs,
    minimum_dbfs,
    maximum_dbfs,
    peak_to_peak_db: maximum_dbfs - minimum_dbfs,
    peak_deviation_db: Math.max(...relative_db.map(Math.abs)),
    relative_db,
  };
}

function setCurveCapture(active, tabId = latestSpectrum?.tabId) {
  curveCaptureActive = active;
  if (active) capturedCurves.clear();
  if (Number.isInteger(tabId)) {
    chrome.tabs.sendMessage(tabId, {
      kind: "tesktop-curve-capture", active,
    }).catch(() => {});
  }
}

function updateSenderStatus(status) {
  latestSenderSsrc = Number.isInteger(status?.sender?.audio_ssrc)
    ? status.sender.audio_ssrc : null;
  latestSender = status?.sender ?? null;
  if (activeTest && activeTest.sender_ssrc == null && latestSenderSsrc != null) {
    activeTest.sender_ssrc = latestSenderSsrc;
  }
  const sweepNow = latestSender?.test_sweep_active === true && latestSender?.send_enabled === true;
  if (sweepNow && !autoSweepActive) {
    autoSweepActive = true;
    autoSweepRunStarted = false;
    autoCaptureStartedAt = 0;
    completedTest = null;
    completedExport = null;
  } else if (!sweepNow && autoSweepActive) {
    autoSweepActive = false;
    if (autoSweepRunStarted) finishTest();
    autoSweepRunStarted = false;
    autoCaptureStartedAt = 0;
    if (curveCaptureActive) setCurveCapture(false);
  }
}

function refreshSenderStatus(force = false) {
  if (senderStatusRequest) {
    if (!force) return senderStatusRequest;
    return senderStatusRequest.then(
      () => refreshSenderStatus(true),
      () => refreshSenderStatus(true),
    );
  }
  if (!force && Date.now() - lastSenderStatusAt < 750) {
    return Promise.resolve(latestSender);
  }
  senderStatusRequest = request("/v1/status")
    .then(status => {
      updateSenderStatus(status);
      lastSenderStatusAt = Date.now();
      diagnostics.senderStatusError = "";
      return status;
    })
    .catch(error => {
      diagnostics.senderStatusError = String(error?.message ?? error).slice(0, 180);
      throw error;
    })
    .finally(() => { senderStatusRequest = null; });
  return senderStatusRequest;
}

function isDiscordSender(sender) {
  return Number.isInteger(sender.tab?.id) && isDiscordUrl(sender.tab.url);
}

function summarizeSamples(samples) {
  if (!samples.length) return null;
  const matched = samples.filter(sample => sample.sender_match === true);
  const mean = field => matched.reduce((sum, sample) => sum + Number(sample[field] ?? 0), 0) / matched.length;
  const max = field => Math.max(...matched.map(sample => Number(sample[field] ?? 0)));
  return {
    samples: samples.length,
    matched_samples: matched.length,
    unmatched_samples: samples.length - matched.length,
    average_loss_percent: matched.length ? mean("loss_percent") : null,
    peak_loss_percent: matched.length ? max("loss_percent") : null,
    average_jitter_ms: matched.length ? mean("jitter_ms") : null,
    peak_jitter_ms: matched.length ? max("jitter_ms") : null,
    concealment_samples_per_second: matched.length ? mean("concealed_samples_per_second") : null,
    concealment_events: matched.reduce((sum, sample) => sum + Number(sample.concealment_events_delta ?? 0), 0),
    discarded_packets: matched.reduce((sum, sample) => sum + Number(sample.discarded_packets_delta ?? 0), 0),
  };
}

function collectTestSamples(report, tabId) {
  const exactMatch = latestSenderSsrc == null ? null :
    report.streams.find(stream => stream.ssrc === latestSenderSsrc) ?? null;
  if (autoSweepActive && !autoSweepRunStarted && exactMatch) {
    if (!activeTest) beginTest(true);
    else if (activeTest.sender_ssrc == null) activeTest.sender_ssrc = latestSenderSsrc;
    autoSweepRunStarted = true;
    autoCaptureStartedAt = Date.now();
    setCurveCapture(true, tabId);
  }

  if (!activeTest) return;
  const expectedSsrc = activeTest.sender_ssrc;
  const matched = expectedSsrc == null ? null :
    report.streams.find(stream => stream.ssrc === expectedSsrc) ?? null;
  const selected = matched ? [matched] : report.streams;
  for (const stream of selected.slice(0, 16)) {
    activeTest.samples.push({
      at_ms: Date.now(),
      ssrc: stream.ssrc ?? null,
      sender_match: expectedSsrc != null && stream.ssrc === expectedSsrc,
      codec: stream.codec ?? "audio/unknown",
      channels: stream.channels ?? null,
      track_channels: stream.track_channels ?? null,
      bitrate_bps: stream.bitrate_bps ?? 0,
      packets_received: stream.packets_received ?? 0,
      packets_lost: stream.packets_lost ?? 0,
      loss_percent: stream.loss_percent ?? 0,
      jitter_ms: stream.jitter_ms ?? 0,
      concealed_samples_per_second: stream.concealed_samples_per_second ?? 0,
      concealment_events_delta: stream.concealment_events_delta ?? 0,
      discarded_packets_delta: stream.discarded_packets_delta ?? 0,
      jitter_buffer_delay_ms: stream.jitter_buffer_delay_ms ?? 0,
      audio_level: stream.audio_level ?? 0,
      active: stream.active === true,
      left_dbfs: stream.left_dbfs ?? null,
      right_dbfs: stream.right_dbfs ?? null,
      side_dbfs: stream.side_dbfs ?? null,
      lr_correlation: stream.lr_correlation ?? null,
      sdp_fmtp_stereo: stream.sdp_fmtp_stereo ?? null,
    });
  }
  if (activeTest.samples.length > MAX_TEST_SAMPLES) {
    activeTest.samples.splice(0, activeTest.samples.length - MAX_TEST_SAMPLES);
  }
  if (autoSweepRunStarted && activeTest && autoCaptureStartedAt > 0 &&
      Date.now() - autoCaptureStartedAt >= AUTO_CAPTURE_DURATION_MS) {
    finishTest();
    setCurveCapture(false, tabId);
  }
}

function publishDiagnostics() {
  if (diagnosticsPostActive || Date.now() - lastDiagnosticsPostAt < 1000) return;
  diagnosticsPostActive = true;
  lastDiagnosticsPostAt = Date.now();
  const currentTest = activeTest ?? completedTest;
  const testSummary = currentTest ? summarizeSamples(currentTest.samples) : null;
  const curve = [...capturedCurves.values()].find(item =>
    item.ssrc === (currentTest?.sender_ssrc ?? latestSenderSsrc));
  const payload = {
    protocol: 1,
    sampled_at_ms: Date.now(),
    content_bridge: Date.now() - diagnostics.contentBridgeAt <= 7000,
    observer_state: diagnostics.observerState || "not-seen",
    observer_api: diagnostics.observerApi === true,
    observer_error: diagnostics.observerError,
    peer_connections: diagnostics.peerConnections,
    inbound_streams: diagnostics.inboundStreams,
    report_count: Math.min(2_000_000_000, diagnostics.reportCount),
    forward_error: diagnostics.forwardError,
    sweep_test: currentTest ? {
      running: activeTest !== null,
      automatic: currentTest.automatic === true,
      capture_active: curveCaptureActive,
      export_ready: completedExport !== null,
      sender_ssrc: currentTest.sender_ssrc,
      sample_count: Math.min(MAX_TEST_SAMPLES, currentTest.samples.length),
      matched_samples: testSummary?.matched_samples ?? 0,
      average_loss_percent: testSummary?.average_loss_percent ?? null,
      peak_loss_percent: testSummary?.peak_loss_percent ?? null,
      average_jitter_ms: testSummary?.average_jitter_ms ?? null,
      peak_jitter_ms: testSummary?.peak_jitter_ms ?? null,
    } : null,
    sweep_curve: curve ? {
      ssrc: curve.ssrc,
      spectrum_dbfs: [...curve.spectrum_dbfs],
    } : null,
  };
  request("/v1/diagnostics", {
    method: "POST",
    headers: { "Content-Type": "text/plain;charset=UTF-8" },
    body: JSON.stringify(payload),
  }).then(() => {
    diagnostics.healthForwardError = "";
  }).catch(error => {
    diagnostics.healthForwardError = String(error?.message ?? error).slice(0, 180);
  }).finally(() => {
    diagnosticsPostActive = false;
  });
}

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
  if (message?.kind === "tesktop-content-bridge-heartbeat") {
    if (!isDiscordSender(sender)) {
      sendResponse({ ok: false, error: "Heartbeat did not come from a Discord tab." });
      return;
    }
    diagnostics.contentBridgeAt = Date.now();
    refreshSenderStatus().catch(() => null);
    publishDiagnostics();
    sendResponse({ ok: true });
    return;
  }

  if (message?.kind === "tesktop-observer-status") {
    if (!isDiscordSender(sender) || !message.observer ||
        typeof message.observer.state !== "string") {
      sendResponse({ ok: false, error: "Observer status did not come from a Discord tab." });
      return;
    }
    diagnostics.observerState = message.observer.state.slice(0, 48);
    diagnostics.observerApi = message.observer.peer_api === true;
    diagnostics.observerError = String(message.observer.error ?? "").slice(0, 180);
    diagnostics.observerAt = Date.now();
    publishDiagnostics();
    sendResponse({ ok: true });
    return;
  }

  if (message?.kind === "tesktop-receiver-report") {
    if (!isDiscordSender(sender) ||
        !message.report || typeof message.report !== "object") {
      sendResponse({ ok: false, error: "Report did not come from a Discord tab." });
      return;
    }
    diagnostics.observerReportAt = Date.now();
    diagnostics.reportCount++;
    diagnostics.peerConnections = Number.isInteger(message.report.peer_connections)
      ? Math.max(0, Math.min(32, message.report.peer_connections)) : 0;
    diagnostics.inboundStreams = Math.min(16, message.report.streams.length);
    diagnostics.forwardError = "";
    publishDiagnostics();

    // Heartbeat/status polling can race with the first receiver report. Refresh here
    // before matching so tests don't permanently record a null sender SSRC.
    refreshSenderStatus().catch(() => null).then(() => {
      collectTestSamples(message.report, sender.tab.id);
    });

    // Keep only numeric spectrum points in extension memory for the popup graph. Never
    // forward this optional field to older desktop builds, whose strict report schema
    // would reject the complete receive-statistics report.
    const streams = message.report.streams.filter(stream =>
      Number.isInteger(stream?.ssrc) && Array.isArray(stream.spectrum_dbfs) &&
      stream.spectrum_dbfs.length === 48 &&
      stream.spectrum_dbfs.every(value => Number.isFinite(value) && value >= -120 && value <= 12));
    latestSpectrum = {
      at: Date.now(),
      tabId: sender.tab.id,
      streams: streams.slice(0, 16).map(stream => ({
        ssrc: stream.ssrc,
        spectrum_dbfs: stream.spectrum_dbfs,
      })),
    };
    if (curveCaptureActive) {
      for (const stream of latestSpectrum.streams) {
        const curveKey = `${sender.tab.id}:${stream.ssrc}`;
        let curve = capturedCurves.get(curveKey);
        if (!curve) {
          if (capturedCurves.size >= 32) {
            capturedCurves.delete(capturedCurves.keys().next().value);
          }
          curve = { tabId: sender.tab.id, ssrc: stream.ssrc, spectrum_dbfs: Array(48).fill(-120) };
          capturedCurves.set(curveKey, curve);
        }
        stream.spectrum_dbfs.forEach((db, index) => {
          curve.spectrum_dbfs[index] = Math.max(curve.spectrum_dbfs[index], db);
        });
      }
    }

    const now = Date.now();
    const last = lastSentByTab.get(sender.tab.id) ?? 0;
    if (now - last < MIN_INTERVAL_MS) {
      sendResponse({ ok: true, throttled: true });
      return;
    }
    lastSentByTab.set(sender.tab.id, now);

    const desktopReport = {
      ...message.report,
      streams: message.report.streams.map(({ spectrum_dbfs, ...stream }) => stream),
    };
    request("/v1/receiver", {
      method: "POST",
      headers: { "Content-Type": "text/plain;charset=UTF-8" },
      body: JSON.stringify(desktopReport),
    }).then(() => sendResponse({ ok: true }))
      .catch(error => {
      diagnostics.forwardError = String(error?.message ?? error).slice(0, 180);
        publishDiagnostics();
        sendResponse({ ok: false, error: diagnostics.forwardError });
      });
    return true;
  }

  if (message?.kind === "tesktop-read-diagnostics") {
    const test = activeTest ?? completedTest;
    sendResponse({
      ...diagnostics,
      test: activeTest ? {
        running: true,
        automatic: activeTest.automatic === true,
        id: activeTest.id,
        started_at_ms: activeTest.started_at_ms,
        sender_ssrc: activeTest.sender_ssrc,
        sample_count: activeTest.samples.length,
      } : completedTest ? {
        running: false,
        automatic: completedTest.automatic === true,
        id: completedTest.id,
        started_at_ms: completedTest.started_at_ms,
        ended_at_ms: completedTest.ended_at_ms,
        sender_ssrc: completedTest.sender_ssrc,
        sample_count: completedTest.samples.length,
      } : { running: false, sample_count: 0 },
      auto_sweep_active: autoSweepActive,
      auto_capture_waiting: autoSweepActive && !autoSweepRunStarted,
      curve_capture_active: curveCaptureActive,
      auto_export_ready: completedExport !== null,
      test_summary: test ? summarizeSamples(test.samples) : null,
      test_samples: test ? test.samples.slice(-300) : [],
    });
    return;
  }

  if (message?.kind === "tesktop-start-test") {
    refreshSenderStatus(true).catch(() => null).finally(() => {
      beginTest(false);
      sendResponse({ ok: true, test: { ...activeTest, samples: undefined } });
    });
    return true;
  }

  if (message?.kind === "tesktop-stop-test") {
    finishTest();
    sendResponse({ ok: true, test: completedTest ? {
      id: completedTest.id,
      started_at_ms: completedTest.started_at_ms,
      ended_at_ms: completedTest.ended_at_ms,
      sample_count: completedTest.samples.length,
    } : null });
    return;
  }

  if (message?.kind === "tesktop-reset-test") {
    activeTest = null;
    completedTest = null;
    completedExport = null;
    sendResponse({ ok: true });
    return;
  }

  if (message?.kind === "tesktop-read-test-data") {
    const test = activeTest ?? completedTest;
    sendResponse({
      ok: true,
      test: test ? { ...test, samples: [...test.samples] } : null,
      export: completedExport ? { ...completedExport, test: { ...completedExport.test, samples: [...completedExport.test.samples] } } : null,
    });
    return;
  }

  if (message?.kind === "tesktop-read-local-spectrum") {
    const fresh = latestSpectrum && Date.now() - latestSpectrum.at <= 5000;
    sendResponse({
      ok: true,
      report: fresh ? latestSpectrum : null,
      curve_capture_active: curveCaptureActive,
      curves: [...capturedCurves.values()].map(curve => ({
        tabId: curve.tabId, ssrc: curve.ssrc, spectrum_dbfs: curve.spectrum_dbfs,
      })),
    });
    return;
  }

  if (message?.kind === "tesktop-set-curve-capture") {
    setCurveCapture(message.active === true);
    sendResponse({ ok: true, active: curveCaptureActive });
    return;
  }

  if (message?.kind === "tesktop-read-status") {
    refreshSenderStatus(true)
      .then(status => {
        sendResponse({ ok: true, status });
      })
      .catch(error => sendResponse({ ok: false, error: String(error?.message ?? error) }));
    return true;
  }

  sendResponse({ ok: false, error: "Unknown request." });
});

chrome.tabs.onRemoved.addListener(tabId => {
  lastSentByTab.delete(tabId);
  if (latestSpectrum?.tabId === tabId) latestSpectrum = null;
  for (const [ssrc, curve] of capturedCurves) {
    if (curve.tabId === tabId) capturedCurves.delete(ssrc);
  }
});
