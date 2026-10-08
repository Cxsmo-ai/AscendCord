// Offline codec check: render one pass of the measurement program (lab.js), round-trip it through ffmpeg/libopus
// with the given options, and analyse the result with lab.js exactly like the page does.
// node codec-lab.mjs <stereo-proof dir> <work dir> <label> -- <ffmpeg encoder args...>
import { readFile, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import vm from "node:vm";

const [dir, work, label, sep, ...encoderArgs] = process.argv.slice(2);
const context = vm.createContext({});
vm.runInContext(await readFile(`${dir}/lab.js`, "utf8"), context);
const lab = context.AscendCordLab;
const P = lab.PROGRAM;
const RATE = 48_000, WINDOW = 16_384, PASSES = 3;

const one = lab.renderPass(RATE);
const frames = one.frames * PASSES;
const timeline = new Float64Array(frames);
{
  const segments = [{ hz: 0, ms: P.silence_ms }];
  for (const hz of P.sweep_hz) segments.push({ hz, ms: P.step_ms });
  for (let k = 0; k < 3; k++) for (const hz of P.channel_hz) segments.push({ hz, ms: P.step_ms });
  P.ladder_hz.forEach(hz => segments.push({ hz, ms: P.ladder_step_ms }));
  let at = 0;
  for (let pass = 0; pass < PASSES; pass++) {
    for (const s of segments) {
      const n = Math.round(RATE * s.ms / 1000);
      timeline.fill(s.hz, at, Math.min(frames, at + n));
      at += n;
    }
  }
}

function wav(left, right) {
  const n = left.length, data = Buffer.alloc(n * 8);
  for (let i = 0; i < n; i++) { data.writeFloatLE(left[i], i * 8); data.writeFloatLE(right[i], i * 8 + 4); }
  const h = Buffer.alloc(44);
  h.write("RIFF", 0); h.writeUInt32LE(36 + data.length, 4); h.write("WAVE", 8); h.write("fmt ", 12);
  h.writeUInt32LE(16, 16); h.writeUInt16LE(3, 20); h.writeUInt16LE(2, 22); h.writeUInt32LE(RATE, 24);
  h.writeUInt32LE(RATE * 8, 28); h.writeUInt16LE(8, 32); h.writeUInt16LE(32, 34); h.write("data", 36);
  h.writeUInt32LE(data.length, 40);
  return Buffer.concat([h, data]);
}

// OFFSET delays the program by that many samples, moving it against the 20 ms Opus frames.
const offset = Number(process.env.OFFSET ?? 0);
const left = new Float32Array(frames + offset), right = new Float32Array(frames + offset);
for (let p = 0; p < PASSES; p++) { left.set(one.left, offset + p * one.frames); right.set(one.right, offset + p * one.frames); }
await writeFile(`${work}/in.wav`, wav(left, right));
const quiet = ["-hide_banner", "-loglevel", "error", "-y"];
if (encoderArgs.length) {
  execFileSync("ffmpeg", [...quiet, "-i", `${work}/in.wav`, "-c:a", "libopus", ...encoderArgs, `${work}/${label}.opus`]);
  execFileSync("ffmpeg", [...quiet, "-i", `${work}/${label}.opus`, "-ar", "48000", "-ac", "2", "-f", "f32le", `${work}/${label}.f32`]);
} else {
  execFileSync("ffmpeg", [...quiet, "-i", `${work}/in.wav`, "-f", "f32le", `${work}/${label}.f32`]);
}
const raw = await readFile(`${work}/${label}.f32`);
const decoded = new Float32Array(raw.buffer, raw.byteOffset, Math.floor(raw.length / 4));
const n = Math.min(frames, decoded.length / 2 - offset);
const L = new Float32Array(n), R = new Float32Array(n);
for (let i = 0; i < n; i++) { L[i] = decoded[2 * (i + offset)]; R[i] = decoded[2 * (i + offset) + 1]; }

const state = lab.createLab();
for (let end = WINDOW; end <= n; end += 4_800) {
  const peak = timeline[end - WINDOW / 2] * 1.002 || 1000;
  lab.addWindow(state, lab.analyzeWindow(L.subarray(end - WINDOW, end), R.subarray(end - WINDOW, end), RATE, peak));
}
const report = lab.finalizeLab(state);
const s = report.summary;
const pick = [0, 12, 24, 30, 34, 38, 40, 42, 43, 44, 45, 46];
console.log("ladder gain", report.linearity.gain_db.map(v => v?.toFixed(2)).join(","), "windows", JSON.stringify(report.windows));
console.log(`${label}: passes ${report.passes} bands ${s.measured_response_bands} ripple ${s.ripple_100_16k_db?.toFixed(2)} ` +
  `median THD+N ${s.median_thdn_db?.toFixed(1)} separation ${s.median_separation_db?.toFixed(0)}`);
console.log("  hz/thd+n/thd: " + pick.map(i => `${Math.round(report.response.frequency_hz[i])}:` +
  `${report.response.left_thdn_db[i]?.toFixed(1)}/${report.response.left_thd_db[i]?.toFixed(1)}`).join(" "));
