//! Receiver analysis of measurement program 2 (see `test_sweep.rs`), the same estimators as
//! `browser-extension/stereo-proof/lab.js`: least-squares tone fits, distortion from the
//! fitted residue, and sections found from frequency and order. AscendCord uses it on audio
//! it receives from a browser that plays the program back, so both directions of a call are
//! measured the same way. Only numbers leave this module.
const POINTS: usize = 48;
const CHANNEL_STRIDE: usize = 4;
const LADDER_DBFS: [f64; 8] = [-60.0, -48.0, -36.0, -24.0, -18.0, -12.0, -6.0, -1.0];
const SOURCE_PEAK_DBFS: f64 = -12.041_199_826_559_248;
const SILENCE_DBFS: f64 = -40.0;
// Comfort noise from a codec makes the silence merely quiet, and then the ladder's quietest
// steps look the same. Quiet after the ladder has reached -36 dBFS is the silence; any
// other quiet stretch counts only if the next tone is the start of the sweep.
const SILENT_DBFS: f64 = -70.0;
const SILENCE_WINDOWS: u32 = 3;
const LADDER_LOUD_INDEX: usize = 2;
const MIN_EXPLAINED: f64 = 0.8;
const STEADY_DB: f64 = 0.1;
const MAX_HARMONIC: usize = 10;

fn grid_hz(step: f64) -> f64 {
	20.0 * 1_000f64.powf(step / (POINTS - 1) as f64)
}

pub(crate) fn sweep_hz(index: usize) -> f64 {
	grid_hz(index as f64)
}

pub(crate) fn ladder_hz(index: usize) -> f64 {
	grid_hz(26.5 + index as f64)
}

fn db20(value: f64) -> f64 {
	if value > 0.0 {
		(20.0 * value.log10()).max(-160.0)
	} else {
		-160.0
	}
}

#[derive(Clone, Copy, Debug)]
struct Fit {
	a: f64,
	b: f64,
	mean: f64,
	signal_power: f64,
	residual_power: f64,
	explained: f64,
}

impl Fit {
	fn amplitude(&self) -> f64 {
		self.a.hypot(self.b)
	}
}

/// Joint least-squares fit of DC, cosine and sine at a known frequency. `known_mean` fixes
/// the DC level for slices too short to estimate it.
fn fit_tone(samples: &[f32], hz: f64, rate: f64, known_mean: Option<f64>) -> Option<Fit> {
	let n = samples.len();
	if n == 0 || hz <= 0.0 || hz >= rate / 2.0 {
		return None;
	}
	let (mut sc, mut sss, mut cc, mut ss, mut cs) = (0.0, 0.0, 0.0, 0.0, 0.0);
	let (mut sy, mut yc, mut ys, mut yy) = (0.0, 0.0, 0.0, 0.0);
	let step = std::f64::consts::TAU * hz / rate;
	let (step_cos, step_sin) = (step.cos(), step.sin());
	let (mut c, mut s) = (1.0f64, 0.0f64);
	for &sample in samples {
		let y = f64::from(sample) - known_mean.unwrap_or(0.0);
		sc += c;
		sss += s;
		cc += c * c;
		ss += s * s;
		cs += c * s;
		sy += y;
		yc += y * c;
		ys += y * s;
		yy += y * y;
		let next = c * step_cos - s * step_sin;
		s = s * step_cos + c * step_sin;
		c = next;
	}
	let n = n as f64;
	let (mean, a, b, dc) = if let Some(mean) = known_mean {
		let determinant = cc * ss - cs * cs;
		if determinant <= 0.0 {
			return None;
		}
		(
			mean,
			(yc * ss - ys * cs) / determinant,
			(ys * cc - yc * cs) / determinant,
			0.0,
		)
	} else {
		let det3 = |m: [f64; 9]| {
			m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
				+ m[2] * (m[3] * m[7] - m[4] * m[6])
		};
		let determinant = det3([n, sc, sss, sc, cc, cs, sss, cs, ss]);
		if determinant.abs() <= 1e-12 {
			return None;
		}
		let mean = det3([sy, sc, sss, yc, cc, cs, ys, cs, ss]) / determinant;
		let a = det3([n, sy, sss, sc, yc, cs, sss, ys, ss]) / determinant;
		let b = det3([n, sc, sy, sc, cc, yc, sss, cs, ys]) / determinant;
		(mean, a, b, mean)
	};
	let total = yy - 2.0 * dc * sy + n * dc * dc;
	let fitted = (a * (yc - dc * sc) + b * (ys - dc * sss)).clamp(0.0, total.max(0.0));
	Some(Fit {
		a,
		b,
		mean,
		signal_power: fitted / n,
		residual_power: (total - fitted).max(0.0) / n,
		explained: if total > 0.0 { fitted / total } else { 0.0 },
	})
}

/// THD (harmonics 2..10 fitted to the residue) and THD+N (all residue), relative to the tone.
fn distortion(
	samples: &[f32],
	hz: f64,
	rate: f64,
	fundamental: &Fit,
) -> (Option<f64>, Option<f64>) {
	if fundamental.signal_power <= 0.0 {
		return (None, None);
	}
	let step = std::f64::consts::TAU * hz / rate;
	let residual: Vec<f32> = samples
		.iter()
		.enumerate()
		.map(|(i, &sample)| {
			let phase = step * i as f64;
			(f64::from(sample)
				- fundamental.mean
				- fundamental.a * phase.cos()
				- fundamental.b * phase.sin()) as f32
		})
		.collect();
	let mut harmonic_power = 0.0;
	let mut harmonics = 0;
	for k in 2..=MAX_HARMONIC {
		let harmonic = hz * k as f64;
		if harmonic >= rate * 0.49 {
			break;
		}
		if let Some(fit) = fit_tone(&residual, harmonic, rate, Some(0.0)) {
			harmonic_power += fit.signal_power;
			harmonics += 1;
		}
	}
	let ratio = |power: f64| db20((power / fundamental.signal_power).sqrt());
	(
		(harmonics > 0).then(|| ratio(harmonic_power)),
		Some(ratio(fundamental.residual_power)),
	)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Grid {
	Sweep,
	Ladder,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Channel {
	pub peak_dbfs: f64,
	pub thd_db: Option<f64>,
	pub thdn_db: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Window {
	Silence {
		loudest_dbfs: f64,
		left_rms_dbfs: f64,
		right_rms_dbfs: f64,
	},
	Tone {
		steady: bool,
		grid: Grid,
		index: usize,
		left: Channel,
		right: Channel,
		correlation: Option<f64>,
	},
	Unknown,
}

fn rms(samples: &[f32]) -> f64 {
	let n = samples.len().max(1) as f64;
	let mean = samples.iter().map(|&s| f64::from(s)).sum::<f64>() / n;
	(samples
		.iter()
		.map(|&s| (f64::from(s) - mean).powi(2))
		.sum::<f64>()
		/ n)
		.sqrt()
}

/// One window of both decoded channels. Without an FFT here, the program tone is found by
/// fitting every grid frequency and keeping the strongest.
pub(crate) fn analyze_window(left: &[f32], right: &[f32], rate: f64) -> Window {
	let loudest = db20(rms(left).max(rms(right)));
	let louder = if rms(left) >= rms(right) { left } else { right };
	let mut best: Option<(Grid, usize, f64, f64)> = None;
	let candidates = (0..POINTS)
		.map(|index| (Grid::Sweep, index, sweep_hz(index)))
		.chain((0..LADDER_DBFS.len()).map(|index| (Grid::Ladder, index, ladder_hz(index))));
	for (grid, index, hz) in candidates {
		if let Some(fit) = fit_tone(louder, hz, rate, None)
			&& best.is_none_or(|(_, _, _, power)| fit.signal_power > power)
		{
			best = Some((grid, index, hz, fit.signal_power));
		}
	}
	let tone = best.map(|(grid, index, hz, _)| (grid, index, hz));
	let fits = tone.map(|(_, _, hz)| {
		(
			fit_tone(left, hz, rate, None),
			fit_tone(right, hz, rate, None),
		)
	});
	let (Some((grid, index, hz)), Some((Some(left_fit), Some(right_fit)))) = (tone, fits) else {
		return Window::Unknown;
	};
	let (dominant, dominant_samples) = if right_fit.amplitude() > left_fit.amplitude() {
		(right_fit, right)
	} else {
		(left_fit, left)
	};
	if dominant.explained < MIN_EXPLAINED {
		return if loudest < SILENCE_DBFS {
			Window::Silence {
				loudest_dbfs: loudest,
				left_rms_dbfs: db20(rms(left)),
				right_rms_dbfs: db20(rms(right)),
			}
		} else {
			Window::Unknown
		};
	}
	let quarter = dominant_samples.len() / 4;
	let levels: Vec<Option<f64>> = (0..4)
		.map(|part| {
			fit_tone(
				&dominant_samples[part * quarter..(part + 1) * quarter],
				hz,
				rate,
				Some(dominant.mean),
			)
			.filter(|fit| fit.amplitude() > 0.0)
			.map(|fit| db20(fit.amplitude()))
		})
		.collect();
	let steady = levels.iter().all(Option::is_some) && {
		let values: Vec<f64> = levels.iter().flatten().copied().collect();
		values.iter().copied().fold(f64::MIN, f64::max)
			- values.iter().copied().fold(f64::MAX, f64::min)
			<= STEADY_DB
	};
	let (mut ll, mut rr, mut lr) = (0.0, 0.0, 0.0);
	for (&l, &r) in left.iter().zip(right) {
		let (a, b) = (f64::from(l) - left_fit.mean, f64::from(r) - right_fit.mean);
		ll += a * a;
		rr += b * b;
		lr += a * b;
	}
	let channel = |fit: Fit, samples: &[f32]| {
		let (thd_db, thdn_db) = if fit.amplitude() >= dominant.amplitude() * 0.1 {
			distortion(samples, hz, rate, &fit)
		} else {
			(None, None)
		};
		Channel {
			peak_dbfs: db20(fit.amplitude()),
			thd_db,
			thdn_db,
		}
	};
	Window::Tone {
		steady,
		grid,
		index,
		left: channel(left_fit, left),
		right: channel(right_fit, right),
		correlation: (ll > 0.0 && rr > 0.0).then(|| (lr / (ll * rr).sqrt()).clamp(-1.0, 1.0)),
	}
}

/// Least-squares line through the measured points, as `lab.js` `linearFit`.
fn linear_fit(xs: &[f64], ys: &[Option<f64>]) -> Option<serde_json::Value> {
	let points: Vec<(f64, f64)> = xs
		.iter()
		.zip(ys)
		.filter_map(|(&x, y)| y.filter(|y| y.is_finite()).map(|y| (x, y)))
		.collect();
	if points.len() < 2 {
		return None;
	}
	let n = points.len() as f64;
	let mx = points.iter().map(|(x, _)| x).sum::<f64>() / n;
	let my = points.iter().map(|(_, y)| y).sum::<f64>() / n;
	let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
	for (x, y) in &points {
		sxx += (x - mx).powi(2);
		sxy += (x - mx) * (y - my);
		syy += (y - my).powi(2);
	}
	if sxx <= 0.0 {
		return None;
	}
	let slope = sxy / sxx;
	let intercept = my - slope * mx;
	let residuals: Vec<f64> = points
		.iter()
		.map(|(x, y)| y - (intercept + slope * x))
		.collect();
	let r2 = if syy > 0.0 {
		(1.0 - residuals.iter().map(|r| r * r).sum::<f64>() / syy).max(0.0)
	} else {
		1.0
	};
	Some(serde_json::json!({
		"slope": slope,
		"intercept": intercept,
		"r2": r2,
		"max_residual_db": residuals.iter().map(|r| r.abs()).fold(0.0, f64::max),
	}))
}

/// Running mean and spread of a dB value, with the linear-power mean beside it.
#[derive(Clone, Copy, Debug, Default)]
struct Stat {
	n: u32,
	mean: f64,
	m2: f64,
	power: f64,
}

impl Stat {
	fn add(&mut self, db: Option<f64>) {
		let Some(db) = db.filter(|db| db.is_finite()) else {
			return;
		};
		self.n += 1;
		let delta = db - self.mean;
		self.mean += delta / f64::from(self.n);
		self.m2 += delta * (db - self.mean);
		self.power += (10f64.powf(db / 10.0) - self.power) / f64::from(self.n);
	}

	fn db(&self) -> Option<f64> {
		(self.n > 0).then(|| 10.0 * self.power.log10())
	}

	fn std(&self) -> Option<f64> {
		(self.n > 1).then(|| (self.m2 / f64::from(self.n - 1)).sqrt())
	}
}

#[derive(Clone, Copy, Debug, Default)]
struct Cell {
	left: Stat,
	right: Stat,
	left_thd: Stat,
	right_thd: Stat,
	left_thdn: Stat,
	right_thdn: Stat,
	correlation_n: u32,
	correlation: f64,
}

impl Cell {
	fn record(&mut self, left: &Channel, right: &Channel, correlation: Option<f64>) {
		self.left.add(Some(left.peak_dbfs));
		self.right.add(Some(right.peak_dbfs));
		self.left_thd.add(left.thd_db);
		self.right_thd.add(right.thd_db);
		self.left_thdn.add(left.thdn_db);
		self.right_thdn.add(right.thdn_db);
		if let Some(correlation) = correlation {
			self.correlation_n += 1;
			self.correlation += (correlation - self.correlation) / f64::from(self.correlation_n);
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Section {
	None,
	Silence,
	Mono,
	Left,
	Right,
	Antiphase,
	Ladder,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Windows {
	pub accepted: u32,
	pub unknown: u32,
	pub contaminated: u32,
	pub transitional: u32,
	pub silence: u32,
	pub out_of_order: u32,
}

/// Aggregates program 2 windows in the order they arrive.
pub(crate) struct Lab {
	section: Section,
	last_index: Option<usize>,
	silence_streak: u32,
	ladder_top: Option<usize>,
	pending_silence: bool,
	started: bool,
	pub passes: u32,
	pub windows: Windows,
	mono: [Cell; POINTS],
	left: [Cell; POINTS / CHANNEL_STRIDE],
	right: [Cell; POINTS / CHANNEL_STRIDE],
	antiphase: [Cell; POINTS / CHANNEL_STRIDE],
	ladder: [Cell; LADDER_DBFS.len()],
	noise_left: Stat,
	noise_right: Stat,
}

impl Default for Lab {
	fn default() -> Self {
		Self {
			section: Section::None,
			last_index: None,
			silence_streak: 0,
			ladder_top: None,
			pending_silence: false,
			started: false,
			passes: 0,
			windows: Windows::default(),
			mono: [Cell::default(); POINTS],
			left: [Cell::default(); POINTS / CHANNEL_STRIDE],
			right: [Cell::default(); POINTS / CHANNEL_STRIDE],
			antiphase: [Cell::default(); POINTS / CHANNEL_STRIDE],
			ladder: [Cell::default(); LADDER_DBFS.len()],
			noise_left: Stat::default(),
			noise_right: Stat::default(),
		}
	}
}

impl Lab {
	fn begin_pass(&mut self) {
		if self.started && self.section == Section::Ladder {
			self.passes += 1;
		}
		self.section = Section::Silence;
		self.last_index = None;
		self.ladder_top = None;
		self.pending_silence = false;
		self.started = true;
	}

	pub(crate) fn add(&mut self, window: Window, contaminated: bool) {
		let (steady, grid, index, left, right, correlation) = match window {
			Window::Unknown => {
				self.windows.unknown += 1;
				return;
			}
			Window::Silence {
				loudest_dbfs,
				left_rms_dbfs,
				right_rms_dbfs,
			} => {
				self.windows.silence += 1;
				self.silence_streak += 1;
				if self.silence_streak < SILENCE_WINDOWS {
					return;
				}
				let after_ladder = self.section == Section::Ladder
					&& self.ladder_top.is_some_and(|top| top >= LADDER_LOUD_INDEX);
				if loudest_dbfs >= SILENT_DBFS && !after_ladder {
					self.pending_silence = true;
					return;
				}
				self.begin_pass();
				if !contaminated {
					self.noise_left.add(Some(left_rms_dbfs));
					self.noise_right.add(Some(right_rms_dbfs));
				}
				return;
			}
			Window::Tone {
				steady,
				grid,
				index,
				left,
				right,
				correlation,
			} => (steady, grid, index, left, right, correlation),
		};
		self.silence_streak = 0;
		if self.pending_silence {
			self.pending_silence = false;
			if grid == Grid::Sweep && index <= 1 {
				self.begin_pass();
			}
		}
		if !self.started {
			// A path that filters out the lowest sweep tones (Opus in voice mode) still shows
			// the ladder, and the silence after it starts the first pass.
			if grid == Grid::Ladder {
				self.section = Section::Ladder;
				self.ladder_top = Some(self.ladder_top.map_or(index, |top| top.max(index)));
			}
			self.windows.out_of_order += 1;
			return;
		}
		if grid == Grid::Ladder {
			if !matches!(self.section, Section::Ladder | Section::Antiphase) {
				self.windows.out_of_order += 1;
				return;
			}
			self.section = Section::Ladder;
			self.ladder_top = Some(self.ladder_top.map_or(index, |top| top.max(index)));
			if contaminated {
				self.windows.contaminated += 1;
			} else if !steady {
				self.windows.transitional += 1;
			} else {
				self.ladder[index].record(&left, &right, correlation);
				self.windows.accepted += 1;
			}
			return;
		}
		let mut section = match self.section {
			Section::Silence => Section::Mono,
			Section::Mono | Section::Left | Section::Right | Section::Antiphase => self.section,
			_ => {
				self.windows.out_of_order += 1;
				return;
			}
		};
		if self.last_index.is_some_and(|last| index + 2 < last) {
			section = match section {
				Section::Mono => Section::Left,
				Section::Left => Section::Right,
				Section::Right => Section::Antiphase,
				_ => {
					self.windows.out_of_order += 1;
					return;
				}
			};
			self.last_index = None;
		}
		self.section = section;
		self.last_index = Some(self.last_index.map_or(index, |last| last.max(index)));
		if contaminated {
			self.windows.contaminated += 1;
			return;
		}
		if !steady {
			self.windows.transitional += 1;
			return;
		}
		let cell = match section {
			Section::Mono => &mut self.mono[index],
			_ if index % CHANNEL_STRIDE != 0 => {
				self.windows.out_of_order += 1;
				return;
			}
			Section::Left => &mut self.left[index / CHANNEL_STRIDE],
			Section::Right => &mut self.right[index / CHANNEL_STRIDE],
			_ => &mut self.antiphase[index / CHANNEL_STRIDE],
		};
		cell.record(&left, &right, correlation);
		self.windows.accepted += 1;
	}

	/// The same report shape as `lab.js` `finalizeLab`.
	pub(crate) fn report(&self) -> serde_json::Value {
		let gain = |stat: &Stat| stat.db().map(|db| db - SOURCE_PEAK_DBFS);
		let difference = |a: Option<f64>, b: Option<f64>| a.zip(b).map(|(a, b)| a - b);
		let correlation = |cell: &Cell| (cell.correlation_n > 0).then_some(cell.correlation);
		let frequencies: Vec<f64> = (0..POINTS).map(sweep_hz).collect();
		let channel_hz: Vec<f64> = (0..POINTS).step_by(CHANNEL_STRIDE).map(sweep_hz).collect();
		let separations: Vec<f64> = self
			.left
			.iter()
			.map(|cell| difference(cell.left.db(), cell.right.db()))
			.chain(
				self.right
					.iter()
					.map(|cell| difference(cell.right.db(), cell.left.db())),
			)
			.flatten()
			.collect();
		let median = |mut values: Vec<f64>| {
			values.retain(|value| value.is_finite());
			if values.is_empty() {
				return None;
			}
			values.sort_by(f64::total_cmp);
			let middle = values.len() / 2;
			Some(if values.len() % 2 == 1 {
				values[middle]
			} else {
				(values[middle - 1] + values[middle]) / 2.0
			})
		};
		let ladder_out: Vec<Option<f64>> = self
			.ladder
			.iter()
			.map(|cell| {
				cell.left.db().zip(cell.right.db()).map(|(l, r)| {
					10.0 * ((10f64.powf(l / 10.0) + 10f64.powf(r / 10.0)) / 2.0).log10()
				})
			})
			.collect();
		let audible: Vec<(Option<f64>, Option<f64>)> = frequencies
			.iter()
			.zip(&self.mono)
			.filter(|(hz, _)| (100.0..=16_000.0).contains(*hz))
			.map(|(_, cell)| (gain(&cell.left), gain(&cell.right)))
			.collect();
		let gains: Vec<f64> = audible
			.iter()
			.flat_map(|(l, r)| [*l, *r])
			.flatten()
			.collect();
		let antiphase_correlation = median(self.antiphase.iter().filter_map(correlation).collect());
		let ladder_gain: Vec<Option<f64>> = ladder_out
			.iter()
			.zip(LADDER_DBFS)
			.map(|(out, input)| out.map(|out| out - input))
			.collect();
		let ladder_gains: Vec<f64> = ladder_gain.iter().flatten().copied().collect();
		let fit = linear_fit(&LADDER_DBFS, &ladder_out);
		serde_json::json!({
			"version": 2,
			"program": {
				"step_ms": 700,
				"ladder_step_ms": 800,
				"silence_ms": 1_200,
				"source_peak_dbfs": SOURCE_PEAK_DBFS,
			},
			"passes": self.passes,
			"windows": {
				"accepted": self.windows.accepted,
				"unknown": self.windows.unknown,
				"contaminated": self.windows.contaminated,
				"transitional": self.windows.transitional,
				"silence": self.windows.silence,
				"out_of_order": self.windows.out_of_order,
			},
			"response": {
				"frequency_hz": frequencies,
				"left_gain_db": self.mono.iter().map(|cell| gain(&cell.left)).collect::<Vec<_>>(),
				"right_gain_db": self.mono.iter().map(|cell| gain(&cell.right)).collect::<Vec<_>>(),
				"left_std_db": self.mono.iter().map(|cell| cell.left.std()).collect::<Vec<_>>(),
				"right_std_db": self.mono.iter().map(|cell| cell.right.std()).collect::<Vec<_>>(),
				"windows": self.mono.iter().map(|cell| cell.left.n).collect::<Vec<_>>(),
				"left_thd_db": self.mono.iter().map(|cell| cell.left_thd.db()).collect::<Vec<_>>(),
				"right_thd_db": self.mono.iter().map(|cell| cell.right_thd.db()).collect::<Vec<_>>(),
				"left_thdn_db": self.mono.iter().map(|cell| cell.left_thdn.db()).collect::<Vec<_>>(),
				"right_thdn_db": self.mono.iter().map(|cell| cell.right_thdn.db()).collect::<Vec<_>>(),
				"correlation": self.mono.iter().map(correlation).collect::<Vec<_>>(),
			},
			"separation": {
				"frequency_hz": channel_hz,
				"left_only_gain_db": self.left.iter().map(|cell| gain(&cell.left)).collect::<Vec<_>>(),
				"right_only_gain_db": self.right.iter().map(|cell| gain(&cell.right)).collect::<Vec<_>>(),
				"left_to_right_db": self.left.iter().map(|cell| difference(cell.left.db(), cell.right.db())).collect::<Vec<_>>(),
				"right_to_left_db": self.right.iter().map(|cell| difference(cell.right.db(), cell.left.db())).collect::<Vec<_>>(),
			},
			"antiphase": {
				"frequency_hz": channel_hz,
				"left_gain_db": self.antiphase.iter().map(|cell| gain(&cell.left)).collect::<Vec<_>>(),
				"right_gain_db": self.antiphase.iter().map(|cell| gain(&cell.right)).collect::<Vec<_>>(),
				"correlation": self.antiphase.iter().map(correlation).collect::<Vec<_>>(),
			},
			"linearity": {
				"frequency_hz": (0..LADDER_DBFS.len()).map(ladder_hz).collect::<Vec<_>>(),
				"input_dbfs": LADDER_DBFS,
				"output_dbfs": ladder_out,
				"gain_db": ladder_gain,
				"thdn_db": self.ladder.iter().map(|cell| cell.left_thdn.db().into_iter().chain(cell.right_thdn.db()).reduce(f64::max)).collect::<Vec<_>>(),
				"fit": fit,
			},
			"noise": {
				"left_rms_dbfs": self.noise_left.db(),
				"right_rms_dbfs": self.noise_right.db(),
				"windows": self.noise_left.n,
			},
			"summary": {
				"measured_response_bands": self.mono.iter().filter(|cell| cell.left.n > 0).count(),
				"median_gain_db": median(gains.clone()),
				"ripple_100_16k_db": (!gains.is_empty()).then(|| gains.iter().copied().fold(f64::MIN, f64::max) - gains.iter().copied().fold(f64::MAX, f64::min)),
				"left_right_balance_db": median(audible.iter().filter_map(|(l, r)| difference(*l, *r)).collect()),
				"median_thdn_db": median(self.mono.iter().flat_map(|cell| [cell.left_thdn.db(), cell.right_thdn.db()]).flatten().collect()),
				"median_separation_db": median(separations.clone()),
				"minimum_separation_db": separations.iter().copied().reduce(f64::min),
				"antiphase_correlation": antiphase_correlation,
				"stereo_preserved": separations.len() >= 6 && median(separations.clone()).is_some_and(|s| s >= 20.0) && antiphase_correlation.is_some_and(|c| c < -0.9),
				"linearity_slope": fit.as_ref().map(|fit| fit["slope"].clone()),
				"level_compression_db": (!ladder_gains.is_empty()).then(|| ladder_gains.iter().copied().fold(f64::MIN, f64::max) - ladder_gains.iter().copied().fold(f64::MAX, f64::min)),
				"noise_floor_dbfs": self.noise_left.db().into_iter().chain(self.noise_right.db()).reduce(f64::max),
			},
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const RATE: f64 = 48_000.0;
	const WINDOW: usize = 16_384;

	/// Runs `passes` passes of the real generator through `channel` and the analysis.
	fn measure(passes: u32, channel: impl Fn(f32, f32) -> (f32, f32)) -> serde_json::Value {
		let mut sweep = crate::test_sweep::Sweep::new(48_000);
		let pass_frames = 48 * (1_200 + 84 * 700 + 8 * 800);
		let total = pass_frames * passes as usize + 48 * 1_200;
		let (mut left, mut right) = (Vec::with_capacity(total), Vec::with_capacity(total));
		for _ in 0..total {
			let [l, r] = sweep.next_frame();
			let (l, r) = channel(l, r);
			left.push(l);
			right.push(r);
		}
		let mut lab = Lab::default();
		let mut end = WINDOW;
		while end <= total {
			lab.add(
				analyze_window(&left[end - WINDOW..end], &right[end - WINDOW..end], RATE),
				false,
			);
			end += 4_800;
		}
		lab.report()
	}

	fn number(value: &serde_json::Value) -> f64 {
		value.as_f64().expect("a measured number")
	}

	#[test]
	fn comfort_noise_neither_hides_the_silence_nor_starts_a_pass_in_the_ladder() {
		// Uniform noise of this width is about -60 dBFS RMS, like decoded Opus comfort noise.
		let seed = std::cell::Cell::new(7u64);
		let noise = || {
			let next = seed
				.get()
				.wrapping_mul(6_364_136_223_846_793_005)
				.wrapping_add(1);
			seed.set(next);
			((next >> 33) as f64 / f64::from(1u32 << 31) - 0.5) as f32 * 0.0035
		};
		let report = measure(3, |l, r| (l + noise(), r + noise()));
		assert!(
			number(&report["passes"]) >= 2.0,
			"passes {}",
			report["passes"]
		);
		let summary = &report["summary"];
		assert_eq!(summary["measured_response_bands"], 48);
		assert_eq!(summary["stereo_preserved"], true);
		assert!((number(&report["noise"]["left_rms_dbfs"]) + 60.0).abs() < 2.0);
		for gain in &report["linearity"]["gain_db"].as_array().unwrap()[2..] {
			assert!(number(gain).abs() < 0.5, "{gain}");
		}
	}

	#[test]
	fn a_path_that_filters_out_the_lowest_tones_still_starts_after_the_ladder() {
		// Four one-pole high-pass stages at 150 Hz remove 20-30 Hz almost entirely, as Opus
		// in voice mode does.
		let a = 1.0 / (1.0 + std::f64::consts::TAU * 150.0 / RATE);
		let state = std::cell::RefCell::new([[0.0f64; 8]; 2]);
		let filter = |channel: usize, input: f32| {
			let mut state = state.borrow_mut();
			let stages = &mut state[channel];
			let mut value = f64::from(input);
			for stage in 0..4 {
				let out = a * (stages[4 + stage] + value - stages[stage]);
				stages[stage] = value;
				stages[4 + stage] = out;
				value = out;
			}
			value as f32
		};
		let report = measure(3, |l, r| (filter(0, l), filter(1, r)));
		assert!(
			number(&report["passes"]) >= 1.0,
			"passes {}",
			report["passes"]
		);
		assert!(number(&report["summary"]["measured_response_bands"]) >= 40.0);
		// 20 Hz is either missing or measured far down, never at full level.
		let lowest = &report["response"]["left_gain_db"][0];
		assert!(lowest.is_null() || number(lowest) < -40.0, "{lowest}");
		assert!(number(&report["response"]["left_gain_db"][30]).abs() < 0.5);
	}

	#[test]
	fn the_real_program_measures_clean_through_a_clean_path() {
		let report = measure(2, |l, r| (l, r));
		assert_eq!(report["passes"], 2);
		let summary = &report["summary"];
		assert_eq!(summary["measured_response_bands"], 48);
		assert!(number(&summary["ripple_100_16k_db"]) < 0.05);
		assert!(number(&summary["median_thdn_db"]) < -80.0);
		assert!(number(&summary["median_separation_db"]) > 100.0);
		assert_eq!(summary["stereo_preserved"], true);
		for gain in report["linearity"]["gain_db"].as_array().unwrap() {
			assert!(number(gain).abs() < 0.05);
		}
		// The same fields as lab.js, so the popup draws both directions alike.
		assert!((number(&summary["linearity_slope"]) - 1.0).abs() < 0.001);
		assert!(number(&summary["level_compression_db"]) < 0.05);
		assert!(number(&report["linearity"]["fit"]["r2"]) > 0.9999);
		assert_eq!(report["linearity"]["thdn_db"].as_array().unwrap().len(), 8);
		assert!(number(&report["linearity"]["thdn_db"][7]) < -80.0);
		assert_eq!(report["program"]["silence_ms"], 1_200);
	}

	#[test]
	fn the_return_path_measures_decoded_frames_fed_by_the_mixer() {
		returns::enable();
		let mut sweep = crate::test_sweep::Sweep::new(48_000);
		let frames = 48 * (1_200 + 84 * 700 + 8 * 800) + 48 * 1_200 * 2;
		for chunk in 0..frames / 960 {
			let stereo: Vec<f32> = (0..960).flat_map(|_| sweep.next_frame()).collect();
			returns::feed(4242, &stereo, false);
			// Calls deliver a frame every 20 ms; the bounded queue drops audio fed far faster.
			if chunk % 16 == 15 {
				std::thread::sleep(std::time::Duration::from_millis(15));
			}
		}
		let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
		let report = loop {
			if let Some(report) = returns::report().filter(|report| report["passes"] == 1) {
				break report;
			}
			assert!(
				std::time::Instant::now() < deadline,
				"return path produced no full pass"
			);
			std::thread::sleep(std::time::Duration::from_millis(100));
		};
		assert_eq!(report["ssrc"], 4242);
		assert_eq!(report["summary"]["measured_response_bands"], 48);
		assert_eq!(report["summary"]["stereo_preserved"], true);
	}

	#[test]
	fn a_mono_fold_and_a_quieter_right_channel_are_reported() {
		let folded = measure(1, |l, r| {
			let mono = (l + r) / 2.0;
			(mono, mono * 0.5)
		});
		assert_eq!(folded["summary"]["stereo_preserved"], false);
		let balance = number(&folded["summary"]["left_right_balance_db"]);
		assert!((balance - 20.0 * 2f64.log10()).abs() < 0.1, "{balance}");
	}
}

/// Audio AscendCord receives from a browser that plays the program back during a test.
/// Off unless enabled, then fed from the mixer after decoding; analysed on its own thread.
pub mod returns {
	use super::{Lab, analyze_window};
	use std::{
		collections::HashMap,
		sync::{
			Arc, Mutex, OnceLock,
			atomic::{AtomicBool, AtomicU64, Ordering},
			mpsc,
		},
	};

	const WINDOW: usize = 16_384;

	/// Receive-side events that explain an empty report: whether audio from a speaker the
	/// voice server never announced arrives at all, carries DAVE encryption, and is matched.
	#[derive(Clone, Copy)]
	pub(crate) enum Receive {
		Packets,
		Unannounced,
		Encrypted,
		Probed,
		Matched,
	}
	static COUNTS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
	const DECODED: usize = 5;

	pub(crate) fn count(event: Receive) {
		if ACTIVE.load(Ordering::Relaxed) {
			COUNTS[event as usize].fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Totals since the lab was enabled, for the sender status.
	pub fn receive_counts() -> serde_json::Value {
		let get = |index: usize| COUNTS[index].load(Ordering::Relaxed);
		serde_json::json!({
			"voice_packets": get(Receive::Packets as usize),
			"unannounced_packets": get(Receive::Unannounced as usize),
			"unannounced_encrypted": get(Receive::Encrypted as usize),
			"probed": get(Receive::Probed as usize),
			"matched": get(Receive::Matched as usize),
			"decoded_frames": get(DECODED),
		})
	}
	const HOP: usize = 4_800;
	const SOURCES: usize = 4;

	struct Shared {
		send: mpsc::SyncSender<(u32, Vec<f32>, bool)>,
		report: Arc<Mutex<Option<serde_json::Value>>>,
	}

	static ACTIVE: AtomicBool = AtomicBool::new(false);
	static SHARED: OnceLock<Shared> = OnceLock::new();

	struct Source {
		left: Vec<f32>,
		right: Vec<f32>,
		/// Frames added since the last window.
		pending: usize,
		/// Frames until the newest concealed audio has left the analysis window.
		concealed: usize,
		lab: Lab,
	}

	/// Starts measuring every remote speaker for the rest of this process.
	pub fn enable() {
		ACTIVE.store(true, Ordering::Release);
		SHARED.get_or_init(|| {
			let (send, receive) = mpsc::sync_channel::<(u32, Vec<f32>, bool)>(256);
			let report = Arc::new(Mutex::new(None));
			let output = report.clone();
			let _ = std::thread::Builder::new()
				.name("return-path-lab".into())
				.spawn(move || {
					let mut sources: HashMap<u32, Source> = HashMap::new();
					let mut since_report = 0u32;
					while let Ok((ssrc, stereo, concealed)) = receive.recv() {
						if !sources.contains_key(&ssrc) && sources.len() >= SOURCES {
							continue;
						}
						let source = sources.entry(ssrc).or_insert_with(|| Source {
							left: Vec::with_capacity(WINDOW * 2),
							right: Vec::with_capacity(WINDOW * 2),
							pending: 0,
							concealed: 0,
							lab: Lab::default(),
						});
						for pair in stereo.as_chunks::<2>().0 {
							source.left.push(pair[0]);
							source.right.push(pair[1]);
						}
						let frames = stereo.len() / 2;
						source.pending += frames;
						if concealed {
							source.concealed = WINDOW + frames;
						}
						while source.pending >= HOP && source.left.len() >= WINDOW {
							source.pending -= HOP;
							let end = source.left.len() - source.pending;
							if end < WINDOW {
								continue;
							}
							let window = analyze_window(
								&source.left[end - WINDOW..end],
								&source.right[end - WINDOW..end],
								48_000.0,
							);
							source.lab.add(window, source.concealed > 0);
							source.concealed = source.concealed.saturating_sub(HOP);
							since_report += 1;
						}
						if source.left.len() > WINDOW * 2 {
							let drop = source.left.len() - WINDOW - source.pending;
							source.left.drain(..drop);
							source.right.drain(..drop);
						}
						if since_report >= 10 {
							since_report = 0;
							let best = sources
								.iter()
								.max_by_key(|(_, source)| source.lab.windows.accepted);
							if let Some((ssrc, source)) = best
								&& let Ok(mut slot) = output.lock()
							{
								let mut report = source.lab.report();
								report["ssrc"] = (*ssrc).into();
								*slot = Some(report);
							}
						}
					}
				});
			Shared { send, report }
		});
	}

	/// One decoded 48 kHz stereo frame of a remote speaker; `concealed` marks loss concealment.
	pub(crate) fn feed(ssrc: u32, stereo: &[f32], concealed: bool) {
		if !ACTIVE.load(Ordering::Acquire) {
			return;
		}
		COUNTS[DECODED].fetch_add(1, Ordering::Relaxed);
		if let Some(shared) = SHARED.get() {
			let _ = shared.send.try_send((ssrc, stereo.to_vec(), concealed));
		}
	}

	/// The best-matching remote speaker's report so far.
	pub fn report() -> Option<serde_json::Value> {
		SHARED.get()?.report.lock().ok()?.clone()
	}
}
