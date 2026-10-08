// Draws a measurement program 2 report as one tall multi-panel figure, optionally against a
// baseline run. Every line is drawn from the numeric report; nothing is generated imagery.
(function (root) {
  "use strict";

  const WIDTH = 2400;
  const COLORS = {
    background: "#1d1f23", panel: "#24272c", grid: "#363a42", axis: "#7c818c", text: "#c9ccd3",
    muted: "#8d929c", left: "#5ce0ae", right: "#7aa7ff", third: "#f2c46d", fourth: "#ff7a90",
    baseline: "#9aa0aa",
  };
  const FONT = "Segoe UI, Inter, sans-serif";
  const PANEL_HEIGHT = 620;
  const MARGIN = { left: 190, right: 60, top: 92, bottom: 96 };

  const finiteValues = values => (values ?? []).filter(Number.isFinite);

  function axisRange(series, fallback, pad = 3) {
    const values = series.flatMap(finiteValues);
    if (!values.length) return fallback;
    let low = Math.min(...values), high = Math.max(...values);
    if (high - low < 1) { low -= 1; high += 1; }
    return [Math.floor((low - pad) / 3) * 3, Math.ceil((high + pad) / 3) * 3];
  }

  /** One panel: frame, title, grid, ticks, and a mapping from data to pixels. */
  function panel(ctx, y, title, subtitle, xScale, yRange, yLabel) {
    const box = { x: MARGIN.left, y: y + MARGIN.top, w: WIDTH - MARGIN.left - MARGIN.right,
      h: PANEL_HEIGHT - MARGIN.top - MARGIN.bottom };
    ctx.fillStyle = COLORS.panel;
    ctx.fillRect(40, y + 20, WIDTH - 80, PANEL_HEIGHT - 32);
    ctx.fillStyle = COLORS.text; ctx.font = `600 34px ${FONT}`; ctx.textAlign = "left";
    ctx.textBaseline = "alphabetic";
    ctx.fillText(title, 70, y + 66);
    const titleWidth = ctx.measureText(title).width;
    ctx.fillStyle = COLORS.muted; ctx.font = `24px ${FONT}`;
    ctx.fillText(subtitle, 70 + titleWidth, y + 66);
    const [ymin, ymax] = yRange;
    const map = {
      x: value => box.x + xScale.position(value) * box.w,
      y: value => box.y + (1 - (value - ymin) / (ymax - ymin)) * box.h,
      box,
    };
    ctx.font = `22px ${FONT}`;
    ctx.strokeStyle = COLORS.grid; ctx.lineWidth = 1.5;
    const yStep = niceStep(ymax - ymin);
    ctx.textAlign = "right"; ctx.textBaseline = "middle";
    for (let value = Math.ceil(ymin / yStep) * yStep; value <= ymax + 1e-9; value += yStep) {
      const py = map.y(value);
      ctx.beginPath(); ctx.moveTo(box.x, py); ctx.lineTo(box.x + box.w, py); ctx.stroke();
      ctx.fillStyle = COLORS.muted; ctx.fillText(formatTick(value), box.x - 14, py);
    }
    ctx.textAlign = "center"; ctx.textBaseline = "top";
    for (const [value, label] of xScale.ticks) {
      const px = map.x(value);
      ctx.beginPath(); ctx.moveTo(px, box.y); ctx.lineTo(px, box.y + box.h); ctx.stroke();
      ctx.fillStyle = COLORS.muted; ctx.fillText(label, px, box.y + box.h + 12);
    }
    ctx.fillStyle = COLORS.muted; ctx.textAlign = "center";
    ctx.fillText(xScale.label, box.x + box.w / 2, box.y + box.h + 48);
    ctx.save();
    ctx.translate(78, box.y + box.h / 2); ctx.rotate(-Math.PI / 2);
    ctx.textBaseline = "middle"; ctx.fillText(yLabel, 0, 0);
    ctx.restore();
    ctx.strokeStyle = COLORS.axis; ctx.lineWidth = 2;
    ctx.strokeRect(box.x, box.y, box.w, box.h);
    return map;
  }

  function niceStep(span) {
    const raw = span / 8;
    const power = 10 ** Math.floor(Math.log10(raw));
    for (const step of [1, 2, 3, 5, 10]) if (step * power >= raw) return step * power;
    return 10 * power;
  }

  const formatTick = value => Math.abs(value) >= 100 || Number.isInteger(value)
    ? `${Math.round(value)}` : value.toFixed(1);

  const frequencyScale = {
    label: "Frequency (Hz)",
    position: hz => Math.log(hz / 20) / Math.log(20_000 / 20),
    ticks: [[20, "20"], [50, "50"], [100, "100"], [200, "200"], [500, "500"], [1_000, "1k"],
      [2_000, "2k"], [5_000, "5k"], [10_000, "10k"], [20_000, "20k"]],
  };

  function line(ctx, map, xs, ys, color, { width = 4, dash = [], points = true } = {}) {
    ctx.save();
    ctx.beginPath(); ctx.rect(map.box.x, map.box.y, map.box.w, map.box.h); ctx.clip();
    ctx.strokeStyle = color; ctx.fillStyle = color; ctx.lineWidth = width; ctx.setLineDash(dash);
    ctx.beginPath();
    let drawing = false;
    xs.forEach((x, index) => {
      const y = ys?.[index];
      if (!Number.isFinite(y)) { drawing = false; return; }
      const px = map.x(x), py = map.y(y);
      if (drawing) ctx.lineTo(px, py); else ctx.moveTo(px, py);
      drawing = true;
    });
    ctx.stroke();
    ctx.setLineDash([]);
    if (points) {
      xs.forEach((x, index) => {
        const y = ys?.[index];
        if (!Number.isFinite(y)) return;
        ctx.beginPath(); ctx.arc(map.x(x), map.y(y), width * 1.4, 0, Math.PI * 2); ctx.fill();
      });
    }
    ctx.restore();
  }

  function errorBars(ctx, map, xs, ys, spread, color) {
    ctx.save();
    ctx.strokeStyle = color; ctx.globalAlpha = 0.55; ctx.lineWidth = 2;
    xs.forEach((x, index) => {
      const y = ys[index], s = spread?.[index];
      if (!Number.isFinite(y) || !Number.isFinite(s) || s <= 0) return;
      const px = map.x(x);
      ctx.beginPath(); ctx.moveTo(px, map.y(y - 2 * s)); ctx.lineTo(px, map.y(y + 2 * s)); ctx.stroke();
    });
    ctx.restore();
  }

  function legend(ctx, map, entries) {
    ctx.font = `24px ${FONT}`; ctx.textBaseline = "middle"; ctx.textAlign = "left";
    let x = map.box.x + map.box.w;
    const y = map.box.y - 30;
    for (const [label, color, dash] of [...entries].reverse()) {
      const width = ctx.measureText(label).width;
      x -= width + 90;
      ctx.strokeStyle = color; ctx.lineWidth = 5; ctx.setLineDash(dash ?? []);
      ctx.beginPath(); ctx.moveTo(x, y); ctx.lineTo(x + 50, y); ctx.stroke(); ctx.setLineDash([]);
      ctx.fillStyle = COLORS.text; ctx.fillText(label, x + 62, y);
    }
  }

  const fmt = (value, digits = 1, suffix = "") =>
    Number.isFinite(value) ? `${value.toFixed(digits)}${suffix}` : "—";

  function header(ctx, report, settings, baseline, title) {
    ctx.fillStyle = COLORS.text; ctx.textAlign = "left"; ctx.textBaseline = "alphabetic";
    ctx.font = `600 52px ${FONT}`;
    ctx.fillText(title, 60, 86);
    ctx.font = `26px ${FONT}`; ctx.fillStyle = COLORS.muted;
    const s = report.summary;
    const rows = [
      `Response 100 Hz–16 kHz: median ${fmt(s.median_gain_db, 2, " dB")} · ripple ${fmt(s.ripple_100_16k_db, 2, " dB")} · L−R balance ${fmt(s.left_right_balance_db, 2, " dB")} · ${s.measured_response_bands}/48 bands`,
      `Stereo: separation median ${fmt(s.median_separation_db, 1, " dB")} (worst ${fmt(s.minimum_separation_db, 1, " dB")}) · antiphase correlation ${fmt(s.antiphase_correlation, 3)} · ${s.stereo_preserved ? "stereo preserved" : "STEREO NOT PRESERVED"}`,
      `Distortion: median THD+N ${fmt(s.median_thdn_db, 1, " dB")} · level slope ${fmt(s.linearity_slope, 3)} · compression ${fmt(s.level_compression_db, 2, " dB")} · decoded silence ${fmt(s.noise_floor_dbfs, 1, " dBFS")}`,
      `Windows: ${report.windows.accepted} measured · ${report.windows.transitional} transitional · ${report.windows.contaminated} disturbed by the network · ${report.windows.glitched ?? 0} glitched · ${report.windows.unknown} unidentified · ${report.passes} passes`,
      `Sender: ${settingsLine(settings)}`,
    ];
    const c = report.content;
    if (c?.sections) {
      const srr = name => fmt(c.sections[name]?.srr_db, 1, " dB");
      rows.push(`Null test (signal to residue): songs ${srr("songs")} · speech ${srr("speech")} · music ${srr("music")} · noise ${srr("noise")} · transients ${srr("transients")} · pre-echo ${fmt(c.pre_echo_db?.median_db, 1, " dB")} (worst ${fmt(c.pre_echo_db?.worst_db, 1, " dB")}) · ${c.slipped_blocks} slips · ${c.dropout_blocks} dropouts · ${c.passes} passes`);
    }
    if (baseline) rows.push(`Baseline (dashed): ${settingsLine(baseline.sender_settings)} · finished ${new Date(baseline.finished_at_ms).toLocaleString()}`);
    rows.forEach((row, index) => ctx.fillText(row, 60, 140 + index * 40));
    return 140 + rows.length * 40 + 10;
  }

  function settingsLine(settings) {
    if (!settings) return "settings unavailable";
    const parts = [];
    if (Number.isFinite(settings.opus_bitrate_target_bps)) parts.push(`${Math.round(settings.opus_bitrate_target_bps / 1000)} kb/s`);
    for (const key of ["opus_application", "opus_signal"]) if (settings[key]) parts.push(settings[key]);
    if (Number.isFinite(settings.opus_complexity)) parts.push(`complexity ${settings.opus_complexity}`);
    if (typeof settings.opus_vbr === "boolean") parts.push(settings.opus_vbr ? "VBR" : "CBR");
    if (typeof settings.opus_fec === "boolean") parts.push(settings.opus_fec ? "FEC" : "no FEC");
    if (settings.force_stereo) parts.push("stereo");
    return parts.join(" · ") || "settings unavailable";
  }

  /** Renders the report; returns the canvas height it needed. */
  function render(canvas, report, {
    settings = null, baseline = null, samples = [],
    title = "AscendCord audio lab · AscendCord to Discord to the browser",
    baselineKey = "measurement_lab",
  } = {}) {
    // A baseline run without a measured report for this direction draws nothing, so it is
    // neither named nor given a difference panel.
    const candidate = baseline?.[baselineKey];
    const base = candidate?.summary?.measured_response_bands > 0 ? candidate : null;
    const content = report.content?.sections ? report.content : null;
    const panels = 5 + (content ? 2 : 0) + (samples.length ? 1 : 0) + (base ? 1 : 0);
    const top = 470 + (base ? 40 : 0) + (content ? 40 : 0);
    canvas.width = WIDTH;
    canvas.height = top + panels * PANEL_HEIGHT + 40;
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = COLORS.background; ctx.fillRect(0, 0, canvas.width, canvas.height);
    header(ctx, report, settings, base ? baseline : null, title);
    let y = top;
    const dashed = [14, 10];

    // 1. Frequency response per channel.
    const r = report.response;
    let map = panel(ctx, y, "Frequency response", "  gain of each channel against the −12.04 dBFS source · bars ±2σ across windows",
      frequencyScale, axisRange([r.left_gain_db, r.right_gain_db, base?.response.left_gain_db], [-12, 6]), "Gain (dB)");
    if (base) {
      line(ctx, map, r.frequency_hz, base.response.left_gain_db, COLORS.left, { width: 3, dash: dashed, points: false });
      line(ctx, map, r.frequency_hz, base.response.right_gain_db, COLORS.right, { width: 3, dash: dashed, points: false });
    }
    errorBars(ctx, map, r.frequency_hz, r.left_gain_db, r.left_std_db, COLORS.left);
    errorBars(ctx, map, r.frequency_hz, r.right_gain_db, r.right_std_db, COLORS.right);
    line(ctx, map, r.frequency_hz, r.left_gain_db, COLORS.left);
    line(ctx, map, r.frequency_hz, r.right_gain_db, COLORS.right);
    legend(ctx, map, [["Left", COLORS.left], ["Right", COLORS.right], ...(base ? [["Baseline", COLORS.baseline, dashed]] : [])]);
    y += PANEL_HEIGHT;

    // 2. Distortion and noise per frequency.
    map = panel(ctx, y, "Distortion per frequency", "  THD+N (all residue) and THD (harmonics 2–10) relative to the tone",
      frequencyScale, axisRange([r.left_thdn_db, r.right_thdn_db, r.left_thd_db], [-100, 0]), "Relative level (dB)");
    if (base) line(ctx, map, r.frequency_hz, base.response.left_thdn_db, COLORS.baseline, { width: 3, dash: dashed, points: false });
    line(ctx, map, r.frequency_hz, r.left_thdn_db, COLORS.left);
    line(ctx, map, r.frequency_hz, r.right_thdn_db, COLORS.right);
    line(ctx, map, r.frequency_hz, r.left_thd_db, COLORS.third, { width: 3, points: false });
    legend(ctx, map, [["THD+N left", COLORS.left], ["THD+N right", COLORS.right], ["THD left", COLORS.third]]);
    y += PANEL_HEIGHT;

    // 3. Stereo separation and opposite-phase behaviour.
    const sep = report.separation, anti = report.antiphase;
    map = panel(ctx, y, "Stereo separation", "  driven channel above the leak into the other one · antiphase gain shows mono folding",
      frequencyScale, axisRange([sep.left_to_right_db, sep.right_to_left_db, anti.left_gain_db], [-60, 120], 6), "dB");
    if (base) line(ctx, map, sep.frequency_hz, base.separation.left_to_right_db, COLORS.baseline, { width: 3, dash: dashed, points: false });
    line(ctx, map, sep.frequency_hz, sep.left_to_right_db, COLORS.left);
    line(ctx, map, sep.frequency_hz, sep.right_to_left_db, COLORS.right);
    line(ctx, map, anti.frequency_hz, anti.left_gain_db, COLORS.fourth, { width: 3 });
    legend(ctx, map, [["Left → right", COLORS.left], ["Right → left", COLORS.right], ["Antiphase gain", COLORS.fourth]]);
    y += PANEL_HEIGHT;

    // 4. Level linearity.
    const lin = report.linearity;
    const linear = { label: "Source level (dBFS)", position: value => (value + 66) / 66,
      ticks: [-60, -48, -36, -24, -18, -12, -6, -1].map(value => [value, `${value}`]) };
    map = panel(ctx, y, "Level linearity", `  output against source at each ladder step · slope ${fmt(lin.fit?.slope, 3)} · worst residual ${fmt(lin.fit?.max_residual_db, 2, " dB")}`,
      linear, axisRange([lin.output_dbfs, lin.input_dbfs], [-70, 6]), "Output (dBFS)");
    line(ctx, map, lin.input_dbfs, lin.input_dbfs, COLORS.baseline, { width: 2, dash: [6, 8], points: false });
    if (base) line(ctx, map, lin.input_dbfs, base.linearity.output_dbfs, COLORS.baseline, { width: 3, dash: dashed, points: false });
    line(ctx, map, lin.input_dbfs, lin.output_dbfs, COLORS.left);
    legend(ctx, map, [["Measured", COLORS.left], ["Ideal", COLORS.baseline, [6, 8]]]);
    y += PANEL_HEIGHT;

    // 5. Gain error and distortion against level.
    map = panel(ctx, y, "Gain error and distortion against level", "  0 dB gain error is a perfectly linear path",
      linear, axisRange([lin.gain_db, lin.thdn_db], [-60, 6]), "dB");
    line(ctx, map, lin.input_dbfs, lin.gain_db, COLORS.left);
    line(ctx, map, lin.input_dbfs, lin.thdn_db, COLORS.third);
    legend(ctx, map, [["Gain error", COLORS.left], ["THD+N", COLORS.third]]);
    y += PANEL_HEIGHT;

    if (content) {
      // 5a. What the content keeps of itself, band by band: higher is closer to the source.
      const kinds = [["songs", COLORS.left], ["speech", COLORS.right], ["music", COLORS.third], ["noise", COLORS.fourth]]
        .filter(([name]) => content.band_srr_db?.[name]);
      map = panel(ctx, y, "Null test by frequency", "  arrived minus the known source · signal to residue per third-octave band",
        frequencyScale, axisRange(kinds.map(([name]) => content.band_srr_db[name]), [0, 60]), "Signal to residue (dB)");
      if (base?.content?.band_srr_db?.songs) {
        line(ctx, map, base.content.bands_hz, base.content.band_srr_db.songs, COLORS.baseline, { width: 3, dash: dashed, points: false });
      }
      for (const [name, color] of kinds) line(ctx, map, content.bands_hz, content.band_srr_db[name], color);
      legend(ctx, map, [...kinds.map(([name, color]) => [name[0].toUpperCase() + name.slice(1), color]),
        ...(base?.content?.band_srr_db?.songs ? [["Baseline songs", COLORS.baseline, dashed]] : [])]);
      y += PANEL_HEIGHT;

      // 5b. Each piece of content on its own.
      const names = Object.keys(content.sections).filter(name => name !== "songs");
      const pieces = {
        label: "Content",
        position: index => (index + 0.5) / names.length,
        ticks: names.map((name, index) => [index, name]),
      };
      map = panel(ctx, y, "Null test by content",
        `  signal to residue · transients' pre-echo median ${fmt(content.pre_echo_db?.median_db, 1, " dB")}, post-echo ${fmt(content.post_echo_db?.median_db, 1, " dB")}`,
        pieces, axisRange([names.map(name => content.sections[name].srr_db)], [0, 60]), "Signal to residue (dB)");
      ctx.font = `22px ${FONT}`; ctx.textAlign = "center"; ctx.textBaseline = "bottom";
      names.forEach((name, index) => {
        const value = content.sections[name].srr_db;
        if (!Number.isFinite(value)) return;
        ctx.fillStyle = name.startsWith("song") ? COLORS.left : COLORS.third;
        ctx.beginPath(); ctx.arc(map.x(index), map.y(value), 9, 0, Math.PI * 2); ctx.fill();
        ctx.fillStyle = COLORS.text;
        ctx.fillText(value.toFixed(1), map.x(index), map.y(value) - 14);
        const title = content.sections[name].title;
        if (title) {
          ctx.save();
          ctx.fillStyle = COLORS.muted; ctx.font = `18px ${FONT}`; ctx.textBaseline = "top";
          ctx.fillText(title.slice(0, 22), map.x(index), map.box.y + 8);
          ctx.restore();
        }
      });
      y += PANEL_HEIGHT;
    }

    // 6. Network timeline during the capture.
    if (samples.length) {
    const matched = samples.filter(sample => sample.sender_match === true);
    const times = matched.map(sample => sample.at_ms);
    const t0 = times[0] ?? 0, t1 = times[times.length - 1] ?? 1;
    const timeScale = { label: "Capture time (s)", position: t => (t - t0) / Math.max(1, t1 - t0),
      ticks: Array.from({ length: 7 }, (_, i) => { const t = t0 + i * (t1 - t0) / 6; return [t, `${Math.round((t - t0) / 1000)}`]; }) };
    const normalize = key => {
      const values = matched.map(sample => Number(sample[key] ?? 0));
      const peak = Math.max(1e-9, ...values);
      return { values: values.map(value => 100 * value / peak), peak };
    };
    const bitrate = normalize("bitrate_bps"), jitter = normalize("jitter_ms"),
      buffer = normalize("jitter_buffer_delay_ms"), loss = normalize("loss_percent");
    map = panel(ctx, y, "Network during the capture", `  each line scaled to its own peak: bitrate ${fmt(bitrate.peak / 1000, 0, " kb/s")} · jitter ${fmt(jitter.peak, 1, " ms")} · jitter buffer ${fmt(buffer.peak, 0, " ms")} · loss ${fmt(loss.peak, 2, " %")}`,
      timeScale, [0, 110], "% of peak");
    line(ctx, map, times, bitrate.values, COLORS.left, { width: 3, points: false });
    line(ctx, map, times, jitter.values, COLORS.right, { width: 3, points: false });
    line(ctx, map, times, buffer.values, COLORS.third, { width: 3, points: false });
    line(ctx, map, times, loss.values, COLORS.fourth, { width: 3, points: false });
    legend(ctx, map, [["Bitrate", COLORS.left], ["Jitter", COLORS.right], ["Jitter buffer", COLORS.third], ["Loss", COLORS.fourth]]);
    y += PANEL_HEIGHT;
    }

    // 7. Difference from the baseline run.
    if (base) {
      const delta = root.AscendCordLab.compare(report, base);
      map = panel(ctx, y, "Change from baseline", "  this run minus the baseline · positive is louder or more distorted",
        frequencyScale, axisRange([delta?.left_gain_db, delta?.right_gain_db, delta?.left_thdn_db], [-6, 6], 1), "Δ dB");
      line(ctx, map, r.frequency_hz, delta?.left_gain_db, COLORS.left);
      line(ctx, map, r.frequency_hz, delta?.right_gain_db, COLORS.right);
      line(ctx, map, r.frequency_hz, delta?.left_thdn_db, COLORS.third, { width: 3 });
      legend(ctx, map, [["Δ gain left", COLORS.left], ["Δ gain right", COLORS.right], ["Δ THD+N left", COLORS.third]]);
    }
    return canvas.height;
  }

  root.AscendCordLabRender = Object.freeze({ render, settingsLine });
})(globalThis);
