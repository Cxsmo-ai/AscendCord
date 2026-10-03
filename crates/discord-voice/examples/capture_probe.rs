//! Owner-run capture format probe: opens the VoiceMeeter Out B2 endpoint (or the capture
//! device whose name contains the first argument) for three seconds per backend and prints
//! the delivered rate, channel count and per-channel level. Reads audio only; sends nothing.
#[cfg(target_os = "windows")]
fn main() {
	use miniaudio::{Backend, Context, Device, DeviceConfig, DeviceType, Format};
	use std::sync::{Arc, Mutex};

	let needle = std::env::args()
		.nth(1)
		.unwrap_or_else(|| "out b2".into())
		.to_ascii_lowercase();
	for (label, backend, rate, channels, format) in [
		("WASAPI native F32", Backend::Wasapi, 0, 0, Format::F32),
		(
			"DirectSound 48k S16 stereo (current)",
			Backend::DSound,
			48_000,
			2,
			Format::S16,
		),
	] {
		let Ok(context) = Context::new(&[backend], None) else {
			println!("{label}: backend unavailable");
			continue;
		};
		let mut id = None;
		let _ = context.with_devices(|_, inputs| {
			id = inputs
				.iter()
				.find(|device| device.name().to_ascii_lowercase().contains(&needle))
				.map(|device| device.id().clone());
		});
		let Some(id) = id else {
			println!("{label}: no capture device matching {needle:?}");
			continue;
		};
		let mut config = DeviceConfig::new(DeviceType::Capture);
		config.set_sample_rate(rate);
		config.capture_mut().set_format(format);
		config.capture_mut().set_channels(channels);
		config.capture_mut().set_device_id(Some(id));
		config.set_wasapi_no_auto_convert_src(true);
		// [frames, peak per channel (8), sum of squares per channel (8), channels]
		let stats = Arc::new(Mutex::new((0u64, [0f32; 8], [0f64; 8], 0usize)));
		let sink = stats.clone();
		config.set_data_callback(move |_, _, input| {
			let channels = input.channels() as usize;
			let mut stats = sink.lock().unwrap();
			stats.3 = channels;
			let mut push = |frame: &[f32]| {
				stats.0 += 1;
				for (channel, sample) in frame.iter().take(8).enumerate() {
					stats.1[channel] = stats.1[channel].max(sample.abs());
					stats.2[channel] += f64::from(*sample) * f64::from(*sample);
				}
			};
			match input.format() {
				Format::F32 => input
					.as_samples::<f32>()
					.chunks_exact(channels)
					.for_each(&mut push),
				_ => input
					.as_samples::<i16>()
					.chunks_exact(channels)
					.for_each(|frame| {
						let converted: Vec<f32> =
							frame.iter().map(|s| f32::from(*s) / 32768.0).collect();
						push(&converted);
					}),
			}
		});
		let device = match Device::new(Some(context), &config) {
			Ok(device) => device,
			Err(error) => {
				println!("{label}: open failed: {error:?}");
				continue;
			}
		};
		let delivered_rate = device.sample_rate();
		if let Err(error) = device.start() {
			println!("{label}: start failed: {error:?}");
			continue;
		}
		std::thread::sleep(std::time::Duration::from_secs(3));
		drop(device);
		let (frames, peak, squares, channels) = *stats.lock().unwrap();
		let db = |value: f64| {
			if value > 0.0 {
				20.0 * value.log10()
			} else {
				-200.0
			}
		};
		println!("{label}: {delivered_rate} Hz · {channels} ch · {frames} frames in 3 s");
		for channel in 0..channels.min(8) {
			let rms = (squares[channel] / frames.max(1) as f64).sqrt();
			println!(
				"  ch{channel}: peak {:.1} dBFS · rms {:.1} dBFS",
				db(f64::from(peak[channel])),
				db(rms)
			);
		}
	}
}

#[cfg(not(target_os = "windows"))]
fn main() {
	println!("The capture probe is Windows-only.");
}
