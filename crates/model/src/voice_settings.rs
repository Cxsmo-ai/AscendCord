//! Device-local microphone processing. Profiles leave the user's custom settings intact.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputProfile {
	VoiceIsolation,
	#[default]
	Studio,
	Custom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoiseSuppression {
	Off,
	#[default]
	RnNoise,
	WebRtc,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpusApplication {
	Voip,
	#[default]
	Audio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpusSignal {
	Auto,
	Voice,
	#[default]
	Music,
}

/// User-facing Opus controls mirrored from Acheron's voice settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpusSettings {
	pub application: OpusApplication,
	/// Target bitrate, in bits per second (8,000–510,000).
	pub bitrate: u32,
	/// Encoder complexity (0–10).
	pub complexity: u8,
	pub signal: OpusSignal,
	pub fec: bool,
	/// Expected packet loss percentage (0–100).
	pub packet_loss_percent: u8,
	/// Variable bitrate, disabled for the default constant-bitrate profile.
	pub vbr: bool,
	/// Constrain VBR packet sizes when VBR is enabled (CVBR); false selects unconstrained VBR.
	#[serde(default = "default_vbr_constraint")]
	pub vbr_constraint: bool,
}

const fn default_vbr_constraint() -> bool {
	true
}

impl Default for OpusSettings {
	fn default() -> Self {
		Self {
			application: OpusApplication::Audio,
			bitrate: 510_000,
			complexity: 10,
			signal: OpusSignal::Music,
			fec: false,
			packet_loss_percent: 0,
			vbr: false,
			vbr_constraint: true,
		}
	}
}

impl OpusSettings {
	pub fn normalized(mut self) -> Self {
		self.bitrate = self.bitrate.clamp(8_000, 510_000);
		self.complexity = self.complexity.min(10);
		self.packet_loss_percent = self.packet_loss_percent.min(100);
		self
	}
}

#[cfg(test)]
mod opus_settings_tests {
	use super::OpusSettings;

	#[test]
	fn legacy_serialized_settings_default_to_constrained_vbr() {
		let settings: OpusSettings = serde_json::from_str(
			 r#"{"application":"Audio","bitrate":510000,"complexity":10,"signal":"Music","fec":false,"packet_loss_percent":0,"vbr":false}"#,
		).unwrap();
		assert!(!settings.vbr);
		assert!(settings.vbr_constraint);
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Processing {
	pub suppression: NoiseSuppression,
	/// WebRTC suppression strength, from low (0) through very high (3).
	pub suppression_level: u8,
	pub echo_cancellation: bool,
	pub automatic_gain: bool,
	/// None is an open microphone; otherwise a dBFS threshold with a short release hold.
	pub sensitivity_db: Option<i16>,
}
impl Default for Processing {
	fn default() -> Self {
		Self {
			suppression: NoiseSuppression::Off,
			suppression_level: 0,
			echo_cancellation: false,
			automatic_gain: false,
			sensitivity_db: None,
		}
	}
}
impl Processing {
	pub fn is_valid(self) -> bool {
		self.suppression_level <= 3 && self.sensitivity_db.is_none_or(|db| (-80..=0).contains(&db))
	}
	pub fn studio() -> Self {
		Self {
			suppression: NoiseSuppression::Off,
			suppression_level: 0,
			echo_cancellation: false,
			automatic_gain: false,
			sensitivity_db: None,
		}
	}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceProcessing {
	/// Optional per-channel RNNoise stage, matching Acheron's processing switch.
	pub noise_suppression: bool,
	pub profile: InputProfile,
	pub custom: Processing,
	/// The Acheron-style RNNoise probability gate. When disabled, the raw RMS
	/// threshold below controls whether captured audio is sent.
	pub use_rnnoise_vad: bool,
	/// Stream continuous stereo audio without speech gating or RNNoise processing.
	/// This is intended for music and line-level sources rather than open-room mics.
	pub always_transmit: bool,
	/// Acheron's signed-16-bit PCM RMS threshold (0–2,000).
	pub vad_threshold_rms: u16,
	/// Runtime Opus encoder controls; transport remains the existing Discord DAVE/AEAD path.
	pub opus: OpusSettings,
}

impl Default for VoiceProcessing {
	fn default() -> Self {
		Self {
			noise_suppression: false,
			profile: InputProfile::Studio,
			custom: Processing::default(),
			use_rnnoise_vad: false,
			always_transmit: true,
			vad_threshold_rms: 0,
			opus: OpusSettings::default(),
		}
	}
}
impl VoiceProcessing {
	pub fn effective(self) -> Processing {
		match self.profile {
			InputProfile::VoiceIsolation => Processing::default(),
			InputProfile::Studio => Processing::studio(),
			InputProfile::Custom => self.custom,
		}
	}
	pub fn from_legacy(noise_suppression: bool) -> Self {
		Self {
			noise_suppression,
			profile: InputProfile::Custom,
			custom: Processing {
				suppression: if noise_suppression {
					NoiseSuppression::RnNoise
				} else {
					NoiseSuppression::Off
				},
				echo_cancellation: true,
				..Processing::studio()
			},
			use_rnnoise_vad: false,
			always_transmit: true,
			vad_threshold_rms: 0,
			opus: OpusSettings::default(),
		}
	}
	pub fn normalized(mut self) -> Self {
		// Capture is intentionally fixed to the raw microphone path. Normalize older saved
		// preferences so stale settings cannot re-enable processing through another caller.
		self.noise_suppression = false;
		self.profile = InputProfile::Studio;
		self.custom = Processing::studio();
		self.use_rnnoise_vad = false;
		self.always_transmit = true;
		self.vad_threshold_rms = 0;
		self.opus = OpusSettings::default();
		self
	}
	/// Editing a preset starts from its visible values, rather than hidden custom values.
	pub fn edit(&mut self) -> &mut Processing {
		if self.profile != InputProfile::Custom {
			self.custom = self.effective();
		}
		self.profile = InputProfile::Custom;
		&mut self.custom
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn saved_processing_options_normalize_to_unfiltered_microphone_defaults() {
		let mut saved = VoiceProcessing::from_legacy(true);
		saved.use_rnnoise_vad = true;
		saved.always_transmit = false;
		saved.vad_threshold_rms = 900;
		saved.opus.bitrate = 32_000;
		assert_eq!(saved.normalized(), VoiceProcessing::default());
	}

	#[test]
	fn default_opus_rate_keeps_maximum_stereo_music_mode() {
		let opus = OpusSettings::default();
		assert_eq!(opus.bitrate, 510_000);
		assert_eq!(opus.application, OpusApplication::Audio);
		assert_eq!(opus.signal, OpusSignal::Music);
		assert_eq!(opus.complexity, 10);
		assert!(!opus.vbr && !opus.fec);
	}
}
