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
    version: 2,
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
    ladder_dbfs: Object.freeze([-60, -48, -36, -24, -18, -12, -6, -1]),
    ladder_hz: Object.freeze(Array.from({ length: 8 }, (_, index) => sweepHz(26 + index + 0.5))),
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
    const frames = segments.reduce((sum, s) => sum + Math.floor(rate * s.ms / 1000), 0);
    const left = new Float32Array(frames), right = new Float32Array(frames);
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
    PROGRAM, renderPass, rms, fitTone, distortion, nearestTone, analyzeWindow,
    createLab, addWindow, finalizeLab, compare, linearFit, finite, db20,
  });
})(globalThis);
