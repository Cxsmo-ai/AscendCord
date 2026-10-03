const byId = id => document.getElementById(id);

function text(node, value) {
  node.textContent = value;
}

function render(status) {
  const sender = status?.sender;
  const receiverConnected = Boolean(status?.receiver_connected);
  const report = status?.receiver;
  const connection = byId("connection");
  const pulse = document.querySelector(".pulse");
  pulse.classList.toggle("live", receiverConnected);
  text(connection, receiverConnected ? "Connected to browser receive stats" : "Waiting for a browser voice session");

  if (sender) {
    const lines = [
      sender.call_connected ? "Call: connected" : "Call: not connected",
      `Input: ${sender.capture_channels ?? "?"} channels · ${sender.capture_rate_hz ?? "?"} Hz · ${sender.input_dbfs ?? "—"} dBFS`,
      `Encoder target: ${sender.opus_bitrate_target_bps ?? "—"} bit/s ${sender.opus_vbr === false ? "CBR (locked)" : sender.opus_vbr === true ? "VBR" : ""} · ${sender.opus_application ?? "—"} · ${sender.opus_signal ?? "—"}`,
      `Measured RTP wire send: ${Math.round((sender.wire_bitrate_bps ?? 0) / 1000)} kb/s · ${sender.wire_packets_per_second ?? 0} packets/s · transport loop stalls ${sender.transport_loop_stalls_per_second ?? 0} · drift catch-ups ${sender.pacing_catchups_per_second ?? 0}/s · worst send gap ${sender.max_send_gap_ms ?? "?"} ms`,
      `Capture queue drops: ${sender.capture_ring_drops ?? 0} callback · ${sender.capture_worker_drops ?? 0} worker`,
      `Stereo forced: ${sender.force_stereo === true ? "yes" : "unknown"} · Suppression: ${sender.noise_suppression === false ? "off" : "on/unknown"} · RNNoise VAD: ${sender.rnnoise_vad === false ? "off" : "on/unknown"}`,
      `AEC/AGC: ${sender.echo_cancellation === false && sender.automatic_gain === false ? "off" : "on/unknown"} · continuous send: ${sender.always_transmit === true ? "on" : "off/unknown"}`,
      `Opus FEC: ${sender.opus_fec === false ? "off" : "on/unknown"} · packet-loss target ${sender.opus_packet_loss_target_percent ?? "?"}%`,
      `SSRC match: ${sender.audio_ssrc ?? "waiting for voice transport"}`,
    ];
    text(byId("sender"), lines.join("\n"));
  } else {
    text(byId("sender"), "Waiting for Tesktop call status");
  }

  const streams = byId("streams");
  streams.replaceChildren();
  const receiver = byId("receiver");
  if (!receiverConnected || !report) {
    text(receiver, "Join voice in the browser to expose receive stats.");
    receiver.hidden = false;
    return;
  }

  const audio = Array.isArray(report.streams) ? report.streams : [];
  const match = audio.find(stream => stream.ssrc != null && stream.ssrc === sender?.audio_ssrc);
  text(receiver, match
    ? "Tesktop sender matched to this browser's inbound RTP stream."
    : `Browser sees ${audio.length} inbound audio stream(s); waiting for the Tesktop sender match.`);
  receiver.hidden = false;

  for (const stream of audio) {
    const card = document.createElement("div");
    card.className = "stream";
    const matched = stream.ssrc != null && stream.ssrc === sender?.audio_ssrc;
    const [verdictClass, verdictText] = stereoVerdict(stream);
    const codecChannels = stream.channels == null ? "codec channels unavailable" : `${stream.channels}ch codec`;
    const trackChannels = stream.track_channels == null ? "track channels unavailable" : `${stream.track_channels}ch track`;
    card.innerHTML = `<strong>${matched ? "Tesktop sender (exact SSRC)" : "Inbound audio · sender not identified"} · ${escapeHtml(stream.codec)}</strong><br>${escapeHtml(codecChannels)} · ${escapeHtml(trackChannels)} · received ${Math.round(stream.bitrate_bps / 1000)} kb/s<br>Loss ${Number(stream.loss_percent).toFixed(2)}% · network jitter ${Number(stream.jitter_ms).toFixed(1)} ms · jitter buffer ${Number(stream.jitter_buffer_delay_ms).toFixed(1)} ms<br>Concealed ${Number(stream.concealed_samples_per_second).toFixed(0)} samples/s · ${stream.concealment_events_delta} concealment events · ${stream.discarded_packets_delta} discarded packets this interval<br>${stream.active ? "Decoded audio energy is active" : "No recent decoded audio energy"}<br>${stream.sdp_fmtp_stereo === true ? "fmtp stereo=1" : stream.sdp_fmtp_stereo === false ? "fmtp: mono decode" : "fmtp unknown"} · <span class="${verdictClass}">${escapeHtml(verdictText)}</span>`;
    streams.append(card);
  }
}

function stereoVerdict(stream) {
  if (stream.left_dbfs == null || stream.right_dbfs == null) return ["warn", "L/R not measured yet (click the Discord tab once to start audio analysis)"];
  const loudest = Math.max(stream.left_dbfs, stream.right_dbfs);
  if (loudest < -70) return ["warn", "L/R silent"];
  const mono = (stream.lr_correlation ?? 1) > 0.995 && (stream.side_dbfs ?? -120) < loudest - 40;
  const detail = `L ${stream.left_dbfs.toFixed(0)} / R ${stream.right_dbfs.toFixed(0)} dBFS · side ${(stream.side_dbfs ?? -120).toFixed(0)} dBFS · corr ${(stream.lr_correlation ?? 1).toFixed(3)}`;
  return mono ? ["warn", `MONO decoded (L = R) · ${detail}`] : ["good", `TRUE STEREO decoded · ${detail}`];
}

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, char => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;",
  })[char]);
}

async function refresh() {
  const result = await chrome.runtime.sendMessage({ kind: "tesktop-read-status" });
  if (!result?.ok) {
    text(byId("connection"), result?.error ?? "Tesktop loopback bridge unavailable");
    document.querySelector(".pulse").classList.remove("live");
    return;
  }
  render(result.status);
}

byId("refresh").addEventListener("click", refresh);
refresh();
