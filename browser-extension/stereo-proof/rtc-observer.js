function fitSinePeak(samples, frequencyHz, sampleRate) {
  const mean = samples.reduce((sum, value) => sum + value, 0) / samples.length;
  let cc = 0, ss = 0, cs = 0, yc = 0, ys = 0, total = 0;
  const phaseStep = Math.PI * 2 * frequencyHz / sampleRate;
  const stepCos = Math.cos(phaseStep), stepSin = Math.sin(phaseStep);
  let c = 1, s = 0;
  for (let i = 0; i < samples.length; i++) {
    const value = samples[i] - mean;
    cc += c * c; ss += s * s; cs += c * s;
    yc += value * c; ys += value * s; total += value * value;
    const nextC = c * stepCos - s * stepSin;
    s = s * stepCos + c * stepSin;
    c = nextC;
  }
  const determinant = cc * ss - cs * cs;
  if (determinant <= 0 || total <= 0) return null;
  const a = (yc * ss - ys * cs) / determinant;
  const b = (ys * cc - yc * cs) / determinant;
  const amplitude = Math.hypot(a, b);
  const explained = Math.max(0, Math.min(1, (a * yc + b * ys) / total));
  return { amplitude, explained };
}

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
        audioContext = new AudioContext({ latencyHint: "playback", sampleRate: 48_000 });
      } catch {
        try {
          audioContext = new AudioContext({ latencyHint: "playback" });
        } catch {
          return null;
        }
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
      // Keep each capture independent so a band transition does not smear into
      // the next measurement window.
      left.smoothingTimeConstant = right.smoothingTimeConstant = 0;
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
    // Identify the received sine using every FFT bin instead of assigning the
    // loudest of 48 sparse display probes to a sweep band.
    let peakBin = 1;
    let peakPower = 0;
    for (let bin = 1; bin < entry.leftSpectrum.length; bin++) {
      const power = (10 ** (entry.leftSpectrum[bin] / 10) +
        10 ** (entry.rightSpectrum[bin] / 10)) / 2;
      if (power > peakPower) {
        peakPower = power;
        peakBin = bin;
      }
    }
    // Refine the FFT peak between bins before assigning it to a known sweep
    // tone. The analyser spectrum is windowed (Blackman), so its displayed
    // magnitude is not an absolute dBFS amplitude measurement. Estimate the
    // sine's peak amplitude directly from the time-domain PCM instead.
    const bin = entry.leftSpectrum;
    const leftDb = bin[Math.max(1, peakBin - 1)];
    const centerDb = bin[peakBin];
    const rightDb = bin[Math.min(bin.length - 1, peakBin + 1)];
    const curvature = leftDb - 2 * centerDb + rightDb;
    const binOffset = curvature < 0
      ? Math.max(-0.5, Math.min(0.5, 0.5 * (leftDb - rightDb) / curvature)) : 0;
    const peakFrequencyHz = (peakBin + binOffset) * audioContext.sampleRate / entry.left.fftSize;
    const logStep = Math.log(20_000 / 20) / 47;
    const toneIndex = Math.max(0, Math.min(47,
      Math.round(Math.log(Math.max(20, peakFrequencyHz) / 20) / logStep)));
    const expectedFrequencyHz = 20 * Math.exp(toneIndex * logStep);
    const frequencyError = Math.abs(peakFrequencyHz / expectedFrequencyHz - 1);
    const leftFit = fitSinePeak(entry.l, expectedFrequencyHz, audioContext.sampleRate);
    const rightFit = fitSinePeak(entry.r, expectedFrequencyHz, audioContext.sampleRate);
    const fitValid = frequencyError <= 0.085 && leftFit && rightFit &&
      leftFit.explained >= 0.8 && rightFit.explained >= 0.8;
    const tonePower = fitValid
      ? (leftFit.amplitude ** 2 + rightFit.amplitude ** 2) / 2 : 0;
    // Program 2 analysis only while a capture runs: it fits harmonics on every window.
    const labWindow = curveCapture && globalThis.AscendCordLab
      ? globalThis.AscendCordLab.analyzeWindow(entry.l, entry.r, audioContext.sampleRate, peakFrequencyHz)
      : null;
    return {
      lab_window: labWindow,
      left_dbfs: db(ll),
      right_dbfs: db(rr),
      side_dbfs: db(diff / 4),
      lr_correlation: correlation === null ? null : Math.max(-1, Math.min(1, correlation)),
      spectrum_dbfs: spectrumDbfs,
      peak_frequency_hz: peakFrequencyHz,
      analysis_sample_rate_hz: audioContext.sampleRate,
      // Peak amplitude referenced to full scale, averaged as channel power.
      // Mixed-tone transition windows are rejected instead of biasing the curve.
      peak_dbfs: tonePower > 0 ? Math.max(-120, Math.min(12, 10 * Math.log10(tonePower))) : -120,
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
      // Samples the jitter buffer removed or inserted to speed playout up or slow it down.
      const stretched = numeric(stat.removedSamplesForAcceleration) + numeric(stat.insertedSamplesForDeceleration);
      const stretchedDelta = old ? Math.max(0, stretched - old.stretched) : 0;

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
        stretched,
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
        stretched_samples_delta: Math.min(1_000_000, Math.floor(stretchedDelta)),
        jitter_buffer_delay_ms: jitterBufferEmittedDelta > 0
          ? Math.min(60000, jitterBufferDelayDelta * 1000 / jitterBufferEmittedDelta) : 0,
        sdp_fmtp_stereo: typeof codec?.sdpFmtpLine === "string" ? /(^|;)\s*stereo=1/.test(codec.sdpFmtpLine) : null,
        left_dbfs: stereo?.left_dbfs ?? null,
        right_dbfs: stereo?.right_dbfs ?? null,
        side_dbfs: stereo?.side_dbfs ?? null,
        lr_correlation: stereo?.lr_correlation ?? null,
        spectrum_dbfs: stereo?.spectrum_dbfs ?? null,
      peak_frequency_hz: stereo?.peak_frequency_hz ?? null,
      peak_dbfs: stereo?.peak_dbfs ?? null,
      analysis_sample_rate_hz: stereo?.analysis_sample_rate_hz ?? null,
      lab_window: stereo?.lab_window ?? null,
      });
    }
  }

  // Return path: while a program 2 test runs, Discord Web's outgoing microphone track is
  // swapped for the same program so AscendCord can measure what arrives on its side. Discord
  // may join before its microphone track exists, or replace the track later (unmute, device
  // change), so the swap is checked again on every sample until the test ends. The latest
  // track Discord set is always put back.
  let returnPath = null;
  let returnPathWanted = false;
  let returnPathBusy = false;
  let returnPathState = "off";

  // Discord replaces its sender's track on its own (speaking changes, device changes). While
  // the return path plays, such a call on a swapped sender is kept as the track to restore
  // instead of being applied; the observer's own swaps use the native method.
  let nativeReplaceTrack = null;

  function swapTrack(sender, track) {
    return nativeReplaceTrack && sender instanceof globalThis.RTCRtpSender
      ? nativeReplaceTrack.call(sender, track)
      : sender.replaceTrack(track);
  }

  function hookReplaceTrack() {
    const prototype = globalThis.RTCRtpSender?.prototype;
    if (nativeReplaceTrack || typeof prototype?.replaceTrack !== "function") return;
    const native = prototype.replaceTrack;
    nativeReplaceTrack = native;
    prototype.replaceTrack = function (track) {
      if (!returnPath?.replaced.has(this)) return native.call(this, track);
      returnPath.replaced.set(this, track ?? null);
      return Promise.resolve();
    };
  }

  function audioSenders() {
    const senders = new Set();
    for (const peer of peers) {
      for (const sender of peer.getSenders?.() ?? []) {
        if (sender.track?.kind === "audio") senders.add(sender);
      }
      // An audio transceiver that sends but has no track yet is the microphone slot.
      for (const transceiver of peer.getTransceivers?.() ?? []) {
        const direction = transceiver.currentDirection ?? transceiver.direction ?? "";
        if (transceiver.stopped || !/send/.test(direction) || !transceiver.sender) continue;
        if (!transceiver.sender.track && transceiver.receiver?.track?.kind === "audio") {
          senders.add(transceiver.sender);
        }
      }
    }
    return [...senders];
  }

  async function ensureReturnPath() {
    if (!returnPathWanted || returnPathBusy || !globalThis.AscendCordLab) return;
    returnPathBusy = true;
    try {
      const ctx = context();
      if (!ctx || ctx.state !== "running") {
        returnPathState = "audio-suspended";
        return;
      }
      const senders = audioSenders();
      if (!senders.length) {
        returnPathState = "no-microphone-sender";
        return;
      }
      if (!returnPath) {
        const { left, right, frames } = globalThis.AscendCordLab.renderPass(ctx.sampleRate);
        const buffer = ctx.createBuffer(2, frames, ctx.sampleRate);
        buffer.copyToChannel(left, 0);
        buffer.copyToChannel(right, 1);
        const source = ctx.createBufferSource();
        source.buffer = buffer;
        source.loop = true;
        const destination = ctx.createMediaStreamDestination();
        destination.channelCount = 2;
        source.connect(destination);
        source.start();
        returnPath = { source, track: destination.stream.getAudioTracks()[0], replaced: new Map() };
        hookReplaceTrack();
      }
      const path = returnPath;
      for (const sender of senders) {
        if (sender.track === path.track) continue;
        const original = sender.track;
        try {
          await swapTrack(sender, path.track);
        } catch {
          continue;
        }
        if (returnPath !== path) {
          // The test ended while this swap was in flight.
          try { await swapTrack(sender, original); } catch {}
          return;
        }
        path.replaced.set(sender, original ?? path.replaced.get(sender) ?? null);
      }
      returnPathState = [...path.replaced.keys()].some(sender => sender.track === path.track)
        ? "playing" : "replace-failed";
    } finally {
      returnPathBusy = false;
    }
  }

  async function startReturnPath() {
    returnPathWanted = true;
    await ensureReturnPath();
  }

  async function stopReturnPath() {
    returnPathWanted = false;
    if (!returnPath) {
      returnPathState = "off";
      return;
    }
    const { source, track, replaced } = returnPath;
    returnPath = null;
    for (const [sender, original] of replaced) {
      // Leave a track Discord set after the swap in place.
      if (sender.track !== track) continue;
      try { await swapTrack(sender, original); } catch {}
    }
    try { source.stop(); } catch {}
    track.stop();
    returnPathState = "off";
  }

  // Discord counts a member as speaking from the microphone stream it opened, and the voice
  // server forwards a member's audio only while it speaks. While AscendCord runs a lab test
  // the microphone Discord opens is a steady quiet tone, so Discord keeps speaking; what
  // the return path sends is still the swapped program track. Outside a lab test, or when
  // the page cannot play audio, Discord gets its real microphone unchanged.
  let labArmed = false;

  async function labMicrophone(stream) {
    const original = stream.getAudioTracks()[0];
    const ctx = context();
    if (!original || !ctx) return stream;
    if (ctx.state !== "running") {
      try { await ctx.resume(); } catch {}
    }
    if (ctx.state !== "running") return stream;
    const tone = ctx.createOscillator();
    tone.frequency.value = 1_000;
    const gain = ctx.createGain();
    gain.gain.value = 0.03;
    const destination = ctx.createMediaStreamDestination();
    tone.connect(gain).connect(destination);
    tone.start();
    const track = destination.stream.getAudioTracks()[0];
    const stop = track.stop.bind(track);
    track.stop = () => {
      stop();
      original.stop();
      try { tone.stop(); } catch {}
    };
    // Discord matches the opened device by these; answer for the real microphone.
    track.getSettings = () => original.getSettings();
    track.getConstraints = () => original.getConstraints();
    track.applyConstraints = constraints => original.applyConstraints(constraints);
    try { Object.defineProperty(track, "label", { get: () => original.label }); } catch {}
    return new MediaStream([track, ...stream.getVideoTracks()]);
  }

  function installMicrophoneHook() {
    const media = navigator?.mediaDevices;
    if (!media?.getUserMedia || media.getUserMedia.ascendcordLab) return;
    const open = media.getUserMedia.bind(media);
    const hooked = async constraints => {
      const stream = await open(constraints);
      if (!labArmed || !constraints?.audio) return stream;
      try {
        return await labMicrophone(stream);
      } catch {
        return stream;
      }
    };
    hooked.ascendcordLab = true;
    media.getUserMedia = hooked;
  }

  async function sample() {
    if (running) return;
    running = true;
    try {
      await ensureReturnPath().catch(() => { returnPathState = "error"; });
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
          return_path: returnPathState,
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
  try { installMicrophoneHook(); } catch {}
  window.addEventListener("message", event => {
    if (event.source !== window || event.origin !== location.origin ||
        event.data?.source !== "tesktop-stereo-proof-control") return;
    if (typeof event.data.labArmed === "boolean") {
      labArmed = event.data.labArmed;
      return;
    }
    if (typeof event.data.returnPath === "boolean") {
      (event.data.returnPath ? startReturnPath() : stopReturnPath()).catch(() => {
        returnPathState = "error";
      });
      return;
    }
    const next = event.data.captureCurve === true;
    if (curveCapture === next) return;
    curveCapture = next;
    clearInterval(sampleTimer);
    // Program 2 tones hold steady for about 0.6 s; a window every 100 ms catches each
    // of them two or three times without a fade in it.
    sampleTimer = setInterval(sample, curveCapture ? 100 : 2000);
    sample();
  });
  for (const kind of ["pointerdown", "keydown"]) {
    window.addEventListener(kind, () => context(), { capture: true, passive: true });
  }
  sampleTimer = setInterval(sample, 2000);
  setTimeout(sample, 750);
})();
