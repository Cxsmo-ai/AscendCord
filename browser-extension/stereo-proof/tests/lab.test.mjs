import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const context = vm.createContext({});
vm.runInContext(await readFile(new URL("../lab.js", import.meta.url), "utf8"), context);
const lab = context.AscendCordLab;
const { PROGRAM } = lab;
const RATE = 48_000;
const WINDOW = 16_384;

/** The program exactly as AscendCord's test_sweep.rs emits it, one pass. */
function programSegments() {
  const segments = [{ kind: "silence", ms: PROGRAM.silence_ms }];
  for (const hz of PROGRAM.sweep_hz) segments.push({ kind: "mono", hz, peak: 0.25, ms: PROGRAM.step_ms });
  for (const kind of ["left", "right", "antiphase"]) {
    for (const hz of PROGRAM.channel_hz) segments.push({ kind, hz, peak: 0.25, ms: PROGRAM.step_ms });
  }
  PROGRAM.ladder_dbfs.forEach((dbfs, index) => segments.push({
    kind: "ladder", hz: PROGRAM.ladder_hz[index], peak: 10 ** (dbfs / 20), ms: PROGRAM.ladder_step_ms,
  }));
  return segments;
}

/** Renders `passes` passes through a channel model; returns [left, right] and a tone timeline. */
function render(passes, channel) {
  const segments = Array.from({ length: passes }, programSegments).flat();
  const total = segments.reduce((sum, s) => sum + Math.round(RATE * s.ms / 1000), 0);
  const left = new Float32Array(total), right = new Float32Array(total);
  const timeline = new Float64Array(total);
  let at = 0;
  let seed = 7;
  const noise = () => {
    seed = (seed * 1_103_515_245 + 12_345) % 2_147_483_648;
    return seed / 2_147_483_648 - 0.5;
  };
  for (const segment of segments) {
    const frames = Math.round(RATE * segment.ms / 1000);
    const fade = RATE / 20;
    for (let i = 0; i < frames; i++, at++) {
      let l = 0, r = 0;
      if (segment.kind !== "silence") {
        const ramp = Math.min(1, Math.min(i, frames - i) / fade);
        const s = segment.peak * ramp * Math.sin(2 * Math.PI * segment.hz * i / RATE);
        if (segment.kind === "left") l = s;
        else if (segment.kind === "right") r = s;
        else if (segment.kind === "antiphase") { l = s; r = -s; }
        else { l = s; r = s; }
        timeline[at] = segment.hz;
      }
      const [outL, outR] = channel(l, r, segment.hz ?? 1000, noise);
      left[at] = outL; right[at] = outR;
    }
  }
  return { left, right, timeline };
}

/** Feeds the receiver analysis exactly as the page does: a 16384-sample window every 100 ms. */
function analyze({ left, right, timeline }) {
  const state = lab.createLab();
  const step = Math.round(RATE * 0.1);
  for (let end = WINDOW; end <= left.length; end += step) {
    const l = left.subarray(end - WINDOW, end), r = right.subarray(end - WINDOW, end);
    // The page refines the FFT peak; the newest audio dominates a window most of the time.
    const peak = timeline[end - WINDOW / 2] * 1.002 || 1000;
    lab.addWindow(state, lab.analyzeWindow(l, r, RATE, peak));
  }
  return lab.finalizeLab(state);
}

const near = (actual, expected, tolerance, label) =>
  assert.ok(Number.isFinite(actual) && Math.abs(actual - expected) <= tolerance,
    `${label}: ${actual} is not within ${tolerance} of ${expected}`);

test("a clean path measures flat, separated, linear and quiet", () => {
  const report = analyze(render(2, (l, r) => [l, r]));
  assert.ok(report.passes >= 1, "a full pass is counted");
  assert.equal(report.summary.measured_response_bands, 48, `missing ${report.response.windows.map((n, i) => n ? null : i).filter(i => i !== null)}`);
  for (const gain of [...report.response.left_gain_db, ...report.response.right_gain_db]) {
    near(gain, 0, 0.1, "clean gain");
  }
  assert.ok(report.summary.median_separation_db > 60);
  assert.equal(report.summary.stereo_preserved, true);
  near(report.summary.antiphase_correlation, -1, 0.01, "antiphase correlation");
  near(report.linearity.fit.slope, 1, 0.01, "linearity slope");
  assert.ok(report.summary.median_thdn_db < -60, `THD+N ${report.summary.median_thdn_db}`);
  assert.ok(report.response.windows.every(n => n >= 1), `windows ${report.response.windows}`);
  assert.ok(report.noise.left_rms_dbfs < -140);
});

test("channel faults are each recovered by the matching measurement", () => {
  const leak = 10 ** (-40 / 20);
  const report = analyze(render(2, (l, r, hz, noise) => {
    // Left loses 3 dB above 8 kHz, right is 1 dB louder, -40 dB crosstalk both ways,
    // a -50 dB second harmonic, soft compression above -12 dBFS and a -90 dBFS noise floor.
    const tilt = hz > 8_000 ? 10 ** (-3 / 20) : 1;
    let outL = l * tilt + r * leak, outR = r * 10 ** (1 / 20) + l * leak;
    const shape = x => {
      const limit = 10 ** (-12 / 20);
      const magnitude = Math.abs(x);
      const squeezed = magnitude > limit ? limit + (magnitude - limit) * 0.5 : magnitude;
      return Math.sign(x) * squeezed + 0.003 * (x * x / 0.25 - 0.5 * Math.abs(x) * 0);
    };
    outL = shape(outL) + noise() * 10 ** (-90 / 20) * 3.4;
    outR = shape(outR) + noise() * 10 ** (-90 / 20) * 3.4;
    return [outL, outR];
  }));
  const index = hz => report.response.frequency_hz.findIndex(f => f >= hz);
  near(report.response.left_gain_db[index(1_000)], 0, 0.3, "left gain at 1 kHz");
  near(report.response.left_gain_db[index(10_000)], -3, 0.3, "left gain above 8 kHz");
  near(report.response.right_gain_db[index(1_000)], 1, 0.3, "right gain");
  near(report.summary.left_right_balance_db, -1, 0.3, "balance");
  near(report.summary.median_separation_db, 40, 1.5, "separation");
  assert.equal(report.summary.stereo_preserved, true);
  // Compression: the top ladder steps lose level, the low ones keep it.
  const gains = report.linearity.gain_db;
  near(gains[0], 0.4, 1, "quiet step gain");
  assert.ok(gains[gains.length - 1] < gains[3] - 2, `top step compressed: ${gains}`);
  assert.ok(report.summary.level_compression_db > 2);
  near(report.noise.left_rms_dbfs, -90, 3, "noise floor");
});

test("a path that folds stereo to mono is reported, not mistaken for a mono sweep", () => {
  const report = analyze(render(2, (l, r) => {
    const mono = (l + r) / 2;
    return [mono, mono];
  }));
  assert.equal(report.summary.stereo_preserved, false);
  assert.ok(report.summary.median_separation_db < 1);
  // Opposite-phase content cancels completely in a mono fold.
  assert.ok(report.antiphase.left_gain_db.every(gain => gain === null || gain < -40));
});

test("windows overlapping packet concealment are counted but not measured", () => {
  const state = lab.createLab();
  const tone = { kind: "tone", steady: true, grid: "sweep", index: 10, hz: PROGRAM.sweep_hz[10],
    left: { peak_dbfs: -12 }, right: { peak_dbfs: -12 }, correlation: 1 };
  for (let i = 0; i < 3; i++) lab.addWindow(state, { kind: "silence", loudest_dbfs: -120, left_rms_dbfs: -120, right_rms_dbfs: -120 });
  lab.addWindow(state, tone, true);
  assert.equal(state.windows.contaminated, 1);
  assert.equal(state.mono[10].left.n, 0);
  lab.addWindow(state, tone);
  assert.equal(state.mono[10].left.n, 1);
});

test("tone matching stays within the program grid", () => {
  assert.equal(lab.nearestTone(PROGRAM.sweep_hz[30] * 1.004, RATE, WINDOW).index, 30);
  assert.equal(lab.nearestTone(PROGRAM.ladder_hz[3], RATE, WINDOW).grid, "ladder");
  assert.equal(lab.nearestTone(PROGRAM.sweep_hz[30] * 1.03, RATE, WINDOW), null);
  assert.equal(lab.nearestTone(0, RATE, WINDOW), null);
});

test("comparison reports the difference of two runs", () => {
  const clean = analyze(render(2, (l, r) => [l, r]));
  const quieter = analyze(render(2, (l, r) => [l * 0.5, r * 0.5]));
  const delta = lab.compare(quieter, clean);
  near(delta.left_gain_db[20], 20 * Math.log10(0.5), 0.1, "comparison gain delta");
  assert.equal(lab.compare(quieter, { version: 1 }), null);
});

test("distortion separates harmonics from noise", () => {
  const n = WINDOW;
  const tone = (harmonic, noiseLevel) => {
    let seed = 3;
    return Float32Array.from({ length: n }, (_, i) => {
      seed = (seed * 1_103_515_245 + 12_345) % 2_147_483_648;
      const white = (seed / 2_147_483_648 - 0.5) * Math.sqrt(12);
      return 0.25 * Math.sin(2 * Math.PI * 997 * i / RATE) +
        0.25 * harmonic * Math.sin(2 * Math.PI * 1994 * i / RATE + 0.3) +
        0.25 / Math.SQRT2 * noiseLevel * white;
    });
  };
  const harmonic = lab.distortion(tone(10 ** (-50 / 20), 0), 997, RATE);
  near(harmonic.thd_db, -50, 0.2, "THD of a -50 dB second harmonic");
  near(harmonic.thdn_db, -50, 0.2, "THD+N without noise");
  const noisy = lab.distortion(tone(0, 10 ** (-60 / 20)), 997, RATE);
  assert.ok(noisy.thd_db < -75, `harmonic THD stays low under noise: ${noisy.thd_db}`);
  near(noisy.thdn_db, -60, 0.5, "THD+N of -60 dB noise");
});
