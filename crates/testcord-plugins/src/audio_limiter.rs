//! Native output volume and peak ceiling settings for the TestCord AudioLimiter port.

use crate::{Fallback, Meta, Plugin, Setting, SettingKind};

const SETTINGS: &[Setting] = &[
	Setting {
		key: "enableVolumeLimiting",
		label: "Limit the speaker volume slider",
		kind: SettingKind::Toggle,
		default: Fallback::Flag(true),
	},
	Setting {
		key: "maxVolume",
		label: "Maximum speaker volume",
		kind: SettingKind::Number { min: 10, max: 100 },
		default: Fallback::Number(80),
	},
	Setting {
		key: "enableDbLimiting",
		label: "Limit output peaks",
		kind: SettingKind::Toggle,
		default: Fallback::Flag(true),
	},
	Setting {
		key: "maxDecibels",
		label: "Peak ceiling",
		kind: SettingKind::Number { min: -20, max: 0 },
		default: Fallback::Number(-3),
	},
];

pub struct AudioLimiter;

impl Plugin for AudioLimiter {
	fn meta(&self) -> Meta {
		Meta {
			id: "audioLimiter",
			name: "AudioLimiter",
			description: "Cap voice playback volume and prevent peaks exceeding a dBFS ceiling.",
			authors: "Vencord · native AscendCord port",
			tags: &["audio", "voice", "accessibility"],
			aliases: &["Audio Limiter"],
			default_enabled: false,
		}
	}

	fn settings(&self) -> &'static [Setting] {
		SETTINGS
	}
}
