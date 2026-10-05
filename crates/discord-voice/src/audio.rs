//! Acheron-shaped native audio engine: miniaudio devices request 48 kHz, stereo S16,
//! 20 ms periods; microphone capture and playback mixing stay on a worker, outside device callbacks.
use crate::{CaptureFrame, Frame, StereoFrame};
use miniaudio::{Context, Device, DeviceConfig, DeviceType, Format};
use model::voice_settings::VoiceProcessing;
use std::{
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering},
		mpsc,
	},
	time::Duration,
};
use tokio::sync::watch;

const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;
const PERIOD: u32 = 960;
const PCM_SAMPLES: usize = PERIOD as usize * CHANNELS as usize;

fn preserve_raw_capture(mut stereo: StereoFrame) -> StereoFrame {
	for sample in &mut stereo {
		if !sample.is_finite() {
			*sample = 0.0;
		}
	}
	stereo
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Devices {
	pub input: Option<String>,
	pub output: Option<String>,
}
pub struct DeviceList {
	pub inputs: Vec<(String, String)>,
	pub outputs: Vec<(String, String)>,
}

fn context() -> Result<Context, &'static str> {
	#[cfg(target_os = "windows")]
	{
		// WASAPI opens endpoints at their native format (see `capture_config`);
		// DirectSound and WinMM remain as fallbacks for unusual drivers.
		use miniaudio::Backend;
		Context::new(&[Backend::Wasapi, Backend::DSound, Backend::WinMM], None)
			.map_err(|_| "Native audio backend is unavailable")
	}
	#[cfg(not(target_os = "windows"))]
	Context::new(&[], None).map_err(|_| "Native audio backend is unavailable")
}

/// IDs are stable name/index selectors, not miniaudio's opaque process-local handles.
pub fn devices() -> Result<DeviceList, &'static str> {
	let context = context()?;
	let mut list = DeviceList {
		inputs: Vec::new(),
		outputs: Vec::new(),
	};
	context
		.with_devices(|playback, capture| {
			for (index, device) in capture.iter().take(128).enumerate() {
				let name = device.name().chars().take(128).collect::<String>();
				list.inputs.push((format!("capture|{index}|{name}"), name));
			}
			for (index, device) in playback.iter().take(128).enumerate() {
				let name = device.name().chars().take(128).collect::<String>();
				list.outputs
					.push((format!("playback|{index}|{name}"), name));
			}
		})
		.map_err(|_| "Audio devices are unavailable")?;
	Ok(list)
}

fn selected_id(
	context: &Context,
	selected: Option<&str>,
	capture: bool,
) -> Option<miniaudio::DeviceId> {
	let selected = selected?;
	let expected = if capture { "capture" } else { "playback" };
	let mut result = None;
	let _ = context.with_devices(|playback, inputs| {
		let devices = if capture { inputs } else { playback };
		let parts = selected.splitn(3, '|').collect::<Vec<_>>();
		if let [kind, index, name] = parts.as_slice() {
			if *kind == expected
				&& let Ok(index) = index.parse::<usize>()
			{
				// Windows may reorder endpoints between enumeration and opening. The
				// persisted index is only a hint; prefer the stable displayed name.
				result = devices
					.get(index)
					.filter(|device| device.name() == *name)
					.or_else(|| devices.iter().find(|device| device.name() == *name))
					.map(|device| device.id().clone());
			}
		} else {
			// Migrate old CPAL IDs where the driver included its device name.
			result = devices
				.iter()
				.find(|device| selected.contains(device.name()))
				.map(|device| device.id().clone());
		}
	});
	result
}

pub struct Gate {
	pub ready: AtomicBool,
	muted: AtomicBool,
	deafened: AtomicBool,
	stopped: AtomicBool,
	input_enabled: AtomicBool,
	failed: AtomicBool,
	output_gain: AtomicU16,
	output_cap: AtomicU16,
	peak_ceiling_db_tenths: AtomicU16,
	preview_level: AtomicU16,
	/// Level measurement in the capture callback; off unless something displays or uses it.
	meter: AtomicBool,
	capture_callbacks: AtomicU64,
	capture_frames: AtomicU64,
	capture_channels: AtomicU16,
	capture_sample_rate: AtomicU64,
	capture_format: AtomicU16,
	capture_ring_drops: AtomicU64,
	capture_worker_drops: AtomicU64,
	playback_ring_drops: AtomicU64,
	revision: AtomicU64,
	acknowledged: AtomicU64,
}
impl Default for Gate {
	fn default() -> Self {
		Self {
			ready: AtomicBool::new(false),
			muted: AtomicBool::new(false),
			deafened: AtomicBool::new(false),
			stopped: AtomicBool::new(false),
			input_enabled: AtomicBool::new(true),
			failed: AtomicBool::new(false),
			output_gain: AtomicU16::new(100),
			output_cap: AtomicU16::new(200),
			peak_ceiling_db_tenths: AtomicU16::new(2000),
			preview_level: AtomicU16::new(0),
			meter: AtomicBool::new(false),
			capture_callbacks: AtomicU64::new(0),
			capture_frames: AtomicU64::new(0),
			capture_channels: AtomicU16::new(0),
			capture_sample_rate: AtomicU64::new(0),
			capture_format: AtomicU16::new(0),
			capture_ring_drops: AtomicU64::new(0),
			capture_worker_drops: AtomicU64::new(0),
			playback_ring_drops: AtomicU64::new(0),
			revision: AtomicU64::new(1),
			acknowledged: AtomicU64::new(0),
		}
	}
}
impl Gate {
	fn capture(&self) -> bool {
		self.ready.load(Ordering::Acquire)
			&& self.input_enabled.load(Ordering::Acquire)
			&& !self.muted.load(Ordering::Acquire)
			&& !self.deafened.load(Ordering::Acquire)
			&& !self.stopped.load(Ordering::Acquire)
			&& !self.failed.load(Ordering::Acquire)
	}
	fn playback(&self) -> bool {
		self.ready.load(Ordering::Acquire)
			&& !self.deafened.load(Ordering::Acquire)
			&& !self.stopped.load(Ordering::Acquire)
			&& !self.failed.load(Ordering::Acquire)
	}
}

pub struct Audio {
	pub gate: Arc<Gate>,
	settings: watch::Sender<Devices>,
	thread: std::thread::Thread,
	done: Option<mpsc::Receiver<()>>,
}
impl Audio {
	pub fn start(
		settings: Devices,
		capture: mpsc::SyncSender<CaptureFrame>,
		playback: mpsc::Receiver<Frame>,
		emit: impl Fn(Result<(), &'static str>) + Send + 'static,
	) -> Result<Self, &'static str> {
		Self::start_inner(settings, capture, playback, emit, false)
	}
	pub fn preview(
		settings: Devices,
		emit: impl Fn(Result<(), &'static str>) + Send + 'static,
	) -> Result<Self, &'static str> {
		let (capture, _) = mpsc::sync_channel(1);
		let (_, playback) = mpsc::sync_channel(1);
		Self::start_inner(settings, capture, playback, emit, true)
	}
	fn start_inner(
		settings_value: Devices,
		capture_send: mpsc::SyncSender<CaptureFrame>,
		playback_receive: mpsc::Receiver<Frame>,
		emit: impl Fn(Result<(), &'static str>) + Send + 'static,
		preview: bool,
	) -> Result<Self, &'static str> {
		let gate = Arc::new(Gate::default());
		// The microphone test exists to show the level.
		gate.meter.store(preview, Ordering::Relaxed);
		let (settings, mut selected) = watch::channel(settings_value);
		let (done_send, done) = mpsc::sync_channel(1);
		let worker_gate = gate.clone();
		let thread = std::thread::Builder::new()
			.name("acheron-audio".into())
			.spawn(move || {
				let mut active: Option<(u64, Device, Device)> = None;
				let mut capture_ring: Option<CaptureInput> = None;
				let mut playback_ring: Option<rtrb::Producer<StereoFrame>> = None;
				let mut last_selection = Devices::default();
				// Owner-run stereo verification: replace captured samples with 440 Hz left /
				// 660 Hz right at -12 dBFS while keeping the device's real callback timing.
				let test_tone =
					std::env::var("ASCENDCORD_TEST_TONE").is_ok_and(|value| value == "1");
				let mut tone_phase = 0u64;
				loop {
					if worker_gate.stopped.load(Ordering::Acquire) {
						break;
					}
					let revision = worker_gate.revision.load(Ordering::Acquire);
					let selection = selected.borrow_and_update().clone();
					if selection != last_selection
						|| active
							.as_ref()
							.is_some_and(|(opened, _, _)| *opened != revision)
					{
						active = None;
						capture_ring = None;
						playback_ring = None;
						last_selection = selection.clone();
					}
					if active.is_none() && worker_gate.ready.load(Ordering::Acquire) {
						match open_devices(&selection, &worker_gate) {
							Ok((capture_device, playback_device, capture_read, playback_write)) => {
								active = Some((revision, capture_device, playback_device));
								capture_ring = Some(capture_read);
								playback_ring = Some(playback_write);
								worker_gate.acknowledged.store(revision, Ordering::Release);
								worker_gate.failed.store(false, Ordering::Release);
								emit(Ok(()));
							}
							Err(error) => {
								worker_gate.failed.store(true, Ordering::Release);
								emit(Err(error));
								break;
							}
						}
					}
					let mut work = false;
					if let Some(input) = &mut capture_ring {
						for _ in 0..8 {
							let Some(mut stereo) = input.next_frame(&worker_gate) else {
								break;
							};
							work = true;
							if !worker_gate.capture() {
								continue;
							}
							if test_tone {
								for (index, pair) in
									stereo.as_chunks_mut::<2>().0.iter_mut().enumerate()
								{
									let time = (tone_phase + index as u64) as f64 / f64::from(RATE);
									let tau = std::f64::consts::TAU;
									pair[0] = ((tau * 440.0 * time).sin() * 0.25) as f32;
									pair[1] = ((tau * 660.0 * time).sin() * 0.25) as f32;
								}
								tone_phase = (tone_phase + u64::from(PERIOD)) % u64::from(RATE);
							}
							// No app DSP is applied: this only replaces invalid IEEE values before encoding.
							stereo = preserve_raw_capture(stereo);
							if capture_send.try_send(CaptureFrame::Stereo(stereo)).is_err() {
								worker_gate
									.capture_worker_drops
									.fetch_add(1, Ordering::Relaxed);
							}
						}
					}
					if let Some(output) = &mut playback_ring {
						for _ in 0..8 {
							let Ok(frame) = playback_receive.try_recv() else {
								break;
							};
							work = true;
							if output.push(frame).is_err() {
								worker_gate
									.playback_ring_drops
									.fetch_add(1, Ordering::Relaxed);
							}
						}
					}
					if !work {
						std::thread::park_timeout(Duration::from_millis(2));
					}
				}
				worker_gate.stopped.store(true, Ordering::Release);
				drop(active);
				let _ = done_send.send(());
			})
			.map_err(|_| "Could not start native audio worker")?;
		Ok(Self {
			gate,
			settings,
			thread: thread.thread().clone(),
			done: Some(done),
		})
	}
	pub fn preview_level_db(&self) -> f32 {
		f32::from(self.gate.preview_level.load(Ordering::Relaxed)) - 100.0
	}
	pub fn capture_diagnostic(&self) -> String {
		let callbacks = self.gate.capture_callbacks.load(Ordering::Relaxed);
		let frames = self.gate.capture_frames.load(Ordering::Relaxed);
		let channels = self.gate.capture_channels.load(Ordering::Relaxed);
		let rate = self.gate.capture_sample_rate.load(Ordering::Relaxed);
		let format = match self.gate.capture_format.load(Ordering::Relaxed) {
			1 => "U8",
			2 => "S16",
			3 => "S24",
			4 => "S32",
			5 => "F32",
			_ => "n/a",
		};
		if !self.is_ready() {
			return "Capture device is not ready yet".into();
		}
		if callbacks == 0 {
			return "Capture device opened · no capture callbacks received".into();
		}
		let conversion = if rate == u64::from(RATE) {
			" (native 48 kHz)"
		} else {
			" → 48 kHz polyphase"
		};
		format!(
			"Capture active · {callbacks} blocks / {frames} frames · {rate} Hz · {channels} ch {format}{conversion} · raw {:.0} dBFS",
			self.preview_level_db()
		)
	}
	/// Current input format and bounded-queue drops for local diagnostics.
	pub fn capture_stats(&self) -> (u32, u16, &'static str, u64, u64) {
		let format = match self.gate.capture_format.load(Ordering::Relaxed) {
			1 => "U8",
			2 => "S16",
			3 => "S24",
			4 => "S32",
			5 => "F32",
			_ => "n/a",
		};
		(
			self.gate
				.capture_sample_rate
				.load(Ordering::Relaxed)
				.min(u64::from(u32::MAX)) as u32,
			self.gate.capture_channels.load(Ordering::Relaxed),
			format,
			self.gate.capture_ring_drops.load(Ordering::Relaxed),
			self.gate.capture_worker_drops.load(Ordering::Relaxed),
		)
	}
	pub fn is_stopped(&self) -> bool {
		self.gate.stopped.load(Ordering::Acquire)
	}
	/// Playback frames dropped because the native output callback fell behind its bounded queue.
	pub fn playback_ring_drops(&self) -> u64 {
		self.gate.playback_ring_drops.load(Ordering::Relaxed)
	}
	pub fn shutdown(mut self) -> mpsc::Receiver<()> {
		self.done.take().expect("audio owns completion")
	}
	pub fn set_devices(&self, settings: Devices) {
		let changed = self.settings.send_if_modified(|current| {
			if *current == settings {
				return false;
			}
			*current = settings;
			true
		});
		if changed {
			self.gate.revision.fetch_add(1, Ordering::AcqRel);
			self.thread.unpark();
		}
	}
	/// Reopen both native devices on the audio worker without blocking the UI or callback.
	/// Used by the explicitly armed voice soak watchdog after a sustained silent capture.
	pub fn reopen_devices(&self) {
		self.gate.revision.fetch_add(1, Ordering::AcqRel);
		self.thread.unpark();
	}
	pub fn set_ready(&self, ready: bool) {
		self.gate.ready.store(ready, Ordering::Release);
		self.thread.unpark();
	}
	pub fn set_input_enabled(&self, enabled: bool) {
		self.gate.input_enabled.store(enabled, Ordering::Release);
	}
	pub fn is_ready(&self) -> bool {
		self.gate.ready.load(Ordering::Acquire)
			&& self.gate.acknowledged.load(Ordering::Acquire)
				== self.gate.revision.load(Ordering::Acquire)
			&& !self.gate.failed.load(Ordering::Acquire)
	}
	pub fn microphone_unavailable(&self) -> bool {
		self.gate.input_enabled.load(Ordering::Acquire) && self.gate.failed.load(Ordering::Acquire)
	}
	pub fn set_controls(&self, muted: bool, deafened: bool) {
		self.gate.muted.store(muted, Ordering::Release);
		self.gate.deafened.store(deafened, Ordering::Release);
	}
	/// Enables the input level meter (`preview_level_db`); while off the capture callback
	/// skips the per-sample level math and the reported level is silence.
	pub fn set_meter(&self, enabled: bool) {
		if !enabled {
			self.gate.preview_level.store(0, Ordering::Relaxed);
		}
		self.gate.meter.store(enabled, Ordering::Relaxed);
	}
	pub fn set_gain(&self, output: u16) {
		self.gate
			.output_gain
			.store(output.min(200), Ordering::Relaxed);
	}
	pub fn set_playback_controls(&self, output_cap_percent: u16, peak_ceiling_db_tenths: i16) {
		self.gate
			.output_cap
			.store(output_cap_percent.clamp(10, 200), Ordering::Relaxed);
		let ceiling = if peak_ceiling_db_tenths < 0 {
			(2000_i16 + peak_ceiling_db_tenths.clamp(-200, 0)) as u16
		} else {
			2000
		};
		self.gate
			.peak_ceiling_db_tenths
			.store(ceiling, Ordering::Relaxed);
	}
}
impl Drop for Audio {
	fn drop(&mut self) {
		self.gate.stopped.store(true, Ordering::Release);
		self.thread.unpark();
	}
}

/// Device-free check, used by the demo preview check, that the default voice path is raw:
/// no suppression, echo cancellation, gain control or gate, continuous 510 kb/s CBR stereo
/// Opus, and native-rate capture converted to whole 48 kHz frames.
pub fn debug_processing_check() {
	use model::voice_settings::{InputProfile, NoiseSuppression, OpusApplication, OpusSignal};
	let settings = VoiceProcessing::default();
	assert_eq!(settings.profile, InputProfile::Studio);
	assert!(settings.always_transmit && !settings.noise_suppression && !settings.use_rnnoise_vad);
	let effective = settings.effective();
	assert!(
		effective.suppression == NoiseSuppression::Off
			&& !effective.echo_cancellation
			&& !effective.automatic_gain
			&& effective.sensitivity_db.is_none()
	);
	let opus = settings.opus;
	assert_eq!(
		(opus.bitrate, opus.complexity, opus.vbr, opus.fec),
		(510_000, 10, false, false)
	);
	assert_eq!(
		(opus.application, opus.signal),
		(OpusApplication::Audio, OpusSignal::Music)
	);
	let mut resampler =
		crate::resample::Resampler::new(96_000).expect("96 kHz capture is supported");
	let mut converted = Vec::new();
	resampler.process(&[0.25; PCM_SAMPLES * 2], &mut converted);
	assert_eq!(converted.len(), PCM_SAMPLES);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn capture_sanitization_preserves_every_finite_sample_bit_for_bit() {
		let mut input = [0.0; PCM_SAMPLES];
		for (index, sample) in input.iter_mut().enumerate() {
			*sample = (index as f32 - 900.0) / 1_000.0;
		}
		let output = preserve_raw_capture(input);
		assert_eq!(
			output.map(f32::to_bits),
			input.map(f32::to_bits),
			"unity mic path must not add gain, clipping, or filtering"
		);
	}

	#[test]
	fn capture_sanitization_only_replaces_non_finite_samples() {
		let mut input = [0.25; PCM_SAMPLES];
		input[0] = f32::NAN;
		input[1] = f32::INFINITY;
		input[2] = f32::NEG_INFINITY;
		let output = preserve_raw_capture(input);
		assert_eq!(&output[..3], &[0.0, 0.0, 0.0]);
		assert!(output[3..].iter().all(|sample| *sample == 0.25));
	}
}

/// Native-rate capture: the device callback copies its first two channels into a sample
/// ring; the audio worker converts them to 48 kHz and slices 20 ms Opus frames.
pub(crate) struct CaptureInput {
	ring: rtrb::Consumer<f32>,
	resampler: crate::resample::Resampler,
	scratch: Vec<f32>,
	converted: Vec<f32>,
}

impl CaptureInput {
	/// Converted frames retained before the oldest is discarded (160 ms).
	const MAX_CONVERTED: usize = PCM_SAMPLES * 8;

	fn next_frame(&mut self, gate: &Gate) -> Option<StereoFrame> {
		if self.converted.len() < PCM_SAMPLES {
			// Whole stereo pairs only; the callback always writes pairs.
			let available = self.ring.slots() & !1;
			if available > 0
				&& let Ok(chunk) = self.ring.read_chunk(available)
			{
				let (first, second) = chunk.as_slices();
				self.scratch.clear();
				self.scratch.extend_from_slice(first);
				self.scratch.extend_from_slice(second);
				chunk.commit_all();
				self.resampler.process(&self.scratch, &mut self.converted);
				if self.converted.len() > Self::MAX_CONVERTED {
					let excess = self.converted.len() - Self::MAX_CONVERTED;
					self.converted.drain(
						..excess
							.next_multiple_of(PCM_SAMPLES)
							.min(self.converted.len()),
					);
					gate.capture_ring_drops.fetch_add(1, Ordering::Relaxed);
				}
			}
		}
		if self.converted.len() < PCM_SAMPLES {
			return None;
		}
		let mut frame = [0.0f32; PCM_SAMPLES];
		frame.copy_from_slice(&self.converted[..PCM_SAMPLES]);
		self.converted.drain(..PCM_SAMPLES);
		Some(frame)
	}
}

/// Capture and playback devices with the ring/converter and ring their callbacks use.
type OpenedDevices = (Device, Device, CaptureInput, rtrb::Producer<StereoFrame>);

fn capture_config(input_id: Option<&miniaudio::DeviceId>, rate: u32) -> DeviceConfig {
	let mut config = DeviceConfig::new(DeviceType::Capture);
	// Zero requests the endpoint's own rate and channel count: no driver or miniaudio
	// resampling, downmixing or 16-bit truncation happens before our converter. 32-bit
	// float carries 24-bit and 32-bit sources without loss.
	config.set_sample_rate(rate);
	config.set_period_size_in_milliseconds(10);
	config.capture_mut().set_format(Format::F32);
	config.capture_mut().set_channels(0);
	// None opens the system default capture device.
	config.capture_mut().set_device_id(input_id.cloned());
	// VoiceMeeter's multichannel endpoints return silence when WASAPI converts them
	// (AUTOCONVERTPCM); at their native format no conversion is requested at all.
	#[cfg(target_os = "windows")]
	config.set_wasapi_no_auto_convert_src(true);
	config
}

fn open_devices(selection: &Devices, gate: &Arc<Gate>) -> Result<OpenedDevices, &'static str> {
	let context = context()?;
	// The endpoint selected in Audio settings, else the system default microphone. Windows
	// may reorder virtual capture devices between enumerations, so the stable name wins.
	let input_id = selected_id(&context, selection.input.as_deref(), true);
	let open_error = "Could not open microphone; check device and microphone permission";
	let mut capture_device =
		Device::new(Some(context.clone()), &capture_config(input_id.as_ref(), 0))
			.map_err(|_| open_error)?;
	let resampler = match crate::resample::Resampler::new(capture_device.sample_rate()) {
		Some(resampler) => resampler,
		None => {
			// An unusual native rate: let the backend deliver 48 kHz instead.
			drop(capture_device);
			capture_device = Device::new(
				Some(context.clone()),
				&capture_config(input_id.as_ref(), RATE),
			)
			.map_err(|_| open_error)?;
			crate::resample::Resampler::new(capture_device.sample_rate()).ok_or(open_error)?
		}
	};
	// 250 ms of native-rate stereo absorbs worker scheduling hiccups at any rate.
	let ring_samples = (capture_device.sample_rate() as usize / 4 * 2).max(PCM_SAMPLES * 4);
	let (mut capture_write, capture_read) = rtrb::RingBuffer::new(ring_samples);
	let capture_input = CaptureInput {
		ring: capture_read,
		resampler,
		scratch: Vec::with_capacity(ring_samples),
		converted: Vec::with_capacity(CaptureInput::MAX_CONVERTED + PCM_SAMPLES * 8),
	};
	let capture_gate = gate.clone();
	capture_device.set_data_callback(move |device, _, input| {
		let format_code = match input.format() {
			Format::U8 => 1,
			Format::S16 => 2,
			Format::S24 => 3,
			Format::S32 => 4,
			Format::F32 => 5,
			_ => 0,
		};
		capture_gate
			.capture_callbacks
			.fetch_add(1, Ordering::Relaxed);
		capture_gate
			.capture_frames
			.fetch_add(input.frame_count() as u64, Ordering::Relaxed);
		capture_gate.capture_channels.store(
			input.channels().min(u16::MAX as u32) as u16,
			Ordering::Relaxed,
		);
		capture_gate
			.capture_sample_rate
			.store(u64::from(device.sample_rate()), Ordering::Relaxed);
		capture_gate
			.capture_format
			.store(format_code, Ordering::Relaxed);
		let channels = input.channels() as usize;
		if channels == 0 || input.format() != Format::F32 {
			return;
		}
		let samples = input.as_samples::<f32>();
		let frames = samples.len() / channels;
		let pair = |frame: &[f32]| [frame[0], if channels == 1 { frame[0] } else { frame[1] }];
		let meter = capture_gate.meter.load(Ordering::Relaxed);
		let mut sum_squares = 0.0f64;
		if meter {
			for frame in samples.chunks_exact(channels) {
				let [left, right] = pair(frame);
				sum_squares += f64::from(left).powi(2) + f64::from(right).powi(2);
			}
		}
		if capture_gate.capture() && frames > 0 {
			match capture_write.write_chunk_uninit(frames * 2) {
				Ok(chunk) => {
					chunk.fill_from_iter(samples.chunks_exact(channels).flat_map(pair));
				}
				Err(_) => {
					capture_gate
						.capture_ring_drops
						.fetch_add(1, Ordering::Relaxed);
				}
			}
		}
		// Update the meter at the device boundary so it shows whether the device is
		// delivering real signal even if the worker or network path is stalled.
		if meter && frames > 0 {
			let rms = (sum_squares / (frames * 2) as f64).sqrt();
			let db = if rms > 0.0 {
				(20.0 * rms.log10()).max(-100.0)
			} else {
				-100.0
			};
			capture_gate
				.preview_level
				.store((db + 100.0).clamp(0.0, 100.0) as u16, Ordering::Relaxed);
		}
	});

	let mut output_config = DeviceConfig::new(DeviceType::Playback);
	output_config.set_sample_rate(RATE);
	output_config.set_period_size_in_frames(PERIOD);
	output_config.playback_mut().set_format(Format::F32);
	output_config.playback_mut().set_channels(CHANNELS);
	output_config.playback_mut().set_device_id(selected_id(
		&context,
		selection.output.as_deref(),
		false,
	));
	// Keep 240 ms of bounded device-side headroom so a short worker scheduling
	// hiccup does not immediately turn into missing output samples.
	let (playback_write, mut playback_read) = rtrb::RingBuffer::new(12);
	let mut playback_device = Device::new(Some(context), &output_config)
		.map_err(|_| "Could not open speaker device; check system sound settings")?;
	let playback_gate = gate.clone();
	let mut pending = [0.0f32; PCM_SAMPLES];
	let mut pending_offset = PCM_SAMPLES;
	let mut limiter_gain = 1.0f32;
	playback_device.set_data_callback(move |_, output, _| {
		let samples = output.as_samples_mut::<f32>();
		if !playback_gate.playback() {
			samples.fill(0.0);
			pending_offset = PCM_SAMPLES;
			return;
		}
		let output_cap = playback_gate.output_cap.load(Ordering::Relaxed).min(200);
		let gain = f32::from(
			playback_gate
				.output_gain
				.load(Ordering::Relaxed)
				.min(output_cap),
		) / 100.0;
		let ceiling_db = playback_gate
			.peak_ceiling_db_tenths
			.load(Ordering::Relaxed)
			.min(2000);
		let ceiling = 10.0_f32.powf((f32::from(ceiling_db) - 2000.0) / 200.0);
		for sample in samples.as_chunks_mut::<2>().0 {
			if pending_offset >= PCM_SAMPLES {
				match playback_read.pop() {
					Ok(frame) => {
						pending = frame;
						pending_offset = 0;
					}
					Err(_) => {
						sample.fill(0.0);
						continue;
					}
				}
			}
			let left = pending[pending_offset] * gain;
			let right = pending[pending_offset + 1] * gain;
			let peak = left.abs().max(right.abs());
			let target = if peak > ceiling { ceiling / peak } else { 1.0 };
			// Instant attack prevents overshoots; gradual release avoids abrupt gain changes.
			limiter_gain = if target < limiter_gain {
				target
			} else {
				(limiter_gain + 0.002).min(target)
			};
			sample[0] = (left * limiter_gain).clamp(-ceiling, ceiling);
			sample[1] = (right * limiter_gain).clamp(-ceiling, ceiling);
			pending_offset += 2;
		}
	});
	capture_device
		.start()
		.map_err(|_| "Could not start microphone; check system microphone permission")?;
	playback_device
		.start()
		.map_err(|_| "Could not start speaker device; check system sound settings")?;
	Ok((
		capture_device,
		playback_device,
		capture_input,
		playback_write,
	))
}
