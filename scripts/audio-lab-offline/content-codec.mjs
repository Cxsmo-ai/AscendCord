// Offline null test of program 3's content (with the song clips) through ffmpeg/libopus,
// analysed by lab.js exactly as the page does. Shows what the codec alone leaves.
// node content-codec.mjs <stereo-proof dir> <songs dir> <work dir> <label> -- <libopus args>
import { readFile, writeFile } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import vm from "node:vm";

const [dir, songsDir, work, label, sep, ...args] = process.argv.slice(2);
const context = vm.createContext({ setTimeout });
vm.runInContext(await readFile(`${dir}/lab.js`, "utf8"), context);
const lab = context.AscendCordLab;
const manifest = (await readFile(`${songsDir}/manifest.tsv`, "utf8")).trim().split("\n").map(l => l.split("\t"));
const clips = await Promise.all(manifest.map(([id]) => readFile(`${songsDir}/${id}.s16`)));
const all = Buffer.concat(clips);
lab.setSongs({ names: manifest.map(m => m[1]), frames: clips.map(c => c.length / 4),
  pcm: new Int16Array(all.buffer.slice(all.byteOffset, all.byteOffset + all.length)) });
const ref = lab.renderContent(48_000);
const pad = 48_000;
const frames = ref.frames + 2 * pad;
const data = Buffer.alloc(frames * 8);
for (let i = 0; i < ref.frames; i++) {
  data.writeFloatLE(ref.left[i], (pad + i) * 8);
  data.writeFloatLE(ref.right[i], (pad + i) * 8 + 4);
}
await writeFile(`${work}/content.f32`, data);
const quiet = ["-hide_banner", "-loglevel", "error", "-y"];
execFileSync("ffmpeg", [...quiet, "-f", "f32le", "-ar", "48000", "-ac", "2", "-i", `${work}/content.f32`, "-c:a", "libopus", ...args, `${work}/${label}.opus`]);
execFileSync("ffmpeg", [...quiet, "-i", `${work}/${label}.opus`, "-ar", "48000", "-ac", "2", "-f", "f32le", `${work}/${label}.dec`]);
const raw = await readFile(`${work}/${label}.dec`);
const decoded = new Float32Array(raw.buffer, raw.byteOffset, raw.length / 4);
const n = decoded.length / 2;
const left = new Float32Array(n), right = new Float32Array(n);
for (let i = 0; i < n; i++) { left[i] = decoded[2 * i]; right[i] = decoded[2 * i + 1]; }
const report = lab.analyzeContent(left, right, 48_000);
console.log(`${label}: ${Object.entries(report.sections).map(([k, v]) => `${k}:${v.srr_db?.toFixed(1)}`).join(" ")}`);
console.log(`  delay ${report.delay_samples?.toFixed(3)} pre-echo ${report.pre_echo_db.median_db?.toFixed(1)} (worst ${report.pre_echo_db.worst_db?.toFixed(1)}) post ${report.post_echo_db.median_db?.toFixed(1)} slips ${report.slipped_blocks}`);
console.log(`  song bands ${report.band_srr_db.songs.map(v => v?.toFixed(0)).join(",")}`);
console.log(`  noise bands ${report.band_srr_db.noise.map(v => v?.toFixed(0)).join(",")}`);
