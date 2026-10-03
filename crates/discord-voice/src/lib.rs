//! Discord DM and guild voice media. No bot manager, relay, recording, or key persistence.
mod activity;
pub mod audio;
pub mod camera;
mod capture;
mod crypto;
mod diagnostics;
mod jitter;
mod mixer;
pub mod resample;
pub mod screen;
mod stream_playout;
mod timer;
mod transport;
mod video;
// Linux has no shared hardware encoder, but the camera's GStreamer encoder still takes the
// same configuration, so the facade is compiled on every supported platform.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
mod video_encode;
mod video_receive;
mod video_sps;
pub use crypto::Identity;
pub use transport::{run, run_stream, run_with_identity, watch_stream};
pub use video_receive::{RemoteFrame, VideoSink};
pub mod camera_video;

pub type MonoFrame = [f32; 960];
pub type Frame = [f32; 1_920];
pub type StereoFrame = Frame;

/// Mic payload before Opus. Capture, optional RNNoise and the stereo Opus encoder
/// keep independent left/right channels end to end.
#[derive(Clone, Copy)]
pub enum CaptureFrame {
	Stereo(StereoFrame),
}

impl CaptureFrame {
	pub fn mono_preview(&self) -> MonoFrame {
		match self {
			Self::Stereo(samples) => {
				std::array::from_fn(|i| (samples[i * 2] + samples[i * 2 + 1]) * 0.5)
			}
		}
	}

	pub fn energy(&self) -> f32 {
		match self {
			Self::Stereo(samples) => samples
				.as_chunks::<2>()
				.0
				.iter()
				.map(|pair| {
					let left = if pair[0].is_finite() { pair[0] } else { 0.0 };
					let right = if pair[1].is_finite() { pair[1] } else { 0.0 };
					(left * left + right * right) * 0.5
				})
				.sum(),
		}
	}
}
#[derive(Clone, Copy)]
pub struct Controls {
	pub muted: bool,
	/// Acheron-compatible encoder settings applied on the next 20 ms voice tick.
	pub opus: model::voice_settings::OpusSettings,
	pub noise_suppression: bool,
	pub rnnoise_vad: bool,
	pub vad_threshold_rms: u16,
	/// Local indicator threshold; independent of received participants.
	pub activity_threshold_db: i16,
	/// Zero means off; a new value invalidates frames from the previous camera instance.
	pub camera: u64,
	/// Preferred Spout wire codec; normal webcams continue to use H.264.
	pub camera_codec: model::CameraCodec,
	/// H.265 encode is available only on the Spout GPU path.
	pub camera_spout: bool,
	pub deafened: bool,
	/// Session-only playback percentages (0–200); zero user IDs are unused.
	pub user_volumes: [(u64, u16); 64],
	/// Watched stream playback percentage, independently muted with zero.
	pub stream_volume: u16,
}
impl Default for Controls {
	fn default() -> Self {
		Self {
			muted: false,
			opus: model::voice_settings::OpusSettings::default(),
			noise_suppression: true,
			rnnoise_vad: true,
			vad_threshold_rms: 100,
			activity_threshold_db: -45,
			camera: 0,
			camera_codec: model::CameraCodec::H264,
			camera_spout: false,
			deafened: false,
			user_volumes: [(0, 100); 64],
			stream_volume: 100,
		}
	}
}
pub enum Status {
	Connecting,
	Discovering,
	TransportReady,
	CameraAvailable(Option<model::CameraCodec>),
	Securing,
	WaitingForPeer,
	Ready {
		privacy_code: String,
	},
	RemoteAudio,
	/// The local audio RTP source assigned by the voice transport.
	AudioSenderSsrc(u32),
	/// One-second rolling audio wire-send rate, capture pacing stalls and drift catch-ups
	/// (ticks that sent two frames to drain a faster capture clock).
	AudioSendStats {
		bitrate_bps: u32,
		packets_per_second: u16,
		pacing_stalls: u16,
		catchups: u16,
		/// Largest interval between consecutive audio packet sends in the window, in ms.
		max_send_gap_ms: u16,
	},
	/// Latest active user IDs, zero-padded to the 64-participant limit.
	Speaking(Box<[u64; 64]>),
}

#[cfg(test)]
mod test_mls;

// Exercise the exact vendored SHAKE adapter, without enabling unused HPKE backends.
#[cfg(test)]
#[path = "../../../vendor/hpke-rs/src/serein_sha3.rs"]
mod hpke_sha3;
