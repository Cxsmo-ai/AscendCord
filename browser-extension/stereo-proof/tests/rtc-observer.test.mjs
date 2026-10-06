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
