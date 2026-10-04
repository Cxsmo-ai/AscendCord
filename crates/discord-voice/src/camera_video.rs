//! Outgoing camera RTP. Unofficial Discord video signaling; live interoperability is unverified.
//! Signaling reference: https://github.com/dank074/Discord-video-stream/blob/master/src/client/voice/BaseMediaConnection.ts
//! H264 packetization follows RFC 6184 section 5 (single NAL and FU-A).
use crate::crypto::Encryption;
use serde_json::{Value, json};
use std::collections::VecDeque;

pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const PAYLOAD: usize = 1100;
const MAX_PACKETS_PER_FRAME: usize = 2048;

pub struct Frame {
	pub generation: u64,
	/// Index assigned by the camera encoder before bounded queues can drop a picture.
	pub index: u64,
	pub timestamp: u32,
	/// Inter pictures are only safe after every preceding picture has been delivered.
	pub keyframe: bool,
	pub width: u32,
	pub height: u32,
	pub frames_per_second: u8,
	pub bitrate_kbps: u16,
	pub codec: model::CameraCodec,
	pub data: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct Sender {
	ssrc: u32,
	rtx: u32,
	sequence: u16,
	pub negotiated: bool,
	pub negotiated_codec: Option<model::CameraCodec>,
	pub generation: u64,
	pub announced: bool,
	width: u32,
	height: u32,
	frames_per_second: u8,
	bitrate_kbps: u16,
	packets: VecDeque<Vec<u8>>,
	next_frame: Option<u64>,
	awaiting_keyframe: bool,
}
impl Sender {
	/// Accept only a continuous prediction chain. A gap requires an IDR before
	/// this sender can resume; camera capture is asked to produce one immediately.
	pub fn accept_frame(&mut self, index: u64, keyframe: bool) -> bool {
		if self.next_frame.is_none_or(|next| next != index) {
			self.awaiting_keyframe = true;
		}
		self.next_frame = Some(index.wrapping_add(1));
		if self.awaiting_keyframe && !keyframe {
			return false;
		}
		if keyframe {
			self.awaiting_keyframe = false;
		}
		true
	}
	pub fn require_keyframe(&mut self) {
		self.awaiting_keyframe = true;
	}
	pub fn configure(&mut self, data: &Value, audio: u32) -> bool {
		// Request one stream and accept only that exact assignment, never guessed SSRCs.
		let Some(stream) = data["streams"]
			.as_array()
			.filter(|s| s.len() <= 4)
			.and_then(|s| s.iter().find(|s| s["type"] == "video" && s["rid"] == "100"))
		else {
			return false;
		};
		let ssrc = stream["ssrc"]
			.as_u64()
			.and_then(|s| u32::try_from(s).ok())
			.unwrap_or(0);
		let rtx = stream["rtx_ssrc"]
			.as_u64()
			.and_then(|s| u32::try_from(s).ok())
			.unwrap_or(0);
		if ssrc != 0 && rtx != 0 && ssrc != audio && rtx != audio && ssrc != rtx {
			self.ssrc = ssrc;
			self.rtx = rtx;
			return true;
		}
		false
	}
	pub fn available(&self) -> bool {
		self.negotiated && self.negotiated_codec.is_some() && self.ssrc != 0
	}
	pub fn announcement(&self, audio: u32, enabled: bool) -> Value {
		json!({"op":12,"d":{"audio_ssrc":audio,"video_ssrc":if enabled {self.ssrc} else {0},"rtx_ssrc":if enabled {self.rtx} else {0},"streams":if enabled {vec![json!({"type":"video","rid":"100","ssrc":self.ssrc,"rtx_ssrc":self.rtx,"active":true,"quality":100,"max_bitrate":u32::from(self.bitrate_kbps)*1000,"max_framerate":self.frames_per_second,"max_resolution":{"type":"fixed","width":self.width,"height":self.height}})]}else{vec![]}}})
	}
	pub fn set_quality(&mut self, frame: &Frame) -> bool {
		let changed = (
			self.width,
			self.height,
			self.frames_per_second,
			self.bitrate_kbps,
		) != (
			frame.width,
			frame.height,
			frame.frames_per_second,
			frame.bitrate_kbps,
		);
		self.width = frame.width;
		self.height = frame.height;
		self.frames_per_second = frame.frames_per_second;
		self.bitrate_kbps = frame.bitrate_kbps;
		changed
	}
	pub fn clear(&mut self) {
		self.packets.clear();
		self.next_frame = None;
		self.awaiting_keyframe = true;
	}
	pub fn reset(&mut self) {
		self.clear();
	}
	pub fn is_empty(&self) -> bool {
		self.packets.is_empty()
	}
	pub fn next(&mut self) -> Option<Vec<u8>> {
		self.packets.pop_front()
	}
	pub fn packetize(
		&mut self,
		frame: &[u8],
		timestamp: u32,
		codec: model::CameraCodec,
		encryption: &mut Encryption,
	) -> Result<(), &'static str> {
		if frame.len() > MAX_FRAME_BYTES + 1024 || !self.packets.is_empty() {
			return Err("Camera frame exceeds the media budget");
		}
		// Use the same DAVE-aware Annex-B/RFC 6184 packetizer already used for
		// Go Live. It preserves the encrypted access-unit framing and enforces the
		// negotiated UDP MTU after transport-encryption overhead.
		if codec == model::CameraCodec::H264 {
			for packet in crate::video::packetize(frame, &mut self.sequence, timestamp, self.ssrc)?
			{
				self.packets
					.push_back(encryption.seal(&packet.header, &packet.payload)?);
			}
			return Ok(());
		}
		let nals = nal_units(frame, codec)?;
		for (index, nal) in nals.iter().enumerate() {
			let last_nal = index + 1 == nals.len();
			if nal.len() <= PAYLOAD {
				self.push(nal, timestamp, last_nal, codec, encryption)?;
			} else {
				match codec {
					model::CameraCodec::H264 => {
						let chunks = nal[1..].chunks(PAYLOAD - 2);
						let count = chunks.len();
						for (i, chunk) in chunks.enumerate() {
							let mut payload = Vec::with_capacity(chunk.len() + 2);
							payload.push((nal[0] & 0xe0) | 28);
							payload.push(
								(nal[0] & 0x1f)
									| if i == 0 { 0x80 } else { 0 }
									| if i + 1 == count { 0x40 } else { 0 },
							);
							payload.extend_from_slice(chunk);
							self.push(
								&payload,
								timestamp,
								last_nal && i + 1 == count,
								codec,
								encryption,
							)?;
						}
					}
					model::CameraCodec::H265 => {
						let nal_type = (nal[0] >> 1) & 0x3f;
						let chunks = nal[2..].chunks(PAYLOAD - 3);
						let count = chunks.len();
						for (i, chunk) in chunks.enumerate() {
							let mut payload = Vec::with_capacity(chunk.len() + 3);
							payload.push((nal[0] & 0x81) | (49 << 1));
							payload.push(nal[1]);
							payload.push(
								nal_type
									| if i == 0 { 0x80 } else { 0 }
									| if i + 1 == count { 0x40 } else { 0 },
							);
							payload.extend_from_slice(chunk);
							self.push(
								&payload,
								timestamp,
								last_nal && i + 1 == count,
								codec,
								encryption,
							)?;
						}
					}
				}
			}
		}
		Ok(())
	}
	fn push(
		&mut self,
		data: &[u8],
		timestamp: u32,
		marker: bool,
		codec: model::CameraCodec,
		encryption: &mut Encryption,
	) -> Result<(), &'static str> {
		if self.packets.len() >= MAX_PACKETS_PER_FRAME {
			return Err("Camera packet queue exceeds its budget");
		}
		let mut header = [0; 12];
		header[0] = 0x80;
		header[1] = (match codec {
			model::CameraCodec::H264 => 101,
			model::CameraCodec::H265 => 103,
		}) | if marker { 0x80 } else { 0 };
		header[2..4].copy_from_slice(&self.sequence.to_be_bytes());
		header[4..8].copy_from_slice(&timestamp.to_be_bytes());
		header[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
		self.sequence = self.sequence.wrapping_add(1);
		self.packets.push_back(encryption.seal(&header, data)?);
		Ok(())
	}
}

fn nal_units(frame: &[u8], codec: model::CameraCodec) -> Result<Vec<&[u8]>, &'static str> {
	let mut starts = Vec::new();
	let mut i = 0;
	while i + 3 <= frame.len() {
		let size = if frame[i..].starts_with(&[0, 0, 0, 1]) {
			4
		} else if frame[i..].starts_with(&[0, 0, 1]) {
			3
		} else {
			i += 1;
			continue;
		};
		if starts.len() >= 64 {
			return Err("Camera frame contains too many NAL units");
		}
		starts.push((i, i + size));
		i += size;
	}
	if starts.first().is_none_or(|s| s.0 != 0) {
		return Err("Invalid Annex B camera frame");
	}
	starts
		.iter()
		.enumerate()
		.map(|(index, &(_, start))| {
			let end = starts.get(index + 1).map_or(frame.len(), |s| s.0);
			let nal = &frame[start..end];
			match codec {
				model::CameraCodec::H264
					if !nal.is_empty() && (1..=23).contains(&(nal[0] & 0x1f)) =>
				{
					Ok(nal)
				}
				model::CameraCodec::H265
					if nal.len() >= 2
						&& nal[0] & 0x80 == 0
						&& nal[1] & 0x07 != 0
						&& (nal[0] >> 1) & 0x3f <= 47 =>
				{
					Ok(nal)
				}
				_ => Err(match codec {
					model::CameraCodec::H264 => "Invalid H264 camera NAL unit",
					model::CameraCodec::H265 => "Invalid H265 camera NAL unit",
				}),
			}
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn camera_sender_waits_for_keyframe_after_start_or_a_frame_gap() {
		let mut sender = Sender::default();
		assert!(!sender.accept_frame(0, false));
		assert!(sender.accept_frame(1, true));
		assert!(sender.accept_frame(2, false));
		assert!(!sender.accept_frame(4, false));
		assert!(!sender.accept_frame(5, false));
		assert!(sender.accept_frame(6, true));
		assert!(sender.accept_frame(7, false));
		sender.require_keyframe();
		assert!(!sender.accept_frame(8, false));
		assert!(sender.accept_frame(9, true));
	}
	#[test]
	fn camera_sender_tracks_frame_indices_across_wrap() {
		let mut sender = Sender::default();
		assert!(sender.accept_frame(u64::MAX, true));
		assert!(sender.accept_frame(0, false));
	}
	#[test]
	fn camera_packetization_is_bounded_and_marks_only_the_last_fragment() {
		let mut sender = Sender::default();
		let mut crypto = Encryption::new(&[7; 32]);
		let mut frame = vec![0, 0, 0, 1, 0x65];
		frame.extend(vec![9; 2400]);
		sender
			.packetize(&frame, 6000, model::CameraCodec::H264, &mut crypto)
			.unwrap();
		assert_eq!(sender.packets.len(), 3);
		for (i, packet) in sender.packets.iter().enumerate() {
			assert!(packet.len() <= 1200);
			assert_eq!(packet[1] & 0x80 != 0, i == 2);
			assert_eq!(&packet[4..8], &6000u32.to_be_bytes());
		}
		sender.clear();
		assert!(
			sender
				.packetize(
					&vec![0; MAX_FRAME_BYTES + 1025],
					0,
					model::CameraCodec::H264,
					&mut crypto,
				)
				.is_err()
		);
		assert!(nal_units(&[0, 0, 1], model::CameraCodec::H264).is_err());
	}
}
