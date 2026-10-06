const byId = id => document.getElementById(id);
const colors = { grid: "#3b3e46", text: "#969aa4", line: "#5ce0ae", warn: "#f2c46d" };

function showPopupError(error) {
  const detail = String(error?.message ?? error ?? "Unknown popup error").slice(0, 180);
  const diagnostics = byId("diagnostics");
  if (diagnostics) diagnostics.textContent = `Popup script error: ${detail}`;
  const connection = byId("connection");
  if (connection) connection.textContent = "Stereo Proof popup encountered an error";
}

window.addEventListener("error", event => showPopupError(event.error ?? event.message));
window.addEventListener("unhandledrejection", event => showPopupError(event.reason));

function text(node, value) {
  node.textContent = value;
}

function safeSend(message) {
  return new Promise(resolve => {
    let settled = false;
    const finish = value => {
      if (settled) return;
      settled = true;
      resolve(value);
    };
    const fail = error => finish({
      ok: false,
      error: String(error?.message ?? error),
    });

    try {
      const pending = chrome.runtime.sendMessage(message, response => {
        const error = chrome.runtime.lastError;
        if (error) fail(error);
        else finish(response);
      });
      if (pending && typeof pending.then === "function") {
        pending.then(finish, fail);
      }
    } catch (error) {
      fail(error);
    }
  });
}

function render(status, localSpectrum, diagnostics, bridgeError = "") {
  const sender = status?.sender;
  const receiverConnected = Boolean(status?.receiver_connected);
  const report = status?.receiver;
  const connection = byId("connection");
  document.querySelector(".pulse").classList.toggle("live", receiverConnected);
  text(connection, bridgeError || (receiverConnected
    ? "Connected to browser receive stats"
    : sender ? "AscendCord connected · waiting for browser receive report"
      : "Waiting for AscendCord call status"));

  renderDiagnostics(diagnostics, status?.extension_diagnostics_connected
    ? status.extension_diagnostics : null);
  renderTest(diagnostics);

  if (sender) {
    const lines = [
      `Call: ${sender.call_connected ? "connected" : "not connected"} · sending: ${sender.send_enabled ? "on" : "off"}`,
      sender.test_sweep_active
        ? "Source: synthetic 48 kHz stereo sweep · physical microphone bypassed"
        : `Input: ${sender.capture_channels ?? "?"} channels · ${sender.capture_rate_hz ?? "?"} Hz · ${sender.input_dbfs ?? "—"} dBFS`,
      `Encoder: ${sender.opus_bitrate_target_bps ?? "—"} bit/s ${sender.opus_vbr === false ? "CBR" : sender.opus_vbr === true ? "VBR" : ""} · ${sender.opus_application ?? "—"} · ${sender.opus_signal ?? "—"}`,
      `RTP send: ${Math.round((sender.wire_bitrate_bps ?? 0) / 1000)} kb/s · ${sender.wire_packets_per_second ?? 0} packets/s · max gap ${sender.max_send_gap_ms ?? "?"} ms`,
      `Send drops: capture ${sender.capture_ring_drops ?? 0}/${sender.capture_worker_drops ?? 0} · playback ${sender.playback_ring_drops ?? 0} · loop stalls ${sender.transport_loop_stalls_per_second ?? 0}/s · pacing catch-ups ${sender.pacing_catchups_per_second ?? 0}/s`,
      `Processing: stereo ${sender.force_stereo === true ? "forced" : "unknown"} · NS ${sender.noise_suppression === false ? "off" : "on/unknown"} · RNNoise ${sender.rnnoise_vad === false ? "off" : "on/unknown"} · AEC/AGC ${sender.echo_cancellation === false && sender.automatic_gain === false ? "off" : "on/unknown"}`,
      `Opus FEC ${sender.opus_fec === false ? "off" : "on/unknown"} · loss target ${sender.opus_packet_loss_target_percent ?? "?"}% · SSRC ${sender.audio_ssrc ?? "waiting"}`,
      `Diagnostic sweep: ${sender.test_sweep_active ? "active (synthetic source)" : "inactive"}`,
    ];
    text(byId("sender"), lines.join("\n"));
  } else {
    text(byId("sender"), "Waiting for AscendCord call status");
  }

  const streams = byId("streams");
  streams.replaceChildren();
  const captureCurve = byId("capture-curve");
  captureCurve.disabled = true;
  const audio = Array.isArray(report?.streams) ? report.streams : [];
  const match = audio.find(stream => stream.ssrc != null && stream.ssrc === sender?.audio_ssrc);
  const measuredMatch = localSpectrum?.streams?.find(stream => stream.ssrc === sender?.audio_ssrc);
  const curve = localSpectrum?.curves?.find(stream =>
    stream.tabId === localSpectrum?.report?.tabId && stream.ssrc === sender?.audio_ssrc);
  const curveValues = curve?.spectrum_dbfs ?? measuredMatch?.spectrum_dbfs;
  const curveSummary = summarizeCurve(curveValues);
  drawSpectrum(curveValues);
  captureCurve.disabled = !(sender?.test_sweep_active && sender?.send_enabled && match);
  captureCurve.textContent = localSpectrum?.curve_capture_active
    ? "Stop curve capture" : "Capture sweep curve";
  byId("curve-note").textContent = curve
    ? `Sweep response captured · ${curveValues.length} matched bands · ${curveSummary.peak_to_peak_db.toFixed(2)} dB peak-to-peak · ±${curveSummary.peak_deviation_db.toFixed(2)} dB from median.`
    : sender?.test_sweep_active && sender?.send_enabled && match
      ? localSpectrum?.curve_capture_active
        ? "Exact sender SSRC matched · capturing the log-frequency response automatically."
        : "Exact sender SSRC matched · ready to capture the log-frequency response."
      : sender?.test_sweep_active
        ? "Sweep is active, but no exact receiver SSRC match is available yet."
        : "Set ASCENDCORD_TEST_SWEEP=1 before launch; the diagnostic replaces microphone audio with an equal-level stepped sweep.";

  text(byId("receiver"), receiverConnected
    ? match
      ? "AscendCord sender matched to this browser’s inbound RTP stream."
      : `Browser reported ${audio.length} inbound audio stream(s); waiting for exact sender SSRC ${sender?.audio_ssrc ?? "—"}.`
    : sender ? "No receiver report reached the local bridge. Read the pipeline diagnosis below."
      : "Join voice in the browser and start an AscendCord call to expose receive stats.");
  byId("receiver").classList.toggle("empty", !receiverConnected);

  for (const stream of audio) {
    const card = document.createElement("div");
    card.className = "stream";
    const matched = stream.ssrc != null && stream.ssrc === sender?.audio_ssrc;
    const [verdictClass, verdictText] = stereoVerdict(stream);
    const codecChannels = stream.channels == null ? "codec channels unavailable" : `${stream.channels}ch codec`;
    const trackChannels = stream.track_channels == null ? "track channels unavailable" : `${stream.track_channels}ch track`;
    card.textContent = `${matched ? "AscendCord exact SSRC" : `Inbound audio SSRC ${stream.ssrc ?? "unknown"}`} · ${stream.codec}\n${codecChannels} · ${trackChannels} · ${Math.round(stream.bitrate_bps / 1000)} kb/s\nLoss ${Number(stream.loss_percent).toFixed(2)}% · jitter ${Number(stream.jitter_ms).toFixed(1)} ms · jitter buffer ${Number(stream.jitter_buffer_delay_ms).toFixed(1)} ms\nConcealment ${Number(stream.concealed_samples_per_second).toFixed(0)} samples/s · ${stream.concealment_events_delta} events · ${stream.discarded_packets_delta} discarded packets this interval\n${stream.active ? "Decoded audio energy active" : "No recent decoded audio energy"} · ${stream.sdp_fmtp_stereo === true ? "stereo fmtp" : stream.sdp_fmtp_stereo === false ? "mono fmtp" : "fmtp unknown"}\n${verdictText}`;
    card.classList.add(verdictClass);
    streams.append(card);
  }
}

function renderDiagnostics(value, bridgeDiagnostics = null) {
  const node = byId("diagnostics");
  const fresh = timestamp => timestamp > 0 && Date.now() - timestamp <= 7000;
  if (value?.healthForwardError) {
    text(node, `5 · Extension health could not reach AscendCord’s local diagnostics endpoint: ${value.healthForwardError}`);
    return;
  }
  if (bridgeDiagnostics) {
    const prefix = bridgeDiagnostics.content_bridge ? "Content bridge live" : "Content bridge heartbeat stale";
    if (bridgeDiagnostics.forward_error) {
      text(node, `4 · ${prefix}; receiver report rejected: ${bridgeDiagnostics.forward_error}`);
      return;
    }
    if (bridgeDiagnostics.observer_state === "hook-install-failed") {
      text(node, `2 · ${prefix}; WebRTC hook install failed: ${bridgeDiagnostics.observer_error || "no browser error supplied"}`);
      return;
    }
    if (bridgeDiagnostics.observer_state === "waiting-for-peer-api") {
      text(node, `2 · ${prefix}; RTCPeerConnection is unavailable in the Discord page context.`);
      return;
    }
  }
  if (!fresh(value?.contentBridgeAt)) {
    text(node, "1 · Discord content bridge not detected. Check extension enablement/site access, then reload the Discord tab.");
    return;
  }
  if (!fresh(value?.observerAt)) {
    text(node, "2 · Content bridge is alive, but the page WebRTC observer has not reported. This isolates the failure to main-world injection or page API access.");
    return;
  }
  if (value.observerState === "hook-install-failed") {
    text(node, `2 · WebRTC hook install failed: ${value.observerError || "no browser error supplied"}`);
    return;
  }
  if (value.observerState === "waiting-for-peer-api") {
    text(node, "2 · Observer loaded but RTCPeerConnection is not available in the Discord page context yet.");
    return;
  }
  if (!fresh(value.observerReportAt)) {
    text(node, `3 · WebRTC hook is ${value.observerState}; no stats sample has reached the isolated bridge yet.`);
    return;
  }
  if (value.forwardError) {
    text(node, `4 · Browser stats were observed, but loopback forwarding failed: ${value.forwardError}`);
    return;
  }
  if (value.inboundStreams === 0) {
    text(node, `3 · Observer is sampling ${value.peerConnections} peer connection(s), but found no inbound audio RTP stats.`);
    return;
  }
  text(node, `4 · Reporting is live: ${value.inboundStreams} inbound audio stream(s), ${value.peerConnections} peer connection(s).`);
}

function renderTest(diagnostics) {
  const run = diagnostics?.test ?? { running: false, sample_count: 0 };
  const summary = diagnostics?.test_summary;
  const active = run.running === true;
  byId("start-test").disabled = active;
  byId("stop-test").disabled = !active;
  byId("reset-test").disabled = active;
  byId("export-test").disabled = active || diagnostics?.auto_export_ready !== true;

  const elapsed = run.started_at_ms
    ? Math.max(0, ((run.ended_at_ms ?? Date.now()) - run.started_at_ms) / 1000) : 0;
  const senderLabel = Number.isInteger(run.sender_ssrc)
    ? `· sender SSRC ${run.sender_ssrc}` : "· waiting for sender match";
  text(byId("test-status"), active
    ? `${run.automatic ? "AUTO-CAPTURING" : "CAPTURING"} · ${elapsed.toFixed(0)} s · ${run.sample_count} samples ${senderLabel} · stops after a full sweep pass.`
    : diagnostics?.auto_capture_waiting
      ? "AUTO-ARMED · synthetic sweep detected; waiting for the exact sender SSRC in browser RTP stats. It will capture and stop automatically."
      : summary && summary.matched_samples === 0
        ? `CAPTURED · ${run.sample_count} samples, but none matched an AscendCord sender SSRC. Receiver metrics are omitted as unverified.`
      : run.sample_count > 0
        ? `CAPTURED · ${elapsed.toFixed(0)} s · ${run.sample_count} samples · JSON report prepared automatically · run ${run.id}`
      : "Waiting for an AscendCord synthetic sweep; capture and JSON preparation start automatically.");

  const metrics = byId("test-summary");
  metrics.replaceChildren();
  const format = (value, digits, suffix) => Number.isFinite(value)
    ? `${value.toFixed(digits)}${suffix}` : "Unverified";
  const items = summary ? [
    ["Average loss", format(summary.average_loss_percent, 2, "%")],
    ["Peak loss", format(summary.peak_loss_percent, 2, "%")],
    ["Average jitter", format(summary.average_jitter_ms, 1, " ms")],
    ["Peak jitter", format(summary.peak_jitter_ms, 1, " ms")],
    ["Concealment", format(summary.concealment_samples_per_second, 0, "/s")],
    ["Conceal events", `${summary.concealment_events}`],
    ["Discarded", `${summary.discarded_packets}`],
    ["Exact match", `${summary.matched_samples}/${summary.samples}`],
  ] : [];
  for (const [label, value] of items) {
    const card = document.createElement("div");
    card.className = "metric";
    const number = document.createElement("b");
    const caption = document.createElement("span");
    text(number, value); text(caption, label);
    card.append(number, caption);
    metrics.append(card);
  }
  drawTimeline(diagnostics?.test_samples ?? []);
}

function drawTimeline(samples) {
  const canvas = byId("timeline");
  const ctx = canvas.getContext("2d");
  const width = canvas.width, height = canvas.height;
  const left = 52, right = width - 12, top = 18, bottom = height - 30;
  ctx.clearRect(0, 0, width, height);
  const key = byId("timeline-metric").value;
  const bitrate = key === "bitrate_bps";
  const label = bitrate ? "kb/s" : key === "loss_percent" ? "%" : key === "jitter_ms" ? "ms" : key === "discarded_packets_delta" ? "packets" : "samples/s";
  const data = samples.filter(sample => sample.sender_match === true)
    .map(sample => Number(sample[key] ?? 0) / (bitrate ? 1000 : 1));
  const peak = Math.max(...data, key === "loss_percent" ? 1 : 0.1);
  const maxY = key === "loss_percent" ? Math.max(1, Math.ceil(peak)) : peak * 1.15;
  ctx.font = "11px Segoe UI, sans-serif";
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  for (let line = 0; line <= 4; line++) {
    const y = top + line * (bottom - top) / 4;
    const value = maxY * (1 - line / 4);
    ctx.strokeStyle = colors.grid; ctx.beginPath(); ctx.moveTo(left, y); ctx.lineTo(right, y); ctx.stroke();
    ctx.fillStyle = colors.text; ctx.fillText(value.toFixed(value < 10 ? 1 : 0), left - 6, y);
  }
  ctx.fillStyle = colors.text; ctx.textAlign = "left"; ctx.fillText(label, 7, 11);
  if (data.length < 2) {
    ctx.textAlign = "center"; ctx.fillStyle = "#a8abb4";
    ctx.fillText("Collecting receiver samples…", width / 2, (top + bottom) / 2);
    return;
  }
  ctx.strokeStyle = colors.line; ctx.lineWidth = 2.5; ctx.beginPath();
  data.forEach((value, index) => {
    const x = left + index / (data.length - 1) * (right - left);
    const y = bottom - Math.max(0, value) / maxY * (bottom - top);
    if (index === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  });
  ctx.stroke();
  ctx.fillStyle = colors.text; ctx.textAlign = "left"; ctx.fillText(`${data.length} latest samples`, left, height - 12);
  ctx.textAlign = "right"; ctx.fillText("newest", right, height - 12);
}

function drawSpectrum(values) {
  const canvas = byId("spectrum");
  const ctx = canvas.getContext("2d");
  const width = canvas.width, height = canvas.height;
  const left = 42, right = width - 8, top = 10, bottom = height - 27;
  ctx.clearRect(0, 0, width, height);
  ctx.font = "11px Segoe UI, sans-serif";
  ctx.textBaseline = "middle"; ctx.textAlign = "right";
  const summary = summarizeCurve(values);
  const bound = summary ? Math.max(0.5, Math.ceil(summary.peak_deviation_db * 2) / 2) : 1;
  for (let line = 0; line <= 4; line++) {
    const deviation = bound - line * bound / 2;
    const y = top + line * (bottom - top) / 4;
    ctx.strokeStyle = colors.grid; ctx.beginPath(); ctx.moveTo(left, y); ctx.lineTo(right, y); ctx.stroke();
    ctx.fillStyle = colors.text; ctx.fillText(`${deviation > 0 ? "+" : ""}${deviation.toFixed(1)} dB`, left - 5, y);
  }
  ctx.textAlign = "center";
  for (const [hz, label] of [[20, "20"], [100, "100"], [1000, "1k"], [10000, "10k"], [20000, "20k Hz"]]) {
    const x = left + Math.log(hz / 20) / Math.log(1000) * (right - left);
    ctx.strokeStyle = "#30333a"; ctx.beginPath(); ctx.moveTo(x, top); ctx.lineTo(x, bottom); ctx.stroke();
    ctx.fillStyle = colors.text; ctx.fillText(label, x, height - 12);
  }
  if (!Array.isArray(values) || values.length !== 48) {
    ctx.fillStyle = "#a8abb4"; ctx.textAlign = "center";
    ctx.fillText("Waiting for exact matched sender spectrum", width / 2, height / 2);
    return;
  }
  const relative = summary.relative_db;
  ctx.strokeStyle = colors.line; ctx.lineWidth = 2; ctx.beginPath();
  relative.forEach((db, index) => {
    const x = left + index / (relative.length - 1) * (right - left);
    const y = top + (bound - Math.max(-bound, Math.min(bound, db))) / (2 * bound) * (bottom - top);
    index ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
  });
  ctx.stroke();
  ctx.strokeStyle = "#727782"; ctx.lineWidth = 1; ctx.setLineDash([3, 3]);
  const centerY = top + (bottom - top) / 2;
  ctx.beginPath(); ctx.moveTo(left, centerY); ctx.lineTo(right, centerY); ctx.stroke(); ctx.setLineDash([]);
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

function stereoVerdict(stream) {
  if (stream.left_dbfs == null || stream.right_dbfs == null) return ["warn", "L/R not measured"];
  const loudest = Math.max(stream.left_dbfs, stream.right_dbfs);
  if (loudest < -70) return ["warn", "L/R silent"];
  const mono = (stream.lr_correlation ?? 1) > 0.995 && (stream.side_dbfs ?? -120) < loudest - 40;
  const detail = `L ${stream.left_dbfs.toFixed(0)} / R ${stream.right_dbfs.toFixed(0)} dBFS · side ${(stream.side_dbfs ?? -120).toFixed(0)} dBFS · corr ${(stream.lr_correlation ?? 1).toFixed(3)}`;
  return mono ? ["warn", `MONO decoded · ${detail}`] : ["good", `TRUE STEREO decoded · ${detail}`];
}

async function refresh() {
  const [statusResult, localSpectrum, diagnostics] = await Promise.all([
    safeSend({ kind: "tesktop-read-status" }),
    safeSend({ kind: "tesktop-read-local-spectrum" }),
    safeSend({ kind: "tesktop-read-diagnostics" }),
  ]);
  render(statusResult?.status ?? null, localSpectrum, diagnostics,
    statusResult?.ok ? "" : statusResult?.error ?? "Local AscendCord bridge unavailable");
}

byId("refresh").addEventListener("click", refresh);
byId("timeline-metric").addEventListener("change", refresh);
byId("start-test").addEventListener("click", async () => {
  await safeSend({ kind: "tesktop-start-test" });
  refresh();
});
byId("stop-test").addEventListener("click", async () => {
  await safeSend({ kind: "tesktop-stop-test" });
  refresh();
});
byId("reset-test").addEventListener("click", async () => {
  await safeSend({ kind: "tesktop-reset-test" });
  refresh();
});
byId("export-test").addEventListener("click", async () => {
  const result = await safeSend({ kind: "tesktop-read-test-data" });
  if (!result?.ok || !result.export) return;
  const payload = result.export;
  const url = URL.createObjectURL(new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" }));
  const link = document.createElement("a");
  link.href = url; link.download = `ascendcord-receiver-test-${Date.now()}.json`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
});
byId("capture-curve").addEventListener("click", async () => {
  const local = await safeSend({ kind: "tesktop-read-local-spectrum" });
  await safeSend({ kind: "tesktop-set-curve-capture", active: !local?.curve_capture_active });
  refresh();
});

refresh();
setInterval(refresh, 2500);
