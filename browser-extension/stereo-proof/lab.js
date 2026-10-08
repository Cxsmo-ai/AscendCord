// Measurement program v2: the signal AscendCord sends with --test-sweep-channel, and the
// estimators that turn the decoded Discord track back into numbers. Shared, unchanged, by
// the page analyser, the service worker, the popup and the tests. Pure functions only: no
// audio leaves the page, only the numbers these return.
(function (root) {
  "use strict";

  const SWEEP_POINTS = 48;
  const LOW_HZ = 20;
  const HIGH_HZ = 20_000;
  const sweepHz = index => LOW_HZ * (HIGH_HZ / LOW_HZ) ** (index / (SWEEP_POINTS - 1));

  /** One pass, in order. Every tone lasts STEP_MS with 50 ms fades; source peak −12.04 dBFS. */
  const PROGRAM = Object.freeze({
    version: 3,
    step_ms: 700,
    ladder_step_ms: 800,
    silence_ms: 1_200,
    source_peak_dbfs: 20 * Math.log10(0.25),
    // Identical left and right: the transfer response of each channel.
    sweep_hz: Object.freeze(Array.from({ length: SWEEP_POINTS }, (_, index) => sweepHz(index))),
    // Every fourth sweep tone, driven on one channel, then the other, then in opposite phase.
    channel_hz: Object.freeze(Array.from({ length: 12 }, (_, index) => sweepHz(index * 4))),
    // Loudness ladder, each step at its own frequency between two sweep tones so a step is
    // identified by frequency, never by the level that is being measured.
    // The last two steps reach full scale and beyond, where peak handling decides.
    ladder_dbfs: Object.freeze([-60, -48, -36, -24, -18, -12, -6, -1, 0, 3]),
    ladder_hz: Object.freeze(Array.from({ length: 10 }, (_, index) => sweepHz(26 + index + 0.5))),
    // Real-signal content after the ladder, measured by subtracting it from what arrives.
    content: Object.freeze([
      Object.freeze({ name: "transients", ms: 2_000 }),
      Object.freeze({ name: "noise", ms: 1_500 }),
      Object.freeze({ name: "music", ms: 2_000 }),
      Object.freeze({ name: "speech", ms: 2_000 }),
    ]),
    sections: Object.freeze(["mono", "left", "right", "antiphase"]),
  });

  const SILENCE_DBFS = -40;
  // The program's silence lasts 1.2 s and decodes far below any tone (the quietest step is
  // -63 dBFS RMS). A noisier path must stay quiet longer than any gap between two tones.
  // Comfort noise from a codec makes the silence merely quiet, and then the ladder's quietest
  // steps look the same. Quiet after the ladder has reached -36 dBFS is the silence; any
  // other quiet stretch counts only if the next tone is the start of the sweep.
  const SILENT_DBFS = -70;
  const SILENCE_WINDOWS = 3;
  const LADDER_LOUD_INDEX = 2;
  const MIN_EXPLAINED = 0.8;
  const STEADY_DB = 0.1;
  // A discontinuity (dropout, splice, time stretch) moves the tone's amplitude or phase in
  // part of a window. Each eighth must match the whole-window fit to 5% (-26 dB); codec
  // noise moves it by about -75 dB. Eighths shorter than two cycles are not judged.
  const BURST_PARTS = 8;
  const BURST_LIMIT = 0.05;
  const BURST_MIN_CYCLES = 2;
  const MAX_HARMONIC = 10;

  const db20 = (value, floor = -160) =>
    value > 0 ? Math.max(floor, 20 * Math.log10(value)) : floor;
  const finite = (value, min, max) =>
    Number.isFinite(value) ? Math.max(min, Math.min(max, value)) : null;

  function rms(samples) {
    let mean = 0;
    for (let i = 0; i < samples.length; i++) mean += samples[i];
    mean /= samples.length || 1;
    let sum = 0;
    for (let i = 0; i < samples.length; i++) {
      const d = samples[i] - mean;
      sum += d * d;
    }
    return Math.sqrt(sum / (samples.length || 1));
  }

  /**
   * Least-squares sine at a known frequency: amplitude, phase coefficients, explained power.
   * `knownMean` fixes the DC level, for slices too short to estimate it from (under ~2 cycles).
   */
  function fitTone(samples, frequencyHz, sampleRate, knownMean = null) {
    const n = samples.length;
    if (!n || !(frequencyHz > 0) || frequencyHz >= sampleRate / 2) return null;
    const fixedMean = Number.isFinite(knownMean);
    // Sums for a joint least-squares fit of DC, cosine and sine. Fitting DC separately
    // first would leave the tone's own average over a fractional cycle as false residue.
    let sc = 0, sss = 0, cc = 0, ss = 0, cs = 0, sy = 0, yc = 0, ys = 0, yy = 0;
    const step = 2 * Math.PI * frequencyHz / sampleRate;
    const stepCos = Math.cos(step), stepSin = Math.sin(step);
    let c = 1, s = 0;
    for (let i = 0; i < n; i++) {
      const y = fixedMean ? samples[i] - knownMean : samples[i];
      sc += c; sss += s; cc += c * c; ss += s * s; cs += c * s;
      sy += y; yc += y * c; ys += y * s; yy += y * y;
      const next = c * stepCos - s * stepSin;
      s = s * stepCos + c * stepSin;
      c = next;
    }
    let mean, a, b;
    if (fixedMean) {
      const determinant = cc * ss - cs * cs;
      if (!(determinant > 0)) return null;
      a = (yc * ss - ys * cs) / determinant;
      b = (ys * cc - yc * cs) / determinant;
      mean = knownMean;
    } else {
      // Solve [n sc sss; sc cc cs; sss cs ss] [m a b] = [sy yc ys] by Cramer's rule.
      const det3 = (m) => m[0] * (m[4] * m[8] - m[5] * m[7]) -
        m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6]);
      const base = [n, sc, sss, sc, cc, cs, sss, cs, ss];
      const determinant = det3(base);
      if (!(Math.abs(determinant) > 1e-12)) return null;
      const m = det3([sy, sc, sss, yc, cc, cs, ys, cs, ss]) / determinant;
      a = det3([n, sy, sss, sc, yc, cs, sss, ys, ss]) / determinant;
      b = det3([n, sc, sy, sc, cc, yc, sss, cs, ys]) / determinant;
      mean = m;
    }
    // Power about the fitted DC level, and the part the fitted sine explains exactly.
    const dc = fixedMean ? 0 : mean;
    const total = yy - 2 * dc * sy + n * dc * dc;
    const fitted = Math.max(0, Math.min(total, a * (yc - dc * sc) + b * (ys - dc * sss)));
    return {
      amplitude: Math.hypot(a, b),
      a, b, mean,
      signal_power: fitted / n,
      residual_power: Math.max(0, total - fitted) / n,
      explained: total > 0 ? fitted / total : 0,
    };
  }

  /**
   * Received distortion of one channel at a known fundamental. THD sums fitted harmonics 2..10
   * below Nyquist; THD+N is everything left after removing the fitted fundamental and DC, so it
   * also holds codec noise, concealment and resampling error. Both relative to the fundamental.
   */
  function distortion(samples, frequencyHz, sampleRate, fundamental = fitTone(samples, frequencyHz, sampleRate)) {
    if (!fundamental || !(fundamental.signal_power > 0)) return null;
    const fundamentalPower = fundamental.signal_power;
    const residualPower = fundamental.residual_power;
    // Harmonics are fitted to what the fundamental leaves, so its leakage into a short,
    // non-integer-cycle window is not counted as distortion.
    const residual = new Float64Array(samples.length);
    const step = 2 * Math.PI * frequencyHz / sampleRate;
    for (let i = 0; i < samples.length; i++) {
      residual[i] = samples[i] - fundamental.mean -
        fundamental.a * Math.cos(step * i) - fundamental.b * Math.sin(step * i);
    }
    let harmonicPower = 0, harmonics = 0;
    for (let k = 2; k <= MAX_HARMONIC; k++) {
      const hz = k * frequencyHz;
      if (hz >= sampleRate * 0.49) break;
      const fit = fitTone(residual, hz, sampleRate, 0);
      if (fit) { harmonicPower += fit.signal_power; harmonics++; }
    }
    return {
      // Above a quarter of the sample rate no harmonic fits below Nyquist: no THD reading.
      thd_db: harmonics ? db20(Math.sqrt(harmonicPower / fundamentalPower), -160) : null,
      thdn_db: db20(Math.sqrt(residualPower / fundamentalPower), -160),
    };
  }

  /**
   * Nearest program tone to a measured peak, or null. The window must be close to one grid
   * frequency: within 1.2 % or one FFT bin, never more than a third of the gap to the next tone.
   */
  function nearestTone(peakHz, sampleRate, fftSize) {
    if (!(peakHz > 0)) return null;
    const binHz = sampleRate / fftSize;
    let best = null;
    const consider = (grid, index, hz) => {
      const distance = Math.abs(Math.log(peakHz / hz));
      if (!best || distance < best.distance) best = { grid, index, hz, distance };
    };
    PROGRAM.sweep_hz.forEach((hz, index) => consider("sweep", index, hz));
    PROGRAM.ladder_hz.forEach((hz, index) => consider("ladder", index, hz));
    if (!best) return null;
    const tolerance = Math.min(0.06, Math.max(0.012, 1.2 * binHz / best.hz));
    return best.distance <= tolerance ? best : null;
  }

  /**
   * One analysis window of both decoded channels. `peakHz` is the refined FFT peak. Returns
   * a silence reading, a tone reading or an unknown window (transitions, foreign audio).
   */
  function residualBurst(samples, frequencyHz, sampleRate, fit) {
    const size = Math.floor(samples.length / BURST_PARTS);
    if (frequencyHz * size / sampleRate < BURST_MIN_CYCLES) return false;
    const step = 2 * Math.PI * frequencyHz / sampleRate;
    const amplitude = Math.hypot(fit.a, fit.b);
    for (let part = 0; part < BURST_PARTS; part++) {
      // The tone's cosine and sine weights over this part, on the window's own time axis.
      let cc = 0, ss = 0, cs = 0, yc = 0, ys = 0;
      for (let i = part * size; i < (part + 1) * size; i++) {
        const c = Math.cos(step * i), s = Math.sin(step * i), y = samples[i] - fit.mean;
        cc += c * c; ss += s * s; cs += c * s; yc += y * c; ys += y * s;
      }
      const determinant = cc * ss - cs * cs;
      if (!(determinant > 0)) return false;
      const a = (yc * ss - ys * cs) / determinant, b = (ys * cc - yc * cs) / determinant;
      if (Math.hypot(a - fit.a, b - fit.b) > amplitude * BURST_LIMIT) return true;
    }
    return false;
  }

  function analyzeWindow(left, right, sampleRate, peakHz) {
    const leftRms = rms(left), rightRms = rms(right);
    const loudest = db20(Math.max(leftRms, rightRms));
    const tone = nearestTone(peakHz, sampleRate, left.length);
    let leftFit = null, rightFit = null;
    if (tone) {
      leftFit = fitTone(left, tone.hz, sampleRate);
      rightFit = fitTone(right, tone.hz, sampleRate);
    }
    const dominant = !leftFit || (rightFit && rightFit.amplitude > leftFit.amplitude)
      ? rightFit : leftFit;
    if (!dominant || dominant.explained < MIN_EXPLAINED) {
      return loudest < SILENCE_DBFS
        ? {
          kind: "silence",
          loudest_dbfs: loudest,
          left_rms_dbfs: db20(leftRms),
          right_rms_dbfs: db20(rightRms),
        }
        : { kind: "unknown", loudest_dbfs: loudest };
    }
    // A window that catches a fade in or out reads low and smears distortion. Keep only
    // steady ones: the tone must have the same level in each quarter of the window.
    const dominantSamples = dominant === leftFit ? left : right;
    const quarter = dominantSamples.length >> 2;
    const levels = [0, 1, 2, 3].map(part => {
      const fit = fitTone(dominantSamples.subarray(part * quarter, (part + 1) * quarter),
        tone.hz, sampleRate, dominant.mean);
      return fit && fit.amplitude > 0 ? db20(fit.amplitude) : null;
    });
    const steady = levels.every(Number.isFinite) &&
      Math.max(...levels) - Math.min(...levels) <= STEADY_DB;
    const glitch = steady && residualBurst(dominantSamples, tone.hz, sampleRate, dominant);
    let ll = 0, rr = 0, lr = 0, side = 0;
    for (let i = 0; i < left.length; i++) {
      const a = left[i] - leftFit.mean, b = right[i] - rightFit.mean;
      ll += a * a; rr += b * b; lr += a * b; side += (a - b) * (a - b);
    }
    const channel = (fit, samples) => {
      const result = {
        peak_dbfs: db20(fit.amplitude),
        explained: fit.explained,
      };
      // Distortion only means something for the channel actually carrying the tone.
      if (fit.amplitude >= dominant.amplitude * 0.1) {
        Object.assign(result, distortion(samples, tone.hz, sampleRate, fit));
      }
      return result;
    };
    return {
      kind: "tone",
      steady,
      glitch,
      grid: tone.grid,
      index: tone.index,
      hz: tone.hz,
      peak_hz: peakHz,
      left: channel(leftFit, left),
      right: channel(rightFit, right),
      correlation: ll > 0 && rr > 0 ? Math.max(-1, Math.min(1, lr / Math.sqrt(ll * rr))) : null,
      side_dbfs: db20(Math.sqrt(side / (4 * left.length))),
    };
  }

  // ---- Aggregation (service worker) -------------------------------------------------

  function stat() {
    return { n: 0, mean: 0, m2: 0, power: 0 };
  }

  /** Running mean and spread of a dB value; `power` keeps the linear-power mean as well. */
  function add(entry, db) {
    if (!Number.isFinite(db)) return;
    entry.n++;
    const delta = db - entry.mean;
    entry.mean += delta / entry.n;
    entry.m2 += delta * (db - entry.mean);
    entry.power += (10 ** (db / 10) - entry.power) / entry.n;
  }

  function summary(entry) {
    if (!entry?.n) return { db: null, std: null, n: 0 };
    return {
      db: 10 * Math.log10(entry.power),
      std: entry.n > 1 ? Math.sqrt(entry.m2 / (entry.n - 1)) : null,
      n: entry.n,
    };
  }

  const table = length => Array.from({ length }, () => ({
    left: stat(), right: stat(),
    left_thd: stat(), right_thd: stat(), left_thdn: stat(), right_thdn: stat(),
    correlation: stat(),
  }));

  function createLab() {
    return {
      version: PROGRAM.version,
      section: null,
      last_index: -1,
      silence_streak: 0,
      ladder_top: -1,
      pending_silence: false,
      passes: 0,
      started_pass: false,
      windows: { accepted: 0, unknown: 0, contaminated: 0, transitional: 0, glitched: 0, silence: 0, out_of_order: 0 },
      mono: table(PROGRAM.sweep_hz.length),
      left: table(PROGRAM.channel_hz.length),
      right: table(PROGRAM.channel_hz.length),
      antiphase: table(PROGRAM.channel_hz.length),
      ladder: table(PROGRAM.ladder_hz.length),
      noise: { left: stat(), right: stat() },
    };
  }

  function record(cell, window) {
    add(cell.left, window.left.peak_dbfs);
    add(cell.right, window.right.peak_dbfs);
    add(cell.left_thd, window.left.thd_db);
    add(cell.right_thd, window.right.thd_db);
    add(cell.left_thdn, window.left.thdn_db);
    add(cell.right_thdn, window.right.thdn_db);
    if (Number.isFinite(window.correlation)) {
      // Correlation is averaged directly; `power` is unused for it.
      const c = cell.correlation;
      c.n++;
      c.mean += (window.correlation - c.mean) / c.n;
    }
  }

  /**
   * Adds one window. Sections are found from the program's order, never from the channel
   * content being measured: a silence starts a pass, the mono sweep rises through all 48
   * tones, and each drop back to a low tone moves on to the left, right and antiphase sweeps.
   * Windows overlapping packet concealment are counted but not measured.
   */
  function beginPass(lab) {
    if (lab.started_pass && lab.section === "ladder") lab.passes++;
    lab.section = "silence";
    lab.last_index = -1;
    lab.ladder_top = -1;
    lab.pending_silence = false;
    lab.started_pass = true;
  }

  function addWindow(lab, window, contaminated = false) {
    if (!window || window.kind === "unknown") {
      lab.windows.unknown++;
      return;
    }
    if (window.kind === "silence") {
      lab.windows.silence++;
      if (++lab.silence_streak < SILENCE_WINDOWS) return;
      const afterLadder = lab.section === "ladder" && lab.ladder_top >= LADDER_LOUD_INDEX;
      if (window.loudest_dbfs >= SILENT_DBFS && !afterLadder) {
        lab.pending_silence = true;
        return;
      }
      beginPass(lab);
      if (!contaminated) {
        add(lab.noise.left, window.left_rms_dbfs);
        add(lab.noise.right, window.right_rms_dbfs);
      }
      return;
    }
    lab.silence_streak = 0;
    if (lab.pending_silence) {
      lab.pending_silence = false;
      if (window.grid === "sweep" && window.index <= 1) beginPass(lab);
    }
    if (!lab.started_pass) {
      // A path that filters out the lowest sweep tones (Opus in voice mode) still shows the
      // ladder, and the silence after it starts the first pass.
      if (window.grid === "ladder") {
        lab.section = "ladder";
        lab.ladder_top = Math.max(lab.ladder_top, window.index);
      }
      lab.windows.out_of_order++;
      return;
    }
    if (window.grid === "ladder") {
      if (lab.section !== "ladder" && lab.section !== "antiphase") {
        lab.windows.out_of_order++;
        return;
      }
      lab.section = "ladder";
      lab.ladder_top = Math.max(lab.ladder_top, window.index);
      if (contaminated) { lab.windows.contaminated++; return; }
      if (!window.steady) { lab.windows.transitional++; return; }
      if (window.glitch) { lab.windows.glitched++; return; }
      record(lab.ladder[window.index], window);
      lab.windows.accepted++;
      return;
    }
    const sections = PROGRAM.sections;
    let section = lab.section === "silence" ? "mono" : lab.section;
    if (!sections.includes(section)) {
      lab.windows.out_of_order++;
      return;
    }
    // A tone well below the last one means the next section has begun.
    if (lab.last_index >= 0 && window.index < lab.last_index - 2) {
      const next = sections[sections.indexOf(section) + 1];
      if (!next) {
        lab.windows.out_of_order++;
        return;
      }
      section = next;
      lab.last_index = -1;
    }
    lab.section = section;
    lab.last_index = Math.max(lab.last_index, window.index);
    if (contaminated) { lab.windows.contaminated++; return; }
    if (!window.steady) { lab.windows.transitional++; return; }
    if (window.glitch) { lab.windows.glitched++; return; }
    if (section === "mono") {
      record(lab.mono[window.index], window);
    } else {
      if (window.index % 4 !== 0) { lab.windows.out_of_order++; return; }
      record(lab[section][window.index / 4], window);
    }
    lab.windows.accepted++;
  }

  function linearFit(xs, ys) {
    const points = xs.map((x, i) => [x, ys[i]]).filter(([, y]) => Number.isFinite(y));
    if (points.length < 2) return null;
    const n = points.length;
    const mx = points.reduce((s, [x]) => s + x, 0) / n;
    const my = points.reduce((s, [, y]) => s + y, 0) / n;
    let sxx = 0, sxy = 0, syy = 0;
    for (const [x, y] of points) {
      sxx += (x - mx) ** 2; sxy += (x - mx) * (y - my); syy += (y - my) ** 2;
    }
    if (!(sxx > 0)) return null;
    const slope = sxy / sxx;
    const intercept = my - slope * mx;
    const residuals = points.map(([x, y]) => y - (intercept + slope * x));
    return {
      slope,
      intercept,
      r2: syy > 0 ? Math.max(0, 1 - residuals.reduce((s, r) => s + r * r, 0) / syy) : 1,
      max_residual_db: Math.max(...residuals.map(Math.abs)),
    };
  }

  const median = values => {
    const sorted = values.filter(Number.isFinite).sort((a, b) => a - b);
    if (!sorted.length) return null;
    const middle = sorted.length >> 1;
    return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
  };

  /** The bounded numeric report of a finished capture. */
  function finalizeLab(lab) {
    const source = PROGRAM.source_peak_dbfs;
    const gain = cell => {
      const s = summary(cell);
      return { db: s.db === null ? null : s.db - source, std: s.std, n: s.n };
    };
    const column = (rows, pick) => rows.map(pick);
    const monoLeft = lab.mono.map(cell => gain(cell.left));
    const monoRight = lab.mono.map(cell => gain(cell.right));
    const response = {
      frequency_hz: [...PROGRAM.sweep_hz],
      left_gain_db: column(monoLeft, value => value.db),
      right_gain_db: column(monoRight, value => value.db),
      left_std_db: column(monoLeft, value => value.std),
      right_std_db: column(monoRight, value => value.std),
      windows: column(monoLeft, value => value.n),
      left_thd_db: lab.mono.map(cell => summary(cell.left_thd).db),
      right_thd_db: lab.mono.map(cell => summary(cell.right_thd).db),
      left_thdn_db: lab.mono.map(cell => summary(cell.left_thdn).db),
      right_thdn_db: lab.mono.map(cell => summary(cell.right_thdn).db),
      correlation: lab.mono.map(cell => cell.correlation.n ? cell.correlation.mean : null),
    };
    const difference = (a, b) => Number.isFinite(a) && Number.isFinite(b) ? a - b : null;
    const separation = {
      frequency_hz: [...PROGRAM.channel_hz],
      left_only_gain_db: lab.left.map(cell => gain(cell.left).db),
      right_only_gain_db: lab.right.map(cell => gain(cell.right).db),
      // Driven channel minus leak into the other one, in dB: higher keeps stereo apart.
      left_to_right_db: lab.left.map(cell => difference(summary(cell.left).db, summary(cell.right).db)),
      right_to_left_db: lab.right.map(cell => difference(summary(cell.right).db, summary(cell.left).db)),
    };
    const antiphase = {
      frequency_hz: [...PROGRAM.channel_hz],
      left_gain_db: lab.antiphase.map(cell => gain(cell.left).db),
      right_gain_db: lab.antiphase.map(cell => gain(cell.right).db),
      correlation: lab.antiphase.map(cell => cell.correlation.n ? cell.correlation.mean : null),
    };
    const ladderOut = lab.ladder.map(cell => {
      const left = summary(cell.left).db, right = summary(cell.right).db;
      return Number.isFinite(left) && Number.isFinite(right)
        ? 10 * Math.log10((10 ** (left / 10) + 10 ** (right / 10)) / 2) : null;
    });
    const linearity = {
      frequency_hz: [...PROGRAM.ladder_hz],
      input_dbfs: [...PROGRAM.ladder_dbfs],
      output_dbfs: ladderOut,
      gain_db: ladderOut.map((out, i) => difference(out, PROGRAM.ladder_dbfs[i])),
      thdn_db: lab.ladder.map(cell => {
        const values = [summary(cell.left_thdn).db, summary(cell.right_thdn).db].filter(Number.isFinite);
        return values.length ? Math.max(...values) : null;
      }),
      fit: linearFit(PROGRAM.ladder_dbfs, ladderOut),
    };
    const audible = response.frequency_hz
      .map((hz, i) => [hz, response.left_gain_db[i], response.right_gain_db[i]])
      .filter(([hz]) => hz >= 100 && hz <= 16_000);
    const gains = audible.flatMap(([, l, r]) => [l, r]).filter(Number.isFinite);
    const separations = [...separation.left_to_right_db, ...separation.right_to_left_db]
      .filter(Number.isFinite);
    const ladderGains = linearity.gain_db.filter(Number.isFinite);
    const summaryReport = {
      measured_response_bands: response.windows.filter(n => n > 0).length,
      median_gain_db: median(gains),
      ripple_100_16k_db: gains.length ? Math.max(...gains) - Math.min(...gains) : null,
      left_right_balance_db: median(audible.map(([, l, r]) => difference(l, r))),
      median_thdn_db: median([...response.left_thdn_db, ...response.right_thdn_db]),
      median_separation_db: median(separations),
      minimum_separation_db: separations.length ? Math.min(...separations) : null,
      antiphase_correlation: median(antiphase.correlation),
      stereo_preserved: separations.length >= 6 && median(separations) >= 20 &&
        (median(antiphase.correlation) ?? 0) < -0.9,
      linearity_slope: linearity.fit?.slope ?? null,
      level_compression_db: ladderGains.length ? Math.max(...ladderGains) - Math.min(...ladderGains) : null,
      noise_floor_dbfs: (() => {
        const values = [summary(lab.noise.left).db, summary(lab.noise.right).db].filter(Number.isFinite);
        return values.length ? Math.max(...values) : null;
      })(),
    };
    return {
      version: PROGRAM.version,
      program: {
        step_ms: PROGRAM.step_ms,
        ladder_step_ms: PROGRAM.ladder_step_ms,
        silence_ms: PROGRAM.silence_ms,
        source_peak_dbfs: source,
      },
      passes: lab.passes,
      windows: { ...lab.windows },
      response,
      separation,
      antiphase,
      linearity,
      noise: {
        left_rms_dbfs: summary(lab.noise.left).db,
        right_rms_dbfs: summary(lab.noise.right).db,
        windows: lab.noise.left.n,
      },
      summary: summaryReport,
    };
  }

  /** Point-by-point difference of two reports' series (current minus baseline). */
  function compare(current, baseline) {
    if (current?.version !== PROGRAM.version || baseline?.version !== PROGRAM.version) return null;
    const delta = (a, b) => a.map((value, i) =>
      Number.isFinite(value) && Number.isFinite(b?.[i]) ? value - b[i] : null);
    return {
      left_gain_db: delta(current.response.left_gain_db, baseline.response.left_gain_db),
      right_gain_db: delta(current.response.right_gain_db, baseline.response.right_gain_db),
      left_thdn_db: delta(current.response.left_thdn_db, baseline.response.left_thdn_db),
      right_thdn_db: delta(current.response.right_thdn_db, baseline.response.right_thdn_db),
      left_to_right_db: delta(current.separation.left_to_right_db, baseline.separation.left_to_right_db),
      right_to_left_db: delta(current.separation.right_to_left_db, baseline.separation.right_to_left_db),
      linearity_gain_db: delta(current.linearity.gain_db, baseline.linearity.gain_db),
    };
  }

  // ---- Real-signal content (program 3) ---------------------------------------------------
  // Every sample is a closed-form function of its index, so AscendCord's test_sweep.rs and
  // this file produce the same signal without sharing state. Noise comes from an integer
  // hash, identical in both languages.
  const TRANSIENTS = Object.freeze({ count: 8, first_ms: 125, spacing_ms: 250, burst_ms: 2, peak: 0.7 });
  const NOISE_SCALE = 0.436; // uniform noise at about -18 dBFS RMS
  const MUSIC_HZ = Object.freeze([196, 246.94, 293.66, 392, 587.33, 880]);
  // Each partial panned to its own place: left and right gains.
  const MUSIC_PAN = Object.freeze(MUSIC_HZ.map((_, j) => {
    const angle = (j + 0.5) / MUSIC_HZ.length * Math.PI / 2;
    return Object.freeze([Math.cos(angle), Math.sin(angle)]);
  }));
  const SPEECH_TILT = Object.freeze(Array.from({ length: 41 }, (_, k) => k ? k ** -0.7 : 0));
  const CONTENT_FADE_MS = 20;

  function hash32(value) {
    let x = value >>> 0;
    x ^= x >>> 16; x = Math.imul(x, 0x7feb352d) >>> 0;
    x ^= x >>> 15; x = Math.imul(x, 0x846ca68b) >>> 0;
    x ^= x >>> 16;
    return x >>> 0;
  }

  /** Uniform noise in [-0.5, 0.5) for stream `seed` at sample `n`. */
  function noiseAt(seed, n) {
    return hash32((Math.imul(seed, 0x9e3779b9) + n) >>> 0) / 4_294_967_296 - 0.5;
  }

  /** Sample offsets of the transient bursts from the start of their section. */
  function transientOnsets(rate) {
    return Array.from({ length: TRANSIENTS.count },
      (_, k) => Math.round(rate * (TRANSIENTS.first_ms + k * TRANSIENTS.spacing_ms) / 1000));
  }

  function formant(hz) {
    let weight = 0.05;
    for (const center of [500, 1_500, 2_500]) weight += 1 / (1 + ((hz - center) / 150) ** 2);
    return weight;
  }

  /** One stereo sample of a content section, before its fade. */
  function contentSample(name, i, rate, onsets) {
    const t = i / rate;
    if (name === "transients") {
      const burst = Math.round(rate * TRANSIENTS.burst_ms / 1000);
      for (let k = 0; k < onsets.length; k++) {
        const local = i - onsets[k];
        if (local >= 0 && local < burst) {
          const window = 0.5 - 0.5 * Math.cos(2 * Math.PI * (local + 0.5) / burst);
          const value = window * noiseAt(1_000 + k, local) * 2 * TRANSIENTS.peak;
          return [value, value];
        }
      }
      return [0, 0];
    }
    if (name === "noise") return [noiseAt(2_001, i) * NOISE_SCALE, noiseAt(2_002, i) * NOISE_SCALE];
    if (name === "music") {
      // Six plucked partials every 0.5 s, 5.5 Hz vibrato, each panned to its own place.
      const since = t % 0.5;
      const envelope = Math.min(1, since / 0.005) * Math.exp(-since / 0.25);
      const wobble = 0.004 / (2 * Math.PI * 5.5) * (1 - Math.cos(2 * Math.PI * 5.5 * t));
      let left = 0, right = 0;
      for (let j = 0; j < MUSIC_HZ.length; j++) {
        const value = 0.09 * envelope * Math.sin(2 * Math.PI * MUSIC_HZ[j] * (t + wobble));
        left += value * MUSIC_PAN[j][0];
        right += value * MUSIC_PAN[j][1];
      }
      return [left, right];
    }
    // Speech-like: a voice gliding 110-180 Hz, harmonics shaped by three formants, four
    // syllables a second.
    const phase = 2 * Math.PI * (145 * t - 35 / Math.PI * Math.sin(Math.PI * t));
    const f0 = 145 - 35 * Math.cos(Math.PI * t);
    // sin(k * phase) by the Chebyshev recurrence, the same arithmetic as test_sweep.rs.
    const twice = 2 * Math.cos(phase);
    let previous = 0, sine = Math.sin(phase), value = 0;
    for (let k = 1; k <= 40 && k * f0 < 7_000; k++) {
      value += formant(k * f0) * SPEECH_TILT[k] * sine;
      const next = twice * sine - previous;
      previous = sine;
      sine = next;
    }
    const syllable = (0.5 - 0.5 * Math.cos(2 * Math.PI * 4 * t)) ** 2;
    return [0.12 * syllable * value, 0.12 * syllable * value];
  }

  const renderedContent = new Map();
  // Song clips AscendCord plays after the content (16-bit, 48 kHz, interleaved), if any.
  let songs = null;

  /** Uses `clips` ({ names, frames, pcm: Int16Array }) as the program's songs; null removes them. */
  function setSongs(clips) {
    const valid = clips && Array.isArray(clips.names) && Array.isArray(clips.frames) &&
      clips.names.length === clips.frames.length && ArrayBuffer.isView(clips.pcm) && clips.pcm.BYTES_PER_ELEMENT === 2 &&
      clips.frames.reduce((sum, n) => sum + n, 0) * 2 === clips.pcm.length;
    songs = valid ? clips : null;
    renderedContent.clear();
    return songs !== null;
  }

  /** The content sections in order: float32 samples, section bounds and transient onsets. */
  function renderContent(rate = 48_000) {
    const cached = renderedContent.get(rate);
    if (cached) return cached;
    const offsets = transientOnsets(rate);
    const sections = [];
    let frames = 0;
    for (const { name, ms } of PROGRAM.content) {
      const length = Math.floor(rate * ms / 1000);
      sections.push({ name, start: frames, frames: length });
      frames += length;
    }
    // Song clips are 48 kHz; at another rate they are left out, as AscendCord does.
    const clips = rate === 48_000 && songs ? songs : null;
    let pcmAt = 0;
    for (let index = 0; clips && index < clips.names.length; index++) {
      sections.push({ name: `song ${index + 1}`, title: clips.names[index], start: frames, frames: clips.frames[index], pcm: pcmAt });
      frames += clips.frames[index];
      pcmAt += clips.frames[index] * 2;
    }
    const left = new Float32Array(frames), right = new Float32Array(frames);
    const fade = Math.floor(rate * CONTENT_FADE_MS / 1000);
    for (const section of sections) {
      for (let i = 0; i < section.frames; i++) {
        const ramp = Math.min(1, Math.min(i, section.frames - i) / fade);
        const [l, r] = section.pcm === undefined
          ? contentSample(section.name, i, rate, offsets)
          : [clips.pcm[section.pcm + 2 * i] / 32_768, clips.pcm[section.pcm + 2 * i + 1] / 32_768];
        left[section.start + i] = Math.fround(l * ramp);
        right[section.start + i] = Math.fround(r * ramp);
      }
    }
    const onsets = offsets.map(onset => sections[0].start + onset);
    const rendered = Object.freeze({ left, right, frames, sections, onsets });
    renderedContent.set(rate, rendered);
    return rendered;
  }

  // ---- Null test: what arrived minus the known content -------------------------------------
  const NULL_BLOCK = 4_096;
  const NULL_SEARCH = 8; // samples a block may move against the last one
  // A jitter buffer speeds playback up or slows it down by whole pitch periods (2.5-15 ms):
  // a block that no longer matches is found again within +-20 ms.
  const NULL_REACQUIRE = 960;
  const NULL_MATCH = 0.95;
  const NULL_BANDS_HZ = Object.freeze([100, 125, 160, 200, 250, 315, 400, 500, 630, 800, 1_000,
    1_250, 1_600, 2_000, 2_500, 3_150, 4_000, 5_000, 6_300, 8_000, 10_000, 12_500, 16_000]);

  const twiddles = new Map();

  function fftInPlace(re, im) {
    const n = re.length;
    let table = twiddles.get(n);
    if (!table) {
      table = { cos: new Float64Array(n / 2), sin: new Float64Array(n / 2) };
      for (let k = 0; k < n / 2; k++) { table.cos[k] = Math.cos(-2 * Math.PI * k / n); table.sin[k] = Math.sin(-2 * Math.PI * k / n); }
      twiddles.set(n, table);
    }
    for (let i = 1, j = 0; i < n; i++) {
      let bit = n >> 1;
      for (; j & bit; bit >>= 1) j ^= bit;
      j ^= bit;
      if (i < j) { [re[i], re[j]] = [re[j], re[i]]; [im[i], im[j]] = [im[j], im[i]]; }
    }
    for (let size = 2; size <= n; size <<= 1) {
      const stride = n / size;
      for (let start = 0; start < n; start += size) {
        for (let k = 0; k < size / 2; k++) {
          const wr = table.cos[k * stride], wi = table.sin[k * stride];
          const a = start + k, b = a + size / 2;
          const xr = re[b] * wr - im[b] * wi, xi = re[b] * wi + im[b] * wr;
          re[b] = re[a] - xr; im[b] = im[a] - xi;
          re[a] += xr; im[a] += xi;
        }
      }
    }
  }

  /** Spectrum of a Hann-windowed stretch of `samples` from `start`. */
  function spectrum(samples, start, n) {
    const re = new Float64Array(n), im = new Float64Array(n);
    for (let i = 0; i < n; i++) re[i] = samples[start + i] * (0.5 - 0.5 * Math.cos(2 * Math.PI * i / n));
    fftInPlace(re, im);
    return { re, im };
  }

  /** Third-octave band of each FFT bin (-1 outside the bands). */
  function binBands(n, rate) {
    const bands = new Int16Array(n / 2 + 1).fill(-1);
    NULL_BANDS_HZ.forEach((center, b) => {
      const low = Math.max(1, Math.round(center / 2 ** (1 / 6) * n / rate));
      const high = Math.min(n / 2, Math.round(center * 2 ** (1 / 6) * n / rate));
      for (let bin = low; bin <= Math.max(low, high); bin++) bands[bin] = b;
    });
    return bands;
  }

  // A constant delay of a fraction of a sample (a resampler on the way) is no loss of quality
  // but leaves a residue that grows with frequency. Each block's delay is fitted from the phase
  // of the cross-spectrum over 100 Hz-12 kHz and taken out before the residue is measured.
  const DELAY_LOW_HZ = 100;
  const DELAY_HIGH_HZ = 12_000;

  /**
   * Finds the content inside `left`/`right` (decoded audio that holds it somewhere), follows
   * its timing block by block, and measures what differs from the known signal. Blocks where
   * the timing slipped or that hold a dropout are counted and left out. Only numbers return.
   */
  function* contentSteps(left, right, rate) {
    const ref = renderContent(rate);
    const length = Math.min(left.length, right.length);
    if (length < ref.frames + NULL_BLOCK) return null;

    // 1. Coarse position: correlate 10 ms energy envelopes.
    const hop = Math.round(rate / 100);
    const envelope = (l, r, n) => {
      const out = new Float64Array(Math.floor(n / hop));
      for (let b = 0; b < out.length; b++) {
        let sum = 0;
        for (let i = b * hop; i < (b + 1) * hop; i++) sum += l[i] * l[i] + r[i] * r[i];
        out[b] = Math.sqrt(sum);
      }
      return out;
    };
    const refEnvelope = envelope(ref.left, ref.right, ref.frames);
    const gotEnvelope = envelope(left, right, length);
    let coarse = -1, coarseScore = 0;
    for (let offset = 0; offset + refEnvelope.length <= gotEnvelope.length; offset++) {
      let dot = 0, norm = 0;
      for (let b = 0; b < refEnvelope.length; b++) {
        dot += refEnvelope[b] * gotEnvelope[offset + b];
        norm += gotEnvelope[offset + b] ** 2;
      }
      const score = norm > 0 ? dot / Math.sqrt(norm) : 0;
      if (score > coarseScore) { coarseScore = score; coarse = offset; }
    }
    if (coarse < 0) return null;

    // 2. Exact lag from the transient bursts (the only non-zero reference samples there).
    const burstSamples = [];
    for (let i = 0; i < ref.sections[0].frames; i++) if (ref.left[i] !== 0) burstSamples.push(i);
    let lag = coarse * hop, lagScore = -Infinity;
    for (let candidate = coarse * hop - hop; candidate <= coarse * hop + hop; candidate++) {
      let dot = 0;
      for (const i of burstSamples) {
        const at = i + candidate;
        if (at < 0 || at >= length) continue;
        dot += ref.left[i] * left[at] + ref.right[i] * right[at];
      }
      if (dot > lagScore) { lagScore = dot; lag = candidate; }
    }

    // 3. Follow the timing block by block; a moved block marks a jitter-buffer slip.
    // Normalised correlation of a block against the arrived audio at `candidate`.
    const match = (start, candidate, refEnergy) => {
      if (start + candidate < 0 || start + candidate + NULL_BLOCK > length) return -Infinity;
      let dot = 0, energy = 0;
      for (let i = start; i < start + NULL_BLOCK; i++) {
        const l = left[i + candidate], r = right[i + candidate];
        dot += ref.left[i] * l + ref.right[i] * r;
        energy += l * l + r * r;
      }
      return energy > 0 ? dot / Math.sqrt(refEnergy * energy) : -Infinity;
    };
    const blocks = [];
    let current = lag;
    for (let start = 0; start + NULL_BLOCK <= ref.frames; start += NULL_BLOCK) {
      let refEnergy = 0;
      for (let i = start; i < start + NULL_BLOCK; i++) refEnergy += ref.left[i] ** 2 + ref.right[i] ** 2;
      let best = current;
      if (refEnergy > 1e-6) {
        let bestScore = -Infinity;
        for (let candidate = current - NULL_SEARCH; candidate <= current + NULL_SEARCH; candidate++) {
          const score = match(start, candidate, refEnergy);
          if (score > bestScore) { bestScore = score; best = candidate; }
        }
        if (bestScore < NULL_MATCH) {
          for (let candidate = current - NULL_REACQUIRE; candidate <= current + NULL_REACQUIRE; candidate++) {
            const score = match(start, candidate, refEnergy);
            if (score > bestScore) { bestScore = score; best = candidate; }
            if (candidate % 64 === 0) yield;
          }
        }
      }
      if (start + best < 0 || start + best + NULL_BLOCK > length) break;
      blocks.push({ start, lag: best, slipped: best !== current, refEnergy });
      current = best;
      yield;
    }

    // 4-5. Gain per section and channel, then the residue of each block. Done twice: blocks
    // that hold a dropout are found on the first round and left out of the second, so they
    // do not bias the gain.
    const sectionOf = start => ref.sections.find(s => start >= s.start && start < s.start + s.frames);
    const bands = binBands(NULL_BLOCK, rate);
    const delayLow = Math.max(1, Math.round(DELAY_LOW_HZ * NULL_BLOCK / rate));
    const delayHigh = Math.min(NULL_BLOCK / 2, Math.round(DELAY_HIGH_HZ * NULL_BLOCK / rate));
    // `spectral` false: integer-lag residue in the time domain, enough to find dropouts.
    const measureBlocks = function* (excluded, spectral) {
      const gains = new Map();
      for (const block of blocks) {
        if (block.slipped || excluded.has(block)) continue;
        const section = sectionOf(block.start);
        const g = gains.get(section.name) ?? { lx: 0, ly: 0, rx: 0, ry: 0 };
        for (let i = block.start; i < block.start + NULL_BLOCK; i++) {
          g.lx += ref.left[i] * left[i + block.lag]; g.ly += ref.left[i] ** 2;
          g.rx += ref.right[i] * right[i + block.lag]; g.ry += ref.right[i] ** 2;
        }
        gains.set(section.name, g);
      }
      const residues = [];
      for (const block of blocks) {
        if (block.slipped) continue;
        const section = sectionOf(block.start);
        const g = gains.get(section.name) ?? { lx: 0, ly: 0, rx: 0, ry: 0 };
        if (!spectral) {
          const gl = g.ly > 0 ? g.lx / g.ly : 0, gr = g.ry > 0 ? g.rx / g.ry : 0;
          let signal = 0, residue = 0;
          for (let i = block.start; i < block.start + NULL_BLOCK; i++) {
            const sl = gl * ref.left[i], sr = gr * ref.right[i];
            signal += sl * sl + sr * sr;
            residue += (left[i + block.lag] - sl) ** 2 + (right[i + block.lag] - sr) ** 2;
          }
          residues.push({ block, section: section.name, signal, residue });
          continue;
        }
        const channels = [
          { gain: g.ly > 0 ? g.lx / g.ly : 0, y: spectrum(ref.left, block.start, NULL_BLOCK), x: spectrum(left, block.start + block.lag, NULL_BLOCK) },
          { gain: g.ry > 0 ? g.rx / g.ry : 0, y: spectrum(ref.right, block.start, NULL_BLOCK), x: spectrum(right, block.start + block.lag, NULL_BLOCK) },
        ];
        // Fractional delay: X = Y e^{-j w d}, so the cross-spectrum phase is -w d.
        let num = 0, den = 0;
        for (const { x, y } of channels) {
          for (let k = delayLow; k <= delayHigh; k++) {
            const cr = x.re[k] * y.re[k] + x.im[k] * y.im[k], ci = x.im[k] * y.re[k] - x.re[k] * y.im[k];
            const weight = Math.hypot(cr, ci);
            if (!(weight > 0)) continue;
            const w = 2 * Math.PI * k / NULL_BLOCK;
            num += weight * Math.atan2(ci, cr) * w;
            den += weight * w * w;
          }
        }
        // The integer lag is the best match, so the rest is within about half a sample; allow
        // a whole sample so a delay near one half is not cut off.
        const delay = den > 0 ? Math.max(-1, Math.min(1, -num / den)) : 0;
        const sigBands = new Float64Array(NULL_BANDS_HZ.length), resBands = new Float64Array(NULL_BANDS_HZ.length);
        let signal = 0, residue = 0, rawResidue = 0;
        for (const { gain, x, y } of channels) {
          for (let k = 1; k <= NULL_BLOCK / 2; k++) {
            const w = 2 * Math.PI * k / NULL_BLOCK * delay;
            const c = Math.cos(w), s = Math.sin(w);
            // g Y e^{-j w d}
            const yr = gain * (y.re[k] * c + y.im[k] * s), yi = gain * (y.im[k] * c - y.re[k] * s);
            const sig = yr * yr + yi * yi;
            const res = (x.re[k] - yr) ** 2 + (x.im[k] - yi) ** 2;
            signal += sig; residue += res;
            rawResidue += (x.re[k] - gain * y.re[k]) ** 2 + (x.im[k] - gain * y.im[k]) ** 2;
            const band = bands[k];
            if (band >= 0) { sigBands[band] += sig; resBands[band] += res; }
          }
        }
        residues.push({ block, section: section.name, signal, residue, rawResidue, delay, sigBands, resBands });
        yield;
      }
      // A dropout raises a block's residue both absolutely and against its own signal; a
      // quiet passage only does the latter.
      const middle = values => { const sorted = values.sort((a, b) => a - b); return sorted.length ? sorted[sorted.length >> 1] : 0; };
      const dropouts = new Set();
      for (const name of new Set(residues.map(r => r.section))) {
        const mine = residues.filter(r => r.section === name && r.signal > 0);
        const usual = { residue: middle(mine.map(r => r.residue)), ratio: middle(mine.map(r => r.residue / r.signal)) };
        for (const r of mine) {
          if (usual.residue > 0 && r.residue > usual.residue * 100 && r.residue / r.signal > usual.ratio * 10) {
            dropouts.add(r.block);
          }
        }
      }
      return { gains, residues, dropouts };
    };
    const first = yield* measureBlocks(new Set(), false);
    const { gains, residues } = yield* measureBlocks(first.dropouts, true);
    const kept = residues.filter(r => !first.dropouts.has(r.block));

    const toDb = (signal, residue) => signal > 0 ? 10 * Math.log10(signal / Math.max(residue, signal * 1e-15)) : null;
    const sections = {};
    const bandSrr = {};
    const songSig = new Float64Array(NULL_BANDS_HZ.length), songRes = new Float64Array(NULL_BANDS_HZ.length);
    let songSignal = 0, songResidue = 0, songBlocks = 0;
    for (const { name, title } of ref.sections) {
      const mine = kept.filter(r => r.section === name);
      const signal = mine.reduce((s, r) => s + r.signal, 0), residue = mine.reduce((s, r) => s + r.residue, 0);
      const rawResidue = mine.reduce((s, r) => s + r.rawResidue, 0);
      const delays = mine.map(r => r.delay).sort((a, b) => a - b);
      const g = gains.get(name);
      sections[name] = {
        blocks: mine.length,
        srr_db: toDb(signal, residue),
        srr_without_delay_db: toDb(signal, rawResidue),
        delay_samples: delays.length ? delays[delays.length >> 1] : null,
        gain_db: g && g.ly > 0 && g.lx > 0 ? 20 * Math.log10(g.lx / g.ly) : null,
        ...(title ? { title: String(title).slice(0, 64) } : {}),
      };
      if (name === "transients") continue;
      const sig = new Float64Array(NULL_BANDS_HZ.length), res = new Float64Array(NULL_BANDS_HZ.length);
      for (const r of mine) {
        r.sigBands.forEach((p, b) => { sig[b] += p; });
        r.resBands.forEach((p, b) => { res[b] += p; });
      }
      yield;
      bandSrr[name] = Array.from(sig, (s, b) => toDb(s, res[b]));
      if (title) {
        sig.forEach((s, b) => { songSig[b] += s; songRes[b] += res[b]; });
        songSignal += signal; songResidue += residue; songBlocks += mine.length;
      }
    }
    if (songBlocks) {
      sections.songs = { blocks: songBlocks, srr_db: toDb(songSignal, songResidue), gain_db: null };
      bandSrr.songs = Array.from(songSig, (s, b) => toDb(s, songRes[b]));
    }

    // 6. Pre-echo: what arrived in the 20 ms before each burst, against the burst itself.
    const burst = Math.round(rate * TRANSIENTS.burst_ms / 1000);
    const before = Math.round(rate * 0.020), guard = Math.round(rate * 0.0005);
    const blockAt = i => blocks.find(b => i >= b.start && i < b.start + NULL_BLOCK);
    const gT = gains.get("transients");
    const gainT = gT && gT.ly > 0 ? gT.lx / gT.ly : 1;
    const preEcho = [], postEcho = [];
    for (const onset of ref.onsets) {
      const block = blockAt(onset);
      if (!block || block.slipped) continue;
      let pre = 0, post = 0, energy = 0;
      for (let i = onset - before; i < onset - guard; i++) pre += left[i + block.lag] ** 2 + right[i + block.lag] ** 2;
      for (let i = onset; i < onset + burst; i++) energy += 2 * (gainT * ref.left[i]) ** 2;
      for (let i = onset + burst; i < onset + burst + before; i++) {
        post += (left[i + block.lag] - gainT * ref.left[i]) ** 2 + (right[i + block.lag] - gainT * ref.right[i]) ** 2;
      }
      // Level of what is there before (and after) the burst relative to the burst: lower is better.
      preEcho.push(pre > 0 && energy > 0 ? 10 * Math.log10(pre / energy) : null);
      postEcho.push(post > 0 && energy > 0 ? 10 * Math.log10(post / energy) : null);
    }
    const finiteSorted = values => values.filter(Number.isFinite).sort((a, b) => a - b);
    const summarize = values => {
      const sorted = finiteSorted(values);
      return {
        values,
        median_db: sorted.length ? sorted[sorted.length >> 1] : null,
        worst_db: sorted.length ? sorted[sorted.length - 1] : null,
      };
    };
    return {
      version: 1,
      lag_samples: lag,
      blocks: blocks.length,
      slipped_blocks: blocks.filter(b => b.slipped).length,
      dropout_blocks: residues.length - kept.length,
      sections,
      bands_hz: [...NULL_BANDS_HZ],
      band_srr_db: bandSrr,
      pre_echo_db: summarize(preEcho),
      post_echo_db: summarize(postEcho),
    };
  }

  /**
   * Several passes' null tests as one: sections and bands by their median across passes,
   * transient pre- and post-echo pooled, disturbances added up. Failed passes are skipped.
   */
  function combineContent(reports) {
    const good = (reports ?? []).filter(report => report && report.version === 1 && report.sections);
    if (!good.length) return null;
    const median = values => {
      const sorted = values.filter(Number.isFinite).sort((a, b) => a - b);
      return sorted.length ? sorted[sorted.length >> 1] : null;
    };
    const names = [...new Set(good.flatMap(report => Object.keys(report.sections)))];
    const sections = Object.fromEntries(names.map(name => {
      const mine = good.map(report => report.sections[name]).filter(Boolean);
      return [name, {
        blocks: mine.reduce((sum, s) => sum + (s.blocks ?? 0), 0),
        srr_db: median(mine.map(s => s.srr_db)),
        srr_without_delay_db: median(mine.map(s => s.srr_without_delay_db)),
        delay_samples: median(mine.map(s => s.delay_samples)),
        gain_db: median(mine.map(s => s.gain_db)),
        ...(mine.find(s => s.title) ? { title: mine.find(s => s.title).title } : {}),
      }];
    }));
    const bandNames = [...new Set(good.flatMap(report => Object.keys(report.band_srr_db ?? {})))];
    const bands = good[0].bands_hz ?? [];
    const band_srr_db = Object.fromEntries(bandNames.map(name => [name,
      bands.map((_, b) => median(good.map(report => report.band_srr_db?.[name]?.[b])))]));
    const pool = key => {
      const values = good.flatMap(report => report[key]?.values ?? []).filter(Number.isFinite);
      const sorted = [...values].sort((a, b) => a - b);
      return { values, median_db: sorted.length ? sorted[sorted.length >> 1] : null, worst_db: sorted.length ? sorted[sorted.length - 1] : null };
    };
    const total = key => good.reduce((sum, report) => sum + (Number(report[key]) || 0), 0);
    return {
      version: 1,
      passes: good.length,
      blocks: total("blocks"),
      slipped_blocks: total("slipped_blocks"),
      dropout_blocks: total("dropout_blocks"),
      gaps: total("gaps"),
      sections,
      bands_hz: bands,
      band_srr_db,
      pre_echo_db: pool("pre_echo_db"),
      post_echo_db: pool("post_echo_db"),
    };
  }

  /** The null test, all at once (tests, the service worker). */
  function analyzeContent(left, right, rate = 48_000) {
    const steps = contentSteps(left, right, rate);
    for (;;) {
      const step = steps.next();
      if (step.done) return step.value;
    }
  }

  /** The null test in slices of about 15 ms, so a page keeps sampling and responding. */
  async function analyzeContentInSlices(left, right, rate = 48_000) {
    const steps = contentSteps(left, right, rate);
    for (;;) {
      const until = Date.now() + 15;
      let step;
      do step = steps.next(); while (!step.done && Date.now() < until);
      if (step.done) return step.value;
      await new Promise(resolve => setTimeout(resolve, 0));
    }
  }

  /**
   * One pass of the program as AscendCord's test_sweep.rs renders it: per-segment phase from
   * zero, a linear 50 ms fade at both ends, left and right in float32.
   */
  function renderPass(rate = 48_000) {
    const segments = [{ kind: "silence", ms: PROGRAM.silence_ms }];
    for (const hz of PROGRAM.sweep_hz) segments.push({ kind: "mono", hz, peak: 0.25, ms: PROGRAM.step_ms });
    for (const kind of ["left", "right", "antiphase"]) {
      for (const hz of PROGRAM.channel_hz) segments.push({ kind, hz, peak: 0.25, ms: PROGRAM.step_ms });
    }
    PROGRAM.ladder_dbfs.forEach((dbfs, index) => segments.push({
      kind: "mono", hz: PROGRAM.ladder_hz[index], peak: 10 ** (dbfs / 20), ms: PROGRAM.ladder_step_ms,
    }));
    const tones = segments.reduce((sum, s) => sum + Math.floor(rate * s.ms / 1000), 0);
    const content = renderContent(rate);
    const frames = tones + content.frames;
    const left = new Float32Array(frames), right = new Float32Array(frames);
    left.set(content.left, tones);
    right.set(content.right, tones);
    const fade = Math.floor(rate * 50 / 1000);
    let at = 0;
    for (const segment of segments) {
      const length = Math.floor(rate * segment.ms / 1000);
      let phase = 0;
      const step = 2 * Math.PI * (segment.hz ?? 0) / rate;
      for (let i = 0; i < length; i++, at++) {
        if (segment.kind === "silence") continue;
        const ramp = Math.min(1, Math.min(i, length - i) / fade);
        const value = Math.fround(Math.sin(phase) * segment.peak * ramp);
        phase += step;
        if (phase >= 2 * Math.PI) phase %= 2 * Math.PI;
        if (segment.kind === "left") left[at] = value;
        else if (segment.kind === "right") right[at] = value;
        else if (segment.kind === "antiphase") { left[at] = value; right[at] = -value; }
        else { left[at] = value; right[at] = value; }
      }
    }
    return { left, right, frames };
  }

  root.AscendCordLab = Object.freeze({
    PROGRAM, renderPass, renderContent, analyzeContent, analyzeContentInSlices, combineContent, setSongs, rms, fitTone, distortion, nearestTone, analyzeWindow,
    createLab, addWindow, finalizeLab, compare, linearFit, finite, db20,
  });
})(globalThis);
