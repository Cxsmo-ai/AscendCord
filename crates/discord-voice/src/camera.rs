//! User-started, ephemeral camera capture. No capture stream starts before `start`.

use openh264::{
	OpenH264API,
	encoder::{BitRate, Encoder, EncoderConfig, FrameRate, Profile},
	formats::{RgbSliceU8, YUVBuffer},
};
use std::sync::{
	Arc, Mutex,
	atomic::{AtomicBool, Ordering},
};
use std::thread;

#[cfg(target_os = "linux")]
#[path = "camera/encode_linux.rs"]
mod encode_linux;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 480;
// Permit bounded 1440p/4K intra frames; transport still limits one frame and its RTP queue.
pub const MAX_ENCODED_BYTES: usize = 2 * 1024 * 1024;
pub const SUPPORTED: bool = cfg!(any(
	target_os = "macos",
	target_os = "windows",
	target_os = "linux"
));
// Includes asynchronous teardown: rapid toggles cannot accumulate camera workers.
static RUNNING: AtomicBool = AtomicBool::new(false);

pub struct Frame {
	/// Omitted for GPU-only sources such as Spout; encoding never maps pixels back to CPU.
	pub rgb: Option<Vec<u8>>,
	/// H.264 or H.265 access unit, identified by `codec`.
	pub data: Vec<u8>,
	pub codec: model::CameraCodec,
	pub width: u32,
	pub height: u32,
	pub frames_per_second: u8,
	pub bitrate_kbps: u16,
}

#[derive(Default)]
struct Shared {
	stopped: AtomicBool,
	active: AtomicBool,
	finished: AtomicBool,
	error: Mutex<Option<&'static str>>,
	error_detail: Mutex<Option<String>>,
}

pub struct Camera {
	shared: Arc<Shared>,
}

/// Device identities and friendly labels, limited to 64 entries and 272 KiB total.
pub type DeviceList = Vec<(String, String)>;

/// Enumeration never opens a capture stream.
pub fn devices() -> Result<DeviceList, &'static str> {
	#[cfg(target_os = "windows")]
	{
		windows::devices()
	}
	#[cfg(target_os = "macos")]
	{
		objc2::rc::autoreleasepool(|_| macos::devices())
	}
	#[cfg(target_os = "linux")]
	{
		linux::devices()
	}
	#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
	{
		Err("Camera selection is unavailable on this platform")
	}
}

impl Camera {
	/// Call only after an explicit camera-on gesture in a call or settings preview.
	pub fn start(
		device: Option<String>,
		quality: model::CameraQuality,
		on_frame: Arc<dyn Fn(Frame) + Send + Sync>,
		wake: Arc<dyn Fn() + Send + Sync>,
	) -> Result<Self, &'static str> {
		if !SUPPORTED {
			return Err("Camera capture is unavailable on this platform");
		}
		if !quality.is_valid() {
			return Err("Invalid camera quality settings");
		}
		if device
			.as_ref()
			.is_some_and(|id| id.len() > 4096 || id.contains('\0'))
		{
			return Err("Invalid camera device selection");
		}
		if RUNNING
			.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
			.is_err()
		{
			return Err("Previous camera session is still closing; try again shortly");
		}
		let shared = Arc::new(Shared::default());
		let worker = shared.clone();
		if thread::Builder::new()
			.name("serein-camera".into())
			.spawn(move || {
				let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
					#[cfg(target_os = "macos")]
					{
						objc2::rc::autoreleasepool(|_| {
							macos::run(&worker, device.as_deref(), quality, &on_frame, &wake)
						})
					}
					#[cfg(any(target_os = "windows", target_os = "linux"))]
					{
						run(&worker, device.as_deref(), quality, &on_frame, &wake)
					}
					#[cfg(not(any(
						target_os = "macos",
						target_os = "windows",
						target_os = "linux"
					)))]
					Err("Camera capture is unavailable on this platform")
				}))
				.unwrap_or(Err("Camera worker failed"));
				worker.active.store(false, Ordering::Release);
				if !worker.stopped.load(Ordering::Acquire)
					&& let Err(error) = result
					&& let Ok(mut slot) = worker.error.lock()
				{
					*slot = Some(error);
				}
				worker.finished.store(true, Ordering::Release);
				RUNNING.store(false, Ordering::Release);
				wake();
			})
			.is_err()
		{
			RUNNING.store(false, Ordering::Release);
			return Err("Camera worker could not start");
		}
		Ok(Self { shared })
	}

	pub fn stop(&self) {
		self.shared.stopped.store(true, Ordering::Release);
		self.shared.active.store(false, Ordering::Release);
	}

	pub fn stopped(&self) -> bool {
		self.shared.finished.load(Ordering::Acquire)
	}

	pub fn error(&self) -> Option<&'static str> {
		*self.shared.error.lock().ok()?
	}

	/// Optional source-specific detail for errors whose useful context is dynamic.
	pub fn error_detail(&self) -> Option<String> {
		self.shared.error_detail.lock().ok()?.clone()
	}

	pub fn active(&self) -> bool {
		!self.shared.stopped.load(Ordering::Acquire) && self.shared.active.load(Ordering::Acquire)
	}
}

impl Shared {
	#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
	pub(super) fn set_error_detail(&self, detail: String) {
		if let Ok(mut slot) = self.error_detail.lock() {
			*slot = Some(detail);
		}
	}
}

/// Camera encoder preferring the platform hardware H.264 encoder (VideoToolbox on macOS,
/// Media Foundation on Windows, VA-API or NVENC through GStreamer on Linux) and falling back
/// to openh264 when it is unavailable or fails mid-stream. Baseline profile and one IDR per
/// picture either way, so the wire format does not change.
struct CameraEncoder {
	quality: model::CameraQuality,
	diagnostics: crate::diagnostics::EncoderRegistration,
	software: Option<Encoder>,
	yuv: YUVBuffer,
	#[cfg(any(target_os = "macos", target_os = "windows"))]
	hardware: Option<crate::video_encode::hardware::Encoder>,
	#[cfg(target_os = "linux")]
	hardware: Option<encode_linux::Encoder>,
}

impl CameraEncoder {
	fn new(quality: model::CameraQuality) -> Result<Self, &'static str> {
		let (width, height) = quality.dimensions();
		#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
		let config = crate::video_encode::Config {
			width,
			height,
			fps: u32::from(quality.frames_per_second),
			bit_rate: u32::from(quality.bitrate_kbps) * 1000,
			max_bytes: MAX_ENCODED_BYTES,
			profile: crate::video_encode::Profile::Baseline,
		};
		#[cfg(target_os = "macos")]
		let hardware = crate::video_encode::hardware::Encoder::new(
			config,
			crate::video_encode::SourceFormat::Rgb,
		)
		.ok();
		#[cfg(target_os = "windows")]
		let hardware = crate::video_encode::hardware::Encoder::new(config).ok();
		#[cfg(target_os = "linux")]
		let hardware = encode_linux::Encoder::new(config).ok();
		#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
		let software = if hardware.is_some() {
			None
		} else {
			Some(encoder(quality)?)
		};
		#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
		let software = Some(encoder(quality)?);
		Ok(Self {
			quality,
			diagnostics: crate::diagnostics::EncoderRegistration::new(false, software.is_none()),
			software,
			yuv: YUVBuffer::new(width as usize, height as usize),
			#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
			hardware,
		})
	}

	/// Encodes the selected bounded RGB picture and retains pixels for local preview.
	fn encode(&mut self, rgb: Vec<u8>) -> Result<Option<Frame>, &'static str> {
		let (width, height) = self.quality.dimensions();
		let (width, height) = (width as usize, height as usize);
		if rgb.len() != width.saturating_mul(height).saturating_mul(3) {
			return Err("Camera did not provide the selected bounded RGB frame");
		}
		#[cfg(target_os = "linux")]
		if let Some(hardware) = self.hardware.as_mut() {
			// ponytail: one IDR per picture, matching the software encoder, because the sender
			// drops to the latest frame; inter prediction would freeze a receiver instead.
			match hardware.encode(&rgb) {
				Ok(Some(h264)) => {
					if h264.len() > MAX_ENCODED_BYTES {
						return Err("Camera encoded frame exceeded its 2 MiB limit");
					}
					return Ok(Some(self.frame(rgb, h264)));
				}
				// The encoder is still holding this picture; the next one returns it.
				Ok(None) => return Ok(None),
				Err(_) => {
					self.hardware = None;
					self.diagnostics.set(None);
					self.software = Some(encoder(self.quality)?);
					self.diagnostics.set(Some(false));
				}
			}
		}
		#[cfg(any(target_os = "macos", target_os = "windows"))]
		if let Some(hardware) = self.hardware.as_mut() {
			// ponytail: every picture stays independently decodable, matching the software
			// encoder, because the sender drops to the latest frame; inter prediction would
			// freeze a receiver until the next refresh.
			#[cfg(target_os = "macos")]
			let encoded = hardware.encode(&rgb, (width, height), true);
			#[cfg(target_os = "windows")]
			let encoded = {
				use openh264::formats::YUVSource;
				self.yuv.read_rgb8(RgbSliceU8::new(&rgb, (width, height)));
				hardware.encode(self.yuv.y(), self.yuv.u(), self.yuv.v(), true)
			};
			match encoded {
				Ok((h264, _)) => {
					if h264.len() > MAX_ENCODED_BYTES {
						return Err("Camera encoded frame exceeded its 2 MiB limit");
					}
					return Ok((!h264.is_empty()).then(|| self.frame(rgb, h264)));
				}
				Err(_) => {
					self.hardware = None;
					self.diagnostics.set(None);
					self.software = Some(encoder(self.quality)?);
					self.diagnostics.set(Some(false));
				}
			}
		}
		let software = self
			.software
			.as_mut()
			.ok_or("Camera H264 encoder could not start")?;
		encode_rgb(software, &mut self.yuv, rgb, width, height, self.quality)
	}

	fn frame(&self, rgb: Vec<u8>, data: Vec<u8>) -> Frame {
		Frame {
			rgb: Some(rgb),
			data,
			codec: model::CameraCodec::H264,
			width: self.quality.dimensions().0,
			height: self.quality.dimensions().1,
			frames_per_second: self.quality.frames_per_second,
			bitrate_kbps: self.quality.bitrate_kbps,
		}
	}
}

fn encoder(quality: model::CameraQuality) -> Result<Encoder, &'static str> {
	Encoder::with_api_config(
		OpenH264API::from_source(),
		EncoderConfig::new()
			.bitrate(BitRate::from_bps(u32::from(quality.bitrate_kbps) * 1000))
			.max_frame_rate(FrameRate::from_hz(f32::from(quality.frames_per_second)))
			.profile(Profile::Baseline)
			.num_threads(1)
			.debug(false),
	)
	.map_err(|_| "Camera H264 encoder could not start")
}

fn encode_rgb(
	encoder: &mut Encoder,
	yuv: &mut YUVBuffer,
	rgb: Vec<u8>,
	width: usize,
	height: usize,
	quality: model::CameraQuality,
) -> Result<Option<Frame>, &'static str> {
	if rgb.len() != width.saturating_mul(height).saturating_mul(3) {
		return Err("Camera did not provide the selected bounded RGB frame");
	}
	yuv.read_rgb8(RgbSliceU8::new(&rgb, (width, height)));
	// ponytail: independently decodable frames tolerate latest-slot drops;
	// add feedback-aware inter frames when bandwidth adaptation is implemented.
	encoder.force_intra_frame();
	let bits = encoder
		.encode(yuv)
		.map_err(|_| "Camera frame could not be encoded")?;
	if bits.raw_info().iFrameSizeInBytes < 0
		|| bits.raw_info().iFrameSizeInBytes as usize > MAX_ENCODED_BYTES
	{
		return Err("Camera encoded frame exceeded its 2 MiB limit");
	}
	let h264 = bits.to_vec();
	let (width, height) = quality.dimensions();
	Ok((!h264.is_empty()).then_some(Frame {
		rgb: Some(rgb),
		data: h264,
		codec: model::CameraCodec::H264,
		width,
		height,
		frames_per_second: quality.frames_per_second,
		bitrate_kbps: quality.bitrate_kbps,
	}))
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn run(
	shared: &Shared,
	device: Option<&str>,
	quality: model::CameraQuality,
	on_frame: &Arc<dyn Fn(Frame) + Send + Sync>,
	wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<(), &'static str> {
	#[cfg(target_os = "windows")]
	if let Some(sender) = device.and_then(|id| id.strip_prefix("spout:")) {
		return windows::spout::run(shared, sender, quality, on_frame, wake);
	}
	let mut encoder = CameraEncoder::new(quality)?;
	let mut emit = |rgb| {
		if !shared.stopped.load(Ordering::Acquire)
			&& let Some(frame) = encoder.encode(rgb)?
			&& !shared.stopped.load(Ordering::Acquire)
		{
			on_frame(frame);
			shared.active.store(true, Ordering::Release);
			wake();
		}
		Ok(())
	};
	#[cfg(target_os = "windows")]
	{
		windows::run(shared, device, quality, &mut emit)
	}
	#[cfg(target_os = "linux")]
	{
		linux::run(shared, device, quality, &mut emit)
	}
}

impl Drop for Camera {
	fn drop(&mut self) {
		self.stop();
	}
}

#[cfg(target_os = "macos")]
mod macos {
	#![allow(unsafe_code)]

	use super::*;
	use block2::RcBlock;
	use dispatch2::{DispatchQueue, DispatchRetained};
	use objc2::{
		AnyThread, DefinedClass, define_class, msg_send,
		rc::Retained,
		runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject},
	};
	use objc2_av_foundation::*;
	use objc2_core_media::CMSampleBuffer;
	use objc2_core_video::*;
	use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
	use std::{
		sync::mpsc::{self, Receiver, SyncSender},
		time::{Duration, Instant},
	};

	const DENIED: &str = "Camera access denied. Allow AscendCord (or your terminal) in System Settings > Privacy & Security > Camera, then try again.";

	pub(super) fn devices() -> Result<DeviceList, &'static str> {
		// SAFETY: Framework-owned device types and discovery only; no stream or permission request.
		unsafe {
			let types = NSArray::from_slice(&[
				AVCaptureDeviceTypeBuiltInWideAngleCamera,
				AVCaptureDeviceTypeExternal,
				AVCaptureDeviceTypeContinuityCamera,
				AVCaptureDeviceTypeDeskViewCamera,
			]);
			let discovery =
				AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
					&types,
					Some(AVMediaTypeVideo.ok_or("Camera media type unavailable")?),
					AVCaptureDevicePosition::Unspecified,
				);
			Ok(discovery
				.devices()
				.iter()
				.take(32)
				.filter_map(|device| {
					let id = device.uniqueID();
					let name = device.localizedName();
					if id.length() > 4096 || name.length() > 256 {
						return None;
					}
					let (id, name) = (id.to_string(), name.to_string());
					(id.len() <= 4096 && name.len() <= 256 && !id.contains('\0'))
						.then_some((id, name))
				})
				.collect())
		}
	}

	struct DelegateState {
		send: SyncSender<Result<Vec<u8>, &'static str>>,
		shared: Arc<Shared>,
		last: Mutex<Instant>,
		interval: Duration,
		dimensions: (usize, usize),
	}

	define_class!(
		// SAFETY: NSObject superclass, initialized Send + Sync ivars, and the
		// exact AVFoundation delegate signature. AVFoundation uses a serial queue.
		#[unsafe(super = NSObject)]
		#[ivars = DelegateState]
		struct AscendCordCameraDelegate;

		unsafe impl NSObjectProtocol for AscendCordCameraDelegate {}
		unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for AscendCordCameraDelegate {
			#[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
			fn capture(
				&self,
				_output: &AVCaptureOutput,
				sample: &CMSampleBuffer,
				_connection: &AVCaptureConnection,
			) {
				let state = self.ivars();
				if state.shared.stopped.load(Ordering::Acquire) {
					return;
				}
				let Ok(mut last) = state.last.try_lock() else {
					return;
				};
				if last.elapsed() < state.interval {
					return;
				}
				*last = Instant::now();
				let _ = state.send.try_send(copy_bgra(sample, state.dimensions));
			}
		}
	);

	fn copy_bgra(
		sample: &CMSampleBuffer,
		dimensions: (usize, usize),
	) -> Result<Vec<u8>, &'static str> {
		let (width, height) = dimensions;
		// SAFETY: The sample is valid for this delegate invocation. Retain its image,
		// verify packed BGRA dimensions/stride before reading, and unlock every path.
		unsafe {
			let pixels = sample.image_buffer().ok_or("Camera returned no image")?;
			let stride = CVPixelBufferGetBytesPerRow(&pixels);
			let bytes = stride
				.checked_mul(height)
				.ok_or("Camera frame exceeds bounds")?;
			if CVPixelBufferGetWidth(&pixels) != width
				|| CVPixelBufferGetHeight(&pixels) != height
				|| CVPixelBufferGetPixelFormatType(&pixels) != kCVPixelFormatType_32BGRA
				|| !(width * 4..=width * 4 + 4096).contains(&stride)
				|| bytes > CVPixelBufferGetDataSize(&pixels)
			{
				return Err("Camera did not provide the selected bounded BGRA frame");
			}
			if CVPixelBufferLockBaseAddress(&pixels, CVPixelBufferLockFlags::ReadOnly) != 0 {
				return Err("Camera image could not be read");
			}
			let base = CVPixelBufferGetBaseAddress(&pixels);
			let result = if base.is_null() {
				Err("Camera returned an empty image")
			} else {
				let source = std::slice::from_raw_parts(base.cast::<u8>(), bytes);
				let mut bgra = vec![0; width * height * 4];
				for (row, dest) in source
					.chunks_exact(stride)
					.zip(bgra.chunks_exact_mut(width * 4))
				{
					dest.copy_from_slice(&row[..width * 4]);
				}
				Ok(bgra)
			};
			CVPixelBufferUnlockBaseAddress(&pixels, CVPixelBufferLockFlags::ReadOnly);
			result
		}
	}

	fn authorize(shared: &Shared) -> Result<(), &'static str> {
		// SAFETY: Framework-owned constant, class methods callable from this worker;
		// AVFoundation copies the block, which owns only a bounded result sender.
		let receive = unsafe {
			let media = AVMediaTypeVideo.ok_or("macOS camera authorization is unavailable")?;
			match AVCaptureDevice::authorizationStatusForMediaType(media) {
				AVAuthorizationStatus::Authorized => return Ok(()),
				AVAuthorizationStatus::NotDetermined => {
					let (send, receive) = mpsc::sync_channel(1);
					let block = RcBlock::new(move |granted: Bool| {
						let _ = send.try_send(granted.as_bool());
					});
					AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block);
					receive
				}
				_ => return Err(DENIED),
			}
		};
		let deadline = Instant::now() + Duration::from_secs(20);
		while !shared.stopped.load(Ordering::Acquire) && Instant::now() < deadline {
			match receive.recv_timeout(Duration::from_millis(100)) {
				Ok(true) => return Ok(()),
				Ok(false) => return Err(DENIED),
				Err(mpsc::RecvTimeoutError::Disconnected) => {
					return Err("Camera permission request failed");
				}
				Err(mpsc::RecvTimeoutError::Timeout) => {}
			}
		}
		Err("Camera permission canceled or timed out; respond to the macOS prompt and try again")
	}

	struct CaptureSession {
		session: Retained<AVCaptureSession>,
		output: Retained<AVCaptureVideoDataOutput>,
		_delegate: Retained<AscendCordCameraDelegate>,
		queue: DispatchRetained<DispatchQueue>,
	}
	impl Drop for CaptureSession {
		fn drop(&mut self) {
			// SAFETY: Owned session is configured, and teardown runs on its worker.
			unsafe {
				self.output.setSampleBufferDelegate_queue(None, None);
				self.session.stopRunning();
			}
			self.queue.exec_sync(|| {});
		}
	}

	pub(super) fn run(
		shared: &Arc<Shared>,
		selected: Option<&str>,
		quality: model::CameraQuality,
		on_frame: &Arc<dyn Fn(Frame) + Send + Sync>,
		wake: &Arc<dyn Fn() + Send + Sync>,
	) -> Result<(), &'static str> {
		authorize(shared)?;
		if shared.stopped.load(Ordering::Acquire) {
			return Ok(());
		}
		let mut encoder = CameraEncoder::new(quality)?;
		let dimensions = quality.dimensions();
		let dimensions = (dimensions.0 as usize, dimensions.1 as usize);
		let interval = Duration::from_secs_f64(1.0 / f64::from(quality.frames_per_second));
		let (send, receive) = mpsc::sync_channel(1);
		let queue = DispatchQueue::new("serein.camera.frames", None);
		// SAFETY: Only this worker configures/owns the session. Delegate lives until
		// capture is stopped and the serial callback queue has drained.
		let capture = unsafe {
			let media = AVMediaTypeVideo.ok_or("Camera media type unavailable")?;
			let device = match selected {
				Some(id) => AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(id))
					.filter(|device| device.hasMediaType(media))
					.ok_or(
						"Selected camera is disconnected or unavailable. Refresh cameras and choose another device.",
					)?,
				None => AVCaptureDevice::defaultDeviceWithMediaType(media)
					.ok_or("No camera is available")?,
			};
			let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
				.map_err(|_| "Camera is busy or unavailable")?;
			let session = AVCaptureSession::new();
			let output = AVCaptureVideoDataOutput::new();
			if !session.canAddInput(&input) || !session.canAddOutput(&output) {
				return Err("Camera cannot join capture session");
			}
			session.addInput(&input);
			session.addOutput(&output);
			let preset = match quality.resolution {
				model::CameraResolution::Sd => AVCaptureSessionPreset640x480,
				model::CameraResolution::Hd => AVCaptureSessionPreset1280x720,
				model::CameraResolution::FullHd => AVCaptureSessionPreset1920x1080,
				model::CameraResolution::QuadHd | model::CameraResolution::UltraHd => {
					AVCaptureSessionPreset3840x2160
				}
			};
			if !session.canSetSessionPreset(preset) {
				return Err("Selected camera does not support the requested resolution");
			}
			session.setSessionPreset(preset);
			let format = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
			let width = NSNumber::new_usize(dimensions.0);
			let height = NSNumber::new_usize(dimensions.1);
			let format_key = NSString::from_str(&kCVPixelBufferPixelFormatTypeKey.to_string());
			let width_key = NSString::from_str(&kCVPixelBufferWidthKey.to_string());
			let height_key = NSString::from_str(&kCVPixelBufferHeightKey.to_string());
			// The session preset alone does not fix the video data output dimensions.
			let settings = NSDictionary::from_slices(
				&[&*format_key, &*width_key, &*height_key],
				&[&*format as &AnyObject, &*width, &*height],
			);
			output.setVideoSettings(Some(&settings));
			output.setAlwaysDiscardsLateVideoFrames(true);
			let allocated = AscendCordCameraDelegate::alloc().set_ivars(DelegateState {
				send,
				shared: shared.clone(),
				last: Mutex::new(Instant::now() - interval),
				interval,
				dimensions,
			});
			let delegate: Retained<AscendCordCameraDelegate> = msg_send![super(allocated), init];
			output.setSampleBufferDelegate_queue(
				Some(ProtocolObject::from_ref(&*delegate)),
				Some(&queue),
			);
			let capture = CaptureSession {
				session,
				output,
				_delegate: delegate,
				queue,
			};
			if !shared.stopped.load(Ordering::Acquire) {
				capture.session.startRunning();
			}
			capture
		};
		let result = encode_loop(shared, on_frame, wake, &receive, &mut encoder, quality);
		drop(capture);
		result
	}

	fn encode_loop(
		shared: &Shared,
		on_frame: &Arc<dyn Fn(Frame) + Send + Sync>,
		wake: &Arc<dyn Fn() + Send + Sync>,
		receive: &Receiver<Result<Vec<u8>, &'static str>>,
		encoder: &mut CameraEncoder,
		quality: model::CameraQuality,
	) -> Result<(), &'static str> {
		let mut last_frame = Instant::now();
		while !shared.stopped.load(Ordering::Acquire) {
			let bgra = match receive.recv_timeout(Duration::from_millis(100)) {
				Ok(frame) => frame?,
				Err(mpsc::RecvTimeoutError::Timeout)
					if last_frame.elapsed() < Duration::from_secs(5) =>
				{
					continue;
				}
				_ => {
					return Err("Camera stopped delivering frames; check the device and try again");
				}
			};
			last_frame = Instant::now();
			let (width, height) = quality.dimensions();
			let mut rgb = vec![0; width as usize * height as usize * 3];
			for (bgra, rgb) in bgra
				.as_chunks::<4>()
				.0
				.iter()
				.zip(rgb.as_chunks_mut::<3>().0)
			{
				rgb.copy_from_slice(&[bgra[2], bgra[1], bgra[0]]);
			}
			let Some(frame) = encoder.encode(rgb)? else {
				continue;
			};
			if shared.stopped.load(Ordering::Acquire) {
				break;
			}
			on_frame(frame);
			shared.active.store(true, Ordering::Release);
			wake();
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn camera_rejects_unbounded_or_nul_device_ids_before_starting_worker() {
		for id in ["x".repeat(4097), "dshow:bad\0id".into()] {
			let error = Camera::start(
				Some(id),
				model::CameraQuality::default(),
				Arc::new(|_| panic!("no capture")),
				Arc::new(|| {}),
			)
			.err();
			if SUPPORTED {
				assert_eq!(error, Some("Invalid camera device selection"));
			} else {
				assert!(error.is_some());
			}
		}
	}
	use openh264::formats::YUVSource;

	#[test]
	fn hardware_camera_frames_decode_and_stay_independently_decodable() {
		let mut encoder = CameraEncoder::new(model::CameraQuality::default()).unwrap();
		// Exactly one encoder is live: hardware when the machine offers it, openh264 otherwise.
		#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
		assert_eq!(encoder.hardware.is_some(), encoder.software.is_none());
		for length in [0, WIDTH * HEIGHT * 3 - 1, WIDTH * HEIGHT * 3 + 1] {
			assert!(encoder.encode(vec![0; length]).is_err());
		}
		let mut decoder = openh264::decoder::Decoder::new().unwrap();
		for value in [0, 96, 255] {
			let mut rgb = vec![value; WIDTH * HEIGHT * 3];
			// Flat pictures compress to almost nothing; vary one row so the size check bites.
			for (index, pixel) in rgb
				.as_chunks_mut::<3>()
				.0
				.iter_mut()
				.take(WIDTH)
				.enumerate()
			{
				*pixel = [(index % 251) as u8, value, (index % 97) as u8];
			}
			let Some(frame) = encoder.encode(rgb).unwrap() else {
				continue;
			};
			assert_eq!(frame.rgb.as_ref().unwrap().len(), WIDTH * HEIGHT * 3);
			assert!(frame.data.len() <= MAX_ENCODED_BYTES);
			// The sender drops to the latest frame, so each picture must stand alone.
			assert!(crate::video_receive::is_keyframe(&frame.data));
			assert!(crate::video_receive::has_parameter_sets(&frame.data));
			let decoded = decoder.decode(&frame.data).unwrap().unwrap();
			assert_eq!(decoded.dimensions(), (WIDTH, HEIGHT));
		}
	}

	#[test]
	fn camera_frames_are_bounded_independently_decodable_and_stop_is_immediate() {
		let quality = model::CameraQuality::default();
		let mut encoder = encoder(quality).unwrap();
		let mut yuv = YUVBuffer::new(WIDTH, HEIGHT);
		for length in [0, WIDTH * HEIGHT * 3 - 1, WIDTH * HEIGHT * 3 + 1] {
			assert!(
				encode_rgb(
					&mut encoder,
					&mut yuv,
					vec![0; length],
					WIDTH,
					HEIGHT,
					quality
				)
				.is_err()
			);
		}
		for value in [0, 127, 255] {
			let frame = encode_rgb(
				&mut encoder,
				&mut yuv,
				vec![value; WIDTH * HEIGHT * 3],
				WIDTH,
				HEIGHT,
				quality,
			)
			.unwrap()
			.unwrap();
			assert!(frame.data.len() <= MAX_ENCODED_BYTES);
			let mut decoder = openh264::decoder::Decoder::new().unwrap();
			let decoded = decoder.decode(&frame.data).unwrap().unwrap();
			assert_eq!(decoded.dimensions(), (WIDTH, HEIGHT));
		}
		let camera = Camera {
			shared: Arc::new(Shared::default()),
		};
		camera.shared.active.store(true, Ordering::Release);
		assert!(camera.active());
		camera.stop();
		// A late native callback cannot turn a stopped camera back on.
		camera.shared.active.store(true, Ordering::Release);
		assert!(!camera.active());
		assert!(!camera.stopped());
		camera.shared.finished.store(true, Ordering::Release);
		assert!(camera.stopped());
	}
}
