//! Device-local camera capture and encode quality preferences.

use serde::{Deserialize, Serialize};

/// Resolution requested from the local camera and advertised to the voice server.
/// Device and receiver support still determine the delivered resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraResolution {
	#[default]
	Sd,
	Hd,
	FullHd,
	QuadHd,
	UltraHd,
}

/// Video bitstream produced by the native Spout GPU encoder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraCodec {
	#[default]
	H264,
	H265,
}

impl CameraResolution {
	pub const fn dimensions(self) -> (u32, u32) {
		match self {
			Self::Sd => (640, 480),
			Self::Hd => (1280, 720),
			Self::FullHd => (1920, 1080),
			Self::QuadHd => (2560, 1440),
			Self::UltraHd => (3840, 2160),
		}
	}
}

/// Native camera controls, persisted with other device-local settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraQuality {
	pub resolution: CameraResolution,
	/// Native Spout encoder output. Other capture backends remain H.264.
	#[serde(default)]
	pub codec: CameraCodec,
	/// Follow the current Spout sender's dimensions instead of the manual preset.
	#[serde(default = "default_true")]
	pub spout_auto_resolution: bool,
	/// Follow the current Spout sender's measured frame rate instead of the manual preset.
	#[serde(default = "default_true")]
	pub spout_auto_fps: bool,
	pub frames_per_second: u8,
	pub bitrate_kbps: u16,
	/// Hardware image adjustments used by the Windows Spout/SpoutGL path.
	#[serde(default)]
	pub image_controls: CameraImageControls,
}

const fn default_true() -> bool {
	true
}

impl Default for CameraQuality {
	fn default() -> Self {
		Self {
			resolution: CameraResolution::Sd,
			codec: CameraCodec::H264,
			spout_auto_resolution: true,
			spout_auto_fps: true,
			frames_per_second: 30,
			bitrate_kbps: 1500,
			image_controls: CameraImageControls::default(),
		}
	}
}

/// Neutral-by-default D3D video-processor adjustments. Signed controls span
/// -100..=100 around the adapter's neutral level; enhancements span 0..=100.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraImageControls {
	pub brightness: i16,
	pub contrast: i16,
	pub saturation: i16,
	pub hue: i16,
	pub sharpness: u8,
	pub noise_reduction: u8,
}

impl CameraImageControls {
	pub const fn is_valid(self) -> bool {
		self.brightness >= -100
			&& self.brightness <= 100
			&& self.contrast >= -100
			&& self.contrast <= 100
			&& self.saturation >= -100
			&& self.saturation <= 100
			&& self.hue >= -100
			&& self.hue <= 100
			&& self.sharpness <= 100
			&& self.noise_reduction <= 100
	}
}

impl CameraQuality {
	pub const fn dimensions(self) -> (u32, u32) {
		self.resolution.dimensions()
	}

	pub const fn is_valid(self) -> bool {
		matches!(self.frames_per_second, 15 | 30 | 60)
			&& self.bitrate_kbps >= 300
			&& self.bitrate_kbps <= 8000
			&& self.image_controls.is_valid()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn native_camera_quality_is_bounded_and_keeps_older_preferences_compatible() {
		assert!(CameraQuality::default().is_valid());
		assert_eq!(CameraQuality::default().dimensions(), (640, 480));
		assert_eq!(CameraResolution::Hd.dimensions(), (1280, 720));
		assert_eq!(CameraResolution::FullHd.dimensions(), (1920, 1080));
		for quality in [
			CameraQuality {
				frames_per_second: 1,
				..Default::default()
			},
			CameraQuality {
				bitrate_kbps: 299,
				..Default::default()
			},
			CameraQuality {
				bitrate_kbps: 8001,
				..Default::default()
			},
		] {
			assert!(!quality.is_valid());
		}
		assert_eq!(
			serde_json::from_str::<CameraQuality>("{}").unwrap(),
			CameraQuality::default()
		);
	}
}
