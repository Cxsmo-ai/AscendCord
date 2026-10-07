//! Measurement program v2 for the opt-in browser receive test (`--test-sweep-channel`).
//!
//! One pass, repeated: 1.2 s of digital silence; 48 tones from 20 Hz to 20 kHz with left and
//! right identical; every fourth of those tones on the left only, on the right only and in
//! opposite phase; then eight steps of rising level, each at its own frequency between two
//! sweep tones. Tones last 700 ms (ladder steps 800 ms) with 50 ms fades, at −12.04 dBFS peak
//! except the ladder. `browser-extension/stereo-proof/lab.js` holds the same program and the
//! receiver analysis that identifies each part from its frequency and order alone.
const POINTS: usize = 48;
const LOW_HZ: f64 = 20.0;
const HIGH_HZ: f64 = 20_000.0;
const AMPLITUDE: f64 = 0.25;
const STEP_MS: u64 = 700;
const LADDER_STEP_MS: u64 = 800;
const SILENCE_MS: u64 = 1_200;
const FADE_MS: u64 = 50;
const CHANNEL_STRIDE: usize = 4;
const LADDER_DBFS: [f64; 8] = [-60.0, -48.0, -36.0, -24.0, -18.0, -12.0, -6.0, -1.0];
const LADDER_FIRST: f64 = 26.5;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
	Silence,
	Mono,
	Left,
	Right,
	Antiphase,
}

#[derive(Clone, Copy, Debug)]
struct Segment {
	kind: Kind,
	hz: f64,
	peak: f64,
	frames: u64,
}

fn frequency(step: f64) -> f64 {
	LOW_HZ * (HIGH_HZ / LOW_HZ).powf(step / (POINTS - 1) as f64)
}

fn program(rate: u32) -> Vec<Segment> {
	let frames = |ms: u64| u64::from(rate) * ms / 1_000;
	let tone = |kind, hz, peak, ms| Segment {
		kind,
		hz,
		peak,
		frames: frames(ms),
	};
	let mut segments = vec![tone(Kind::Silence, 0.0, 0.0, SILENCE_MS)];
	for step in 0..POINTS {
		segments.push(tone(Kind::Mono, frequency(step as f64), AMPLITUDE, STEP_MS));
	}
	for kind in [Kind::Left, Kind::Right, Kind::Antiphase] {
		for step in (0..POINTS).step_by(CHANNEL_STRIDE) {
			segments.push(tone(kind, frequency(step as f64), AMPLITUDE, STEP_MS));
		}
	}
	for (index, dbfs) in LADDER_DBFS.into_iter().enumerate() {
		segments.push(tone(
			Kind::Mono,
			frequency(LADDER_FIRST + index as f64),
			10f64.powf(dbfs / 20.0),
			LADDER_STEP_MS,
		));
	}
	segments
}

pub(crate) struct Sweep {
	rate: u32,
	segments: Vec<Segment>,
	segment: usize,
	within: u64,
	phase: f64,
}

impl Sweep {
	pub(crate) fn new(rate: u32) -> Self {
		Self {
			rate,
			segments: program(rate),
			segment: 0,
			within: 0,
			phase: 0.0,
		}
	}

	pub(crate) fn next_frame(&mut self) -> [f32; 2] {
		let segment = self.segments[self.segment];
		let fade_frames = u64::from(self.rate) * FADE_MS / 1_000;
		let fade =
			(self.within.min(segment.frames - self.within) as f64 / fade_frames as f64).min(1.0);
		let value = (self.phase.sin() * segment.peak * fade) as f32;
		self.phase += std::f64::consts::TAU * segment.hz / f64::from(self.rate);
		if self.phase >= std::f64::consts::TAU {
			self.phase %= std::f64::consts::TAU;
		}
		self.within += 1;
		if self.within >= segment.frames {
			self.within = 0;
			self.phase = 0.0;
			self.segment = (self.segment + 1) % self.segments.len();
		}
		match segment.kind {
			Kind::Silence => [0.0, 0.0],
			Kind::Mono => [value, value],
			Kind::Left => [value, 0.0],
			Kind::Right => [0.0, value],
			Kind::Antiphase => [value, -value],
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const RATE: u32 = 48_000;

	/// Frames of one segment from a sweep positioned at its start.
	fn segment_frames(sweep: &mut Sweep, segment: Segment) -> Vec<[f32; 2]> {
		(0..segment.frames).map(|_| sweep.next_frame()).collect()
	}

	#[test]
	fn program_matches_the_extension_grid_and_order() {
		let segments = program(RATE);
		assert_eq!(
			segments.len(),
			1 + POINTS + 3 * POINTS / CHANNEL_STRIDE + LADDER_DBFS.len()
		);
		assert_eq!(segments[0].kind, Kind::Silence);
		assert_eq!(segments[1].hz, LOW_HZ);
		assert!((segments[POINTS].hz - HIGH_HZ).abs() < 1e-6);
		for window in segments[1..=POINTS].windows(2) {
			assert!(window[1].hz > window[0].hz);
		}
		// The ladder sits between sweep tones 26..34, never on one.
		for (index, segment) in segments[segments.len() - LADDER_DBFS.len()..]
			.iter()
			.enumerate()
		{
			let low = frequency(26.0 + index as f64);
			let high = frequency(27.0 + index as f64);
			assert!(segment.hz > low * 1.03 && segment.hz < high / 1.03);
			assert!((20.0 * segment.peak.log10() - LADDER_DBFS[index]).abs() < 1e-9);
		}
		let pass_ms: u64 = segments
			.iter()
			.map(|segment| segment.frames * 1_000 / u64::from(RATE))
			.sum();
		assert_eq!(
			pass_ms,
			SILENCE_MS + (POINTS as u64 + 36) * STEP_MS + 8 * LADDER_STEP_MS
		);
	}

	#[test]
	fn every_section_drives_the_channels_it_names() {
		let mut sweep = Sweep::new(RATE);
		for segment in program(RATE) {
			let frames = segment_frames(&mut sweep, segment);
			for [left, right] in &frames {
				match segment.kind {
					Kind::Silence => assert_eq!((*left, *right), (0.0, 0.0)),
					Kind::Mono => assert_eq!(left.to_bits(), right.to_bits()),
					Kind::Left => assert_eq!(*right, 0.0),
					Kind::Right => assert_eq!(*left, 0.0),
					Kind::Antiphase => assert_eq!(*left, -*right),
				}
				assert!(left.abs() <= 0.9 && right.abs() <= 0.9);
			}
		}
		// The program repeats from silence.
		assert_eq!(sweep.next_frame(), [0.0, 0.0]);
	}

	#[test]
	fn tones_hold_their_level_between_fades() {
		let mut sweep = Sweep::new(RATE);
		let fade = u64::from(RATE) * FADE_MS / 1_000;
		for segment in program(RATE) {
			let frames = segment_frames(&mut sweep, segment);
			if segment.kind == Kind::Silence {
				continue;
			}
			let level = |[left, right]: [f32; 2]| left.abs().max(right.abs());
			assert!(level(frames[0]) < 0.001);
			assert!(level(*frames.last().unwrap()) < 0.001);
			// Two cycles of the slowest tone fit in the steady part, so its peak is reached.
			let steady = &frames[fade as usize..(segment.frames - fade) as usize];
			let peak = steady.iter().copied().map(level).fold(0.0f32, f32::max);
			assert!(
				(f64::from(peak) - segment.peak).abs() < segment.peak * 0.01,
				"{segment:?}"
			);
		}
	}
}
