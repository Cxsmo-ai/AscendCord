//! Equal-level signal used by the opt-in browser receive-curve diagnostic.
const POINTS: usize = 48;
const LOW_HZ: f64 = 20.0;
const HIGH_HZ: f64 = 20_000.0;
const AMPLITUDE: f64 = 0.25;

pub(crate) struct Sweep {
	rate: u32,
	frames: u64,
	phase: f64,
	step: usize,
	frequency: f64,
}

impl Sweep {
	pub(crate) fn new(rate: u32) -> Self {
		Self {
			rate,
			frames: 0,
			phase: 0.0,
			step: usize::MAX,
			frequency: LOW_HZ,
		}
	}

	fn frames_per_step(&self) -> u64 {
		u64::from(self.rate) * 7 / 10
	}

	fn frequency(step: usize) -> f64 {
		LOW_HZ * (HIGH_HZ / LOW_HZ).powf(step as f64 / (POINTS - 1) as f64)
	}

	pub(crate) fn next_frame(&mut self) -> [f32; 2] {
		let frames_per_step = self.frames_per_step();
		let step = (self.frames / frames_per_step) as usize % POINTS;
		if step != self.step {
			self.step = step;
			self.frequency = Self::frequency(step);
		}
		let within_step = self.frames % frames_per_step;
		let fade_frames = u64::from(self.rate) / 20;
		let fade =
			(within_step.min(frames_per_step - within_step) as f64 / fade_frames as f64).min(1.0);
		let sample = (self.phase.sin() * AMPLITUDE * fade) as f32;
		self.phase += std::f64::consts::TAU * self.frequency / f64::from(self.rate);
		if self.phase >= std::f64::consts::TAU {
			self.phase %= std::f64::consts::TAU;
		}
		self.frames = self.frames.wrapping_add(1);
		[sample, sample]
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sweep_has_48_log_spaced_points_matching_the_extension_bins() {
		let first = Sweep::frequency(0);
		let last = Sweep::frequency(POINTS - 1);
		assert_eq!(first, LOW_HZ);
		assert_eq!(last, HIGH_HZ);
		for step in 1..POINTS {
			assert!(Sweep::frequency(step) > Sweep::frequency(step - 1));
		}
	}

	#[test]
	fn sweep_is_equal_level_and_fades_at_step_boundaries() {
		let mut sweep = Sweep::new(48_000);
		let mut peak = 0.0f32;
		let mut first = 0.0f32;
		let mut last = 0.0f32;
		let mut first_frequency_peak = 0.0f32;
		let frames_per_step = sweep.frames_per_step();
		for frame in 0..frames_per_step {
			let [left, right] = sweep.next_frame();
			assert_eq!(left.to_bits(), right.to_bits());
			peak = peak.max(left.abs());
			if frame == 0 {
				first = left;
			}
			if frame == frames_per_step - 1 {
				last = left;
			}
			if frame > 2_400 && frame < frames_per_step - 2_400 {
				first_frequency_peak = first_frequency_peak.max(left.abs());
			}
		}
		assert!(first.abs() < 0.001);
		assert!(last.abs() < 0.001);
		assert!((first_frequency_peak - AMPLITUDE as f32).abs() < 0.001);
		assert!(peak <= AMPLITUDE as f32);
	}
}
