import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

test("calibrates peak sine amplitude and rejects mixed-tone windows", async () => {
  const source = await readFile(new URL("../rtc-observer.js", import.meta.url), "utf8");
  const start = source.indexOf("function fitSinePeak(");
  const end = source.indexOf("\n(() => {", start);
  assert.ok(start >= 0 && end > start, "the observer exposes its pure analysis function to this test");
  const analysisContext = vm.createContext({});
  vm.runInContext(`${source.slice(start, end)}\nglobalThis.fitSinePeak = fitSinePeak;`, analysisContext);
  const { fitSinePeak } = analysisContext;
  const sampleRate = 48_000;
  const length = 16_384;
  const pureTone = Float32Array.from({ length }, (_, index) =>
    0.25 * Math.sin(2 * Math.PI * 1_000 * index / sampleRate + 0.7));
  const fit = fitSinePeak(pureTone, 1_000, sampleRate);
  assert.ok(Math.abs(fit.amplitude - 0.25) < 1e-5);
  assert.ok(Math.abs(20 * Math.log10(fit.amplitude) - 20 * Math.log10(0.25)) < 0.001);
  assert.ok(fit.explained > 0.999);

  const transition = Float32Array.from({ length }, (_, index) =>
    0.125 * (Math.sin(2 * Math.PI * 1_000 * index / sampleRate) +
      Math.sin(2 * Math.PI * 1_300 * index / sampleRate)));
  const mixedFit = fitSinePeak(transition, 1_000, sampleRate);
  assert.ok(mixedFit.explained < 0.8, "a band transition should fail the fit-quality gate");
});

test("auto-starts a matched sweep capture without a popup button press", async () => {
  const source = await readFile(new URL("../service-worker.js", import.meta.url), "utf8");
  let onMessage;
  let onRemoved;
  let now = 10_000;
  let forwarded;
  let healthForwarded;
  let captureControl;
  let senderSweep = true;
  const chrome = {
    runtime: { onMessage: { addListener(listener) { onMessage = listener; } } },
    tabs: {
      onRemoved: { addListener(listener) { onRemoved = listener; } },
      sendMessage: async (_tabId, message) => { captureControl = message; return {}; },
    },
  };
  const context = {
    chrome,
    URL,
    Date: class extends Date { static now() { return now; } },
    AbortController,
    setTimeout,
    clearTimeout,
    fetch: async (url, options) => {
      if (url.endsWith("/v1/status")) {
        return { ok: true, status: 200, json: async () => ({ sender: {
          audio_ssrc: 77, test_sweep_active: senderSweep, send_enabled: true,
        } }) };
      }
      if (url.endsWith("/v1/diagnostics")) healthForwarded = JSON.parse(options.body);
      else forwarded = JSON.parse(options.body);
      return { ok: true, status: 204 };
    },
  };
  vm.runInNewContext(source, context);

  const spectrum = Array.from({ length: 48 }, (_, index) => -90 + index);
  const peakFrequency = 20_000;
  const report = {
    protocol: 1, sampled_at_ms: now, peer_connections: 1,
    streams: [{ ssrc: 77, codec: "audio/opus", channels: 2, track_channels: 2,
      sdp_fmtp_stereo: true, active: true, spectrum_dbfs: spectrum,
      peak_frequency_hz: peakFrequency, peak_dbfs: -18, analysis_sample_rate_hz: 48_000 }],
  };
  const sender = { tab: { id: 5, url: "https://discord.com/channels/1/2" } };
  const send = message => new Promise(resolve => {
    const result = onMessage(message, sender, resolve);
    if (result !== true) resolve(undefined);
  });

  const heartbeat = await send({ kind: "tesktop-content-bridge-heartbeat" });
  assert.equal(heartbeat.ok, true);
  await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(healthForwarded.content_bridge, true);
  assert.equal(healthForwarded.observer_state, "not-seen");
  let diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.contentBridgeAt, now);
  assert.equal(diagnostics.observerReportAt, 0);
  now += 1_100;

  await send({ kind: "tesktop-receiver-report", report });
  await new Promise(resolve => setTimeout(resolve, 10));
  diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.observerReportAt, now);
  assert.equal(diagnostics.inboundStreams, 1);
  assert.equal(diagnostics.peerConnections, 1);
  assert.equal(diagnostics.test.running, true);
  assert.equal(diagnostics.test.automatic, true);
  assert.equal(diagnostics.test.sender_ssrc, 77);
  assert.equal(diagnostics.test_summary.matched_samples, 1);
  await new Promise(resolve => setTimeout(resolve, 1_050));
  assert.equal(healthForwarded.inbound_streams, 1);
  assert.equal(healthForwarded.peer_connections, 1);
  assert.equal(healthForwarded.sweep_test.running, true,
    "the post-capture diagnostic must include the automatically started test");
  assert.equal(healthForwarded.sweep_test.automatic, true);
  assert.equal("gain_db" in (healthForwarded.sweep_curve ?? {}), false,
    "the desktop endpoint keeps its strict schema; richer curve fields remain local");
  const testData = await send({ kind: "tesktop-read-test-data" });
  assert.equal(testData.test.samples[0].ssrc, 77);
  assert.equal(testData.test.samples[0].sender_match, true);
  assert.equal(captureControl.active, true, "an exact sweep match should start curve capture automatically");
  assert.equal("spectrum_dbfs" in forwarded.streams[0], false);
  let local = await send({ kind: "tesktop-read-local-spectrum" });
  assert.deepEqual(Array.from(local.report.streams[0].spectrum_dbfs), spectrum);

  await send({ kind: "tesktop-set-curve-capture", active: true });
  assert.equal(captureControl.active, true);
  now += 2_000;
  const next = { ...report, sampled_at_ms: now, streams: [{
    ...report.streams[0], ssrc: 77, codec: "audio/opus",
    spectrum_dbfs: spectrum.map(value => value + 1), peak_dbfs: -17, analysis_sample_rate_hz: 48_000,
  }] };
  await send({ kind: "tesktop-receiver-report", report: next });
  local = await send({ kind: "tesktop-read-local-spectrum" });
  assert.equal(local.curve_capture_active, true);
  assert.equal(local.curves[0].ssrc, 77);
  assert.equal(local.curves[0].tabId, 5);
  assert.equal(local.curves[0].spectrum_dbfs[47], -17);
  assert.equal(local.curves[0].samples_per_band[47], 1);
  senderSweep = false;
  await send({ kind: "tesktop-read-status" });
  diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, false);
  assert.equal(diagnostics.test.sample_count, 2);
  assert.equal(diagnostics.auto_export_ready, true);
  assert.equal(diagnostics.curve_capture_active, false);
  const exported = await send({ kind: "tesktop-read-test-data" });
  assert.equal(exported.export.format, "AscendCord Stereo Proof receiver test v1");
  assert.equal(exported.export.test.samples.length, 2);
  assert.equal(exported.export.diagnostics.test_summary.matched_samples, 2);
  onRemoved(5);
  local = await send({ kind: "tesktop-read-local-spectrum" });
  assert.equal(local.report, null);
  assert.equal(local.curves.length, 0);
});

test("manual capture binds a sender that appears after capture starts and excludes unmatched stats", async () => {
  const source = await readFile(new URL("../service-worker.js", import.meta.url), "utf8");
  let onMessage;
  let now = 20_000;
  let senderAvailable = false;
  const chrome = {
    runtime: { onMessage: { addListener(listener) { onMessage = listener; } } },
    tabs: {
      onRemoved: { addListener() {} },
      sendMessage: async () => ({}),
    },
  };
  const context = {
    chrome,
    URL,
    Date: class extends Date { static now() { return now; } },
    AbortController,
    setTimeout,
    clearTimeout,
    fetch: async (url, options) => {
      if (url.endsWith("/v1/status")) {
        return { ok: true, status: 200, json: async () => ({ sender: senderAvailable ? {
          audio_ssrc: 88, test_sweep_active: true, send_enabled: true,
        } : null }) };
      }
      return { ok: true, status: 204 };
    },
  };
  vm.runInNewContext(source, context);
  const sender = { tab: { id: 6, url: "https://discord.com/channels/1/2" } };
  const send = message => new Promise(resolve => {
    const result = onMessage(message, sender, resolve);
    if (result !== true) resolve(undefined);
  });

  const started = await send({ kind: "tesktop-start-test" });
  assert.equal(started.test.sender_ssrc, null);
  const report = loss => ({
    protocol: 1, sampled_at_ms: now, peer_connections: 1,
    streams: [{ ssrc: 88, codec: "audio/opus", loss_percent: loss }],
  });
  await send({ kind: "tesktop-receiver-report", report: report(99) });
  await new Promise(resolve => setTimeout(resolve, 0));
  let diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test_summary.matched_samples, 0);
  assert.equal(diagnostics.test_summary.average_loss_percent, null);
  assert.equal(diagnostics.test_summary.unmatched_samples, 1);

  senderAvailable = true;
  now += 2_000;
  await send({ kind: "tesktop-receiver-report", report: report(2) });
  await new Promise(resolve => setTimeout(resolve, 0));
  diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.sender_ssrc, 88);
  assert.equal(diagnostics.test.automatic, false);
  assert.equal(diagnostics.test_summary.matched_samples, 1);
  assert.equal(diagnostics.test_summary.unmatched_samples, 1);
  assert.equal(diagnostics.test_summary.average_loss_percent, 2);
  const data = await send({ kind: "tesktop-read-test-data" });
  assert.equal(data.test.samples[0].sender_match, false);
  assert.equal(data.test.samples[1].sender_match, true);
});

test("automatically captures two full sweeps and prepares a measured response report", async () => {
  const source = await readFile(new URL("../service-worker.js", import.meta.url), "utf8");
  let onMessage;
  let now = 30_000;
  const chrome = {
    runtime: { onMessage: { addListener(listener) { onMessage = listener; } } },
    tabs: {
      onRemoved: { addListener() {} },
      sendMessage: async () => ({}),
    },
  };
  const context = {
    chrome,
    URL,
    Date: class extends Date { static now() { return now; } },
    AbortController,
    setTimeout,
    clearTimeout,
    fetch: async url => url.endsWith("/v1/status")
      ? { ok: true, status: 200, json: async () => ({ sender: {
        audio_ssrc: 99, test_sweep_active: true, send_enabled: true,
      } }) }
      : { ok: true, status: 204 },
  };
  vm.runInNewContext(source, context);
  const sender = { tab: { id: 7, url: "https://discord.com/channels/1/2" } };
  const send = message => new Promise(resolve => {
    const result = onMessage(message, sender, resolve);
    if (result !== true) resolve(undefined);
  });
  const startAt = now;
  const expectedFrequency = index => 20 * 1000 ** (index / 47);
  const reportAt = sampleAt => {
    const elapsed = sampleAt - startAt;
    const band = Math.floor(elapsed / 700) % 48;
    const pass = Math.floor(elapsed / (48 * 700));
    return { protocol: 1, sampled_at_ms: sampleAt, peer_connections: 1, streams: [{
      ssrc: 99, codec: "audio/opus", channels: 2, track_channels: 2,
      sdp_fmtp_stereo: true, active: true, loss_percent: 0,
      spectrum_dbfs: Array(48).fill(-100),
      peak_frequency_hz: expectedFrequency(band), peak_dbfs: -18 + pass * 0.5,
      analysis_sample_rate_hz: 48_000,
    }] };
  };

  await send({ kind: "tesktop-receiver-report", report: reportAt(now) });
  await new Promise(resolve => setTimeout(resolve, 0));
  let diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, true);
  assert.equal(diagnostics.curve_capture_active, true);

  for (let elapsed = 250; elapsed <= 72_000; elapsed += 250) {
    now = startAt + elapsed;
    await send({ kind: "tesktop-receiver-report", report: reportAt(now) });
    await new Promise(resolve => setTimeout(resolve, 0));
  }
  diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, false);
  assert.equal(diagnostics.auto_export_ready, true);
  assert.equal(diagnostics.curve_capture_active, false);
  const exported = await send({ kind: "tesktop-read-test-data" });
  assert.ok(exported.export.test.samples.length > 100);
  assert.equal(exported.export.test.samples.every(sample => sample.sender_match), true);
  assert.equal(exported.export.diagnostics.sweep_curve.ssrc, 99);
  assert.equal(exported.export.diagnostics.sweep_curve.spectrum_dbfs.length, 48);
  assert.equal(exported.export.diagnostics.sweep_curve.samples_per_band.length, 48);
  assert.ok(exported.export.diagnostics.sweep_curve.samples_per_band.every(count => count >= 2));
  assert.equal(exported.export.diagnostics.sweep_curve.measured_bins, 48);
  assert.equal(exported.export.diagnostics.sweep_curve.coverage_complete, true);
  assert.equal(exported.export.diagnostics.pipeline_validation.passed, true);
  assert.equal(exported.export.diagnostics.sweep_curve.peak_frequency_hz.length, 48);
});

test("automatically persists a completed proof so an MV3 worker restart keeps the report", async () => {
  const source = await readFile(new URL("../service-worker.js", import.meta.url), "utf8");
  let onMessage;
  let now = 40_000;
  let savedExport = null;
  const storage = {
    async get() { return savedExport ? { tesktopCompletedSweepExport: savedExport } : {}; },
    async set(value) { savedExport = value.tesktopCompletedSweepExport; },
    async remove() { savedExport = null; },
  };
  const sender = { tab: { id: 8, url: "https://discord.com/channels/1/2" } };
  const report = () => ({
    protocol: 1, sampled_at_ms: now, peer_connections: 1,
    streams: [{
      ssrc: 101, codec: "audio/opus", channels: 2, track_channels: 2,
      sdp_fmtp_stereo: true, active: true, loss_percent: 0, jitter_ms: 1,
      spectrum_dbfs: Array(48).fill(-18),
      peak_frequency_hz: 20 * 1000 ** (Math.floor((now - 40_000) / 700) % 48 / 47),
      peak_dbfs: -18, analysis_sample_rate_hz: 48_000,
    }],
  });

  const createWorker = () => {
    const chrome = {
      storage: { local: storage },
      runtime: { onMessage: { addListener(listener) { onMessage = listener; } } },
      tabs: {
        onRemoved: { addListener() {} },
        sendMessage: async () => ({}),
      },
    };
    vm.runInNewContext(source, {
      chrome, URL,
      Date: class extends Date { static now() { return now; } },
      AbortController, setTimeout, clearTimeout,
      fetch: async url => url.endsWith("/v1/status")
        ? { ok: true, status: 200, json: async () => ({ sender: {
          audio_ssrc: 101, test_sweep_active: true, send_enabled: true,
        } }) }
        : { ok: true, status: 204 },
    });
    return message => new Promise(resolve => {
      onMessage(message, sender, resolve);
    });
  };
  const waitForWork = () => new Promise(resolve => setTimeout(resolve, 5));

  const firstWorker = createWorker();
  await firstWorker({ kind: "tesktop-receiver-report", report: report() });
  await waitForWork();
  for (let elapsed = 250; elapsed <= 72_000; elapsed += 250) {
    now = 40_000 + elapsed;
    await firstWorker({ kind: "tesktop-receiver-report", report: report() });
    if (elapsed % 1_000 === 0) await waitForWork();
  }
  const beforeRestart = await firstWorker({ kind: "tesktop-read-test-data" });
  assert.equal(beforeRestart.export.diagnostics.pipeline_validation.passed, true);
  assert.ok(savedExport, "the completed JSON is written to extension local storage");

  const restartedWorker = createWorker();
  const afterRestart = await restartedWorker({ kind: "tesktop-read-test-data" });
  const diagnostics = await restartedWorker({ kind: "tesktop-read-diagnostics" });
  assert.equal(afterRestart.export.test.id, beforeRestart.export.test.id);
  assert.ok(afterRestart.export.test.samples.length > 100);
  assert.equal(diagnostics.auto_export_ready, true);
  assert.equal(diagnostics.auto_export_saved, true);
  assert.equal(diagnostics.pipeline_validation.passed, true);
});

test("program 3 captures until two full passes and keeps a comparable lab report", async () => {
  const labSource = await readFile(new URL("../lab.js", import.meta.url), "utf8");
  const source = await readFile(new URL("../service-worker.js", import.meta.url), "utf8");
  let onMessage;
  let now = 50_000;
  let forwarded;
  let senderSweep = true;
  const storage = {};
  const chrome = {
    runtime: { onMessage: { addListener(listener) { onMessage = listener; } } },
    tabs: { onRemoved: { addListener() {} }, sendMessage: async () => ({}) },
    storage: { local: {
      get: async key => ({ [key]: storage[key] }),
      set: async values => Object.assign(storage, values),
      remove: async key => { delete storage[key]; },
    } },
  };
  const context = vm.createContext({
    chrome, URL, AbortController, setTimeout, clearTimeout,
    Date: class extends Date { static now() { return now; } },
    fetch: async (url, options) => {
      if (url.endsWith("/v1/status")) {
        return { ok: true, status: 200, json: async () => ({ sender: {
          audio_ssrc: 91, test_sweep_active: senderSweep, send_enabled: true, test_program: 3,
          opus_bitrate_target_bps: 510_000, opus_application: "Audio", force_stereo: true,
          audio_ssrc_secret: "not kept",
        } }) };
      }
      if (!url.endsWith("/v1/diagnostics")) forwarded = JSON.parse(options.body);
      return { ok: true, status: 204 };
    },
  });
  vm.runInContext(labSource, context);
  vm.runInContext(source, context);
  const { PROGRAM } = context.AscendCordLab;
  const sender = { tab: { id: 8, url: "https://discord.com/channels/1/2" } };
  const send = message => new Promise(resolve => {
    const result = onMessage(message, sender, resolve);
    if (result !== true) resolve(undefined);
  });
  const tone = (grid, index, left, right, correlation) => ({
    kind: "tone", steady: true, grid, index,
    left: { peak_dbfs: left, thd_db: -70, thdn_db: -60 },
    right: { peak_dbfs: right, thd_db: -70, thdn_db: -60 },
    correlation,
  });
  const silence = { kind: "silence", loudest_dbfs: -120, left_rms_dbfs: -110, right_rms_dbfs: -111 };
  const pass = () => [
    silence, silence, silence,
    ...PROGRAM.sweep_hz.map((_, i) => tone("sweep", i, -12.04, -13.04, 1)),
    ...Array.from({ length: 12 }, (_, i) => tone("sweep", i * 4, -12.04, -62.04, 0)),
    ...Array.from({ length: 12 }, (_, i) => tone("sweep", i * 4, -62.04, -12.04, 0)),
    ...Array.from({ length: 12 }, (_, i) => tone("sweep", i * 4, -12.04, -12.04, -1)),
    ...PROGRAM.ladder_dbfs.map((dbfs, i) => tone("ladder", i, dbfs - 0.5, dbfs - 0.5, 1)),
  ];
  const windows = [{ kind: "unknown" }, ...pass(), ...pass(), silence, silence, silence];
  // The page reports each pass's null test a little after the pass ends.
  const passLength = pass().length;
  const contentAfter = new Set([1 + passLength + 2, windows.length - 1]);
  const nullTest = index => ({
    version: 1, blocks: 80, slipped_blocks: 0, dropout_blocks: 0, gaps: 0,
    sections: { music: { blocks: 20, srr_db: index < passLength + 5 ? 28 : 30, gain_db: 0 } },
    bands_hz: [1_000], band_srr_db: { music: [40] },
    pre_echo_db: { values: [-50, -48], median_db: -48, worst_db: -48 },
    post_echo_db: { values: [-40], median_db: -40, worst_db: -40 },
  });
  for (const [index, window] of windows.entries()) {
    now += 120;
    await send({ kind: "tesktop-receiver-report", report: {
      protocol: 1, sampled_at_ms: now, peer_connections: 1,
      streams: [{ ssrc: 91, codec: "audio/opus", channels: 2, track_channels: 2,
        sdp_fmtp_stereo: true, active: true, spectrum_dbfs: Array(48).fill(-30),
        peak_frequency_hz: 1_000, peak_dbfs: -12, analysis_sample_rate_hz: 48_000,
        concealment_events_delta: 0, discarded_packets_delta: 0, stretched_samples_delta: 0,
        lab_window: window, lab_content: contentAfter.has(index) ? nullTest(index) : undefined }],
    } });
    await new Promise(resolve => setTimeout(resolve, 1));
  }
  const diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, false, "two passes and their null tests complete the capture");
  const lab = diagnostics.measurement_lab;
  assert.equal(lab.version, 3);
  assert.equal(lab.passes, 2);
  assert.equal(lab.summary.measured_response_bands, 48);
  assert.ok(Math.abs(lab.response.left_gain_db[10]) < 0.01);
  assert.ok(Math.abs(lab.response.right_gain_db[10] + 1) < 0.01);
  assert.ok(Math.abs(lab.summary.median_separation_db - 50) < 0.01);
  assert.equal(lab.summary.stereo_preserved, true);
  assert.ok(Math.abs(lab.linearity.fit.slope - 1) < 0.001);
  assert.equal(lab.linearity.gain_db.filter(Number.isFinite).length, PROGRAM.ladder_dbfs.length,
    "every ladder step, up to +3 dBFS, is measured");
  assert.ok(Math.abs(lab.summary.noise_floor_dbfs + 110) < 0.01);
  assert.equal(diagnostics.sender_settings.opus_bitrate_target_bps, 510_000);
  assert.equal("audio_ssrc_secret" in diagnostics.sender_settings, false);
  assert.equal(diagnostics.pipeline_validation.checks.all_48_sweep_bands_measured, true);
  for (const field of ["lab_window", "lab_content", "stretched_samples_delta"]) {
    assert.equal(field in forwarded.streams[0], false, `the desktop report keeps its schema: ${field}`);
  }
  assert.equal(lab.content.passes, 2, "each pass's null test is kept");
  assert.equal(lab.content.sections.music.srr_db, 30);
  await new Promise(resolve => setTimeout(resolve, 5));
  const history = await send({ kind: "tesktop-read-lab-history" });
  assert.equal(history.history.length, 1);
  assert.equal(history.history[0].measurement_lab.passes, 2);
});

test("program 3 also plays back from the browser and waits for AscendCord's measurement", async () => {
  const labSource = await readFile(new URL("../lab.js", import.meta.url), "utf8");
  const source = await readFile(new URL("../service-worker.js", import.meta.url), "utf8");
  let onMessage;
  let now = 80_000;
  let returnPasses = 0;
  const controls = [];
  const storage = {};
  const chrome = {
    runtime: { onMessage: { addListener(listener) { onMessage = listener; } } },
    tabs: { onRemoved: { addListener() {} }, sendMessage: async (_tab, message) => { controls.push(message); return {}; } },
    storage: { local: {
      get: async key => ({ [key]: storage[key] }),
      set: async values => Object.assign(storage, values),
      remove: async key => { delete storage[key]; },
    } },
  };
  const context = vm.createContext({
    chrome, URL, AbortController, setTimeout, clearTimeout,
    Date: class extends Date { static now() { return now; } },
    fetch: async url => {
      if (url.endsWith("/v1/status")) {
        return { ok: true, status: 200, json: async () => ({ sender: {
          audio_ssrc: 93, test_sweep_active: true, send_enabled: true, test_program: 3,
          return_lab_supported: true,
          return_lab: { version: 3, passes: returnPasses, ssrc: 5, summary: { measured_response_bands: 48 } },
        } }) };
      }
      return { ok: true, status: 204 };
    },
  });
  vm.runInContext(labSource, context);
  vm.runInContext(source, context);
  const { PROGRAM } = context.AscendCordLab;
  const sender = { tab: { id: 9, url: "https://discord.com/channels/1/2" } };
  const send = message => new Promise(resolve => {
    const result = onMessage(message, sender, resolve);
    if (result !== true) resolve(undefined);
  });
  const tone = (grid, index) => ({ kind: "tone", steady: true, grid, index,
    left: { peak_dbfs: -12.04 }, right: { peak_dbfs: -12.04 }, correlation: 1 });
  const silence = { kind: "silence", loudest_dbfs: -120, left_rms_dbfs: -120, right_rms_dbfs: -120 };
  const pass = () => [silence, silence, silence,
    ...PROGRAM.sweep_hz.map((_, i) => tone("sweep", i)),
    ...[0, 1, 2].flatMap(() => Array.from({ length: 12 }, (_, i) => tone("sweep", i * 4))),
    ...PROGRAM.ladder_dbfs.map((_, i) => tone("ladder", i))];
  const nullTest = { version: 1, sections: { noise: { blocks: 10, srr_db: 40, gain_db: 0 } } };
  const deliver = async (windows, contentAt = new Set()) => {
    for (const [index, window] of windows.entries()) {
      now += 120;
      await send({ kind: "tesktop-receiver-report", report: { protocol: 1, sampled_at_ms: now,
        peer_connections: 1, streams: [{ ssrc: 93, codec: "audio/opus", channels: 2, track_channels: 2,
          sdp_fmtp_stereo: true, active: true, spectrum_dbfs: Array(48).fill(-30), peak_frequency_hz: 1_000,
          peak_dbfs: -12, analysis_sample_rate_hz: 48_000, lab_window: window,
          lab_content: contentAt.has(index) ? nullTest : undefined }] } });
      await new Promise(resolve => setTimeout(resolve, 1));
    }
  };
  const both = [{ kind: "unknown" }, ...pass(), ...pass(), silence, silence, silence];
  await deliver(both, new Set([5, both.length - 1]));
  assert.ok(controls.some(message => message.kind === "tesktop-return-path" && message.active === true),
    "the browser is asked to play the program back");
  let diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, true, "the capture waits for AscendCord's two return passes");
  returnPasses = 2;
  now += 2_000;
  await send({ kind: "tesktop-read-status" });
  await deliver([silence]);
  diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, false);
  assert.equal(diagnostics.return_lab.passes, 2);
  assert.ok(controls.some(message => message.kind === "tesktop-return-path" && message.active === false),
    "the browser microphone is restored when the test ends");
});

test("a new AscendCord session after a crash starts a new capture", async () => {
  const source = await readFile(new URL("../service-worker.js", import.meta.url), "utf8");
  let onMessage;
  let now = 120_000;
  let ssrc = 300;
  const controls = [];
  const chrome = {
    runtime: { onMessage: { addListener(listener) { onMessage = listener; } } },
    tabs: { onRemoved: { addListener() {} }, sendMessage: async (_t, message) => { controls.push(message); return {}; } },
  };
  vm.runInNewContext(source, {
    chrome, URL, AbortController, setTimeout, clearTimeout,
    Date: class extends Date { static now() { return now; } },
    fetch: async url => url.endsWith("/v1/status")
      ? { ok: true, status: 200, json: async () => ({ sender: { audio_ssrc: ssrc, test_sweep_active: true, send_enabled: true } }) }
      : { ok: true, status: 204 },
  });
  const sender = { tab: { id: 4, url: "https://discord.com/channels/1/2" } };
  const send = message => new Promise(resolve => {
    const result = onMessage(message, sender, resolve);
    if (result !== true) resolve(undefined);
  });
  const report = () => ({ protocol: 1, sampled_at_ms: now, peer_connections: 1,
    streams: [{ ssrc, codec: "audio/opus", channels: 2, track_channels: 2, active: true }] });
  await send({ kind: "tesktop-receiver-report", report: report() });
  await new Promise(resolve => setTimeout(resolve, 5));
  let diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.sender_ssrc, 300);
  // AscendCord is killed mid-sweep and started again: same sweep flag, new SSRC.
  ssrc = 301;
  now += 2_000;
  await send({ kind: "tesktop-read-status" });
  await send({ kind: "tesktop-receiver-report", report: report() });
  await new Promise(resolve => setTimeout(resolve, 5));
  diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, true);
  assert.equal(diagnostics.test.sender_ssrc, 301, "the new session gets its own capture");
});

test("the developer channel answers Discord tabs only and can reload the extension", async () => {
  const source = await readFile(new URL("../service-worker.js", import.meta.url), "utf8");
  let onMessage;
  let reloads = 0;
  const chrome = {
    runtime: {
      onMessage: { addListener(listener) { onMessage = listener; } },
      reload() { reloads++; },
      getManifest: () => ({ version: "9.9.9" }),
    },
    tabs: { onRemoved: { addListener() {} }, sendMessage: async () => ({}) },
    storage: { local: {
      get: async () => ({ ascendcordLabHistory: [{ id: "run" }] }),
      set: async () => {},
      remove: async () => {},
    } },
  };
  const context = {
    chrome, URL, Date, AbortController, setTimeout, clearTimeout,
    fetch: async () => ({ ok: true, status: 204 }),
  };
  vm.runInNewContext(source, context);
  const ask = (request, url) => new Promise(resolve => {
    const result = onMessage({ kind: "tesktop-dev", request }, { tab: { id: 3, url } }, resolve);
    if (result !== true) resolve(undefined);
  });
  const discord = "https://discord.com/channels/1/2";

  assert.equal((await ask("history", "https://example.com/")).ok, false);
  assert.equal((await ask("format-disk", discord)).ok, false);
  assert.deepEqual((await ask("history", discord)).history.map(entry => entry.id), ["run"]);
  assert.equal(typeof (await ask("diagnostics", discord)).observerState, "string");
  assert.equal((await ask("reload", discord)).version, "9.9.9");
  await new Promise(resolve => setTimeout(resolve, 150));
  assert.equal(reloads, 1);
});
