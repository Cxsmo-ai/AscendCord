//! `--media-probe`: briefly opens each camera and one screen without joining a call, and
//! reports what arrived. Lets a capture problem be diagnosed from a terminal.

use discord_voice::{camera, screen};
use std::{
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	},
	time::{Duration, Instant},
};

const CAMERA_FRAMES: usize = 30;
const CAMERA_LIMIT: Duration = Duration::from_secs(8);
const SCREEN_LIMIT: Duration = Duration::from_secs(5);

pub fn run(selected: Option<&str>, mut print: impl FnMut(String)) {
	print(format!("AscendCord {} media probe", env!("CARGO_PKG_VERSION")));
	match camera::devices() {
		Ok(devices) => {
			print(format!("cameras: {}", devices.len()));
			for (id, name) in devices
				.iter()
				.filter(|(id, _)| selected.is_none_or(|selected| selected == id))
			{
				for resolution in [model::CameraResolution::Sd, model::CameraResolution::FullHd] {
					let quality = model::CameraQuality {
						resolution,
						..Default::default()
					};
					print(format!("camera \"{name}\" [{id}] {}", probe_camera(id, quality)));
				}
			}
		}
		Err(error) => print(format!("cameras: {error}")),
	}
	if selected.is_none() {
		print(format!("screen {}", probe_screen()));
	}
}

/// `--video-probe FILE…`: decodes the start of each file as the inline player would.
pub fn videos(files: impl Iterator<Item = String>, mut print: impl FnMut(String)) {
	print(format!("AscendCord {} video probe", env!("CARGO_PKG_VERSION")));
	for file in files {
		print(format!("video \"{file}\" {}", probe_video(&file)));
	}
}

fn probe_video(file: &str) -> String {
	use platform::video::{Decoder, Sample};
	use std::task::Poll;
	let source = match std::fs::File::open(file) {
		Ok(source) => source,
		Err(error) => return format!("cannot open: {error}"),
	};
	let started = Instant::now();
	let mut decoder = match Decoder::open(Box::new(std::io::BufReader::new(source))) {
		Ok(decoder) => decoder,
		Err(error) => return format!("error: {error}"),
	};
	let info = decoder.info();
	let opened = started.elapsed();
	let (mut pictures, mut audio, mut size, mut last_pts) = (0usize, 0usize, (0, 0), 0.0);
	let mut failure = None;
	while pictures < 60 && started.elapsed() < Duration::from_secs(20) {
		match decoder.poll_video() {
			Ok(Poll::Ready(Some(Sample::Video {
				pts, width, height, ..
			}))) => {
				pictures += 1;
				size = (width, height);
				last_pts = pts;
			}
			Ok(Poll::Ready(Some(Sample::Audio { .. }))) => {}
			Ok(Poll::Ready(None)) => break,
			Ok(Poll::Pending) => std::thread::sleep(Duration::from_millis(1)),
			Err(error) => {
				failure = Some(error);
				break;
			}
		}
		if info.sample_rate > 0
			&& let Ok(Poll::Ready(Some(Sample::Audio { frames, .. }))) = decoder.poll_audio()
		{
			audio += frames.len();
		}
	}
	let mut report = format!(
		"{}x{} {:.1} s, audio {} Hz x{}: opened in {} ms, {pictures} pictures ({}x{}, to {:.2} s) and {audio} audio frames in {} ms",
		info.width,
		info.height,
		info.duration,
		info.sample_rate,
		info.channels,
		opened.as_millis(),
		size.0,
		size.1,
		last_pts,
		started.elapsed().as_millis()
	);
	if let Some(error) = failure {
		report.push_str(&format!(", error: {error}"));
	} else if pictures == 0 {
		report.push_str(", error: no pictures");
	}
	report
}

fn probe_camera(id: &str, quality: model::CameraQuality) -> String {
	let (width, height) = quality.dimensions();
	let frames = Arc::new(AtomicUsize::new(0));
	let pictures = Arc::new(AtomicUsize::new(0));
	let bytes = Arc::new(AtomicUsize::new(0));
	let on_frame = {
		let (frames, pictures, bytes) = (frames.clone(), pictures.clone(), bytes.clone());
		Arc::new(move |frame: camera::Frame| {
			frames.fetch_add(1, Ordering::Relaxed);
			bytes.fetch_add(frame.data.len(), Ordering::Relaxed);
			if frame
				.rgb
				.as_ref()
				.is_some_and(|rgb| rgb.len() == frame.width as usize * frame.height as usize * 3)
			{
				pictures.fetch_add(1, Ordering::Relaxed);
			}
		})
	};
	let started = Instant::now();
	let camera = match camera::Camera::start(Some(id.to_owned()), quality, on_frame, Arc::new(|| {}))
	{
		Ok(camera) => camera,
		Err(error) => return format!("{width}x{height}: did not start: {error}"),
	};
	while started.elapsed() < CAMERA_LIMIT
		&& !camera.stopped()
		&& frames.load(Ordering::Relaxed) < CAMERA_FRAMES
	{
		std::thread::sleep(Duration::from_millis(20));
	}
	let elapsed = started.elapsed();
	let failure = camera.error_detail().or(camera.error().map(str::to_owned));
	camera.stop();
	// The next camera can only start once this worker has closed the device.
	let closing = Instant::now();
	while !camera.stopped() && closing.elapsed() < Duration::from_secs(5) {
		std::thread::sleep(Duration::from_millis(20));
	}
	let frames = frames.load(Ordering::Relaxed);
	let mut report = format!(
		"{width}x{height}: {frames} frames in {:.1} s, {} with a local preview picture, {} KiB encoded",
		elapsed.as_secs_f32(),
		pictures.load(Ordering::Relaxed),
		bytes.load(Ordering::Relaxed) / 1024
	);
	if let Some(failure) = failure {
		report.push_str(&format!(", error: {failure}"));
	} else if frames == 0 {
		report.push_str(", error: no frames");
	}
	report
}

fn probe_screen() -> String {
	let sources = match screen::sources() {
		Ok(sources) => sources,
		Err(error) => return format!("sources: {error}"),
	};
	let Some(source) = sources
		.iter()
		.find(|source| matches!(source.id, screen::SourceId::Display(_)))
		.or(sources.first())
	else {
		return "sources: none".into();
	};
	let settings = screen::Settings {
		source: source.id,
		width: 1280,
		height: 720,
		fps: 30,
		cursor: true,
		audio: false,
	};
	let (worker, mut video) = match screen::Worker::start(settings, || {}) {
		Ok(started) => started,
		Err(error) => return format!("\"{}\": did not start: {error}", source.name),
	};
	// Stand in for a connected call so the worker encodes as it would when sharing.
	video.ready.store(true, Ordering::Release);
	let started = Instant::now();
	let (mut encoded, mut keyframes, mut bytes) = (0usize, 0usize, 0usize);
	let (mut result, mut previews) = (None, 0usize);
	while started.elapsed() < SCREEN_LIMIT {
		while let Ok(frame) = video.frames.try_recv() {
			encoded += 1;
			keyframes += usize::from(frame.keyframe);
			bytes += frame.data.len();
		}
		previews += usize::from(worker.take_preview().is_some());
		if let Some(done) = worker.result() {
			result = Some(done);
			break;
		}
		std::thread::sleep(Duration::from_millis(10));
	}
	drop(worker);
	let mut report = format!(
		"\"{}\" 1280x720@30: {encoded} encoded frames ({keyframes} keyframes, {} KiB) and {previews} previews in {:.1} s",
		source.name,
		bytes / 1024,
		started.elapsed().as_secs_f32()
	);
	match result {
		Some(Err(error)) => report.push_str(&format!(", error: {error}")),
		Some(Ok(())) => report.push_str(", capture ended early"),
		None if encoded == 0 => report.push_str(", error: no encoded frames"),
		None => {}
	}
	report
}
