//! `--screenshot-to <file.png>`: render a demo scene, save one frame as PNG, then exit.
//!
//! Used by `scripts/capture-screenshots.ps1` for the repository's screenshots. The scene
//! comes from the usual `--demo-*` flags, so only synthetic data is ever captured.
use eframe::egui;
use std::{
	path::PathBuf,
	sync::{Arc, Mutex},
	time::{Duration, Instant},
};

/// Frames and time a scene gets to finish loading fonts, emoji, avatars and layout.
const SETTLE_FRAMES: u32 = 90;
const SETTLE_TIME: Duration = Duration::from_secs(4);
/// A scene that never settles is an error, not a half-drawn screenshot.
const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Capture {
	path: PathBuf,
	size: Option<[f32; 2]>,
	started: Instant,
	frames: u32,
	requested: bool,
	done: Arc<Mutex<Option<Result<(), String>>>>,
}

impl Capture {
	/// `--screenshot-to=PATH` (or `--screenshot-to PATH`), optional `--screenshot-size=WxH`.
	pub fn from_args() -> Option<Self> {
		let args: Vec<String> = std::env::args().collect();
		let value = |flag: &str| {
			args.iter().enumerate().find_map(|(i, arg)| {
				arg.strip_prefix(&format!("{flag}="))
					.map(str::to_owned)
					.or_else(|| (arg == flag).then(|| args.get(i + 1).cloned()).flatten())
			})
		};
		let path = PathBuf::from(value("--screenshot-to")?);
		let size = value("--screenshot-size").and_then(|size| {
			let (width, height) = size.split_once('x')?;
			Some([width.parse().ok()?, height.parse().ok()?])
		});
		Some(Self {
			path,
			size,
			started: Instant::now(),
			frames: 0,
			requested: false,
			done: Arc::new(Mutex::new(None)),
		})
	}

	/// Call once per frame. Returns the process exit code once the capture finished.
	pub fn frame(&mut self, ctx: &egui::Context) -> Option<i32> {
		self.frames += 1;
		if self.frames == 1
			&& let Some(size) = self.size
		{
			ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size.into()));
		}
		if let Some(result) = self.done.lock().ok()?.take() {
			return Some(match result {
				Ok(()) => 0,
				Err(error) => {
					eprintln!("[AscendCord screenshot] {error}");
					1
				}
			});
		}
		if self.started.elapsed() > TIMEOUT {
			eprintln!("[AscendCord screenshot] the scene did not settle in time");
			return Some(2);
		}
		if !self.requested && self.frames >= SETTLE_FRAMES && self.started.elapsed() >= SETTLE_TIME
		{
			self.requested = true;
			let path = self.path.clone();
			let done = self.done.clone();
			let repaint = ctx.clone();
			ctx.request_screenshot(move |image| {
				let result = save(&path, &image);
				if let Ok(mut slot) = done.lock() {
					*slot = Some(result);
				}
				repaint.request_repaint();
			});
		}
		ctx.request_repaint();
		None
	}
}

fn save(path: &std::path::Path, image: &egui::ColorImage) -> Result<(), String> {
	let [width, height] = image.size;
	let pixels: Vec<u8> = image
		.pixels
		.iter()
		.flat_map(|pixel| pixel.to_srgba_unmultiplied())
		.collect();
	let buffer = image::RgbaImage::from_raw(width as u32, height as u32, pixels)
		.ok_or("the captured frame has an unexpected size")?;
	if let Some(parent) = path
		.parent()
		.filter(|parent| !parent.as_os_str().is_empty())
	{
		std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
	}
	buffer.save(path).map_err(|error| error.to_string())
}
