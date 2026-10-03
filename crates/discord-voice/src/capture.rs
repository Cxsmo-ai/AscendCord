//! Pace callback batches into 20 ms packets without discarding normal speech.
//!
//! The capture device and the 20 ms network tick run on different clocks. Taking exactly
//! one frame per tick lets the queue creep to its cap (adding ~160 ms of latency) and then
//! drop frames, while a single late callback empties it and used to inject silence. This
//! pacer keeps a small primed cushion, sends a second frame in a tick when the cushion has
//! grown, and simply holds (no silence, no timestamp gap) through a brief underrun.
use crate::CaptureFrame;
use std::{collections::VecDeque, sync::mpsc::Receiver};

/// Frames buffered before sending starts or resumes after an underrun (40 ms).
const PRIME: usize = 2;
/// Above this many buffered frames a tick sends two frames to drain drift (80 ms).
const HIGH: usize = 4;
/// Hard bound on locally retained frames; the oldest is discarded beyond this.
const MAX: usize = 8;

#[derive(Default)]
pub(crate) struct CapturePacer {
	buffered: VecDeque<CaptureFrame>,
	primed: bool,
	/// Set when buffered audio was flushed, so the next frame is not contiguous.
	discontinuity: bool,
}

/// Up to two contiguous frames to encode in one tick.
#[derive(Default)]
pub(crate) struct Batch {
	frames: [Option<CaptureFrame>; 2],
}

impl Batch {
	pub fn is_empty(&self) -> bool {
		self.frames[0].is_none()
	}
	pub fn last(&self) -> Option<&CaptureFrame> {
		self.frames.iter().rev().find_map(Option::as_ref)
	}
	pub fn into_frames(self) -> impl Iterator<Item = CaptureFrame> {
		self.frames.into_iter().flatten()
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
				self.buffered.pop_front();
				self.discontinuity = true;
			}
			self.buffered.push_back(frame);
		}
		if !self.primed {
			if self.buffered.len() < PRIME {
				return Batch::default();
			}
			self.primed = true;
		}
		let mut batch = Batch::default();
		batch.frames[0] = self.buffered.pop_front();
		if batch.frames[0].is_none() {
			// Underrun: re-prime; the next frames are still contiguous audio.
			self.primed = false;
			return batch;
		}
		if self.buffered.len() >= HIGH {
			batch.frames[1] = self.buffered.pop_front();
		}
		batch
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
		let Some(CaptureFrame::Stereo(samples)) = batch.frames[0] else {
			panic!("empty batch");
		};
		samples[0]
	}

	#[test]
	fn primes_then_sends_in_order() {
		let (send, receive) = sync_channel(8);
		let mut pacer = CapturePacer::default();
		send.send(frame(1.0)).unwrap();
		assert!(pacer.next(&receive, true, false).is_empty());
		send.send(frame(2.0)).unwrap();
		let batch = pacer.next(&receive, true, false);
		assert_eq!(
			(batch.frames.iter().flatten().count(), first(&batch)),
			(1, 1.0)
		);
		let batch = pacer.next(&receive, true, false);
		assert_eq!(
			(batch.frames.iter().flatten().count(), first(&batch)),
			(1, 2.0)
		);
	}

	#[test]
	fn faster_capture_clock_never_overflows_the_channel() {
		// 2% fast device clock plus an extra frame every 10 ticks for 30 s.
		let (send, receive) = sync_channel(8);
		let mut pacer = CapturePacer::default();
		let mut overflows = 0;
		let mut sent = 0;
		let mut produced = 0;
		for tick in 0..1_500 {
			let count = if tick % 10 == 0 { 2 } else { 1 } + usize::from(tick % 50 == 0);
			for _ in 0..count {
				produced += 1;
				if send.try_send(frame(0.1)).is_err() {
					overflows += 1;
				}
			}
			sent += pacer.next(&receive, true, false).into_frames().count();
			assert!(
				pacer.buffered.len() <= HIGH + 1,
				"latency must stay bounded"
			);
		}
		assert_eq!(overflows, 0);
		assert!(!pacer.take_discontinuity());
		assert!(produced - sent <= HIGH + 1);
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
