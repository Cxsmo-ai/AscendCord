import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

test("installs the WebRTC observer when RTCPeerConnection appears after document_start", async () => {
  const source = await readFile(new URL("../rtc-observer.js", import.meta.url), "utf8");
  const intervals = [];
  const window = {
    addEventListener() {},
    postMessage() {},
  };
  vm.runInNewContext(source, vm.createContext({
    window,
    location: { origin: "https://discord.com" },
    setInterval(callback) {
      const timer = { callback, cleared: false };
      intervals.push(timer);
      return timer;
    },
    clearInterval(timer) {
      timer.cleared = true;
    },
    setTimeout() {},
  }));

  const installTimer = intervals[0];
  assert.ok(installTimer, "observer should schedule delayed installation");
  assert.equal(window.RTCPeerConnection, undefined);
  installTimer.callback();
  assert.equal(window.RTCPeerConnection, undefined);

  class NativePeerConnection {
    constructor() {
      this.connectionState = "new";
      this.signalingState = "stable";
    }
    addEventListener() {}
    createOffer() { return Promise.resolve({ type: "offer", sdp: "v=0\r\n" }); }
    createAnswer() { return Promise.resolve({ type: "answer", sdp: "v=0\r\n" }); }
    setLocalDescription(value) { return Promise.resolve(value); }
    setRemoteDescription(value) { return Promise.resolve(value); }
  }
  window.RTCPeerConnection = NativePeerConnection;
  installTimer.callback();

  const wrapped = window.RTCPeerConnection;
  assert.notEqual(wrapped, NativePeerConnection);
  assert.equal(wrapped.prototype, NativePeerConnection.prototype);
  installTimer.callback();
  assert.equal(window.RTCPeerConnection, wrapped, "installation should be idempotent");
  assert.equal(installTimer.cleared, true);
});

test("the return path swaps the outgoing microphone for the program and puts it back", async () => {
  const labSource = await readFile(new URL("../lab.js", import.meta.url), "utf8");
  const source = await readFile(new URL("../rtc-observer.js", import.meta.url), "utf8");
  const listeners = [];
  const posted = [];
  const window = {
    addEventListener(kind, listener) { if (kind === "message") listeners.push(listener); },
    postMessage(message) { posted.push(message); },
  };
  const microphone = { kind: "audio", id: "mic" };
  const replaced = [];
  const sender = {
    track: microphone,
    async replaceTrack(track) { replaced.push(track); this.track = track; },
  };
  class Peer {
    addEventListener() {}
    getSenders() { return [sender, { track: { kind: "video" }, replaceTrack() { throw new Error("video"); } }]; }
  }
  const programTrack = { kind: "audio", id: "program", stopped: false, stop() { this.stopped = true; } };
  let started = 0, stopped = 0, copied = 0;
  class AudioContext {
    constructor() { this.state = "running"; this.sampleRate = 48_000; }
    resume() { return Promise.resolve(); }
    createBuffer(channels, frames) {
      return { channels, frames, copyToChannel(data) { copied += data.length; } };
    }
    createBufferSource() {
      return { connect() {}, start() { started++; }, stop() { stopped++; } };
    }
    createMediaStreamDestination() {
      return { stream: { getAudioTracks: () => [programTrack] } };
    }
  }
  const context = vm.createContext({
    window, AudioContext, MediaStream: class {},
    location: { origin: "https://discord.com" },
    setInterval: () => ({}), clearInterval() {}, setTimeout() {},
  });
  window.RTCPeerConnection = Peer;
  vm.runInContext(labSource, context);
  vm.runInContext(source, context);
  new window.RTCPeerConnection();
  const control = returnPath => listeners.forEach(listener => listener({
    source: window, origin: "https://discord.com",
    data: { source: "tesktop-stereo-proof-control", returnPath },
  }));

  control(true);
  await new Promise(resolve => setTimeout(resolve, 50));
  assert.equal(sender.track, programTrack);
  assert.equal(started, 1);
  assert.equal(copied, 2 * context.AscendCordLab.renderPass(48_000).frames);

  control(false);
  await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(sender.track, microphone, "the microphone track is restored");
  assert.equal(stopped, 1);
  assert.equal(programTrack.stopped, true);
  assert.deepEqual(replaced.map(track => track.id), ["program", "mic"]);
});

test("the return path waits for a late microphone and survives Discord replacing it", async () => {
  const labSource = await readFile(new URL("../lab.js", import.meta.url), "utf8");
  const source = await readFile(new URL("../rtc-observer.js", import.meta.url), "utf8");
  const listeners = [];
  const posted = [];
  const ticks = [];
  const window = {
    addEventListener(kind, listener) { if (kind === "message") listeners.push(listener); },
    postMessage(message) { posted.push(message); },
  };
  const sender = { track: null, async replaceTrack(track) { this.track = track; } };
  const slots = [];
  class Peer {
    addEventListener() {}
    getSenders() { return slots.map(slot => slot.sender); }
    getTransceivers() { return slots; }
    getReceivers() { return []; }
    getStats() { return Promise.resolve(new Map()); }
  }
  const programTrack = { kind: "audio", id: "program", stop() {} };
  class AudioContext {
    constructor() { this.state = "running"; this.sampleRate = 48_000; }
    resume() { return Promise.resolve(); }
    createBuffer(channels, frames) { return { channels, frames, copyToChannel() {} }; }
    createBufferSource() { return { connect() {}, start() {}, stop() {} }; }
    createMediaStreamDestination() { return { stream: { getAudioTracks: () => [programTrack] } }; }
  }
  const context = vm.createContext({
    window, AudioContext, MediaStream: class {},
    location: { origin: "https://discord.com" },
    setInterval: callback => { ticks.push(callback); return {}; }, clearInterval() {},
    setTimeout: callback => { ticks.push(callback); return {}; },
  });
  window.RTCPeerConnection = Peer;
  vm.runInContext(labSource, context);
  vm.runInContext(source, context);
  new window.RTCPeerConnection();
  const control = returnPath => listeners.forEach(listener => listener({
    source: window, origin: "https://discord.com",
    data: { source: "tesktop-stereo-proof-control", returnPath },
  }));
  const settle = () => new Promise(resolve => setTimeout(resolve, 20));
  const tick = async () => { for (const callback of ticks) callback(); await settle(); };
  const state = () => posted.filter(message => message.observer).at(-1)?.observer.return_path;

  control(true);
  await settle();
  await tick();
  assert.equal(state(), "no-microphone-sender");

  // Discord's microphone slot appears without a track, then gets one.
  slots.push({ direction: "sendrecv", sender, receiver: { track: { kind: "audio" } } });
  await tick();
  assert.equal(sender.track, programTrack);
  await tick();
  assert.equal(state(), "playing");

  const microphone = { kind: "audio", id: "mic" };
  sender.track = microphone;
  await tick();
  assert.equal(sender.track, programTrack, "the swap is restored after Discord replaces the track");

  control(false);
  await settle();
  assert.equal(sender.track, microphone, "the latest Discord track is put back");
});

test("during a lab test Discord's microphone is a steady tone that answers for the real device", async () => {
  const source = await readFile(new URL("../rtc-observer.js", import.meta.url), "utf8");
  const listeners = [];
  const window = {
    addEventListener(kind, listener) { if (kind === "message") listeners.push(listener); },
    postMessage() {},
  };
  let stopped = 0;
  const microphone = {
    kind: "audio", label: "Real microphone",
    stop() { stopped++; },
    getSettings: () => ({ deviceId: "real" }),
    getConstraints: () => ({}),
    applyConstraints: async () => {},
  };
  class MediaStream {
    constructor(tracks = []) { this.tracks = tracks; }
    getAudioTracks() { return this.tracks.filter(track => track.kind === "audio"); }
    getVideoTracks() { return this.tracks.filter(track => track.kind === "video"); }
  }
  const navigator = { mediaDevices: { getUserMedia: async () => new MediaStream([microphone]) } };
  let toneStarted = 0;
  class AudioContext {
    constructor() { this.state = "running"; this.sampleRate = 48_000; }
    resume() { return Promise.resolve(); }
    createOscillator() { return { frequency: {}, connect: node => node, start() { toneStarted++; }, stop() {} }; }
    createGain() { return { gain: {}, connect: node => node }; }
    createMediaStreamDestination() {
      const track = { kind: "audio", label: "MediaStreamAudioDestinationNode", stop() {} };
      return { stream: new MediaStream([track]) };
    }
  }
  const context = vm.createContext({
    window, navigator, AudioContext, MediaStream,
    location: { origin: "https://discord.com" },
    setInterval: () => ({}), clearInterval() {}, setTimeout() {},
  });
  window.RTCPeerConnection = class { addEventListener() {} };
  vm.runInContext(source, context);
  const arm = labArmed => listeners.forEach(listener => listener({
    source: window, origin: "https://discord.com",
    data: { source: "tesktop-stereo-proof-control", labArmed },
  }));

  const normal = await navigator.mediaDevices.getUserMedia({ audio: true });
  assert.equal(normal.getAudioTracks()[0], microphone, "outside a lab test the microphone is untouched");

  arm(true);
  const lab = await navigator.mediaDevices.getUserMedia({ audio: true });
  const track = lab.getAudioTracks()[0];
  assert.notEqual(track, microphone);
  assert.equal(toneStarted, 1);
  assert.equal(track.label, "Real microphone");
  assert.equal(track.getSettings().deviceId, "real");
  track.stop();
  assert.equal(stopped, 1, "stopping the tone also releases the real microphone");

  arm(false);
  assert.equal((await navigator.mediaDevices.getUserMedia({ audio: true })).getAudioTracks()[0], microphone);
});

test("Discord replacing its track during the return path is held until the test ends", async () => {
  const labSource = await readFile(new URL("../lab.js", import.meta.url), "utf8");
  const source = await readFile(new URL("../rtc-observer.js", import.meta.url), "utf8");
  const listeners = [];
  const window = {
    addEventListener(kind, listener) { if (kind === "message") listeners.push(listener); },
    postMessage() {},
  };
  class RTCRtpSender {
    constructor(track) { this.track = track; }
    async replaceTrack(track) { this.track = track; }
  }
  const microphone = { kind: "audio", id: "mic" };
  const sender = new RTCRtpSender(microphone);
  class Peer {
    addEventListener() {}
    getSenders() { return [sender]; }
    getReceivers() { return []; }
    getStats() { return Promise.resolve(new Map()); }
  }
  const programTrack = { kind: "audio", id: "program", stop() {} };
  class AudioContext {
    constructor() { this.state = "running"; this.sampleRate = 48_000; }
    resume() { return Promise.resolve(); }
    createBuffer(channels, frames) { return { channels, frames, copyToChannel() {} }; }
    createBufferSource() { return { connect() {}, start() {}, stop() {} }; }
    createMediaStreamDestination() { return { stream: { getAudioTracks: () => [programTrack] } }; }
  }
  const context = vm.createContext({
    window, AudioContext, RTCRtpSender, MediaStream: class {},
    location: { origin: "https://discord.com" },
    setInterval: () => ({}), clearInterval() {}, setTimeout() {},
  });
  window.RTCPeerConnection = Peer;
  vm.runInContext(labSource, context);
  vm.runInContext(source, context);
  new window.RTCPeerConnection();
  const control = returnPath => listeners.forEach(listener => listener({
    source: window, origin: "https://discord.com",
    data: { source: "tesktop-stereo-proof-control", returnPath },
  }));
  const settle = () => new Promise(resolve => setTimeout(resolve, 20));

  control(true);
  await settle();
  assert.equal(sender.track, programTrack);

  // Discord sets a new microphone track mid-test: the program keeps playing.
  const newMicrophone = { kind: "audio", id: "new-mic" };
  await sender.replaceTrack(newMicrophone);
  assert.equal(sender.track, programTrack);

  control(false);
  await settle();
  assert.equal(sender.track, newMicrophone, "Discord's latest track is applied when the test ends");

  // After the test Discord's calls go straight through again.
  await sender.replaceTrack(microphone);
  assert.equal(sender.track, microphone);
});
