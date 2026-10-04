//! Adaptive per-speaker packet buffer: a 40 ms startup cushion that grows by one packet
//! interval whenever a packet arrives after its playout time (up to 160 ms) and shrinks after
//! 4 s of playout without one.
//!
//! Discord clients stop sending between phrases without skipping sequence numbers, so silence
//! is not loss: a packet that continues the last one received starts a new talk spurt instead
//! of being discarded as late, and pauses never widen the cushion.
const SLOTS: usize = 8;
/// Concealment covers short losses; longer gaps play silence instead of synthesized noise.
const CONCEALED: u8 = 3;
/// After this many empty ticks (500 ms) the speaker is idle and playout restarts on arrival.
const IDLE: u8 = 25;
const SHRINK_AFTER: u16 = 200;

pub(crate) struct Jitter {
	packets: Vec<(u16, Vec<u8>)>,
	next: Option<u16>,
	last_received: Option<u16>,
	/// Bit `n` is set when `last_received - n` arrived.
	received: u64,
	target: u8,
	warmup: u8,
	good: u16,
	consecutive_misses: u8,
}
impl Default for Jitter {
	fn default() -> Self {
		Self {
			packets: Vec::with_capacity(SLOTS),
			next: None,
			last_received: None,
			received: 0,
			target: 3,
			warmup: 0,
			good: 0,
			consecutive_misses: 0,
		}
	}
}
impl Jitter {
	pub fn clear(&mut self) {
		*self = Self::default();
	}

	/// Restart playout at `sequence` with the current cushion, keeping what was learned.
	fn restart(&mut self, sequence: u16) {
		self.packets.clear();
		self.next = Some(sequence);
		self.warmup = self.target - 1;
		self.consecutive_misses = 0;
	}

	pub fn push(&mut self, sequence: u16, opus: Vec<u8>) {
		if opus.len() > 1275 {
			return;
		}
		let Some(next) = self.next else {
			self.restart(sequence);
			return self.insert(sequence, opus);
		};
		let distance = sequence.wrapping_sub(next);
		let behind = next.wrapping_sub(sequence);
		if distance >= 32768 && behind <= 64 {
			if self
				.last_received
				.is_some_and(|last| last.wrapping_add(1) == sequence)
			{
				// The sender paused between phrases and resumed: nothing was lost.
				self.restart(sequence);
				return self.insert(sequence, opus);
			}
			if !self.seen(sequence) {
				// It arrived after its playout time: the cushion is too small. One step per
				// burst, so several packets held up together widen it once.
				if self.good > 0 {
					self.target = (self.target + 1).min(SLOTS as u8);
					self.good = 0;
				}
				self.mark(sequence);
			}
			return;
		}
		if distance >= SLOTS as u16 {
			// Far ahead of playout, after a stall or a sender restart.
			self.restart(sequence);
		}
		self.insert(sequence, opus);
	}

	fn seen(&self, sequence: u16) -> bool {
		self.last_received.is_some_and(|last| {
			let age = last.wrapping_sub(sequence);
			age < 64 && self.received & (1 << age) != 0
		})
	}

	fn mark(&mut self, sequence: u16) {
		let Some(last) = self.last_received else {
			self.last_received = Some(sequence);
			self.received = 1;
			return;
		};
		let ahead = sequence.wrapping_sub(last);
		if ahead != 0 && ahead < 32768 {
			self.received = self.received.checked_shl(u32::from(ahead)).unwrap_or(0) | 1;
			self.last_received = Some(sequence);
		} else if let Some(bit) = 1u64.checked_shl(u32::from(last.wrapping_sub(sequence))) {
			self.received |= bit;
		}
	}

	fn insert(&mut self, sequence: u16, opus: Vec<u8>) {
		self.mark(sequence);
		if self.packets.len() < SLOTS && !self.packets.iter().any(|(id, _)| *id == sequence) {
			self.packets.push((sequence, opus));
		}
	}

	/// An empty packet requests Opus packet-loss concealment, at most three consecutive packets.
	pub fn pop(&mut self) -> Option<Vec<u8>> {
		if self.warmup > 0 {
			// Short packets can fill the window before the cushion elapses. Start
			// then, rather than letting the next arrival reset a full window.
			if self.packets.len() < SLOTS {
				self.warmup -= 1;
				return None;
			}
			self.warmup = 0;
		}
		let next = self.next?;
		self.next = Some(next.wrapping_add(1));
		if let Some(index) = self.packets.iter().position(|(id, _)| *id == next) {
			self.consecutive_misses = 0;
			self.good = self.good.saturating_add(1);
			if self.good >= SHRINK_AFTER && self.target > 2 {
				self.target -= 1;
				self.good = 0;
			}
			return Some(self.packets.swap_remove(index).1);
		}
		self.consecutive_misses = self.consecutive_misses.saturating_add(1);
		if self.consecutive_misses >= IDLE {
			self.packets.clear();
			self.next = None;
			self.consecutive_misses = 0;
			return None;
		}
		(self.consecutive_misses <= CONCEALED).then(Vec::new)
	}
}
#[cfg(test)]
mod tests {
	use super::*;

	/// Plays `ticks` 20 ms ticks, returning the packets heard (empty = concealment).
	fn play(jitter: &mut Jitter, ticks: usize) -> Vec<Vec<u8>> {
		(0..ticks).filter_map(|_| jitter.pop()).collect()
	}

	#[test]
	fn bounded_reordering_loss_duplicates_and_wrap() {
		let mut jitter = Jitter::default();
		jitter.push(u16::MAX, vec![1]);
		jitter.push(1, vec![3]);
		jitter.push(0, vec![2]);
		jitter.push(0, vec![9]);
		assert!(jitter.pop().is_none());
		assert!(jitter.pop().is_none());
		assert_eq!(jitter.pop(), Some(vec![1]));
		assert_eq!(jitter.pop(), Some(vec![2]));
		assert_eq!(jitter.pop(), Some(vec![3]));
		for _ in 0..3 {
			assert_eq!(jitter.pop(), Some(vec![]));
		}
		assert!(jitter.pop().is_none());
		assert!(jitter.pop().is_none());
		for sequence in 0..1000 {
			jitter.push(sequence, vec![1; 1275]);
			assert!(jitter.packets.len() <= SLOTS);
		}
		jitter.push(1000, vec![0; 1276]);
		assert!(jitter.packets.len() <= SLOTS);
	}

	#[test]
	fn a_pause_between_phrases_keeps_the_next_phrase_and_the_latency() {
		for pause in [3, 5, 10, 15, 24, 30, 100] {
			let mut jitter = Jitter::default();
			let mut sequence = 0u16;
			let mut heard = Vec::new();
			// Senders transmit nothing while silent, then continue the same sequence.
			for phrase in 0..4u8 {
				for _ in 0..50 {
					jitter.push(sequence, vec![phrase + 1]);
					sequence = sequence.wrapping_add(1);
					heard.extend(play(&mut jitter, 1));
				}
				heard.extend(play(&mut jitter, pause));
			}
			heard.extend(play(&mut jitter, 20));
			let audible: Vec<_> = heard.iter().filter(|packet| !packet.is_empty()).collect();
			assert_eq!(audible.len(), 200, "pause of {pause} ticks lost speech");
			assert!(
				jitter.target <= 3,
				"pause of {pause} ticks widened the cushion"
			);
		}
	}

	#[test]
	fn late_packets_widen_the_cushion_and_clean_playout_narrows_it() {
		let mut jitter = Jitter::default();
		for sequence in 0..10u16 {
			jitter.push(sequence, vec![1]);
			play(&mut jitter, 1);
		}
		// 10 and 11 are held up by the network past their playout time; 12 is on time.
		play(&mut jitter, 4);
		jitter.push(12, vec![1]);
		jitter.push(10, vec![1]);
		jitter.push(11, vec![1]);
		assert_eq!(jitter.target, 4);
		let mut sequence = 13u16;
		for _ in 0..(SHRINK_AFTER as usize + 10) {
			jitter.push(sequence, vec![1]);
			sequence += 1;
			play(&mut jitter, 1);
		}
		assert_eq!(jitter.target, 3);
	}

	#[test]
	fn a_sender_restart_or_long_stall_restarts_playout() {
		let mut jitter = Jitter::default();
		for sequence in 0..5u16 {
			jitter.push(sequence, vec![1]);
			play(&mut jitter, 1);
		}
		jitter.push(1_000, vec![2]);
		let heard = play(&mut jitter, 5);
		assert!(heard.contains(&vec![2]));
	}
}
