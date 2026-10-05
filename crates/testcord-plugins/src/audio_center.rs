//! Kept as a compatibility entry for older settings files. The raw mic path does not mix inputs.

use crate::{Meta, Plugin, Setting};

const SETTINGS: &[Setting] = &[];

pub struct AudioCenter;

impl Plugin for AudioCenter {
	fn meta(&self) -> Meta {
		Meta {
			id: "audioCenter",
			name: "AudioCenter",
			description: "Unavailable: the microphone path is kept single-source and unprocessed.",
			authors: "Vencord · native AscendCord port",
			tags: &["audio", "voice"],
			aliases: &["Audio Center"],
			default_enabled: false,
		}
	}

	fn settings(&self) -> &'static [Setting] {
		SETTINGS
	}
}
