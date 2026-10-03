use local_store::AppPreferences;

#[derive(Default)]
pub struct Settings {
	pub current: AppPreferences,
	pub loaded: bool,
	pub state: crate::toggle_setting::Settings,
}

impl Settings {
	pub fn save(&mut self, cache: Option<&crate::cache::Cache>, generation: u64) -> bool {
		if !self.state.dirty || self.state.saving {
			return false;
		}
		let accepted = cache.is_some_and(|cache| {
			cache.queue(
				generation,
				model::Id(0),
				crate::cache::Operation::SaveAppPreferences(Box::new(self.current.clone())),
			)
		});
		// A full cache queue must not turn a device preference into a session-only change.
		self.state.dirty = !accepted;
		self.state.saving = accepted;
		self.state.failed = !accepted;
		accepted
	}
	pub fn observe(&mut self, ui: &ui::MessagingUi) {
		let value = AppPreferences {
			notifications_enabled: ui.notifications_enabled,
			auto_update: ui.updates.auto_update,
			update_nightly: ui.updates.nightly,
			notification_options: ui.notification_options,
			show_hidden_channels: ui.show_hidden_channels,
			hide_title_bar: ui.hide_title_bar,
			gpu_preference: ui.gpu_preference,
			primary_color: ui.primary_color,
			transparency_blur: ui.transparency_blur,
			transparency: ui.transparency,
			blur: ui.blur,
			transparent_all: ui.transparent_all,
			voice_noise_suppression: ui.voice_processing.noise_suppression,
			voice_processing: Some(ui.voice_processing),
			voice_push_to_talk: ui.voice_push_to_talk,
			voice_muted: ui.voice_muted,
			voice_deafened: ui.voice_deafened,
			voice_input: ui.voice_input.clone(),
			voice_output: ui.voice_output.clone(),
			camera_quality: ui.camera_quality,
			input_percent: ui.voice_gain.input_percent,
			output_percent: ui.voice_gain.output_percent,
			keybinds: ui.keybinds.clone(),
			expanded_folders: ui.expanded_folders.clone(),
			user_volumes: ui.voice_user_volume_overrides(),
			muted_users: ui.voice_user_mutes().to_vec(),
			streamer_mode: ui.streamer_mode,
			reduce_motion_sync: ui.reduce_motion_sync,
			reduce_motion: ui.reduce_motion,
			always_underline_links: ui.always_underline_links,
			high_contrast: ui.high_contrast,
			reduce_saturation: ui.reduce_saturation,
			font_scale: ui.font_scale,
			animate_emoji: ui.animate_emoji,
			legacy_chat_input: ui.legacy_chat_input,
			show_shortcuts_list: ui.show_shortcuts_list,
			tts_messages: ui.tts_messages,
			developer_mode: ui.developer_mode,
			app_icon: ui.app_icon,
			locale: ui.locale.clone(),
		};
		if value != self.current {
			self.state.touched = true;
			self.state.failed = !value.is_valid();
			if value.is_valid() {
				self.current = value;
				self.state.dirty = true;
			}
		}
	}
	pub fn apply(&self, ui: &mut ui::MessagingUi) {
		let value = &self.current;
		ui.notifications_enabled = value.notifications_enabled;
		ui.updates.auto_update = value.auto_update;
		ui.updates.nightly = value.update_nightly;
		ui.notification_options = value.notification_options;
		ui.show_hidden_channels = value.show_hidden_channels;
		ui.hide_title_bar = value.hide_title_bar;
		ui.gpu_preference = value.gpu_preference;
		ui.primary_color = value.primary_color;
		ui.transparency_blur = value.transparency_blur;
		ui.transparency = value.transparency;
		ui.blur = value.blur;
		ui.transparent_all = value.transparent_all;
		ui.voice_processing = value.voice_processing.unwrap_or_else(|| {
			model::voice_settings::VoiceProcessing::from_legacy(value.voice_noise_suppression)
		});
		ui.voice_push_to_talk = value.voice_push_to_talk;
		ui.voice_muted = value.voice_muted;
		ui.voice_deafened = value.voice_deafened;
		ui.voice_input.clone_from(&value.voice_input);
		ui.voice_output.clone_from(&value.voice_output);
		ui.camera_quality = value.camera_quality;
		ui.voice_gain.input_percent = value.input_percent;
		ui.voice_gain.output_percent = value.output_percent;
		ui.keybinds = value.keybinds.clone();
		ui.expanded_folders.clone_from(&value.expanded_folders);
		ui.set_voice_user_volume_overrides(&value.user_volumes);
		ui.set_voice_user_mutes(&value.muted_users);
		ui.streamer_mode = value.streamer_mode;
		ui.reduce_motion_sync = value.reduce_motion_sync;
		ui.reduce_motion = value.reduce_motion;
		ui.always_underline_links = value.always_underline_links;
		ui.high_contrast = value.high_contrast;
		ui.reduce_saturation = value.reduce_saturation;
		// Older builds could store 0 here; the setting itself ranges over 80-125%.
		ui.font_scale = if (80..=125).contains(&value.font_scale) {
			value.font_scale
		} else {
			AppPreferences::default().font_scale
		};
		ui.animate_emoji = value.animate_emoji;
		ui.legacy_chat_input = value.legacy_chat_input;
		ui.show_shortcuts_list = value.show_shortcuts_list;
		ui.tts_messages = value.tts_messages;
		ui.developer_mode = value.developer_mode;
		ui.app_icon = value.app_icon;
		if value.locale.is_empty() {
			ui.locale = AppPreferences::default().locale;
		} else {
			ui.locale.clone_from(&value.locale);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn startup_defaults_do_not_overwrite_pending_saved_preferences() {
		let mut settings = Settings::default();
		let defaults = AppPreferences::default();
		// The same start-up path as the desktop app: the UI begins from stored defaults.
		let mut ui = ui::MessagingUi::default();
		settings.apply(&mut ui);
		assert_eq!(ui.font_scale, defaults.font_scale);
		assert_eq!(ui.locale, defaults.locale);
		settings.observe(&ui);
		assert!(
			!settings.state.touched,
			"startup defaults must not count as a user edit"
		);
		assert!(!settings.state.dirty);

		settings.current.notification_options.current_channel = true;
		settings.loaded = true;
		settings.apply(&mut ui);
		settings.observe(&ui);
		assert!(ui.notification_options.current_channel);
		assert!(!settings.state.touched);
		assert!(!settings.state.dirty);
	}

	#[test]
	fn impossible_text_size_and_empty_locale_load_as_defaults() {
		let settings = Settings {
			current: AppPreferences {
				font_scale: 0,
				locale: String::new(),
				app_icon: model::AppIcon::Sakura,
				..Default::default()
			},
			..Default::default()
		};
		let mut ui = ui::MessagingUi::default();
		settings.apply(&mut ui);
		assert_eq!(ui.font_scale, 100);
		assert_eq!(ui.locale, "en-US");
		assert_eq!(ui.app_icon, model::AppIcon::Sakura);
	}

	#[test]
	fn legacy_preferences_without_voice_settings_keep_suppression_disabled() {
		let current: AppPreferences = serde_json::from_str("{}").unwrap();
		assert!(!current.voice_noise_suppression);
		assert!(current.voice_processing.is_none());
		let settings = Settings {
			current,
			..Default::default()
		};
		let mut ui = ui::MessagingUi::default();
		settings.apply(&mut ui);
		assert!(!ui.voice_processing.noise_suppression);
	}
}
