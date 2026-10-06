(() => {
  "use strict";

  const marker = "tesktop-stereo-proof-v1";
  const SPECTRUM_POINTS = 48;
  const SPECTRUM_LOW_HZ = 20;
  const SPECTRUM_HIGH_HZ = 20_000;
  let NativePeerConnection = null;
  let InstalledPeerConnection = null;

  const peers = new Set();
  const meters = new Map();
  let audioContext = null;

  // Chromium decodes Opus as mono unless the fmtp line carries stereo=1, even when the
  // codec reports two channels. Ask for stereo decode so the measured channels below
  // reflect what the sender transmitted. This changes only this tab's receiver.
  function stereoSdp(sdp) {
    if (typeof sdp !== "string") return sdp;
    const payloads = [...sdp.matchAll(/^a=rtpmap:(\d+) opus\/48000\/2\r?$/gim)].map(match => match[1]);
    let result = sdp;
    for (const payload of payloads) {
      result = result.replace(new RegExp(`^(a=fmtp:${payload} )(.*?)(\r?)$`, "gm"), (line, head, params, cr) => {
        const kept = params.split(";").map(part => part.trim())
          .filter(part => part && !/^(stereo|sprop-stereo)=/i.test(part));
        kept.push("stereo=1", "sprop-stereo=1");
        return `${head}${kept.join(";")}${cr}`;
      });
    }
    return result;
  }

  function wrapDescription(name) {
    const original = NativePeerConnection.prototype[name];
    if (typeof original !== "function") return;
    NativePeerConnection.prototype[name] = function (description, ...rest) {
      if (description && typeof description.sdp === "string") {
        description = { type: description.type, sdp: stereoSdp(description.sdp) };
      }
      return original.call(this, description, ...rest);
    };
  }

  function wrapCreate(name) {
    const original = NativePeerConnection.prototype[name];
    if (typeof original !== "function") return;
    NativePeerConnection.prototype[name] = async function (...args) {
      const description = await original.apply(this, args);
      return description && typeof description.sdp === "string"
        ? { type: description.type, sdp: stereoSdp(description.sdp) }
        : description;
    };
  }

  function context() {
    if (!audioContext) {
      try {
        audioContext = new AudioContext({ latencyHint: "playback" });
      } catch {
        return null;
      }
    }
    if (audioContext.state === "suspended") audioContext.resume().catch(() => {});
    return audioContext;
  }

  // Per-channel level and left/right correlation of the decoded remote track. Only these
  // numbers leave the page; no samples are retained beyond one analysis window.
  function meter(track) {
    let entry = meters.get(track.id);
    if (entry || track.readyState !== "live") return entry ?? null;
    const ctx = context();
    if (!ctx) return null;
    try {
      const source = ctx.createMediaStreamSource(new MediaStream([track]));
      const splitter = ctx.createChannelSplitter(2);
      const left = ctx.createAnalyser();
      const right = ctx.createAnalyser();
      left.fftSize = right.fftSize = 16384;
      left.minDecibels = right.minDecibels = -120;
      left.maxDecibels = right.maxDecibels = 12;
      source.connect(splitter);
      splitter.connect(left, 0);
      splitter.connect(right, 1);
      entry = {
        source, splitter, left, right,
        l: new Float32Array(16384), r: new Float32Array(16384),
        leftSpectrum: new Float32Array(left.frequencyBinCount),
        rightSpectrum: new Float32Array(right.frequencyBinCount),
        channels: source.channelCount,
      };
      meters.set(track.id, entry);
      track.addEventListener("ended", () => {
        try { source.disconnect(); splitter.disconnect(); } catch {}
        meters.delete(track.id);
      });
      return entry;
    } catch {
      return null;
    }
  }

  function measure(track) {
    const entry = track ? meter(track) : null;
    if (!entry || audioContext?.state !== "running") return null;
    entry.left.getFloatTimeDomainData(entry.l);
    entry.right.getFloatTimeDomainData(entry.r);
    entry.left.getFloatFrequencyData(entry.leftSpectrum);
    entry.right.getFloatFrequencyData(entry.rightSpectrum);
    let ll = 0, rr = 0, lr = 0, diff = 0;
    for (let i = 0; i < entry.l.length; i++) {
      const a = entry.l[i], b = entry.r[i];
      ll += a * a; rr += b * b; lr += a * b; diff += (a - b) * (a - b);
    }
    const n = entry.l.length;
    const db = power => power > 0 ? Math.max(-120, 10 * Math.log10(power / n)) : -120;
    const correlation = ll > 0 && rr > 0 ? lr / Math.sqrt(ll * rr) : null;
    const spectrumDbfs = [];
    for (let point = 0; point < SPECTRUM_POINTS; point++) {
      const fraction = point / (SPECTRUM_POINTS - 1);
      const frequency = SPECTRUM_LOW_HZ * (SPECTRUM_HIGH_HZ / SPECTRUM_LOW_HZ) ** fraction;
      const bin = Math.max(1, Math.min(entry.leftSpectrum.length - 1,
        Math.round(frequency * entry.leftSpectrum.length * 2 / audioContext.sampleRate)));
      // Average L/R power. AudioBuffer analyser values are in dBFS; averaging in
      // linear power avoids a channel from dominating the display by a few dB.
      const leftDb = entry.leftSpectrum[bin];
      const rightDb = entry.rightSpectrum[bin];
      const power = (10 ** (leftDb / 10) + 10 ** (rightDb / 10)) / 2;
      spectrumDbfs.push(Math.max(-120, Math.min(12, 10 * Math.log10(power))));
    }
    return {
      left_dbfs: db(ll),
      right_dbfs: db(rr),
      side_dbfs: db(diff / 4),
      lr_correlation: correlation === null ? null : Math.max(-1, Math.min(1, correlation)),
      spectrum_dbfs: spectrumDbfs,
    };
  }
  const previous = new Map();
  const peerIds = new WeakMap();
  let nextPeerId = 1;
  let running = false;
  let curveCapture = false;
  let sampleTimer;
  let lastInstallError = "";
  let lastReportedObserverState = "";

  function reportObserverState(state, error = "") {
    const identity = `${state}:${error}`;
    if (identity === lastReportedObserverState) return;
    lastReportedObserverState = identity;
    window.postMessage({
      source: marker,
      observer: {
        state,
        peer_api: typeof window.RTCPeerConnection === "function",
        error: String(error).slice(0, 180),
        at_ms: Date.now(),
      },
    }, location.origin);
  }

  function installPeerConnection() {
    const current = window.RTCPeerConnection;
    if (typeof current !== "function") {
      reportObserverState("waiting-for-peer-api", lastInstallError);
      return false;
    }
    if (InstalledPeerConnection === current) return true;
    try {
      NativePeerConnection = current;
      wrapDescription("setLocalDescription");
      wrapDescription("setRemoteDescription");
      wrapCreate("createOffer");
      wrapCreate("createAnswer");

      const Wrapped = function (...args) {
        const peer = new NativePeerConnection(...args);
        peerIds.set(peer, nextPeerId++);
        peers.add(peer);
        peer.addEventListener("connectionstatechange", () => {
          if (peer.connectionState === "closed") peers.delete(peer);
        });
        peer.addEventListener("signalingstatechange", () => {
          if (peer.signalingState === "closed") peers.delete(peer);
        });
        return peer;
      };
      Wrapped.prototype = NativePeerConnection.prototype;
      Object.setPrototypeOf(Wrapped, NativePeerConnection);
      Object.defineProperty(window, "RTCPeerConnection", {
        configurable: true,
        writable: true,
        value: Wrapped,
      });
      InstalledPeerConnection = Wrapped;
      lastInstallError = "";
      reportObserverState("hook-installed");
      return true;
    } catch (error) {
      lastInstallError = String(error?.message ?? error);
      reportObserverState("hook-install-failed", lastInstallError);
      return false;
    }
  }

  function numeric(value, fallback = 0) {
    return Number.isFinite(value) ? value : fallback;
  }

  async function collectPeer(peer, output) {
    if (peer.connectionState === "closed") {
      peers.delete(peer);
      return;
    }

    let report;
    try {
      report = await peer.getStats();
    } catch {
      return;
    }

    const all = [...report.values()];
    const codecs = new Map(all.filter(stat => stat.type === "codec").map(stat => [stat.id, stat]));
    const tracks = new Map(peer.getReceivers().filter(receiver => receiver.track)
      .map(receiver => [receiver.track.id, receiver.track]));

    for (const stat of all) {
      if (stat.type !== "inbound-rtp" ||
          !(stat.kind === "audio" || stat.mediaType === "audio")) continue;

      const codec = codecs.get(stat.codecId);
      const track = tracks.get(stat.trackIdentifier);
      const key = `${peerIds.get(peer)}:${stat.id}`;
      const old = previous.get(key);
      const elapsedMs = old ? Math.max(1, numeric(stat.timestamp) - old.timestamp) : 0;
      const byteDelta = old ? Math.max(0, numeric(stat.bytesReceived) - old.bytes) : 0;
      const packetDelta = old ? Math.max(0, numeric(stat.packetsReceived) - old.packets) : 0;
      const lostDelta = old ? numeric(stat.packetsLost) - old.lost : 0;
      const lossPercent = packetDelta + Math.max(0, lostDelta) > 0
        ? 100 * Math.max(0, lostDelta) / (packetDelta + Math.max(0, lostDelta))
        : 0;
      const energyDelta = old && Number.isFinite(stat.totalAudioEnergy)
        ? Math.max(0, stat.totalAudioEnergy - old.energy)
        : 0;
      const concealmentDelta = old ? Math.max(0, numeric(stat.concealedSamples) - old.concealed) : 0;
      const concealmentEventsDelta = old ? Math.max(0, numeric(stat.concealmentEvents) - old.concealmentEvents) : 0;
      const discardedDelta = old ? Math.max(0, numeric(stat.packetsDiscarded) - old.discarded) : 0;
      const jitterBufferDelayDelta = old ? Math.max(0, numeric(stat.jitterBufferDelay) - old.jitterBufferDelay) : 0;
      const jitterBufferEmittedDelta = old ? Math.max(0, numeric(stat.jitterBufferEmittedCount) - old.jitterBufferEmitted) : 0;

      previous.set(key, {
        timestamp: numeric(stat.timestamp),
        bytes: numeric(stat.bytesReceived),
        packets: numeric(stat.packetsReceived),
        lost: numeric(stat.packetsLost),
        energy: numeric(stat.totalAudioEnergy),
        concealed: numeric(stat.concealedSamples),
        concealmentEvents: numeric(stat.concealmentEvents),
        discarded: numeric(stat.packetsDiscarded),
        jitterBufferDelay: numeric(stat.jitterBufferDelay),
        jitterBufferEmitted: numeric(stat.jitterBufferEmittedCount),
      });

      const stereo = measure(track);
      output.push({
        ssrc: Number.isInteger(stat.ssrc) && stat.ssrc >= 0 && stat.ssrc <= 0xffffffff
          ? stat.ssrc : null,
        codec: typeof codec?.mimeType === "string" ? codec.mimeType.slice(0, 32) : "audio/unknown",
        channels: Number.isInteger(codec?.channels) ? codec.channels : null,
        track_channels: Number.isInteger(track?.getSettings?.().channelCount)
          ? track.getSettings().channelCount : null,
        sample_rate_hz: Number.isInteger(codec?.clockRate) ? codec.clockRate : null,
        bitrate_bps: elapsedMs > 0 ? Math.min(10_000_000, Math.round(byteDelta * 8000 / elapsedMs)) : 0,
        packets_received: Math.max(0, Math.floor(numeric(stat.packetsReceived))),
        packets_lost: Math.max(-1_000_000, Math.min(1_000_000, Math.trunc(numeric(stat.packetsLost)))),
        loss_percent: lossPercent,
        jitter_ms: Math.max(0, numeric(stat.jitter) * 1000),
        concealed_samples: Math.max(0, Math.floor(numeric(stat.concealedSamples))),
        concealment_events: Math.max(0, Math.floor(numeric(stat.concealmentEvents))),
        discarded_packets: Math.max(0, Math.floor(numeric(stat.packetsDiscarded))),
        audio_level: Math.max(0, Math.min(1, numeric(stat.audioLevel))),
        active: energyDelta > 0.00001 || numeric(stat.audioLevel) > 0.005,
        concealed_samples_per_second: elapsedMs > 0 ? Math.min(384000, concealmentDelta * 1000 / elapsedMs) : 0,
        concealment_events_delta: Math.floor(concealmentEventsDelta),
        discarded_packets_delta: Math.floor(discardedDelta),
        jitter_buffer_delay_ms: jitterBufferEmittedDelta > 0
          ? Math.min(60000, jitterBufferDelayDelta * 1000 / jitterBufferEmittedDelta) : 0,
        sdp_fmtp_stereo: typeof codec?.sdpFmtpLine === "string" ? /(^|;)\s*stereo=1/.test(codec.sdpFmtpLine) : null,
        left_dbfs: stereo?.left_dbfs ?? null,
        right_dbfs: stereo?.right_dbfs ?? null,
        side_dbfs: stereo?.side_dbfs ?? null,
        lr_correlation: stereo?.lr_correlation ?? null,
        spectrum_dbfs: stereo?.spectrum_dbfs ?? null,
      });
    }
  }

  async function sample() {
    if (running) return;
    running = true;
    try {
      const activePeers = [...peers].slice(0, 32);
      const streams = [];
      for (const peer of activePeers) {
        await collectPeer(peer, streams);
        if (streams.length >= 16) break;
      }
      window.postMessage({
        source: marker,
        observer: {
          state: InstalledPeerConnection ? "sampling" : "hook-not-installed",
          peer_api: typeof window.RTCPeerConnection === "function",
          error: lastInstallError,
          at_ms: Date.now(),
        },
        report: {
          protocol: 1,
          sampled_at_ms: Date.now(),
          peer_connections: activePeers.length,
          streams: streams.slice(0, 16),
        },
      }, location.origin);
    } finally {
      running = false;
    }
  }

  // At document_start some Chromium builds expose WebRTC a little later. Keep
  // trying instead of permanently disabling the observer on its first tick.
  let installAttempts = 0;
  const installTimer = setInterval(() => {
    installAttempts++;
    if (installPeerConnection() || installAttempts >= 200) clearInterval(installTimer);
  }, 50);
  installPeerConnection();
  window.addEventListener("message", event => {
    if (event.source !== window || event.origin !== location.origin ||
        event.data?.source !== "tesktop-stereo-proof-control") return;
    const next = event.data.captureCurve === true;
    if (curveCapture === next) return;
    curveCapture = next;
    clearInterval(sampleTimer);
    sampleTimer = setInterval(sample, curveCapture ? 250 : 2000);
    sample();
  });
  for (const kind of ["pointerdown", "keydown"]) {
    window.addEventListener(kind, () => context(), { capture: true, passive: true });
  }
  sampleTimer = setInterval(sample, 2000);
  setTimeout(sample, 750);
})();
