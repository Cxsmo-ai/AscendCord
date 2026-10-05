//! Stereo polyphase windowed-sinc conversion from a capture device's native rate to the
//! 48 kHz Opus rate. Any rate whose ratio to 48 kHz reduces to at most 1,024 phases is
//! supported (8–384 kHz, including the 44.1 kHz family). The Kaiser-windowed prototype
//! keeps the passband flat to 20 kHz (or 45% of the lower rate) and rejects aliases by
//! about 120 dB, at a group delay of roughly 1–2 ms.

/// Opus's internal rate; every capture rate is converted to this.
pub const OUTPUT_RATE: u32 = 48_000;
const MAX_PHASES: usize = 1_024;
const MAX_COEFFICIENTS: usize = 1 << 18;
const STOPBAND_DB: f64 = 120.0;

pub struct Resampler {
	up: usize,
	down: usize,
	taps: usize,
	/// `coefficients[phase * taps + j]` weights the input `j` samples before the newest.
	coefficients: Vec<f32>,
	/// Doubled history so the newest `taps` frames are always one contiguous slice.
	history: Vec<[f32; 2]>,
	head: usize,
	phase: usize,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
	while b != 0 {
		(a, b) = (b, a % b);
	}
	a
}

/// Zeroth-order modified Bessel function of the first kind, for the Kaiser window.
fn bessel_i0(x: f64) -> f64 {
	let mut sum = 1.0;
	let mut term = 1.0;
	let half = x / 2.0;
	for k in 1..64 {
		term *= half / k as f64;
		let squared = term * term;
		sum += squared;
		if squared < sum * 1e-17 {
			break;
		}
	}
	sum
}

impl Resampler {
	/// Returns `None` for rates outside 8–384 kHz or with an impractical ratio to 48 kHz.
	pub fn new(input_rate: u32) -> Option<Self> {
		if !(8_000..=384_000).contains(&input_rate) {
			return None;
		}
		let divisor = gcd(u64::from(input_rate), u64::from(OUTPUT_RATE));
		let up = (u64::from(OUTPUT_RATE) / divisor) as usize;
		let down = (u64::from(input_rate) / divisor) as usize;
		if up > MAX_PHASES {
			return None;
		}
		if up == 1 && down == 1 {
			return Some(Self {
				up,
				down,
				taps: 1,
				coefficients: vec![1.0],
				history: vec![[0.0; 2]; 2],
				head: 0,
				phase: 0,
			});
		}
		let lower = f64::from(input_rate.min(OUTPUT_RATE));
		let stop = lower / 2.0;
		let pass = (lower * 0.45).min(20_000.0);
		let cutoff = (pass + stop) / 2.0;
		let prototype_rate = f64::from(input_rate) * up as f64;
		let transition = std::f64::consts::TAU * (stop - pass) / prototype_rate;
		let length = ((STOPBAND_DB - 7.95) / (2.285 * transition)).ceil() as usize + 1;
		let taps = length.div_ceil(up).max(2);
		let total = taps * up;
		if total > MAX_COEFFICIENTS {
			return None;
		}
		let beta = 0.1102 * (STOPBAND_DB - 8.7);
		let norm = bessel_i0(beta);
		let centre = (total - 1) as f64 / 2.0;
		let normalized = 2.0 * cutoff / prototype_rate;
		let mut coefficients = vec![0.0f32; total];
		for (k, slot) in (0..total).map(|k| (k, k % up * taps + k / up)) {
			let offset = k as f64 - centre;
			let sinc = if offset == 0.0 {
				1.0
			} else {
				let x = std::f64::consts::PI * normalized * offset;
				x.sin() / x
			};
			let ratio = offset / centre.max(1.0);
			let window = bessel_i0(beta * (1.0 - ratio * ratio).max(0.0).sqrt()) / norm;
			// Interpolation by `up` inserts zeros, so each phase is scaled back by `up`.
			coefficients[slot] = (normalized * sinc * window * up as f64) as f32;
		}
		Some(Self {
			up,
			down,
			taps,
			coefficients,
			history: vec![[0.0; 2]; taps * 2],
			head: 0,
			phase: 0,
		})
	}

	/// Converts interleaved stereo input, appending interleaved 48 kHz stereo to `output`.
	pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
		for frame in input.as_chunks::<2>().0 {
			let frame = frame.map(|sample| if sample.is_finite() { sample } else { 0.0 });
			self.head = (self.head + self.taps - 1) % self.taps;
			self.history[self.head] = frame;
			self.history[self.head + self.taps] = frame;
			let recent = &self.history[self.head..self.head + self.taps];
			while self.phase < self.up {
				let weights = &self.coefficients[self.phase * self.taps..][..self.taps];
				let (mut left, mut right) = (0.0f32, 0.0f32);
				for (weight, [l, r]) in weights.iter().zip(recent) {
					left += weight * l;
					right += weight * r;
				}
				output.push(left);
				output.push(right);
				self.phase += self.down;
			}
			self.phase -= self.up;
		}
	}

	/// Forgets buffered history after a capture discontinuity.
	pub fn reset(&mut self) {
		self.history.fill([0.0; 2]);
		self.head = 0;
		self.phase = 0;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sine(rate: u32, frequency: f64, seconds: f64, amplitude: f64) -> Vec<f32> {
		let count = (f64::from(rate) * seconds) as usize;
		(0..count)
			.flat_map(|n| {
				let t = n as f64 / f64::from(rate);
				let left = (std::f64::consts::TAU * frequency * t).sin() * amplitude;
				let right = (std::f64::consts::TAU * frequency * 1.5 * t).sin() * amplitude;
				[left as f32, right as f32]
			})
			.collect()
	}

	fn stereo_sine(rate: u32, frequency: f64, seconds: f64, amplitude: f64) -> Vec<f32> {
		let count = (f64::from(rate) * seconds) as usize;
		(0..count)
			.flat_map(|n| {
				let phase = std::f64::consts::TAU * frequency * n as f64 / f64::from(rate);
				[
					(phase.sin() * amplitude) as f32,
					(phase.cos() * amplitude) as f32,
				]
			})
			.collect()
	}

	/// Least-squares fit of a sinusoid at `frequency`; returns (amplitude, residual RMS).
	fn fit(samples: &[f32], channel: usize, rate: f64, frequency: f64) -> (f64, f64) {
		let values: Vec<f64> = samples
			.iter()
			.skip(channel)
			.step_by(2)
			.map(|v| f64::from(*v))
			.collect();
		let (mut ss, mut cc, mut sc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
		for (n, y) in values.iter().enumerate() {
			let w = std::f64::consts::TAU * frequency * n as f64 / rate;
			let (s, c) = w.sin_cos();
			ss += s * s;
			cc += c * c;
			sc += s * c;
			ys += y * s;
			yc += y * c;
		}
		let det = ss * cc - sc * sc;
		let a = (ys * cc - yc * sc) / det;
		let b = (yc * ss - ys * sc) / det;
		let mut residual = 0.0;
		for (n, y) in values.iter().enumerate() {
			let w = std::f64::consts::TAU * frequency * n as f64 / rate;
			residual += (y - a * w.sin() - b * w.cos()).powi(2);
		}
		(
			(a * a + b * b).sqrt(),
			(residual / values.len() as f64).sqrt(),
		)
	}

	fn convert(rate: u32, input: &[f32]) -> Vec<f32> {
		let mut resampler = Resampler::new(rate).expect("supported rate");
		let mut output = Vec::new();
		// Uneven callback sizes must not change the result.
		for chunk in input.chunks(2 * 733) {
			resampler.process(chunk, &mut output);
		}
		output
	}

	#[test]
	fn native_48k_is_bit_exact() {
		let input = sine(48_000, 997.0, 0.1, 0.5);
		assert_eq!(convert(48_000, &input), input);
	}

	#[test]
	fn common_rates_keep_tones_clean_and_level_exact() {
		for rate in [
			44_100, 88_200, 96_000, 176_400, 192_000, 32_000, 16_000, 22_050,
		] {
			let input = sine(rate, 1_000.0, 0.5, 0.5);
			let output = convert(rate, &input);
			let expected = input.len() as f64 / 2.0 * 48_000.0 / f64::from(rate);
			assert!(
				(output.len() as f64 / 2.0 - expected).abs() <= 2.0,
				"rate {rate} length"
			);
			// Skip the filter's start-up transient.
			let steady = &output[output.len() / 4..];
			for (channel, frequency) in [(0, 1_000.0), (1, 1_500.0)] {
				let (amplitude, residual) = fit(steady, channel, 48_000.0, frequency);
				let gain_db = 20.0 * (amplitude / 0.5).log10();
				let snr_db = 20.0 * (amplitude / residual).log10();
				assert!(
					gain_db.abs() < 0.01,
					"rate {rate} ch{channel} gain {gain_db:.4} dB"
				);
				assert!(snr_db > 95.0, "rate {rate} ch{channel} SNR {snr_db:.1} dB");
			}
		}
	}

	#[test]
	fn passband_is_flat_to_20_khz_at_96k() {
		for frequency in [20.0, 5_000.0, 15_000.0, 19_000.0] {
			let input = stereo_sine(96_000, frequency, 0.5, 0.5);
			let output = convert(96_000, &input);
			for channel in 0..2 {
				let (amplitude, _) = fit(&output[output.len() / 4..], channel, 48_000.0, frequency);
				let gain_db = 20.0 * (amplitude / 0.5).log10();
				assert!(
					gain_db.abs() < 0.05,
					"{frequency} Hz ch{channel} gain {gain_db:.4} dB"
				);
			}
		}
	}

	#[test]
	fn common_44100_hz_microphone_rate_stays_flat_through_19_khz() {
		for frequency in [20.0, 500.0, 5_000.0, 15_000.0, 18_000.0, 19_000.0] {
			let input = stereo_sine(44_100, frequency, 0.25, 0.5);
			let output = convert(44_100, &input);
			for channel in 0..2 {
				let (amplitude, _) = fit(&output[output.len() / 4..], channel, 48_000.0, frequency);
				let gain_db = 20.0 * (amplitude / 0.5).log10();
				assert!(
					gain_db.abs() < 0.05,
					"{frequency} Hz at 44.1 kHz ch{channel} gain {gain_db:.4} dB"
				);
			}
		}
	}

	#[test]
	fn ultrasonic_content_does_not_alias_into_the_audio_band() {
		// 30 kHz at 96 kHz would fold to 18 kHz at 48 kHz without filtering.
		let input = sine(96_000, 30_000.0, 0.5, 0.9);
		let output = convert(96_000, &input);
		let steady = &output[output.len() / 4..];
		let peak = steady.iter().fold(0.0f32, |peak, v| peak.max(v.abs()));
		let db = 20.0 * f64::from(peak.max(1e-12)).log10();
		assert!(db < -100.0, "alias peak {db:.1} dBFS");
	}

	#[test]
	fn unsupported_rates_are_rejected() {
		assert!(Resampler::new(47_999).is_none());
		assert!(Resampler::new(4_000).is_none());
		assert!(Resampler::new(768_000).is_none());
	}
}
