// Offline null test of real music through libopus: encode each clip with the given options,
// decode, align, subtract, and report signal-to-residual overall and per band.
// node songs-null.mjs <clip dir> <work dir> -- <ffmpeg libopus args...>
import { readFile, readdir } from "node:fs/promises";
import { execFileSync } from "node:child_process";

const [dir, work, sep, ...args] = process.argv.slice(2);
const RATE = 48_000;
const BANDS = [50, 100, 200, 400, 800, 1_600, 3_150, 6_300, 10_000, 12_500, 16_000, 20_000];
const quiet = ["-hide_banner", "-loglevel", "error", "-y"];

function fft(re, im) {
  const n = re.length;
  for (let i = 1, j = 0; i < n; i++) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) { [re[i], re[j]] = [re[j], re[i]]; [im[i], im[j]] = [im[j], im[i]]; }
  }
  for (let size = 2; size <= n; size <<= 1) {
    const step = -2 * Math.PI / size;
    for (let s = 0; s < n; s += size) for (let k = 0; k < size / 2; k++) {
      const wr = Math.cos(step * k), wi = Math.sin(step * k), a = s + k, b = a + size / 2;
      const xr = re[b] * wr - im[b] * wi, xi = re[b] * wi + im[b] * wr;
      re[b] = re[a] - xr; im[b] = im[a] - xi; re[a] += xr; im[a] += xi;
    }
  }
}
const bandsOf = block => {
  const n = block.length, re = Float64Array.from(block, (v, i) => v * (0.5 - 0.5 * Math.cos(2 * Math.PI * i / n))), im = new Float64Array(n);
  fft(re, im);
  return BANDS.slice(0, -1).map((low, b) => {
    let p = 0;
    for (let k = Math.round(low * n / RATE); k < Math.round(BANDS[b + 1] * n / RATE); k++) p += re[k] ** 2 + im[k] ** 2;
    return p;
  });
};

const names = (await readFile(`${dir}/manifest.tsv`, "utf8")).trim().split("\n").map(l => l.split("\t"));
const rows = [];
for (const [id, name] of names) {
  const raw = await readFile(`${dir}/${id}.s16`);
  const ref = new Float32Array(raw.length / 2);
  for (let i = 0; i < ref.length; i++) ref[i] = raw.readInt16LE(i * 2) / 32768;
  execFileSync("ffmpeg", [...quiet, "-f", "s16le", "-ar", "48000", "-ac", "2", "-i", `${dir}/${id}.s16`, "-c:a", "libopus", ...args, `${work}/${id}.opus`]);
  execFileSync("ffmpeg", [...quiet, "-i", `${work}/${id}.opus`, "-ar", "48000", "-ac", "2", "-f", "f32le", `${work}/${id}.f32`]);
  const d = await readFile(`${work}/${id}.f32`);
  const got = new Float32Array(d.buffer, d.byteOffset, d.length / 4);
  const frames = Math.min(ref.length, got.length) / 2;
  // Align within +-400 samples (codec delay is compensated by ffmpeg; check anyway).
  let lag = 0, best = -Infinity;
  for (let c = -400; c <= 400; c++) {
    let dot = 0;
    for (let i = 4_800; i < frames - 4_800; i += 3) dot += ref[2 * i] * (got[2 * (i + c)] ?? 0);
    if (dot > best) { best = dot; lag = c; }
  }
  // Skip 50 ms at each end (codec start-up, clip edges).
  const from = 2_400, to = frames - 2_400;
  let sig = 0, res = 0, peakIn = 0, peakOut = 0;
  const bs = new Float64Array(BANDS.length - 1), br = new Float64Array(BANDS.length - 1);
  for (let start = from; start + 4_096 <= to; start += 4_096) {
    for (const ch of [0, 1]) {
      const s = new Float64Array(4_096), r = new Float64Array(4_096);
      for (let j = 0; j < 4_096; j++) {
        const i = start + j, x = ref[2 * i + ch], y = got[2 * (i + lag) + ch];
        s[j] = x; r[j] = y - x;
        sig += x * x; res += (y - x) ** 2;
        peakIn = Math.max(peakIn, Math.abs(x)); peakOut = Math.max(peakOut, Math.abs(y));
      }
      bandsOf(s).forEach((p, b) => { bs[b] += p; });
      bandsOf(r).forEach((p, b) => { br[b] += p; });
    }
  }
  const db = (a, b) => (10 * Math.log10(a / b)).toFixed(1);
  rows.push(`${id} ${name.slice(0, 28).padEnd(28)} SRR ${db(sig, res)} dB  lag ${lag}  peak in ${(20 * Math.log10(peakIn)).toFixed(1)} out ${(20 * Math.log10(peakOut)).toFixed(1)} dBFS | bands ${Array.from(bs, (s, b) => db(s, br[b])).join(" ")}`);
}
console.log(`bands Hz: ${BANDS.slice(0, -1).map((l, b) => `${l}-${BANDS[b + 1]}`).join(" ")}`);
console.log(rows.join("\n"));
