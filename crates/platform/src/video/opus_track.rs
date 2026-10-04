//! Opus audio from MPEG-4 attachments. Media Foundation on Windows 10 has no Opus decoder,
//! so these tracks are demuxed by `mp4` and decoded with libopus, while Media Foundation
//! still decodes the picture.
use super::{INVALID, ReadSeek, Sample, mp4};
use std::{
	io::{Read, Seek, SeekFrom},
	sync::{Arc, Mutex},
};

const RATE: f64 = 48_000.0;
/// Opus needs this much decoded audio before a seek target to converge.
const PRE_ROLL: f64 = 0.08;
/// One packet decodes to at most 120 ms of stereo audio.
const MAX_FRAMES: usize = 5_760;
/// The largest Opus packet: 120 ms of frames at their 1275-byte maximum, with framing.
const MAX_PACKET: u32 = 6 * 1_275 + 16;

pub(super) struct OpusTrack {
	source: Arc<Mutex<Box<dyn ReadSeek>>>,
	samples: Vec<mp4::SampleEntry>,
	timescale: f64,
	pre_skip: usize,
	next: usize,
	/// Decoded samples still to drop: the encoder delay at the start, or pre-roll after a seek.
	skip: usize,
	/// After a seek, audio before this time is decoded only to prime the decoder.
	from: f64,
	decoder: opus2::Decoder,
	pcm: Vec<f32>,
}

impl OpusTrack {
	/// The Opus track of an MPEG-4 file, or `None` when it has no such track.
	pub fn open(
		source: &Arc<Mutex<Box<dyn ReadSeek>>>,
		length: u64,
	) -> Result<Option<Self>, &'static str> {
		let movie = {
			let mut stream = source.lock().map_err(|_| INVALID)?;
			mp4::parse(stream.as_mut(), length)
		};
		let Some(audio) = movie.ok().and_then(|movie| movie.audio) else {
			return Ok(None);
		};
		let mp4::AudioCodec::Opus { pre_skip } = audio.track.codec else {
			return Ok(None);
		};
		Ok(Some(Self {
			source: source.clone(),
			samples: audio.track.samples,
			timescale: f64::from(audio.track.timescale.max(1)),
			pre_skip: usize::from(pre_skip),
			next: 0,
			skip: usize::from(pre_skip),
			from: 0.0,
			decoder: opus2::Decoder::new(48_000, opus2::Channels::Stereo).map_err(|_| INVALID)?,
			pcm: vec![0.0; MAX_FRAMES * 2],
		}))
	}

	pub fn read(&mut self) -> Result<Option<Sample>, &'static str> {
		while let Some(entry) = self.samples.get(self.next).copied() {
			self.next += 1;
			if entry.size == 0 || entry.size > MAX_PACKET {
				return Err(INVALID);
			}
			let mut packet = vec![0; entry.size as usize];
			{
				let mut stream = self.source.lock().map_err(|_| INVALID)?;
				stream
					.seek(SeekFrom::Start(entry.offset))
					.and_then(|_| stream.read_exact(&mut packet))
					.map_err(|_| INVALID)?;
			}
			let count = self
				.decoder
				.decode_float(&packet, &mut self.pcm, false)
				.map_err(|_| INVALID)?;
			let start = entry.pts as f64 / self.timescale;
			let mut dropped = self.skip.min(count);
			self.skip -= dropped;
			if start < self.from {
				let early = ((self.from - start) * RATE).ceil() as usize;
				dropped = dropped.max(early.min(count));
			}
			if dropped == count {
				continue;
			}
			let frames = self.pcm[dropped * 2..count * 2]
				.as_chunks::<2>()
				.0
				.iter()
				.map(|frame| {
					frame.map(|sample| {
						if sample.is_finite() {
							sample.clamp(-1.0, 1.0)
						} else {
							0.0
						}
					})
				})
				.collect();
			return Ok(Some(Sample::Audio {
				pts: start + dropped as f64 / RATE,
				frames,
			}));
		}
		Ok(None)
	}

	pub fn seek(&mut self, seconds: f64) {
		let primed = (seconds - PRE_ROLL).max(0.0);
		self.next = self
			.samples
			.partition_point(|entry| (entry.pts as f64 / self.timescale) < primed);
		let _ = self.decoder.reset_state();
		self.skip = if self.next == 0 { self.pre_skip } else { 0 };
		self.from = seconds;
	}
}
