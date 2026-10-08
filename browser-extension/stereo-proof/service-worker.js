// Measurement program 2 analysis, shared with the page and the popup.
if (!globalThis.AscendCordLab && typeof importScripts === "function") {
  try { importScripts("lab.js"); } catch {}
}
const Lab = globalThis.AscendCordLab ?? null;
const LAB_HISTORY_KEY = "ascendcordLabHistory";
const LAB_HISTORY_LIMIT = 12;
// A program 2 pass lasts about 66 s; finding the first silence can take one more pass.
// Two passes plus up to one more to reach the first silence; program 3 passes run about 76 s
// without songs and about 100 s with them.
const LAB_MAX_CAPTURE_MS = 330_000;
const LAB_PASSES = 2;
// The page analyses 16384 samples (341 ms at 48 kHz) at a time.
const LAB_WINDOW_HOLD_MS = 400;
// Null-test reports from the page are numbers only; anything larger is not a report.
const LAB_CONTENT_MAX_BYTES = 32_768;
// Song clips AscendCord plays in a lab test, fetched once per set and handed to the page.
let labSongs = null;
let labSongsKey = "";
let labSongsRequest = null;

const DEV_REQUESTS = Object.freeze({
  diagnostics: "tesktop-read-diagnostics",
  history: "tesktop-read-lab-history",
  start: "tesktop-start-test",
  stop: "tesktop-stop-test",
  reset: "tesktop-reset-test",
  export: "tesktop-read-test-data",
});
const BASE = "http://127.0.0.1:43721";
const MIN_INTERVAL_MS = 1500;
const MAX_TEST_SAMPLES = 1800;
// Two full 48-band passes (2 × 48 × 700 ms), with a margin for phase alignment.
const AUTO_CAPTURE_DURATION_MS = 72_000;
const COMPLETED_EXPORT_KEY = "tesktopCompletedSweepExport";
const MIN_SIGNAL_DBFS = -90;
const MIN_BIN_SAMPLES = 2;
const SWEEP_BANDS = 48;
const SWEEP_LOW_HZ = 20;
const SWEEP_HIGH_HZ = 20_000;
const MAX_PEAK_FREQUENCY_ERROR_PERCENT = 8.5;
const MIN_REPEAT_INTERVAL_MS = 5_000;
const SOURCE_PEAK_DBFS = 20 * Math.log10(0.25);
const lastSentByTab = new Map();
let latestSpectrum = null;
let curveCaptureActive = false;
const capturedCurves = new Map();
let diagnosticsPostActive = false;
let lastDiagnosticsPostAt = 0;
let diagnosticsPostDirty = false;
let diagnosticsPostTimer = null;
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
  storageError: "",
};
let latestSenderSsrc = null;
let latestSender = null;
let activeTest = null;
let completedTest = null;
let completedExport = null;
let completedExportPersisted = false;
let exportPersistPromise = Promise.resolve();
let autoSweepActive = false;
let autoSweepSsrc = null;
let autoSweepRunStarted = false;
let autoCaptureStartedAt = 0;
let senderStatusRequest = null;
let lastSenderStatusAt = 0;

// MV3 service workers can be suspended between the automatic capture and the
// user's next popup visit. Keep the completed numeric report in extension-local
// storage so a browser worker restart does not make the result disappear.
const restoreCompletedExport = chrome.storage?.local?.get
  ? chrome.storage.local.get(COMPLETED_EXPORT_KEY).then(saved => {
    const restored = saved?.[COMPLETED_EXPORT_KEY];
    if (restored?.format === "AscendCord Stereo Proof receiver test v1" &&
        restored?.test && Array.isArray(restored.test.samples) &&
        restored.test.samples.length <= MAX_TEST_SAMPLES) {
      completedExport = restored;
      completedTest = restored.test;
      completedExportPersisted = true;
    }
  }).catch(error => {
    diagnostics.storageError = String(error?.message ?? error).slice(0, 180);
  })
  : Promise.resolve();

function persistCompletedExport() {
  if (!chrome.storage?.local?.set || !completedExport) return Promise.resolve();
  exportPersistPromise = chrome.storage.local.set({
    [COMPLETED_EXPORT_KEY]: completedExport,
  }).then(() => {
    diagnostics.storageError = "";
    completedExportPersisted = true;
  }).catch(error => {
    completedExportPersisted = false;
    diagnostics.storageError = String(error?.message ?? error).slice(0, 180);
  });
  return exportPersistPromise;
}

function clearCompletedExport() {
  completedExportPersisted = false;
  if (!chrome.storage?.local?.remove) return;
  chrome.storage.local.remove(COMPLETED_EXPORT_KEY).catch(error => {
    diagnostics.storageError = String(error?.message ?? error).slice(0, 180);
  });
}

function beginTest(automatic = false, tabId = null) {
  activeTest = {
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    started_at_ms: Date.now(),
    sender_ssrc: latestSenderSsrc,
    tab_id: Number.isInteger(tabId) ? tabId : null,
    automatic,
    program: labProgram() ? Lab.PROGRAM.version : 1,
    return_path: labProgram() && latestSender?.return_lab_supported === true,
    lab: labProgram() ? Lab.createLab() : null,
    analysis_sample_rate_hz: null,
    samples: [],
  };
  completedTest = null;
  completedExport = null;
  completedExportPersisted = false;
  clearCompletedExport();
}

function finishTest() {
  if (!activeTest) return;
  activeTest.ended_at_ms = Date.now();
  completedTest = activeTest;
  activeTest = null;
  const measurementLab = completedTest.lab ? Lab.finalizeLab(completedTest.lab) : null;
  if (measurementLab) measurementLab.content = Lab.combineContent(completedTest.content_reports);
  const curve = measurementLab
    ? curveFromLab(measurementLab, completedTest)
    : [...capturedCurves.values()].find(item =>
      item.ssrc === completedTest.sender_ssrc &&
      (completedTest.tab_id == null || item.tabId === completedTest.tab_id));
  const curveSummary = curve ? summarizeCurve(curve) : null;
  const senderSettings = describeSender(latestSender);
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
        analysis_sample_rate_hz: curve.analysis_sample_rate_hz,
        frequency_hz: sweepFrequencies(),
        spectrum_dbfs: [...curve.spectrum_dbfs],
        gain_db: curve.spectrum_dbfs.map(value => value - SOURCE_PEAK_DBFS),
        peak_frequency_hz: curve.mean_peak_frequency_hz.map((mean, index) =>
          curve.samples_per_band[index] > 0 ? mean : null),
        peak_frequency_error_percent: curve.mean_peak_frequency_hz.map((mean, index) =>
          curve.samples_per_band[index] > 0
            ? (mean / sweepFrequencies()[index] - 1) * 100 : null),
        standard_deviation_db: curve.mean_dbfs.map((_, index) =>
          curve.samples_per_band[index] > 1
            ? Math.sqrt(curve.m2_db[index] / (curve.samples_per_band[index] - 1)) : null),
        samples_per_band: [...curve.samples_per_band],
        measured_bins: curve.covered_bins.filter(Boolean).length,
        coverage_complete: curve.covered_bins.every(Boolean),
        ...curveSummary,
      } : null,
      pipeline_validation: validateCompletedPipeline(completedTest, curve, curveSummary),
      measurement_lab: measurementLab,
      return_lab: measurementLab ? returnLab() : null,
      sender_settings: senderSettings,
      test_samples: completedTest.samples.slice(-300),
    },
    test: { ...completedTest, lab: undefined, samples: [...completedTest.samples] },
  };
  persistCompletedExport();
  if (measurementLab) saveLabHistory(completedTest, measurementLab, senderSettings, returnLab());
}

/** The settings a run was made with, so two runs can be told apart and compared. */
/** AscendCord sends the measurement program this extension analyses. */
function labProgram() {
  return Boolean(Lab) && latestSender?.test_program === Lab.PROGRAM.version;
}

function describeSender(sender) {
  if (!sender || typeof sender !== "object") return null;
  const keep = {};
  for (const [key, value] of Object.entries(sender)) {
    if (!/^(opus_|force_stereo|always_transmit|noise_suppression|rnnoise_vad|echo_cancellation|automatic_gain|input_gain_percent|capture_(rate_hz|channels|format)|test_program)/.test(key)) continue;
    if (typeof value === "boolean" || (typeof value === "number" && Number.isFinite(value))) {
      keep[key] = value;
    } else if (typeof value === "string") {
      keep[key] = value.slice(0, 32);
    }
  }
  return keep;
}

/** Program 2 measures the identical-channel sweep itself; present it in the v1 curve shape. */
function curveFromLab(report, test) {
  const bands = report.response.frequency_hz.length;
  const level = index => {
    const left = report.response.left_gain_db[index], right = report.response.right_gain_db[index];
    if (!Number.isFinite(left) || !Number.isFinite(right)) return -120;
    return 10 * Math.log10((10 ** (left / 10) + 10 ** (right / 10)) / 2) + SOURCE_PEAK_DBFS;
  };
  const spectrum = Array.from({ length: bands }, (_, index) => level(index));
  const windows = report.response.windows;
  return {
    tabId: test.tab_id,
    ssrc: test.sender_ssrc,
    analysis_sample_rate_hz: test.analysis_sample_rate_hz,
    spectrum_dbfs: spectrum,
    mean_power: spectrum.map(value => 10 ** (value / 10)),
    mean_peak_frequency_hz: [...report.response.frequency_hz],
    mean_dbfs: spectrum,
    m2_db: report.response.left_std_db.map((std, index) =>
      Number.isFinite(std) && windows[index] > 1 ? std * std * (windows[index] - 1) : 0),
    samples_per_band: [...windows],
    covered_bins: windows.map(count => count >= MIN_BIN_SAMPLES),
    last_sample_at_ms: Array(bands).fill(0),
  };
}

function saveLabHistory(test, report, settings, returned = null) {
  if (!chrome.storage?.local?.get || !chrome.storage?.local?.set) return;
  const entry = {
    id: test.id,
    finished_at_ms: test.ended_at_ms,
    sender_settings: settings,
    measurement_lab: report,
    return_lab: returned,
    sender_health: test.sender_health ?? null,
    network: test.samples?.length ? summarizeSamples(test.samples) : null,
  };
  chrome.storage.local.get(LAB_HISTORY_KEY).then(saved => {
    const history = Array.isArray(saved?.[LAB_HISTORY_KEY]) ? saved[LAB_HISTORY_KEY] : [];
    const next = [entry, ...history.filter(item => item?.id !== entry.id)].slice(0, LAB_HISTORY_LIMIT);
    return chrome.storage.local.set({ [LAB_HISTORY_KEY]: next });
  }).catch(error => {
    diagnostics.storageError = String(error?.message ?? error).slice(0, 180);
  });
}

/** Bounds every number a page window carries; anything malformed is dropped. */
function sanitizeLabWindow(window) {
  if (!window || typeof window !== "object") return null;
  const db = value => Number.isFinite(value) ? Math.max(-200, Math.min(40, value)) : null;
  if (window.kind === "silence") {
    return {
      kind: "silence",
      loudest_dbfs: db(window.loudest_dbfs),
      left_rms_dbfs: db(window.left_rms_dbfs),
      right_rms_dbfs: db(window.right_rms_dbfs),
    };
  }
  if (window.kind !== "tone" || !["sweep", "ladder"].includes(window.grid)) {
    return { kind: "unknown" };
  }
  const limit = window.grid === "sweep" ? 48 : 8;
  if (!Number.isInteger(window.index) || window.index < 0 || window.index >= limit) {
    return { kind: "unknown" };
  }
  const channel = value => ({
    peak_dbfs: db(value?.peak_dbfs),
    thd_db: db(value?.thd_db),
    thdn_db: db(value?.thdn_db),
  });
  return {
    kind: "tone",
    steady: window.steady === true,
    glitch: window.glitch === true,
    grid: window.grid,
    index: window.index,
    left: channel(window.left),
    right: channel(window.right),
    correlation: Number.isFinite(window.correlation)
      ? Math.max(-1, Math.min(1, window.correlation)) : null,
  };
}

function validateCompletedPipeline(test, curve, curveSummary) {
  const matched = test.samples.filter(sample => sample.sender_match === true);
  const stereoSamples = matched.filter(sample => sample.channels === 2 &&
    sample.track_channels === 2 && sample.sdp_fmtp_stereo === true && sample.active === true);
  const curveCovered = curveSummary !== null &&
    curve.covered_bins.every(Boolean) &&
    curve.samples_per_band.every(count => count >= MIN_BIN_SAMPLES) &&
    Math.max(...curve.spectrum_dbfs) > MIN_SIGNAL_DBFS;
  const expectedFrequencies = sweepFrequencies();
  const frequencyAssignmentValid = curveCovered && curve.mean_peak_frequency_hz.every((frequency, index) =>
    Number.isFinite(frequency) && Math.abs(frequency / expectedFrequencies[index] - 1) * 100 <=
      MAX_PEAK_FREQUENCY_ERROR_PERCENT);
  const checks = {
    matched_receiver_samples: matched.length >= 3,
    stereo_opus_receive: stereoSamples.length >= 3,
    all_48_sweep_bands_measured: curveCovered,
    measured_peak_frequencies_match_sweep: frequencyAssignmentValid,
  };
  return {
    passed: Object.values(checks).every(Boolean),
    checks,
    stereo_samples: stereoSamples.length,
    measured_bands: curve?.covered_bins.filter(Boolean).length ?? 0,
    signal_threshold_dbfs: MIN_SIGNAL_DBFS,
    max_peak_frequency_error_percent: MAX_PEAK_FREQUENCY_ERROR_PERCENT,
  };
}

function sweepFrequencies() {
  return Array.from({ length: SWEEP_BANDS }, (_, index) =>
    SWEEP_LOW_HZ * (SWEEP_HIGH_HZ / SWEEP_LOW_HZ) ** (index / (SWEEP_BANDS - 1)));
}

function addCurveSample(curve, frequencyHz, peakLevel) {
  if (!Number.isFinite(frequencyHz) || frequencyHz < SWEEP_LOW_HZ * 0.9 ||
      frequencyHz > SWEEP_HIGH_HZ * 1.1 || !Number.isFinite(peakLevel) ||
      peakLevel <= MIN_SIGNAL_DBFS) return;
  const logStep = Math.log(SWEEP_HIGH_HZ / SWEEP_LOW_HZ) / (SWEEP_BANDS - 1);
  const peakIndex = Math.round(Math.log(frequencyHz / SWEEP_LOW_HZ) / logStep);
  if (peakIndex < 0 || peakIndex >= SWEEP_BANDS) return;
  const expectedHz = SWEEP_LOW_HZ * Math.exp(peakIndex * logStep);
  if (Math.abs(Math.log(frequencyHz / expectedHz)) > logStep * 0.51) return;
  const now = Date.now();
  if (now - curve.last_sample_at_ms[peakIndex] < MIN_REPEAT_INTERVAL_MS) return;
  curve.last_sample_at_ms[peakIndex] = now;
  const count = ++curve.samples_per_band[peakIndex];
  const delta = peakLevel - curve.mean_dbfs[peakIndex];
  curve.mean_dbfs[peakIndex] += delta / count;
  curve.m2_db[peakIndex] += delta * (peakLevel - curve.mean_dbfs[peakIndex]);
  curve.mean_peak_frequency_hz[peakIndex] +=
    (frequencyHz - curve.mean_peak_frequency_hz[peakIndex]) / count;
  curve.mean_power[peakIndex] +=
    ((10 ** (peakLevel / 10)) - curve.mean_power[peakIndex]) / count;
  curve.spectrum_dbfs[peakIndex] = 10 * Math.log10(curve.mean_power[peakIndex]);
  curve.covered_bins[peakIndex] = count >= MIN_BIN_SAMPLES;
}

function summarizeCurve(curve) {
  const values = curve?.spectrum_dbfs;
  if (!Array.isArray(values) || values.length !== 48 ||
      !values.every(value => Number.isFinite(value) && value >= -120 && value <= 12)) return null;
  const usable = values.filter((value, index) =>
    curve.covered_bins[index] && value > -120);
  if (!usable.length) return null;
  const sorted = [...usable].sort((a, b) => a - b);
  const median_dbfs = sorted.length % 2
    ? sorted[(sorted.length - 1) / 2]
    : (sorted[sorted.length / 2 - 1] + sorted[sorted.length / 2]) / 2;
  const relative_db = values.map((value, index) =>
    curve.covered_bins[index] && value > -120 ? value - median_dbfs : null);
  const minimum_dbfs = Math.min(...usable);
  const maximum_dbfs = Math.max(...usable);
  return {
    median_dbfs,
    minimum_dbfs,
    maximum_dbfs,
    peak_to_peak_db: maximum_dbfs - minimum_dbfs,
    peak_deviation_db: Math.max(...relative_db.filter(Number.isFinite).map(Math.abs)),
    relative_db,
  };
}

function setCurveCapture(active, tabId = latestSpectrum?.tabId) {
  curveCaptureActive = active;
  if (active) capturedCurves.clear();
  if (Number.isInteger(tabId)) {
    // The page needs the songs before it renders the program for the return path.
    if (active) sendLabSongs(tabId);
    chrome.tabs.sendMessage(tabId, {
      kind: "tesktop-curve-capture", active,
    }).catch(() => {});
    // Program 2 also plays back from the browser so AscendCord measures what it receives.
    const returnPath = active && activeTest?.program === Lab?.PROGRAM.version &&
      latestSender?.return_lab_supported === true;
    if (returnPath || !active) {
      chrome.tabs.sendMessage(tabId, {
        kind: "tesktop-return-path", active: returnPath,
      }).catch(() => {});
    }
  }
}

/** AscendCord's measurement of the program the browser sent back, bounded before keeping. */
function returnLab() {
  const report = latestSender?.return_lab;
  if (!report || typeof report !== "object" || report.version !== Lab?.PROGRAM.version) return null;
  return JSON.stringify(report).length <= 65_536 ? report : null;
}

// AscendCord's send-side readings are momentary; a lab run keeps the worst of each so a
// disturbed run shows whether the sender stalled or dropped audio.
const SENDER_HEALTH_KEYS = Object.freeze([
  "max_send_gap_ms", "transport_loop_stalls_per_second", "pacing_catchups_per_second",
  "capture_ring_drops", "capture_worker_drops", "playback_ring_drops",
]);

function trackSenderHealth(test, sender) {
  if (!sender || typeof sender !== "object") return;
  test.sender_health ??= {};
  for (const key of SENDER_HEALTH_KEYS) {
    const value = Number(sender[key]);
    if (Number.isFinite(value)) test.sender_health[key] = Math.max(test.sender_health[key] ?? 0, value);
  }
}

function base64(bytes) {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

/** Fetches the song clips named in AscendCord's status, once per set. */
function refreshLabSongs(manifest) {
  const list = Array.isArray(manifest) ? manifest.slice(0, 16) : [];
  const key = JSON.stringify(list);
  if (!list.length || key === labSongsKey || labSongsRequest) return;
  // A POST: Chromium attaches the extension's Origin to it, which AscendCord requires.
  labSongsRequest = fetch(`${BASE}/v1/lab/songs`, { method: "POST", cache: "no-store", credentials: "omit" })
    .then(response => response.ok ? response.arrayBuffer() : null)
    .then(buffer => {
      const frames = list.map(item => Math.max(0, Math.floor(Number(item?.frames) || 0)));
      if (!buffer || buffer.byteLength !== frames.reduce((sum, n) => sum + n, 0) * 4) return;
      labSongs = {
        names: list.map(item => String(item?.name ?? "").slice(0, 64)),
        frames,
        pcm: base64(new Uint8Array(buffer)),
      };
      labSongsKey = key;
      if (curveCaptureActive) sendLabSongs(latestSpectrum?.tabId);
    })
    .catch(() => {})
    .finally(() => { labSongsRequest = null; });
}

function sendLabSongs(tabId) {
  if (!labSongs || !Number.isInteger(tabId)) return;
  chrome.tabs.sendMessage(tabId, { kind: "tesktop-lab-songs", songs: labSongs }).catch(() => {});
}

function sanitizeLabContent(report) {
  if (!report || typeof report !== "object") return null;
  try {
    const text = JSON.stringify(report);
    return text.length <= LAB_CONTENT_MAX_BYTES ? JSON.parse(text) : null;
  } catch {
    return null;
  }
}

function updateSenderStatus(status) {
  latestSenderSsrc = Number.isInteger(status?.sender?.audio_ssrc)
    ? status.sender.audio_ssrc : null;
  latestSender = status?.sender ?? null;
  if (activeTest?.lab) trackSenderHealth(activeTest, latestSender);
  if (latestSender?.lab_songs) refreshLabSongs(latestSender.lab_songs);
  if (activeTest && activeTest.sender_ssrc == null && latestSenderSsrc != null) {
    activeTest.sender_ssrc = latestSenderSsrc;
  }
  const sweepNow = latestSender?.test_sweep_active === true && latestSender?.send_enabled === true;
  // AscendCord closed mid-sweep never reports the sweep ending; its next session has a new
  // sender SSRC. Close what was left of the old run and arm for the new one.
  if (sweepNow && autoSweepActive && latestSenderSsrc != null && autoSweepSsrc != null &&
      latestSenderSsrc !== autoSweepSsrc) {
    if (autoSweepRunStarted && activeTest) finishTest();
    if (curveCaptureActive) setCurveCapture(false);
    autoSweepActive = false;
  }
  if (sweepNow && !autoSweepActive) {
    autoSweepActive = true;
    autoSweepSsrc = latestSenderSsrc;
    autoSweepRunStarted = false;
    autoCaptureStartedAt = 0;
    completedTest = null;
    completedExport = null;
    completedExportPersisted = false;
    clearCompletedExport();
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
    time_stretched_samples: matched.reduce((sum, sample) => sum + Number(sample.stretched_samples_delta ?? 0), 0),
  };
}

function collectTestSamples(report, tabId) {
  const exactMatch = latestSenderSsrc == null ? null :
    report.streams.find(stream => stream.ssrc === latestSenderSsrc) ?? null;
  if (autoSweepActive && !autoSweepRunStarted && exactMatch) {
    if (!activeTest) beginTest(true, tabId);
    else if (activeTest.sender_ssrc == null) activeTest.sender_ssrc = latestSenderSsrc;
    if (activeTest && activeTest.tab_id == null) activeTest.tab_id = tabId;
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
      stretched_samples_delta: stream.stretched_samples_delta ?? 0,
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
  const elapsed = Date.now() - autoCaptureStartedAt;
  const returnDone = activeTest?.return_path !== true || (returnLab()?.passes ?? 0) >= LAB_PASSES;
  // Program 3 also waits for the null test of each pass's content.
  const contentDone = activeTest?.program !== 3 ||
    (activeTest.content_reports ?? []).filter(report => report.version === 1).length >= LAB_PASSES;
  const labDone = activeTest?.lab
    ? (activeTest.lab.passes >= LAB_PASSES && returnDone && contentDone) || elapsed >= LAB_MAX_CAPTURE_MS : null;
  if (autoSweepRunStarted && activeTest && autoCaptureStartedAt > 0 &&
      (labDone ?? elapsed >= AUTO_CAPTURE_DURATION_MS)) {
    finishTest();
    setCurveCapture(false, tabId);
  }
}

function publishDiagnostics(rateLimitElapsed = false) {
  diagnosticsPostDirty = true;
  if (diagnosticsPostActive || diagnosticsPostTimer) return;
  const delay = rateLimitElapsed ? 0 : Math.max(0, 1000 - (Date.now() - lastDiagnosticsPostAt));
  if (delay > 0) {
    diagnosticsPostTimer = setTimeout(() => {
      diagnosticsPostTimer = null;
      publishDiagnostics(true);
    }, delay);
    return;
  }
  diagnosticsPostDirty = false;
  diagnosticsPostActive = true;
  lastDiagnosticsPostAt = Date.now();
  const currentTest = activeTest ?? completedTest;
  const testSummary = currentTest ? summarizeSamples(currentTest.samples) : null;
  const curve = [...capturedCurves.values()].find(item =>
    item.ssrc === (currentTest?.sender_ssrc ?? latestSenderSsrc) &&
    (currentTest?.tab_id == null || item.tabId === currentTest.tab_id));
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
    if (diagnosticsPostDirty) publishDiagnostics();
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

function handleMessage(message, sender, sendResponse) {
  if (message?.kind === "tesktop-content-bridge-heartbeat") {
    if (!isDiscordSender(sender)) {
      sendResponse({ ok: false, error: "Heartbeat did not come from a Discord tab." });
      return;
    }
    diagnostics.contentBridgeAt = Date.now();
    refreshSenderStatus().catch(() => null).finally(publishDiagnostics);
    publishDiagnostics();
    sendResponse({
      ok: true,
      lab_armed: latestSender?.test_sweep_active === true && latestSender?.return_lab_supported === true,
    });
    return;
  }

  // Developer channel: the Discord tab can drive what the popup does, so a test run can
  // be repeated, read and the extension reloaded without opening extension pages.
  if (message?.kind === "tesktop-dev") {
    if (!isDiscordSender(sender)) {
      sendResponse({ ok: false, error: "Developer requests come only from a Discord tab." });
      return;
    }
    if (message.request === "reload") {
      sendResponse({ ok: true, version: chrome.runtime.getManifest?.().version ?? null });
      setTimeout(() => chrome.runtime.reload(), 100);
      return;
    }
    const kind = DEV_REQUESTS[message.request];
    if (!kind) {
      sendResponse({ ok: false, error: "Unknown developer request." });
      return;
    }
    return handleMessage({ kind }, sender, sendResponse);
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
    diagnostics.returnPathState = typeof message.observer.return_path === "string"
      ? message.observer.return_path.slice(0, 32) : "off";
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
      publishDiagnostics();
    });

    // Keep only numeric spectrum points in extension memory for the popup graph. Never
    // forward this optional field to older desktop builds, whose strict report schema
    // would reject the complete receive-statistics report.
    const streams = message.report.streams.filter(stream =>
      Number.isInteger(stream?.ssrc) && Array.isArray(stream.spectrum_dbfs) &&
      stream.spectrum_dbfs.length === 48 &&
      Number.isFinite(stream.peak_frequency_hz) && Number.isFinite(stream.peak_dbfs) &&
      Number.isFinite(stream.analysis_sample_rate_hz) &&
      stream.spectrum_dbfs.every(value => Number.isFinite(value) && value >= -120 && value <= 12));
    latestSpectrum = {
      at: Date.now(),
      tabId: sender.tab.id,
      streams: streams.slice(0, 16).map(stream => ({
        ssrc: stream.ssrc,
        spectrum_dbfs: stream.spectrum_dbfs,
        peak_frequency_hz: stream.peak_frequency_hz,
        peak_dbfs: stream.peak_dbfs,
        analysis_sample_rate_hz: stream.analysis_sample_rate_hz,
      })),
    };
    if (curveCaptureActive) {
      for (const stream of latestSpectrum.streams) {
        if (!activeTest || stream.ssrc !== activeTest.sender_ssrc ||
            (activeTest.tab_id != null && activeTest.tab_id !== sender.tab.id)) continue;
        if (activeTest.lab) {
          // Program 2: every window goes through the shared lab analysis instead.
          const raw = message.report.streams.find(item => item?.ssrc === stream.ssrc);
          activeTest.analysis_sample_rate_hz ??= stream.analysis_sample_rate_hz;
          // Concealment, discarded packets or jitter-buffer time stretching change the audio
          // for as long as the event stays inside the analysis window.
          const now = Date.now();
          if (Number(raw?.concealment_events_delta) > 0 || Number(raw?.discarded_packets_delta) > 0 ||
              Number(raw?.stretched_samples_delta) > 0) {
            activeTest.disturbed_until_ms = now + LAB_WINDOW_HOLD_MS;
          }
          Lab.addWindow(activeTest.lab, sanitizeLabWindow(raw?.lab_window),
            now < (activeTest.disturbed_until_ms ?? 0));
          const content = sanitizeLabContent(raw?.lab_content);
          if (content && (activeTest.content_reports ??= []).length < 8) activeTest.content_reports.push(content);
          continue;
        }
        const curveKey = `${sender.tab.id}:${stream.ssrc}`;
        let curve = capturedCurves.get(curveKey);
        if (!curve) {
          if (capturedCurves.size >= 32) {
            capturedCurves.delete(capturedCurves.keys().next().value);
          }
          curve = {
            tabId: sender.tab.id,
            ssrc: stream.ssrc,
            analysis_sample_rate_hz: stream.analysis_sample_rate_hz,
            spectrum_dbfs: Array(SWEEP_BANDS).fill(-120),
            mean_power: Array(SWEEP_BANDS).fill(0),
            mean_peak_frequency_hz: Array(SWEEP_BANDS).fill(0),
            mean_dbfs: Array(SWEEP_BANDS).fill(0),
            m2_db: Array(SWEEP_BANDS).fill(0),
            samples_per_band: Array(SWEEP_BANDS).fill(0),
            covered_bins: Array(SWEEP_BANDS).fill(false),
            last_sample_at_ms: Array(SWEEP_BANDS).fill(0),
          };
          capturedCurves.set(curveKey, curve);
        }
        addCurveSample(curve, stream.peak_frequency_hz, stream.peak_dbfs);
      }
    }

    const now = Date.now();
    const last = lastSentByTab.get(sender.tab.id) ?? 0;
    if (now - last < MIN_INTERVAL_MS) {
      sendResponse({ ok: true, throttled: true });
      return;
    }
    lastSentByTab.set(sender.tab.id, now);

    // AscendCord's receiver schema rejects unknown fields: the lab's extras stay here.
    const desktopReport = {
      ...message.report,
      streams: message.report.streams.map(({
        spectrum_dbfs, peak_frequency_hz, peak_dbfs, analysis_sample_rate_hz, lab_window,
        lab_content, stretched_samples_delta, ...stream
      }) => stream),
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
      auto_capture_waiting: autoSweepActive && !autoSweepRunStarted,
      auto_sweep_active: autoSweepActive,
      auto_sweep_run_started: autoSweepRunStarted,
      auto_capture_elapsed_ms: autoCaptureStartedAt > 0
        ? Math.max(0, Date.now() - autoCaptureStartedAt) : 0,
      latest_sender_ssrc: latestSenderSsrc,
      sender_status_at: lastSenderStatusAt,
      sender_status_error: diagnostics.senderStatusError,
      curve_capture_active: curveCaptureActive,
      auto_export_ready: completedExport !== null,
      auto_export_saved: completedExport !== null && completedExportPersisted,
      test_summary: test ? summarizeSamples(test.samples) : null,
      test_samples: test ? test.samples.slice(-300) : [],
      pipeline_validation: completedExport?.diagnostics?.pipeline_validation ?? null,
      sweep_curve: completedExport?.diagnostics?.sweep_curve ?? null,
      measurement_lab: completedExport?.diagnostics?.measurement_lab ?? null,
      return_lab: completedExport?.diagnostics?.return_lab ?? null,
      return_path_state: diagnostics.returnPathState ?? "off",
      sender_settings: completedExport?.diagnostics?.sender_settings ?? null,
      lab_progress: activeTest?.lab ? {
        passes: activeTest.lab.passes,
        return_passes: activeTest.return_path ? returnLab()?.passes ?? 0 : null,
        passes_needed: LAB_PASSES,
        section: activeTest.lab.section,
        windows: { ...activeTest.lab.windows },
      } : null,
      storage_error: diagnostics.storageError,
    });
    return;
  }

  if (message?.kind === "tesktop-read-lab-history") {
    const read = chrome.storage?.local?.get
      ? chrome.storage.local.get(LAB_HISTORY_KEY) : Promise.resolve({});
    read.then(saved => sendResponse({
      ok: true,
      history: Array.isArray(saved?.[LAB_HISTORY_KEY]) ? saved[LAB_HISTORY_KEY] : [],
    })).catch(error => sendResponse({ ok: false, error: String(error?.message ?? error) }));
    return true;
  }

  if (message?.kind === "tesktop-start-test") {
    refreshSenderStatus(true).catch(() => null).finally(() => {
      beginTest(false, Number.isInteger(sender?.tab?.id) ? sender.tab.id : null);
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
    clearCompletedExport();
    sendResponse({ ok: true });
    return;
  }

  if (message?.kind === "tesktop-read-test-data") {
    exportPersistPromise.finally(() => {
      const test = activeTest ?? completedTest;
      sendResponse({
        ok: true,
        test: test ? { ...test, samples: [...test.samples] } : null,
        export: completedExport ? { ...completedExport, test: { ...completedExport.test, samples: [...completedExport.test.samples] } } : null,
      });
    });
    return true;
    return;
  }

  if (message?.kind === "tesktop-read-local-spectrum") {
    const fresh = latestSpectrum && Date.now() - latestSpectrum.at <= 5000;
    sendResponse({
      ok: true,
      report: fresh ? latestSpectrum : null,
      curve_capture_active: curveCaptureActive,
      curves: [...capturedCurves.values()].map(curve => ({
        tabId: curve.tabId,
        ssrc: curve.ssrc,
        analysis_sample_rate_hz: curve.analysis_sample_rate_hz,
        spectrum_dbfs: curve.spectrum_dbfs,
        gain_db: curve.spectrum_dbfs.map(value => value - SOURCE_PEAK_DBFS),
        standard_deviation_db: curve.mean_dbfs.map((_, index) =>
          curve.samples_per_band[index] > 1
            ? Math.sqrt(curve.m2_db[index] / (curve.samples_per_band[index] - 1)) : null),
        samples_per_band: curve.samples_per_band,
        frequency_hz: sweepFrequencies(),
        covered_bins: curve.covered_bins,
        peak_frequency_hz: curve.mean_peak_frequency_hz.map((mean, index) =>
          curve.samples_per_band[index] > 0 ? mean : null),
        peak_frequency_error_percent: curve.mean_peak_frequency_hz.map((mean, index) =>
          curve.samples_per_band[index] > 0
            ? (mean / sweepFrequencies()[index] - 1) * 100 : null),
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
}

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  // Hydrate a saved result before any event can clear it or report that no export
  // exists. MV3 workers are routinely restarted while their extension stays enabled.
  restoreCompletedExport
    .then(() => handleMessage(message, sender, sendResponse))
    .catch(error => {
      diagnostics.storageError = String(error?.message ?? error).slice(0, 180);
      handleMessage(message, sender, sendResponse);
    });
  return true;
});

chrome.tabs.onRemoved.addListener(tabId => {
  lastSentByTab.delete(tabId);
  if (latestSpectrum?.tabId === tabId) latestSpectrum = null;
  for (const [ssrc, curve] of capturedCurves) {
    if (curve.tabId === tabId) capturedCurves.delete(ssrc);
  }
});
