//! Pace callback batches into 20 ms packets without discarding normal speech.
//!
//! The capture device and the 20 ms network tick run on different clocks. This pacer keeps a
//! small primed cushion, slowly matches the input sample rate to buffer occupancy, and sends
//! at most one frame per tick. Fractional windowed-sinc resampling avoids whole-frame slips;
//! a bounded queue discards old audio only for clock errors beyond the correction range.
//!
//! Buffer occupancy jumps by a whole frame whenever a callback lands just before or after a
//! tick, so the rate correction follows only its long-term trend: a fast correction turned
//! that jitter into audible flutter (the audio lab measured -38 dB THD+N with 3 ms of jitter).
use crate::CaptureFrame;
use std::{
	collections::VecDeque,
	sync::{OnceLock, mpsc::Receiver},
};

/// Frames buffered before sending starts or resumes after an underrun (60 ms).
const PRIME: usize = 3;
/// Hard bound on locally retained frames; the oldest is discarded beyond this.
const MAX: usize = 8;
const SAMPLES_PER_FRAME: usize = 960;
const ASRC_TAPS: usize = 32;
const ASRC_PHASES: usize = 2_048;
const ASRC_BETA: f64 = 10.06;
/// Occupancy aimed for, as measured before a tick's frame is taken: within the deadband
/// below, at least about 1.75 frames stay queued after each tick.
const TARGET_BUFFER_FRAMES: f64 = 3.25;
/// Occupancy is averaged over about five seconds of 20 ms ticks.
const DRIFT_FILTER_ALPHA: f64 = 0.004;
/// Proportional and integral gains of the rate correction, per frame of occupancy error:
/// a well-damped loop (natural period about a minute) where the proportional part already
/// covers the whole range and the integral only removes the remaining offset.
const DRIFT_GAIN: f64 = 0.001;
const DRIFT_INTEGRAL_GAIN: f64 = 0.000_000_25;
/// The correction changes by at most 2 ppm per tick (100 ppm a second): never a quick wobble.
const DRIFT_SLEW: f64 = 0.000_002;
/// Near empty or full the correction may move ten times faster, so even a clock 0.1% off
/// is caught before the cushion runs out.
const DRIFT_URGENT_FRAMES: f64 = 1.5;
/// Occupancy within half a frame of the target is callback jitter, not clock drift: the
/// rate is left alone, so a steady clock passes audio at exactly its own rate.
const DRIFT_DEADBAND: f64 = 0.5;
const MAX_DRIFT_CORRECTION: f64 = 0.001;

type StereoSample = [f64; 2];

fn i0(x: f64) -> f64 {
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

fn asrc_coefficients() -> &'static [[f64; ASRC_TAPS]] {
	static COEFFICIENTS: OnceLock<Vec<[f64; ASRC_TAPS]>> = OnceLock::new();
	COEFFICIENTS.get_or_init(|| {
		let normalization = i0(ASRC_BETA);
		(0..=ASRC_PHASES)
			.map(|phase| {
				let fraction = phase as f64 / ASRC_PHASES as f64;
				let mut coefficients = [0.0; ASRC_TAPS];
				let mut sum = 0.0;
				for (tap, coefficient) in coefficients.iter_mut().enumerate() {
					let offset = tap as isize - (ASRC_TAPS as isize - 1);
					// Causal polyphase interpolation: the filter's fixed group delay is
					// constant, so it needs no future packet lookahead.
					let distance = offset as f64 + (ASRC_TAPS as f64 - 1.0) / 2.0 - fraction;
					let sinc = if distance.abs() < 1e-12 {
						1.0
					} else {
						let x = std::f64::consts::PI * distance;
						x.sin() / x
					};
					let normalized = (distance / ((ASRC_TAPS as f64 - 1.0) / 2.0)).clamp(-1.0, 1.0);
					let window =
						i0(ASRC_BETA * (1.0 - normalized * normalized).sqrt()) / normalization;
					*coefficient = sinc * window;
					sum += *coefficient;
				}
				for coefficient in &mut coefficients {
					*coefficient /= sum;
				}
				coefficients
			})
			.collect()
	})
}

#[derive(Clone, Copy, Debug)]
struct CaptureClockMatcher {
	filtered_buffer_frames: f64,
	integral: f64,
	input_samples_per_output: f64,
}

impl Default for CaptureClockMatcher {
	fn default() -> Self {
		Self {
			filtered_buffer_frames: TARGET_BUFFER_FRAMES,
			integral: 0.0,
			input_samples_per_output: 1.0,
		}
	}
}

impl CaptureClockMatcher {
	fn update(&mut self, buffered_frames: f64) -> f64 {
		self.filtered_buffer_frames +=
			DRIFT_FILTER_ALPHA * (buffered_frames - self.filtered_buffer_frames);
		let offset = self.filtered_buffer_frames - TARGET_BUFFER_FRAMES;
		let error = offset.signum() * (offset.abs() - DRIFT_DEADBAND).max(0.0);
		self.integral = (self.integral + error * DRIFT_INTEGRAL_GAIN)
			.clamp(-MAX_DRIFT_CORRECTION, MAX_DRIFT_CORRECTION);
		let wanted =
			(error * DRIFT_GAIN + self.integral).clamp(-MAX_DRIFT_CORRECTION, MAX_DRIFT_CORRECTION);
		let current = self.input_samples_per_output - 1.0;
		let slew = if offset.abs() > DRIFT_URGENT_FRAMES {
			DRIFT_SLEW * 10.0
		} else {
			DRIFT_SLEW
		};
		let correction = current + (wanted - current).clamp(-slew, slew);
		self.input_samples_per_output = 1.0 + correction;
		self.input_samples_per_output
	}

	fn reset(&mut self) {
		*self = Self::default();
	}
}

pub(crate) struct CapturePacer {
	buffered: VecDeque<CaptureFrame>,
	primed: bool,
	/// Fractional source-sample position relative to the oldest buffered frame.
	input_phase: f64,
	clock_matcher: CaptureClockMatcher,
	history: VecDeque<StereoSample>,
	/// Set when buffered audio was flushed, so the next frame is not contiguous.
	discontinuity: bool,
}

impl Default for CapturePacer {
	fn default() -> Self {
		Self {
			buffered: VecDeque::new(),
			primed: false,
			input_phase: 0.0,
			clock_matcher: CaptureClockMatcher::default(),
			history: VecDeque::with_capacity(ASRC_TAPS),
			discontinuity: false,
		}
	}
}

/// At most one frame to encode in one 20 ms tick.
#[derive(Default)]
pub(crate) struct Batch {
	frame: Option<CaptureFrame>,
}

impl Batch {
	pub fn is_empty(&self) -> bool {
		self.frame.is_none()
	}
	pub fn last(&self) -> Option<&CaptureFrame> {
		self.frame.as_ref()
	}
	pub fn into_frames(self) -> impl Iterator<Item = CaptureFrame> {
		self.frame.into_iter()
	}
}

impl CapturePacer {
	/// Local detection while alone: consume audio without retaining it for transmission.
	pub fn preview(&mut self, input: &Receiver<CaptureFrame>) -> Option<CaptureFrame> {
		self.flush();
		let mut latest = None;
		for _ in 0..MAX {
			let Ok(frame) = input.try_recv() else {
				break;
			};
			latest = Some(frame);
		}
		latest
	}

	fn flush(&mut self) {
		if !self.buffered.is_empty() || self.primed {
			self.discontinuity = true;
		}
		self.buffered.clear();
		self.primed = false;
		self.input_phase = 0.0;
		self.clock_matcher.reset();
		self.history.clear();
	}

	fn sample_at(&self, index: isize) -> StereoSample {
		if index < 0 {
			return self
				.history
				.get((self.history.len() as isize + index).max(0) as usize)
				.copied()
				.unwrap_or([0.0; 2]);
		}
		let index = index as usize;
		let Some(frame) = self.buffered.get(index / SAMPLES_PER_FRAME) else {
			return [0.0; 2];
		};
		let CaptureFrame::Stereo(samples) = frame;
		let offset = (index % SAMPLES_PER_FRAME) * 2;
		[f64::from(samples[offset]), f64::from(samples[offset + 1])]
	}

	fn remember_frame(&mut self, frame: CaptureFrame) {
		let CaptureFrame::Stereo(samples) = frame;
		for pair in samples
			.as_chunks::<2>()
			.0
			.iter()
			.rev()
			.take(ASRC_TAPS)
			.rev()
		{
			if self.history.len() == ASRC_TAPS {
				self.history.pop_front();
			}
			self.history
				.push_back([f64::from(pair[0]), f64::from(pair[1])]);
		}
	}

	fn resample_packet(&mut self, output: &mut [f32; 1_920], step: f64) -> bool {
		let available_samples = self.buffered.len() * SAMPLES_PER_FRAME;
		let coefficients = asrc_coefficients();
		let maximum_step =
			(available_samples as f64 - 1.0 - self.input_phase) / (SAMPLES_PER_FRAME - 1) as f64;
		if maximum_step < 0.999 {
			return false;
		}
		let step = step.min(maximum_step);
		for output_index in 0..SAMPLES_PER_FRAME {
			let source_position = self.input_phase + output_index as f64 * step;
			let base = source_position.floor() as isize;
			let fraction = source_position - base as f64;
			let coefficient_position = fraction * ASRC_PHASES as f64;
			let phase = (coefficient_position.floor() as usize).min(ASRC_PHASES - 1);
			let blend = coefficient_position - phase as f64;
			let mut left = 0.0;
			let mut right = 0.0;
			for (tap, (&coefficient_start, &coefficient_end)) in coefficients[phase]
				.iter()
				.zip(coefficients[phase + 1].iter())
				.enumerate()
			{
				let coefficient = coefficient_start * (1.0 - blend) + coefficient_end * blend;
				let offset = tap as isize - (ASRC_TAPS as isize - 1);
				let sample = self.sample_at(base + offset);
				left += coefficient * sample[0];
				right += coefficient * sample[1];
			}
			output[output_index * 2] = left as f32;
			output[output_index * 2 + 1] = right as f32;
		}

		self.input_phase += SAMPLES_PER_FRAME as f64 * step;
		let frames_to_pop = (self.input_phase / SAMPLES_PER_FRAME as f64).floor() as usize;
		self.input_phase -= frames_to_pop as f64 * SAMPLES_PER_FRAME as f64;
		for _ in 0..frames_to_pop {
			if let Some(frame) = self.buffered.pop_front() {
				self.remember_frame(frame);
			}
		}
		true
	}

	pub fn next(&mut self, input: &Receiver<CaptureFrame>, enabled: bool, stalled: bool) -> Batch {
		if !enabled || stalled {
			self.flush();
			for _ in 0..MAX {
				if input.try_recv().is_err() {
					break;
				}
			}
			return Batch::default();
		}
		// Drain the channel every tick so the producer never sees it full.
		for _ in 0..MAX * 2 {
			let Ok(frame) = input.try_recv() else {
				break;
			};
			if self.buffered.len() == MAX {
				if let Some(frame) = self.buffered.pop_front() {
					self.remember_frame(frame);
				}
				self.input_phase = 0.0;
				self.clock_matcher.reset();
				self.history.clear();
				self.primed = false;
				self.discontinuity = true;
			}
			self.buffered.push_back(frame);
		}
		if !self.primed {
			if self.buffered.len() < PRIME {
				return Batch::default();
			}
			self.primed = true;
			let CaptureFrame::Stereo(first) = self.buffered[0];
			for _ in 0..ASRC_TAPS {
				self.history
					.push_back([f64::from(first[0]), f64::from(first[1])]);
			}
			self.clock_matcher.filtered_buffer_frames =
				(self.buffered.len() as f64 - self.input_phase / SAMPLES_PER_FRAME as f64).max(0.0);
		}
		let buffered_frames =
			self.buffered.len() as f64 - self.input_phase / SAMPLES_PER_FRAME as f64;
		let step = self.clock_matcher.update(buffered_frames);
		let mut output = [0.0; 1_920];
		if !self.resample_packet(&mut output, step) {
			// Underrun: re-prime; the next frames are still contiguous audio.
			self.primed = false;
			return Batch::default();
		}
		Batch {
			frame: Some(CaptureFrame::Stereo(output)),
		}
	}

	/// Returns and clears whether audio was flushed or discarded since the last call.
	pub fn take_discontinuity(&mut self) -> bool {
		std::mem::take(&mut self.discontinuity)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::mpsc::sync_channel;

	fn frame(value: f32) -> CaptureFrame {
		CaptureFrame::Stereo([value; 1_920])
	}
	fn first(batch: &Batch) -> f32 {
		let Some(CaptureFrame::Stereo(samples)) = batch.frame else {
			panic!("empty batch");
		};
		samples[0]
	}
	fn last(batch: &Batch) -> f32 {
		let Some(CaptureFrame::Stereo(samples)) = batch.frame else {
			panic!("empty batch");
		};
		samples[1_918]
	}

	#[test]
	fn primes_then_sends_in_order() {
		let (send, receive) = sync_channel(8);
		let mut pacer = CapturePacer::default();
		send.send(frame(1.0)).unwrap();
		assert!(pacer.next(&receive, true, false).is_empty());
		send.send(frame(2.0)).unwrap();
		assert!(pacer.next(&receive, true, false).is_empty());
		send.send(frame(3.0)).unwrap();
		let batch = pacer.next(&receive, true, false);
		assert_eq!(first(&batch), 1.0);
		send.send(frame(4.0)).unwrap();
		let batch = pacer.next(&receive, true, false);
		assert!(first(&batch) > 0.99 && first(&batch) < 1.01);
		assert!(last(&batch) > 1.99, "the new frame arrives in order");
	}

	#[test]
	fn capture_clock_drift_never_bursts_or_blocks_capture() {
		// A deliberately 12% fast producer must never cause packet bursts or block capture.
		let (send, receive) = sync_channel(8);
		let mut pacer = CapturePacer::default();
		let mut overflows = 0;
		let mut sent = 0;
		let mut produced = 0;
		for tick in 0..1_500 {
			let count = (if tick % 10 == 0 { 2 } else { 1 }) + usize::from(tick % 50 == 0);
			for _ in 0..count {
				produced += 1;
				if send.try_send(frame(0.1)).is_err() {
					overflows += 1;
				}
			}
			let batch = pacer.next(&receive, true, false);
			let count = batch.into_frames().count();
			assert!(count <= 1, "packet pacing never bursts frames");
			sent += count;
			assert!(pacer.buffered.len() <= MAX, "latency must stay bounded");
		}
		assert_eq!(overflows, 0);
		assert!(
			pacer.take_discontinuity(),
			"persistent clock mismatch is visible"
		);
		assert!(sent <= 1_500);
		assert!(produced > sent);
	}

	#[test]
	fn sample_rate_matcher_tracks_both_clock_drift_directions_without_slips() {
		for drift in [-0.001, -0.000_2, 0.000_2, 0.001] {
			let mut matcher = CaptureClockMatcher::default();
			let mut buffered_frames = TARGET_BUFFER_FRAMES;
			let (mut minimum, mut maximum) = (buffered_frames, buffered_frames);
			let mut last_step = 1.0;
			for _ in 0..30_000 {
				buffered_frames += 1.0 + drift;
				last_step = matcher.update(buffered_frames);
				buffered_frames -= last_step;
				minimum = minimum.min(buffered_frames);
				maximum = maximum.max(buffered_frames);
			}
			// The correction waits out the jitter deadband and moves slowly, so the cushion
			// swings further than one frame but stays well inside the queue.
			assert!(minimum > 0.5, "buffer underflowed at drift {drift}");
			assert!(maximum < (MAX - 2) as f64, "buffer grew at drift {drift}");
			// At the full correction range the cushion stops drifting but keeps its offset.
			// Occupancy is measured before the tick's frame is taken, as the pacer does.
			if drift.abs() < MAX_DRIFT_CORRECTION {
				let measured = buffered_frames + last_step;
				assert!(
					(measured - TARGET_BUFFER_FRAMES).abs() <= DRIFT_DEADBAND + 0.25,
					"settled near the target at drift {drift}: {measured}"
				);
			}
			if drift > 0.0 {
				assert!(last_step > 1.0, "fast input must be consumed faster");
			} else {
				assert!(last_step < 1.0, "slow input must be consumed slower");
			}
			assert!((last_step - 1.0).abs() <= MAX_DRIFT_CORRECTION + 1e-12);
		}
	}

	#[test]
	fn callback_jitter_alone_leaves_the_rate_untouched() {
		// Callbacks landing either side of the tick make occupancy jump by a whole frame;
		// with no clock drift the audio must pass at exactly its own rate.
		let mut matcher = CaptureClockMatcher::default();
		let mut seed = 1u64;
		for _ in 0..50_000 {
			seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
			let jump = ((seed >> 62) as f64) - 1.5; // -1.5, -0.5, 0.5 or 1.5 frames
			assert_eq!(matcher.update(TARGET_BUFFER_FRAMES + jump * 0.6), 1.0);
		}
	}

	#[test]
	fn the_rate_correction_never_changes_quickly() {
		let mut matcher = CaptureClockMatcher::default();
		let mut previous = 1.0;
		for tick in 0..20_000 {
			// A large, sudden occupancy error: the step still moves at most the slew limit.
			let level = if tick < 10_000 {
				TARGET_BUFFER_FRAMES + 4.0
			} else {
				0.5
			};
			let step = matcher.update(level);
			assert!((step - previous).abs() <= DRIFT_SLEW * 10.0 + 1e-12);
			previous = step;
		}
	}

	#[test]
	fn asynchronous_interpolator_stays_flat_through_20khz() {
		let coefficients = asrc_coefficients();
		let omega = std::f64::consts::TAU * 20_000.0 / 48_000.0;
		let mut worst_gain_db: f64 = 0.0;
		for phase in coefficients.iter().step_by(32) {
			let (mut real, mut imaginary) = (0.0, 0.0);
			for (tap, coefficient) in phase.iter().enumerate() {
				let offset = tap as isize - (ASRC_TAPS as isize - 1);
				real += coefficient * (omega * offset as f64).cos();
				imaginary -= coefficient * (omega * offset as f64).sin();
			}
			let gain_db = 20.0 * real.hypot(imaginary).log10();
			worst_gain_db = worst_gain_db.max(gain_db.abs());
		}
		assert!(
			worst_gain_db < 0.05,
			"20 kHz ASRC ripple {worst_gain_db:.4} dB"
		);
	}

	#[test]
	fn late_callback_holds_without_losing_audio() {
		let (send, receive) = sync_channel(8);
		let mut pacer = CapturePacer::default();
		let mut sent = 0;
		for tick in 0..200 {
			// Two frames arrive together every other tick (bursty callbacks).
			if tick % 2 == 0 {
				send.send(frame(0.2)).unwrap();
				send.send(frame(0.2)).unwrap();
			}
			sent += pacer.next(&receive, true, false).into_frames().count();
		}
		assert!(sent >= 198);
		assert!(!pacer.take_discontinuity());
	}

	#[test]
	fn mute_and_stall_flush_and_mark_discontinuity() {
		let (send, receive) = sync_channel(8);
		let mut pacer = CapturePacer::default();
		for _ in 0..3 {
			send.send(frame(0.3)).unwrap();
		}
		assert!(!pacer.next(&receive, true, false).is_empty());
		assert!(pacer.next(&receive, false, false).is_empty());
		assert_eq!(pacer.buffered.len(), 0);
		assert!(pacer.take_discontinuity());
		send.send(frame(0.3)).unwrap();
		assert!(pacer.next(&receive, true, true).is_empty());
		assert!(receive.try_recv().is_err());
	}
}
