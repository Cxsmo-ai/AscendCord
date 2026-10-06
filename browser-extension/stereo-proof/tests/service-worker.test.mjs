import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

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
  const report = {
    protocol: 1, sampled_at_ms: now, peer_connections: 1,
    streams: [{ ssrc: 77, codec: "audio/opus", spectrum_dbfs: spectrum }],
  };
  const sender = { tab: { id: 5, url: "https://discord.com/channels/1/2" } };
  const send = message => new Promise(resolve => {
    const result = onMessage(message, sender, resolve);
    if (result !== true) resolve(undefined);
  });

  const heartbeat = await send({ kind: "tesktop-content-bridge-heartbeat" });
  assert.equal(heartbeat.ok, true);
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(healthForwarded.content_bridge, true);
  assert.equal(healthForwarded.observer_state, "not-seen");
  let diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.contentBridgeAt, now);
  assert.equal(diagnostics.observerReportAt, 0);
  now += 1_100;

  await send({ kind: "tesktop-receiver-report", report });
  await new Promise(resolve => setTimeout(resolve, 0));
  diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.observerReportAt, now);
  assert.equal(diagnostics.inboundStreams, 1);
  assert.equal(diagnostics.peerConnections, 1);
  assert.equal(diagnostics.test.running, true);
  assert.equal(diagnostics.test.automatic, true);
  assert.equal(diagnostics.test.sender_ssrc, 77);
  assert.equal(diagnostics.test_summary.matched_samples, 1);
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(healthForwarded.inbound_streams, 1);
  assert.equal(healthForwarded.peer_connections, 1);
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
    ssrc: 77, codec: "audio/opus", spectrum_dbfs: spectrum.map(value => value + 1),
  }] };
  await send({ kind: "tesktop-receiver-report", report: next });
  local = await send({ kind: "tesktop-read-local-spectrum" });
  assert.equal(local.curve_capture_active, true);
  assert.equal(local.curves[0].ssrc, 77);
  assert.equal(local.curves[0].tabId, 5);
  assert.equal(local.curves[0].spectrum_dbfs[0], spectrum[0] + 1);
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

test("automatically stops after a full sweep pass and prepares the JSON report", async () => {
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
  const report = { protocol: 1, sampled_at_ms: now, peer_connections: 1, streams: [
    { ssrc: 99, codec: "audio/opus", loss_percent: 0, spectrum_dbfs: Array(48).fill(-18) },
  ] };

  await send({ kind: "tesktop-receiver-report", report });
  await new Promise(resolve => setTimeout(resolve, 0));
  let diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, true);
  assert.equal(diagnostics.curve_capture_active, true);

  now += 39_999;
  await send({ kind: "tesktop-receiver-report", report: { ...report, sampled_at_ms: now } });
  await new Promise(resolve => setTimeout(resolve, 0));
  diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, true);

  now += 1;
  await send({ kind: "tesktop-receiver-report", report: { ...report, sampled_at_ms: now } });
  await new Promise(resolve => setTimeout(resolve, 0));
  diagnostics = await send({ kind: "tesktop-read-diagnostics" });
  assert.equal(diagnostics.test.running, false);
  assert.equal(diagnostics.auto_export_ready, true);
  assert.equal(diagnostics.curve_capture_active, false);
  const exported = await send({ kind: "tesktop-read-test-data" });
  assert.equal(exported.export.test.samples.length, 3);
  assert.equal(exported.export.test.samples.every(sample => sample.sender_match), true);
  assert.equal(exported.export.diagnostics.sweep_curve.ssrc, 99);
  assert.equal(exported.export.diagnostics.sweep_curve.spectrum_dbfs.length, 48);
  assert.equal(exported.export.diagnostics.sweep_curve.peak_to_peak_db, 0);
});
