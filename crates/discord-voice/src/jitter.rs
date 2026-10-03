//! Adaptive per-speaker packet buffer: a 40 ms startup cushion that grows by one packet
//! interval after each loss burst (up to 160 ms) and shrinks after 4 s of clean playout.
const SLOTS: usize = 8;
pub(crate) struct Jitter {
	packets: Vec<(u16, Vec<u8>)>,
	next: Option<u16>,
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
	pub fn push(&mut self, sequence: u16, opus: Vec<u8>) {
		if opus.len() > 1275 {
			return;
		}
		if self.next.is_none() {
			self.next = Some(sequence);
			self.warmup = self.target - 1;
		}
		let distance = sequence.wrapping_sub(self.next.unwrap());
		if distance >= 32768 {
			return;
		}
		if distance >= SLOTS as u16 {
			self.packets.clear();
			self.next = Some(sequence);
			self.warmup = self.target - 1;
			self.good = 0;
			self.consecutive_misses = 0;
		}
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
			if self.good >= 200 && self.target > 2 {
				self.target -= 1;
				self.good = 0;
			}
			return Some(self.packets.swap_remove(index).1);
		}
		self.consecutive_misses = self.consecutive_misses.saturating_add(1);
		self.good = 0;
		if self.consecutive_misses >= 25 {
			self.clear();
			return None;
		}
		if self.consecutive_misses > 3 {
			// Concealing longer gaps only synthesizes noise; play silence instead.
			return None;
		}
		if self.consecutive_misses == 3 {
			// A loss burst: widen the cushion and rebuffer before the next packet.
			self.target = (self.target + 1).min(SLOTS as u8);
			self.warmup = self.target - 1;
		}
		Some(Vec::new())
	}
}
#[cfg(test)]
mod tests {
	use super::*;
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
}
