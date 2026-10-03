//! Spout2 camera source using shared D3D11 textures and hardware H.264 only.
//! The media pixels never cross to CPU memory on this path.
#![allow(unsafe_code)]

use super::super::{Frame, MAX_ENCODED_BYTES, Shared};
use crate::{video_encode::Config, video_encode::Profile};
use model::CameraQuality;
use spout2::dx::Receiver;

use spout2_sys as spout_ffi;
use std::{
	ffi::CString,
	sync::{Arc, atomic::Ordering},
	thread,
	time::{Duration, Instant},
};
use windows::{
	Win32::{
		Graphics::{
			Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL},
			Direct3D11::{
				D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
				D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11CreateDevice, ID3D11Device,
				ID3D11Texture2D,
			},
			Dxgi::{
				Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM},
				CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1,
			},
		},
		System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
	},
	core::Interface,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
// D3D11's mandatory maximum 2D texture edge. The shared Spout texture is
// imported directly; do not allocate a second full-resolution receiver copy.
const MAX_GPU_SOURCE_DIMENSION: u32 = 16_384;

fn supported_source_size(width: u32, height: u32) -> bool {
	width > 0
		&& height > 0
		&& width <= MAX_GPU_SOURCE_DIMENSION
		&& height <= MAX_GPU_SOURCE_DIMENSION
}

fn unsupported_source_size(shared: &Shared, width: u32, height: u32) -> &'static str {
	shared.set_error_detail(format!(
		"Spout reports a {width}×{height} px texture; D3D11 supports at most 16,384 px per edge. Reduce the OBS Spout output dimensions, then reconnect the sender."
	));
	"Spout sender exceeds D3D11's 16,384-pixel texture edge limit"
}

fn source_frame_rate(reported: f64, fallback: u8) -> Result<u8, &'static str> {
	if !reported.is_finite() || reported < 1.0 {
		return Ok(fallback.max(1));
	}
	let rounded = reported.round();
	if rounded > f64::from(u8::MAX) {
		return Err("Spout frame rate exceeds the Discord camera metadata limit");
	}
	Ok(rounded as u8)
}

fn encoded_size(width: u32, height: u32) -> Result<(u32, u32), &'static str> {
	if width == 0 || height == 0 {
		return Err("Spout sender reported an empty texture");
	}
	// H.264/H.265 4:2:0 surfaces require even dimensions. Preserve every
	// source pixel and pad only the final row/column on the GPU when odd.
	let encoded_width = width
		.checked_add(width % 2)
		.ok_or("Spout input width cannot be represented by the video encoder")?;
	let encoded_height = height
		.checked_add(height % 2)
		.ok_or("Spout input height cannot be represented by the video encoder")?;
	Ok((encoded_width, encoded_height))
}

/// Spout receiver pinned to the sender's D3D11 adapter. This narrow FFI wrapper
/// exposes the SDK's existing-device constructor, which spout2-rs does not wrap.
struct GpuReceiver(*mut spout_ffi::spout_dx_t);

impl GpuReceiver {
	fn new(sender: &str, device: &ID3D11Device) -> Result<Self, &'static str> {
		let name = CString::new(sender).map_err(|_| "Invalid Spout sender name")?;
		unsafe {
			let raw = spout_ffi::spout_dx_create();
			if raw.is_null() {
				return Err("Spout2 could not allocate its DirectX receiver");
			}
			if spout_ffi::spout_dx_open_directx11(raw, device.as_raw().cast()) == 0
				|| spout_ffi::spout_dx_get_device(raw).is_null()
			{
				spout_ffi::spout_dx_destroy(raw);
				return Err("Spout2 could not bind to the sender GPU");
			}
			spout_ffi::spout_dx_set_receiver_name(raw, name.as_ptr());
			Ok(Self(raw))
		}
	}

	fn receive(&mut self) -> bool {
		unsafe { spout_ffi::spout_dx_receive_texture(self.0) != 0 }
	}

	fn connected(&self) -> bool {
		unsafe { spout_ffi::spout_dx_is_connected(self.0) != 0 }
	}

	fn take_update(&self) -> bool {
		unsafe { spout_ffi::spout_dx_is_updated(self.0) != 0 }
	}

	fn frame_new(&self) -> bool {
		unsafe { spout_ffi::spout_dx_is_frame_new(self.0) != 0 }
	}

	fn sender_fps(&self) -> f64 {
		unsafe { spout_ffi::spout_dx_get_sender_fps(self.0) }
	}
}

impl Drop for GpuReceiver {
	fn drop(&mut self) {
		// SAFETY: This receiver uniquely owns the Spout SDK object. Its external
		// D3D device outlives the receiver and is not released by Spout.
		unsafe {
			spout_ffi::spout_dx_release_receiver(self.0);
			spout_ffi::spout_dx_destroy(self.0);
		}
	}
}

/// All COM objects for one named sender. Field order keeps the Spout receiver
/// and its texture ahead of the externally-owned D3D device during teardown.
struct GpuSource {
	receiver: GpuReceiver,
	texture: ID3D11Texture2D,
	device: ID3D11Device,
	info: spout2::dx::SenderInfo,
}

impl GpuSource {
	fn connect(sender: &str, info: spout2::dx::SenderInfo) -> Result<Self, &'static str> {
		let (device, texture) = video_device_for_sender(&info)?;
		let receiver = GpuReceiver::new(sender, &device)?;
		Ok(Self {
			receiver,
			texture,
			device,
			info,
		})
	}

	fn dimensions(&self) -> (u32, u32) {
		// `SenderInfo` was checked against the imported DXGI texture descriptor
		// before this source was created. Avoid the receiver's independent size
		// getters, which can briefly report stale dimensions during sender updates.
		(self.info.width, self.info.height)
	}
}

fn video_device_for_sender(
	sender: &spout2::dx::SenderInfo,
) -> Result<(ID3D11Device, ID3D11Texture2D), &'static str> {
	if sender.format != DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32
		&& sender.format != DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32
	{
		return Err("Spout sender must use a supported 8-bit BGRA or RGBA GPU texture");
	}
	if sender.share_handle.is_null() {
		return Err("Spout sender did not expose a shared GPU texture");
	}
	// Discover the adapter that owns the sender texture instead of assuming Windows'
	// default GPU. Spout's ReceiveTexture call maintains its shared-resource access
	// synchronization before the video processor reads this imported texture.
	unsafe {
		let factory: IDXGIFactory1 =
			CreateDXGIFactory1().map_err(|_| "Windows GPU enumeration is unavailable")?;
		let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
		for index in 0..16 {
			let Ok(adapter) = factory.EnumAdapters1(index) else {
				break;
			};
			let adapter: IDXGIAdapter = adapter
				.cast()
				.map_err(|_| "Spout GPU adapter could not be opened")?;
			let mut video_device = None;
			let mut feature_level = D3D_FEATURE_LEVEL::default();
			if D3D11CreateDevice(
				Some(&adapter),
				D3D_DRIVER_TYPE_UNKNOWN,
				Default::default(),
				flags,
				None,
				D3D11_SDK_VERSION,
				Some(&mut video_device),
				Some(&mut feature_level),
				None,
			)
			.is_err()
			{
				continue;
			}
			let Some(video_device) = video_device else {
				continue;
			};
			let mut sender_texture: Option<ID3D11Texture2D> = None;
			if video_device
				.OpenSharedResource(
					windows::Win32::Foundation::HANDLE(sender.share_handle),
					&mut sender_texture,
				)
				.is_ok() && let Some(sender_texture) = sender_texture
			{
				let mut desc = D3D11_TEXTURE2D_DESC::default();
				sender_texture.GetDesc(&mut desc);
				if (desc.Width, desc.Height, desc.Format.0 as u32)
					== (sender.width, sender.height, sender.format)
				{
					return Ok((video_device, sender_texture));
				}
			}
		}
		Err("No video-capable GPU could open the Spout sender texture")
	}
}

struct ComRuntime;
impl ComRuntime {
	fn open() -> Result<Self, &'static str> {
		// SAFETY: The camera worker owns this COM apartment for the full Spout session.
		unsafe {
			CoInitializeEx(None, COINIT_MULTITHREADED)
				.ok()
				.map_err(|_| "Spout GPU worker could not initialize COM")?;
		}
		Ok(Self)
	}
}
impl Drop for ComRuntime {
	fn drop(&mut self) {
		// SAFETY: Balanced with ComRuntime::open on this worker thread.
		unsafe { CoUninitialize() };
	}
}

pub(in crate::camera) fn run(
	shared: &Shared,
	sender: &str,
	quality: CameraQuality,
	on_frame: &Arc<dyn Fn(Frame) + Send + Sync>,
	wake: &Arc<dyn Fn() + Send + Sync>,
) -> Result<(), &'static str> {
	if sender.is_empty() || sender.len() > 256 || sender.contains('\0') {
		return Err("Invalid Spout sender name");
	}
	let _com = ComRuntime::open()?;
	let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
	let discovery = Receiver::new(Some(sender)).map_err(|_| "Spout2 could not initialize")?;
	let sender_info = loop {
		if let Ok(info) = discovery.sender_info(sender) {
			break info;
		}
		if Instant::now() >= connect_deadline {
			return Err("Spout sender did not become available");
		}
		thread::sleep(Duration::from_millis(100));
	};
	let mut source = GpuSource::connect(sender, sender_info)?;
	let (mut source_width, mut source_height) = source.dimensions();
	if !supported_source_size(source_width, source_height) {
		return Err(unsupported_source_size(shared, source_width, source_height));
	}
	let (mut width, mut height) = if quality.spout_auto_resolution {
		encoded_size(source_width, source_height)?
	} else {
		quality.dimensions()
	};
	let mut frames_per_second = if quality.spout_auto_fps {
		source_frame_rate(source.receiver.sender_fps(), quality.frames_per_second)?
	} else {
		quality.frames_per_second
	};
	let mut frame_interval = Duration::from_secs_f64(1.0 / f64::from(frames_per_second));
	let mut config = Config {
		width,
		height,
		fps: u32::from(frames_per_second),
		bit_rate: u32::from(quality.bitrate_kbps) * 1000,
		max_bytes: MAX_ENCODED_BYTES,
		profile: Profile::Baseline,
	};
	let mut pipeline: Option<(
		crate::video_encode::windows::GpuConverter,
		crate::video_encode::windows::Encoder,
		(u32, u32),
	)> = None;
	let mut last_output = Instant::now() - frame_interval;
	// Poll at the camera rate to avoid locking a shared frame the encoder will
	// immediately discard. ReceiveTexture also applies Spout's resource access sync.
	let mut next_receive = Instant::now();
	let mut connected_once = false;
	let mut last_sender_probe = Instant::now();
	let mut encoded_frames = 0_u64;
	let mut keyframe_interval = u64::from(config.fps.max(1)) * 2;
	while !shared.stopped.load(Ordering::Acquire) {
		// Sender processes can restart while the camera is selected. Rebind on
		// handle/format/size changes so a same-name sender on another adapter does
		// not leave the old D3D device attached indefinitely.
		if last_sender_probe.elapsed() >= Duration::from_millis(500) {
			last_sender_probe = Instant::now();
			if let Ok(info) = discovery.sender_info(sender)
				&& info != source.info
			{
				pipeline = None;
				drop(source);
				source = GpuSource::connect(sender, info)?;
			}
			let (input_width, input_height) = source.dimensions();
			if !supported_source_size(input_width, input_height) {
				return Err(unsupported_source_size(shared, input_width, input_height));
			}
			let (output_width, output_height) = if quality.spout_auto_resolution {
				encoded_size(input_width, input_height)?
			} else {
				quality.dimensions()
			};
			let input_fps = if quality.spout_auto_fps {
				source_frame_rate(source.receiver.sender_fps(), frames_per_second)?
			} else {
				quality.frames_per_second
			};
			if (input_width, input_height, input_fps)
				!= (source_width, source_height, frames_per_second)
			{
				source_width = input_width;
				source_height = input_height;
				width = output_width;
				height = output_height;
				frames_per_second = input_fps;
				frame_interval = Duration::from_secs_f64(1.0 / f64::from(frames_per_second));
				config.width = width;
				config.height = height;
				config.fps = u32::from(frames_per_second);
				keyframe_interval = u64::from(config.fps.max(1)) * 2;
				pipeline = None;
				encoded_frames = 0;
				next_receive = Instant::now();
				last_output = Instant::now() - frame_interval;
			}
		}
		let now = Instant::now();
		if now < next_receive {
			thread::sleep(next_receive - now);
			continue;
		}
		next_receive = now + frame_interval;
		if !source.receiver.receive() {
			if !source.receiver.connected() && !connected_once && Instant::now() >= connect_deadline
			{
				return Err("Spout sender did not become available");
			}
			thread::sleep(Duration::from_millis(if source.receiver.connected() {
				3
			} else {
				40
			}));
			continue;
		}
		if source.receiver.take_update() {
			// Spout also raises this once when a receiver first attaches. The first
			// ReceiveTexture call after that signal does not copy a frame, so refresh
			// metadata before retrying and only rebuild when the sender really changed.
			let update_deadline = Instant::now() + CONNECT_TIMEOUT;
			let info = loop {
				if let Ok(info) = discovery.sender_info(sender) {
					break info;
				}
				if Instant::now() >= update_deadline {
					return Err("Spout sender metadata stayed unavailable after a texture update");
				}
				thread::sleep(Duration::from_millis(25));
			};
			if info != source.info {
				pipeline = None;
				drop(source);
				source = GpuSource::connect(sender, info)?;
				last_sender_probe = Instant::now() - Duration::from_millis(500);
			}
			// The update was the initial receiver handshake or a metadata refresh
			// that preserved the same shared resource. Read the next copied frame.
			continue;
		}
		if !source.receiver.frame_new() {
			continue;
		}
		connected_once = true;
		if last_output.elapsed() < frame_interval {
			continue;
		}
		let (source_width, source_height) = source.dimensions();
		if !supported_source_size(source_width, source_height) {
			return Err(unsupported_source_size(shared, source_width, source_height));
		}
		let input_size = (source_width, source_height);
		if pipeline
			.as_ref()
			.is_none_or(|(_, _, size)| *size != input_size)
		{
			let converter = crate::video_encode::windows::GpuConverter::new(
				&source.device,
				&source.texture,
				(width, height),
				config.fps,
				quality.image_controls,
			)
			.map_err(|_| "GPU texture conversion is unavailable for this Spout sender")?;
			let encoder = crate::video_encode::windows::Encoder::new_gpu_codec(
				config,
				&source.device,
				quality.codec,
			)?;
			pipeline = Some((converter, encoder, input_size));
		}
		let Some((converter, encoder, _)) = pipeline.as_mut() else {
			return Err("Spout GPU pipeline could not initialize");
		};
		let nv12 = converter
			.convert()
			.map_err(|_| "Spout GPU frame conversion failed")?;
		let force_keyframe =
			encoded_frames == 0 || encoded_frames.is_multiple_of(keyframe_interval);
		encoded_frames = encoded_frames.wrapping_add(1);
		let (h264, _) = encoder
			.encode_surface(nv12, force_keyframe)
			.map_err(|_| "Spout GPU hardware encoding failed")?;
		if h264.len() > MAX_ENCODED_BYTES {
			return Err("Spout encoded frame exceeds the camera media budget");
		}
		if h264.is_empty() || shared.stopped.load(Ordering::Acquire) {
			continue;
		}
		last_output = Instant::now();
		on_frame(Frame {
			rgb: None,
			data: h264,
			codec: quality.codec,
			width,
			height,
			frames_per_second,
			bitrate_kbps: quality.bitrate_kbps,
		});
		shared.active.store(true, Ordering::Release);
		wake();
	}
	Ok(())
}
