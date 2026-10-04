//! One bounded, off-thread startup operation at a time; demo never touches the OS entry.
use eframe::egui;
use platform::startup::Settings;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Default)]
pub struct Startup {
	saved: Settings,
	pending: Option<Receiver<Result<Settings, &'static str>>>,
}

impl Startup {
	pub fn new(
		ctx: &egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
		demo: bool,
	) -> Self {
		view.startup_available = platform::startup::available();
		let mut startup = Self::default();
		if !demo && view.startup_available {
			startup.begin(ctx, runtime, view, None);
		}
		startup
	}

	fn begin(
		&mut self,
		ctx: &egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
		setting: Option<Settings>,
	) {
		let (send, receive) = mpsc::sync_channel(1);
		self.pending = Some(receive);
		view.startup_busy = true;
		view.startup_status = if setting.is_some() {
			"Saving startup settings…"
		} else {
			"Loading startup settings…"
		};
		let ctx = ctx.clone();
		runtime.spawn_blocking(move || {
			let result = match setting {
				Some(setting) => platform::startup::save(setting).map(|()| setting),
				None => platform::startup::load(),
			};
			let _ = send.send(result);
			ctx.request_repaint();
		});
	}

	fn poll(&mut self, view: &mut ui::MessagingUi) {
		let Some(pending) = &self.pending else { return };
		let result = match pending.try_recv() {
			Ok(result) => result,
			Err(TryRecvError::Empty) => return,
			Err(TryRecvError::Disconnected) => {
				Err("Startup settings worker stopped. Toggle the setting to retry.")
			}
		};
		self.pending = None;
		view.startup_busy = false;
		match result {
			Ok(saved) => {
				self.saved = saved;
				view.startup_status = "";
			}
			Err(error) => view.startup_status = error,
		}
		view.startup_enabled = self.saved.enabled;
		view.startup_minimized = self.saved.minimized;
	}

	pub fn sync(
		&mut self,
		ctx: &egui::Context,
		runtime: &tokio::runtime::Runtime,
		view: &mut ui::MessagingUi,
		demo: bool,
	) {
		if demo || !view.startup_available {
			return;
		}
		self.poll(view);
		let disable = std::mem::take(&mut view.startup_disable_requested);
		let desired = if disable {
			Settings::default()
		} else {
			Settings {
				enabled: view.startup_enabled,
				minimized: view.startup_enabled && view.startup_minimized,
			}
		};
		if self.pending.is_none() && (disable || desired != self.saved) {
			self.begin(ctx, runtime, view, Some(desired));
		}
	}
}

/// Text for `--version` or `--help`; these print and exit without opening a window.
pub fn info_flag(mut args: impl Iterator<Item = String>) -> Option<String> {
	let version = format!("AscendCord {}", env!("CARGO_PKG_VERSION"));
	args.find_map(|arg| match arg.as_str() {
		"--version" | "-V" => Some(version.clone()),
		"--help" | "-h" => Some(format!(
			"{version}\n\nUsage: ascendcord [--start-minimized]\n\n  \
			 --start-minimized  Start in the tray (with --autostart)\n  \
			 --media-probe [ID] Test each camera (or one) and a screen, then exit\n  \
			 --video-probe FILE Decode the start of each video file, then exit\n  \
			 -V, --version      Print the version and exit\n  \
			 -h, --help         Print this help and exit"
		)),
		_ => None,
	})
}

pub fn minimized_launch(demo: bool, args: impl Iterator<Item = String>) -> bool {
	let mut autostart = false;
	let mut minimized = false;
	for arg in args {
		autostart |= arg == "--autostart";
		minimized |= arg == "--start-minimized";
	}
	!demo && autostart && minimized
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn version_and_help_flags_exit_before_the_window() {
		let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
		let version = info_flag(args(&["--version"]).into_iter()).unwrap();
		assert_eq!(version, format!("AscendCord {}", env!("CARGO_PKG_VERSION")));
		assert_eq!(info_flag(args(&["-V"]).into_iter()), Some(version.clone()));
		assert!(
			info_flag(args(&["--help"]).into_iter())
				.unwrap()
				.contains("--version")
		);
		assert_eq!(
			info_flag(args(&["--autostart", "--start-minimized"]).into_iter()),
			None
		);
		assert_eq!(info_flag(args(&[]).into_iter()), None);
	}
	#[test]
	fn startup_completion_rolls_back_errors_and_only_autostart_can_minimize() {
		let mut startup = Startup::default();
		let mut view = ui::MessagingUi::default();
		for result in [
			Ok(Settings {
				enabled: true,
				minimized: true,
			}),
			Err("Synthetic denied write"),
		] {
			let (send, receive) = mpsc::sync_channel(1);
			startup.pending = Some(receive);
			view.startup_busy = true;
			view.startup_enabled = false;
			startup.poll(&mut view);
			assert!(view.startup_busy);
			send.send(result).unwrap();
			startup.poll(&mut view);
			assert!(!view.startup_busy);
			assert!(view.startup_enabled && view.startup_minimized);
			assert_eq!(view.startup_status, result.err().unwrap_or(""));
		}
		for demo in [false, true] {
			for args in [
				vec![],
				vec!["--start-minimized"],
				vec!["--autostart"],
				vec!["--autostart", "--start-minimized"],
			] {
				assert_eq!(
					minimized_launch(demo, args.iter().map(|s| s.to_string())),
					!demo && args.len() == 2
				);
			}
		}
	}
}
