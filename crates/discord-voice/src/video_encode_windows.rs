//! Bounded Media Foundation hardware H.264 encoder for Windows screen sharing and camera video.
#![allow(unsafe_code)]

use super::{Config, Profile};
use crate::screen::i420_to_nv12;
use model::{CameraCodec, CameraImageControls};
use std::{
	marker::PhantomData,
	rc::Rc,
	time::{Duration, Instant},
};
use windows::{
	Win32::{
		Graphics::{
			Direct3D11::*,
			Dxgi::Common::{
				DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8B8A8_UNORM,
				DXGI_RATIONAL, DXGI_SAMPLE_DESC,
			},
		},
		Media::MediaFoundation::*,
		System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
		System::Variant::VARIANT,
	},
	core::Interface,
};

const MAX_GPU_SOURCE_DIMENSION: u32 = 16_384;

fn supported_gpu_source_size(width: u32, height: u32) -> bool {
	width > 0
		&& height > 0
		&& width <= MAX_GPU_SOURCE_DIMENSION
		&& height <= MAX_GPU_SOURCE_DIMENSION
}

/// GPU-only Spout conversion. The shared BGRA texture is scaled and converted to
/// an NV12 video surface by D3D11's video processor; no staging texture, map, or
/// CPU pixel buffer is created on this path.
pub(crate) struct GpuConverter {
	video_context: ID3D11VideoContext,
	processor: ID3D11VideoProcessor,
	input_view: ID3D11VideoProcessorInputView,
	output: ID3D11Texture2D,
	output_view: ID3D11VideoProcessorOutputView,
}

impl GpuConverter {
	pub(crate) fn new(
		device: &ID3D11Device,
		input: &ID3D11Texture2D,
		output_size: (u32, u32),
		fps: u32,
		controls: CameraImageControls,
	) -> Result<Self, &'static str> {
		let mut input_desc = D3D11_TEXTURE2D_DESC::default();
		unsafe { input.GetDesc(&mut input_desc) };
		let input_size = (input_desc.Width, input_desc.Height);
		let input_format = input_desc.Format;
		if !supported_gpu_source_size(input_size.0, input_size.1) {
			return Err(FAILED);
		}
		if input_format != DXGI_FORMAT_B8G8R8A8_UNORM && input_format != DXGI_FORMAT_R8G8B8A8_UNORM
		{
			return Err(FAILED);
		}
		// SAFETY: All interfaces are created from Spout's live D3D11 texture device;
		// this worker owns them until its receiver and encoder are torn down.
		unsafe {
			let video_device: ID3D11VideoDevice = device.cast().map_err(|_| FAILED)?;
			let immediate = device.GetImmediateContext().map_err(|_| FAILED)?;
			let video_context: ID3D11VideoContext = immediate.cast().map_err(|_| FAILED)?;
			let description = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
				InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
				InputFrameRate: DXGI_RATIONAL {
					Numerator: fps.max(1),
					Denominator: 1,
				},
				InputWidth: input_size.0,
				InputHeight: input_size.1,
				OutputFrameRate: DXGI_RATIONAL {
					Numerator: fps.max(1),
					Denominator: 1,
				},
				OutputWidth: output_size.0,
				OutputHeight: output_size.1,
				Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
			};
			let enumerator = video_device
				.CreateVideoProcessorEnumerator(&description)
				.map_err(|_| FAILED)?;
			let input_device = input.GetDevice().map_err(|_| FAILED)?;
			if !input_device
				.cast::<ID3D11Device>()
				.is_ok_and(|input_device| input_device == *device)
			{
				return Err(FAILED);
			}
			let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
				FourCC: 0,
				ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
				Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
					Texture2D: D3D11_TEX2D_VPIV {
						MipSlice: 0,
						ArraySlice: 0,
					},
				},
			};
			let mut input_view = None;
			video_device
				.CreateVideoProcessorInputView(
					input,
					&enumerator,
					&input_desc,
					Some(&mut input_view),
				)
				.map_err(|_| FAILED)?;
			let input_view = input_view.ok_or(FAILED)?;
			let processor = video_device
				.CreateVideoProcessor(&enumerator, 0)
				.map_err(|_| FAILED)?;
			let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
			enumerator
				.GetVideoProcessorCaps(&mut caps)
				.map_err(|_| FAILED)?;
			for (filter, capability, value) in [
				(
					D3D11_VIDEO_PROCESSOR_FILTER_BRIGHTNESS,
					D3D11_VIDEO_PROCESSOR_FILTER_CAPS_BRIGHTNESS,
					controls.brightness,
				),
				(
					D3D11_VIDEO_PROCESSOR_FILTER_CONTRAST,
					D3D11_VIDEO_PROCESSOR_FILTER_CAPS_CONTRAST,
					controls.contrast,
				),
				(
					D3D11_VIDEO_PROCESSOR_FILTER_HUE,
					D3D11_VIDEO_PROCESSOR_FILTER_CAPS_HUE,
					controls.hue,
				),
				(
					D3D11_VIDEO_PROCESSOR_FILTER_SATURATION,
					D3D11_VIDEO_PROCESSOR_FILTER_CAPS_SATURATION,
					controls.saturation,
				),
			] {
				apply_signed_filter(
					&video_context,
					&processor,
					&enumerator,
					caps.FeatureCaps,
					filter,
					capability.0 as u32,
					value,
				);
			}
			for (filter, capability, value) in [
				(
					D3D11_VIDEO_PROCESSOR_FILTER_EDGE_ENHANCEMENT,
					D3D11_VIDEO_PROCESSOR_FILTER_CAPS_EDGE_ENHANCEMENT,
					controls.sharpness,
				),
				(
					D3D11_VIDEO_PROCESSOR_FILTER_NOISE_REDUCTION,
					D3D11_VIDEO_PROCESSOR_FILTER_CAPS_NOISE_REDUCTION,
					controls.noise_reduction,
				),
			] {
				apply_enhancement_filter(
					&video_context,
					&processor,
					&enumerator,
					caps.FeatureCaps,
					filter,
					capability.0 as u32,
					value,
				);
			}
			let support = enumerator
				.CheckVideoProcessorFormat(input_format)
				.map_err(|_| FAILED)?;
			if support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
				return Err(FAILED);
			}
			let support = enumerator
				.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)
				.map_err(|_| FAILED)?;
			if support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
				return Err(FAILED);
			}
			let output_desc = D3D11_TEXTURE2D_DESC {
				Width: output_size.0,
				Height: output_size.1,
				MipLevels: 1,
				ArraySize: 1,
				Format: DXGI_FORMAT_NV12,
				SampleDesc: DXGI_SAMPLE_DESC {
					Count: 1,
					Quality: 0,
				},
				Usage: D3D11_USAGE_DEFAULT,
				BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
				CPUAccessFlags: 0,
				MiscFlags: 0,
			};
			let mut output = None;
			device
				.CreateTexture2D(&output_desc, None, Some(&mut output))
				.map_err(|_| FAILED)?;
			let output = output.ok_or(FAILED)?;
			let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
				ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
				Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
					Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
				},
			};
			let mut output_view = None;
			video_device
				.CreateVideoProcessorOutputView(
					&output,
					&enumerator,
					&output_desc,
					Some(&mut output_view),
				)
				.map_err(|_| FAILED)?;
			let output_view = output_view.ok_or(FAILED)?;
			let source = windows::Win32::Foundation::RECT {
				left: 0,
				top: 0,
				right: input_size.0 as i32,
				bottom: input_size.1 as i32,
			};
			let (draw_width, draw_height) = if u64::from(input_size.0) * u64::from(output_size.1)
				> u64::from(input_size.1) * u64::from(output_size.0)
			{
				(
					output_size.0,
					(output_size.0 * input_size.1 / input_size.0) & !1,
				)
			} else {
				(
					(output_size.1 * input_size.0 / input_size.1) & !1,
					output_size.1,
				)
			};
			let destination = windows::Win32::Foundation::RECT {
				left: ((output_size.0 - draw_width) / 2) as i32,
				top: ((output_size.1 - draw_height) / 2) as i32,
				right: ((output_size.0 + draw_width) / 2) as i32,
				bottom: ((output_size.1 + draw_height) / 2) as i32,
			};
			video_context.VideoProcessorSetOutputBackgroundColor(
				&processor,
				false,
				&D3D11_VIDEO_COLOR {
					Anonymous: D3D11_VIDEO_COLOR_0 {
						RGBA: D3D11_VIDEO_COLOR_RGBA {
							R: 0.0,
							G: 0.0,
							B: 0.0,
							A: 1.0,
						},
					},
				},
			);
			video_context.VideoProcessorSetStreamFrameFormat(
				&processor,
				0,
				D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
			);
			video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source));
			video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&destination));
			Ok(Self {
				video_context,
				processor,
				input_view,
				output,
				output_view,
			})
		}
	}

	pub(crate) fn convert(&self) -> Result<&ID3D11Texture2D, &'static str> {
		// SAFETY: The cached input view owns its D3D resource reference. The cloned
		// interface is explicitly released after VideoProcessorBlt to avoid one COM
		// reference leak per frame.
		unsafe {
			let stream = D3D11_VIDEO_PROCESSOR_STREAM {
				Enable: true.into(),
				pInputSurface: std::mem::ManuallyDrop::new(Some(self.input_view.clone())),
				..Default::default()
			};
			let mut streams = [stream];
			let blit = self.video_context.VideoProcessorBlt(
				&self.processor,
				&self.output_view,
				0,
				&streams,
			);
			std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
			blit.map_err(|_| FAILED)?;
			Ok(&self.output)
		}
	}
}

fn apply_signed_filter(
	context: &ID3D11VideoContext,
	processor: &ID3D11VideoProcessor,
	enumerator: &ID3D11VideoProcessorEnumerator,
	caps: u32,
	filter: D3D11_VIDEO_PROCESSOR_FILTER,
	capability: u32,
	value: i16,
) {
	if caps & capability == 0 {
		return;
	}
	unsafe {
		let Ok(range) = enumerator.GetVideoProcessorFilterRange(filter) else {
			return;
		};
		if value == 0 {
			context.VideoProcessorSetStreamFilter(processor, 0, filter, false, range.Default);
			return;
		}
		let value = i64::from(value.clamp(-100, 100));
		let level = if value < 0 {
			i64::from(range.Default)
				+ (i64::from(range.Default) - i64::from(range.Minimum)) * value / 100
		} else {
			i64::from(range.Default)
				+ (i64::from(range.Maximum) - i64::from(range.Default)) * value / 100
		};
		context.VideoProcessorSetStreamFilter(
			processor,
			0,
			filter,
			true,
			level.clamp(i64::from(range.Minimum), i64::from(range.Maximum)) as i32,
		);
	}
}

fn apply_enhancement_filter(
	context: &ID3D11VideoContext,
	processor: &ID3D11VideoProcessor,
	enumerator: &ID3D11VideoProcessorEnumerator,
	caps: u32,
	filter: D3D11_VIDEO_PROCESSOR_FILTER,
	capability: u32,
	value: u8,
) {
	if caps & capability == 0 {
		return;
	}
	unsafe {
		let Ok(range) = enumerator.GetVideoProcessorFilterRange(filter) else {
			return;
		};
		if value == 0 {
			context.VideoProcessorSetStreamFilter(processor, 0, filter, false, range.Default);
			return;
		}
		let level = i64::from(range.Default)
			+ (i64::from(range.Maximum) - i64::from(range.Default)) * i64::from(value.min(100))
				/ 100;
		context.VideoProcessorSetStreamFilter(
			processor,
			0,
			filter,
			true,
			level.clamp(i64::from(range.Minimum), i64::from(range.Maximum)) as i32,
		);
	}
}

const UNAVAILABLE: &str = "Windows hardware video encoding is unavailable";
const FAILED: &str = "Windows hardware video encoding failed";

struct Runtime(PhantomData<Rc<()>>);

impl Runtime {
	fn open() -> Result<Self, &'static str> {
		// SAFETY: The screen encoder owns this worker thread and balances both calls in Drop.
		unsafe {
			CoInitializeEx(None, COINIT_MULTITHREADED)
				.ok()
				.map_err(|_| UNAVAILABLE)?;
			if MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).is_err() {
				CoUninitialize();
				return Err(UNAVAILABLE);
			}
		}
		Ok(Self(PhantomData))
	}
}

impl Drop for Runtime {
	fn drop(&mut self) {
		// SAFETY: Balanced with Runtime::open on the same worker thread.
		unsafe {
			let _ = MFShutdown();
			CoUninitialize();
		}
	}
}

pub(crate) struct Encoder {
	transform: IMFTransform,
	events: IMFMediaEventGenerator,
	codec: ICodecAPI,
	_activate: Activated,
	_runtime: Runtime,
	frame: i64,
	duration: i64,
	need_input: usize,
	have_output: usize,
	provides_samples: bool,
	max_bytes: usize,
	max_buffer_bytes: usize,
	dxgi_manager: Option<IMFDXGIDeviceManager>,
}

struct Activated(IMFActivate);

impl Drop for Activated {
	fn drop(&mut self) {
		// SAFETY: The activation and its object stay on the Media Foundation worker.
		unsafe {
			let _ = self.0.ShutdownObject();
		}
	}
}

impl Encoder {
	pub(crate) fn new(config: Config) -> Result<Self, &'static str> {
		Self::create(config, None, CameraCodec::H264, false)
	}

	/// Creates a GPU-bound encoder for Spout texture frames in the selected codec.
	pub(crate) fn new_gpu_codec(
		config: Config,
		device: &ID3D11Device,
		codec: CameraCodec,
	) -> Result<Self, &'static str> {
		Self::create(config, Some(device), codec, true)
	}

	fn create(
		config: Config,
		gpu_device: Option<&ID3D11Device>,
		codec_kind: CameraCodec,
		amd_only: bool,
	) -> Result<Self, &'static str> {
		let runtime = Runtime::open()?;
		let codec_subtype = match codec_kind {
			CameraCodec::H264 => MFVideoFormat_H264,
			// AMD's Media Foundation MFT registers HEVC (FourCC "HEVC"), not
			// the distinct H265 subtype. Query and negotiate the registered GUID.
			CameraCodec::H265 => MFVideoFormat_HEVC,
		};
		// SAFETY: Media Foundation owns returned COM objects; the activation array is cleared
		// before its CoTaskMem allocation is released.
		unsafe {
			let activate = Activated(hardware_encoder(codec_subtype, amd_only)?);
			let transform: IMFTransform = activate.0.ActivateObject().map_err(|_| UNAVAILABLE)?;
			let attributes = transform.GetAttributes().map_err(|_| UNAVAILABLE)?;
			if attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 0 {
				return Err(UNAVAILABLE);
			}
			attributes
				.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
				.map_err(|_| UNAVAILABLE)?;
			// The D3D manager must be installed before either media type is set.
			let dxgi_manager = if let Some(device) = gpu_device {
				let (mut reset_token, mut manager) = (0, None);
				MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
					.map_err(|_| UNAVAILABLE)?;
				let manager = manager.ok_or(UNAVAILABLE)?;
				manager
					.ResetDevice(device, reset_token)
					.map_err(|_| UNAVAILABLE)?;
				transform
					.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
					.map_err(|_| UNAVAILABLE)?;
				Some(manager)
			} else {
				None
			};
			let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
			let codec: ICodecAPI = transform.cast().map_err(|_| UNAVAILABLE)?;
			let _ = codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
			let _ = codec.SetValue(
				&CODECAPI_AVEncCommonRateControlMode,
				&VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
			);
			let _ = codec.SetValue(
				&CODECAPI_AVEncCommonMeanBitRate,
				&VARIANT::from(config.bit_rate),
			);
			let _ = codec.SetValue(
				&CODECAPI_AVEncMPVDefaultBPictureCount,
				&VARIANT::from(0_u32),
			);
			let _ = codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(config.fps * 2));

			let output = video_type(config, codec_subtype)?;
			output
				.SetUINT32(&MF_MT_AVG_BITRATE, config.bit_rate)
				.map_err(|_| UNAVAILABLE)?;
			// Advisory: an encoder that rejects the attribute keeps its own default profile.
			let profile = match codec_kind {
				CameraCodec::H264 => Some(match config.profile {
					Profile::Baseline => eAVEncH264VProfile_Base.0 as u32,
					Profile::Main => eAVEncH264VProfile_Main.0 as u32,
				}),
				CameraCodec::H265 => Some(eAVEncH265VProfile_Main_420_8.0 as u32),
			};
			if let Some(profile) = profile {
				let _ = output.SetUINT32(&MF_MT_MPEG2_PROFILE, profile);
			}
			transform
				.SetOutputType(0, &output, 0)
				.map_err(|_| UNAVAILABLE)?;

			let input = video_type(config, MFVideoFormat_NV12)?;
			input
				.SetUINT32(&MF_MT_DEFAULT_STRIDE, config.width)
				.map_err(|_| UNAVAILABLE)?;
			transform
				.SetInputType(0, &input, 0)
				.map_err(|_| UNAVAILABLE)?;
			let info = transform.GetOutputStreamInfo(0).map_err(|_| UNAVAILABLE)?;
			// cbSize is minimum output-buffer capacity, not the encoded sample length.
			// Permit a bounded raw-picture-sized allocation while keeping the transport's
			// compressed frame cap enforced by sample_bytes after ProcessOutput.
			let max_buffer_bytes = (config.width as usize)
				.checked_mul(config.height as usize)
				.and_then(|pixels| pixels.checked_mul(4))
				.filter(|bytes| *bytes <= crate::screen::MAX_RAW_BYTES)
				.ok_or(UNAVAILABLE)?
				.max(config.max_bytes);
			let provides_samples = info.dwFlags
				& (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
					| MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
				!= 0;
			if !provides_samples {
				output_buffer_size(&info, max_buffer_bytes)?;
			}
			let events = transform.cast().map_err(|_| UNAVAILABLE)?;
			transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
				.map_err(|_| UNAVAILABLE)?;
			transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
				.map_err(|_| UNAVAILABLE)?;
			Ok(Self {
				transform,
				events,
				codec,
				_activate: activate,
				_runtime: runtime,
				frame: 0,
				duration: 10_000_000 / i64::from(config.fps),
				max_bytes: config.max_bytes,
				max_buffer_bytes,
				dxgi_manager,
				need_input: 0,
				have_output: 0,
				provides_samples,
			})
		}
	}

	/// Submits an NV12 D3D11 surface directly to the hardware transform.
	/// The surface, transform, and DXGI device manager must share one D3D11 device.
	pub(crate) fn encode_surface(
		&mut self,
		texture: &ID3D11Texture2D,
		force_keyframe: bool,
	) -> Result<(Vec<u8>, bool), &'static str> {
		if self.dxgi_manager.is_none() {
			return Err(FAILED);
		}
		self.wait_for_input()?;
		if force_keyframe {
			// SAFETY: Codec control is called on the owning camera worker.
			unsafe {
				self.codec
					.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &force_keyframe_value())
					.map_err(|_| FAILED)?;
			}
		}
		// SAFETY: The retained D3D surface remains alive through the input sample;
		// Media Foundation holds its own reference to the DXGI surface buffer.
		unsafe {
			let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)
				.map_err(|_| FAILED)?;
			let sample = MFCreateSample().map_err(|_| FAILED)?;
			sample.AddBuffer(&buffer).map_err(|_| FAILED)?;
			sample
				.SetSampleTime(self.frame * self.duration)
				.map_err(|_| FAILED)?;
			sample
				.SetSampleDuration(self.duration)
				.map_err(|_| FAILED)?;
			self.frame += 1;
			self.transform
				.ProcessInput(0, &sample, 0)
				.map_err(|_| FAILED)?;
		}
		self.wait_for_output()?;
		self.take_output()
	}

	pub(crate) fn set_bitrate(&mut self, bitrate: u32) -> Result<(), &'static str> {
		// SAFETY: Dynamic codec control stays on the encoder's owning worker.
		unsafe {
			self.codec
				.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(bitrate))
				.map_err(|_| FAILED)
		}
	}

	pub(crate) fn encode(
		&mut self,
		y: &[u8],
		u: &[u8],
		v: &[u8],
		force_keyframe: bool,
	) -> Result<(Vec<u8>, bool), &'static str> {
		let length = y
			.len()
			.checked_add(u.len())
			.and_then(|length| length.checked_add(v.len()))
			.filter(|_| u.len() == v.len() && y.len() == u.len() * 4)
			.ok_or(FAILED)?;
		self.encode_with(length, force_keyframe, |picture| {
			i420_to_nv12(y, u, v, picture)
		})
	}

	/// Encode one `length`-byte NV12 picture that `fill` writes into the native buffer.
	pub(crate) fn encode_with(
		&mut self,
		length: usize,
		force_keyframe: bool,
		fill: impl FnOnce(&mut [u8]) -> Result<(), &'static str>,
	) -> Result<(Vec<u8>, bool), &'static str> {
		self.wait_for_input()?;
		if force_keyframe {
			// SAFETY: Codec control is called on the owning worker before this input sample.
			unsafe {
				self.codec
					// Microsoft specifies ULONG (VT_UI4), not VARIANT_BOOL. A rejected
					// command otherwise silently sends camera and screen share to software.
					.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &force_keyframe_value())
					.map_err(|_| FAILED)?;
			}
		}
		// SAFETY: The allocation is exactly the validated NV12 size; Lock is balanced by Unlock.
		unsafe {
			let native_length = u32::try_from(length).map_err(|_| FAILED)?;
			let buffer = MFCreateMemoryBuffer(native_length).map_err(|_| FAILED)?;
			let mut data = std::ptr::null_mut();
			let mut capacity = 0;
			buffer
				.Lock(&mut data, Some(&mut capacity), None)
				.map_err(|_| FAILED)?;
			if data.is_null() || capacity < native_length {
				let _ = buffer.Unlock();
				return Err(FAILED);
			}
			let destination = std::slice::from_raw_parts_mut(data, length);
			if fill(destination).is_err() {
				let _ = buffer.Unlock();
				return Err(FAILED);
			}
			buffer.Unlock().map_err(|_| FAILED)?;
			buffer.SetCurrentLength(native_length).map_err(|_| FAILED)?;
			let sample = MFCreateSample().map_err(|_| FAILED)?;
			sample.AddBuffer(&buffer).map_err(|_| FAILED)?;
			sample
				.SetSampleTime(self.frame * self.duration)
				.map_err(|_| FAILED)?;
			sample
				.SetSampleDuration(self.duration)
				.map_err(|_| FAILED)?;
			self.frame += 1;
			self.transform
				.ProcessInput(0, &sample, 0)
				.map_err(|_| FAILED)?;
		}
		self.wait_for_output()?;
		self.take_output()
	}

	fn wait_for_input(&mut self) -> Result<(), &'static str> {
		self.wait_until(|encoder| encoder.need_input != 0)?;
		self.need_input -= 1;
		Ok(())
	}

	fn wait_for_output(&mut self) -> Result<(), &'static str> {
		self.wait_until(|encoder| encoder.have_output != 0)?;
		self.have_output -= 1;
		Ok(())
	}

	fn wait_until(&mut self, ready: impl Fn(&Self) -> bool) -> Result<(), &'static str> {
		let deadline = Instant::now() + Duration::from_millis(250);
		while !ready(self) {
			// SAFETY: Event polling stays on the worker that owns this MFT.
			let event = unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) };
			match event {
				Ok(event) => unsafe {
					if event.GetStatus().map_err(|_| FAILED)?.is_err() {
						return Err(FAILED);
					}
					match event.GetType().map_err(|_| FAILED)? as i32 {
						kind if kind == METransformNeedInput.0 => self.need_input += 1,
						kind if kind == METransformHaveOutput.0 => self.have_output += 1,
						kind if kind == MEError.0 => return Err(FAILED),
						_ => {}
					}
				},
				Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => {
					if Instant::now() >= deadline {
						return Err(FAILED);
					}
					std::thread::sleep(Duration::from_millis(1));
				}
				Err(_) => return Err(FAILED),
			}
		}
		Ok(())
	}

	fn take_output(&self) -> Result<(Vec<u8>, bool), &'static str> {
		// SAFETY: The output struct is fully initialized. We take ownership of either the
		// transform-provided sample or our cloned sample before releasing its native fields.
		unsafe {
			let own = if self.provides_samples {
				None
			} else {
				let info = self.transform.GetOutputStreamInfo(0).map_err(|_| FAILED)?;
				let size = output_buffer_size(&info, self.max_buffer_bytes)?;
				let buffer = MFCreateMemoryBuffer(size).map_err(|_| FAILED)?;
				let sample = MFCreateSample().map_err(|_| FAILED)?;
				sample.AddBuffer(&buffer).map_err(|_| FAILED)?;
				Some(sample)
			};
			let mut output = MFT_OUTPUT_DATA_BUFFER {
				dwStreamID: 0,
				pSample: std::mem::ManuallyDrop::new(own.clone()),
				dwStatus: 0,
				pEvents: std::mem::ManuallyDrop::new(None),
			};
			let mut status = 0;
			let result =
				self.transform
					.ProcessOutput(0, std::slice::from_mut(&mut output), &mut status);
			let sample = std::mem::ManuallyDrop::into_inner(output.pSample).or(own);
			drop(std::mem::ManuallyDrop::into_inner(output.pEvents));
			result.map_err(|_| FAILED)?;
			let sample = sample.ok_or(FAILED)?;
			let data = sample_bytes(&sample, self.max_bytes)?;
			crate::video::validate_source(&data).map_err(|_| FAILED)?;
			let keyframe = crate::video_receive::is_keyframe(&data);
			Ok((data, keyframe))
		}
	}
}

fn force_keyframe_value() -> VARIANT {
	VARIANT::from(1_u32)
}

fn output_buffer_size(info: &MFT_OUTPUT_STREAM_INFO, limit: usize) -> Result<u32, &'static str> {
	if info.cbSize == 0 || info.cbSize as usize > limit {
		return Err(FAILED);
	}
	Ok(info.cbSize)
}

impl Drop for Encoder {
	fn drop(&mut self) {
		// SAFETY: Stop immediately; unsent video is disposable and flushing avoids blocking teardown.
		unsafe {
			let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
			let _ = self
				.transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
		}
	}
}

unsafe fn hardware_encoder(
	output_subtype: windows::core::GUID,
	amd_only: bool,
) -> Result<IMFActivate, &'static str> {
	unsafe {
		let input = MFT_REGISTER_TYPE_INFO {
			guidMajorType: MFMediaType_Video,
			guidSubtype: MFVideoFormat_NV12,
		};
		let output = MFT_REGISTER_TYPE_INFO {
			guidMajorType: MFMediaType_Video,
			guidSubtype: output_subtype,
		};
		let mut entries = std::ptr::null_mut();
		let mut count = 0;
		MFTEnumEx(
			MFT_CATEGORY_VIDEO_ENCODER,
			// Hardware MFTs are already asynchronous. Do not ask MFTEnumEx to
			// merit-filter vendor encoders; enumerate hardware and select AMD below.
			MFT_ENUM_FLAG_HARDWARE,
			Some(&input),
			Some(&output),
			&mut entries,
			&mut count,
		)
		.map_err(|_| UNAVAILABLE)?;
		if entries.is_null() {
			return Err(UNAVAILABLE);
		}
		if count == 0 {
			CoTaskMemFree(Some(entries.cast()));
			return Err(UNAVAILABLE);
		}
		let entries_slice = std::slice::from_raw_parts_mut(entries, count as usize);
		let mut selected = None;
		for entry in entries_slice {
			if let Some(activation) = entry.take()
				&& selected.is_none()
				&& (!amd_only || is_amd_encoder(&activation))
			{
				selected = Some(activation);
			}
		}
		CoTaskMemFree(Some(entries.cast()));
		selected.ok_or(if amd_only {
			"AMD hardware encoder for the selected codec is unavailable"
		} else {
			UNAVAILABLE
		})
	}
}

unsafe fn is_amd_encoder(activation: &IMFActivate) -> bool {
	unsafe {
		let Ok(length) = activation.GetStringLength(&MFT_FRIENDLY_NAME_Attribute) else {
			return false;
		};
		let mut name = vec![0_u16; length as usize + 1];
		if activation
			.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut name, None)
			.is_err()
		{
			return false;
		}
		String::from_utf16_lossy(&name)
			.to_ascii_lowercase()
			.contains("amd")
	}
}

unsafe fn video_type(
	config: Config,
	subtype: windows::core::GUID,
) -> Result<IMFMediaType, &'static str> {
	unsafe {
		let media_type = MFCreateMediaType().map_err(|_| UNAVAILABLE)?;
		media_type
			.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
			.map_err(|_| UNAVAILABLE)?;
		media_type
			.SetGUID(&MF_MT_SUBTYPE, &subtype)
			.map_err(|_| UNAVAILABLE)?;
		media_type
			.SetUINT64(
				&MF_MT_FRAME_SIZE,
				(u64::from(config.width) << 32) | u64::from(config.height),
			)
			.map_err(|_| UNAVAILABLE)?;
		media_type
			.SetUINT64(&MF_MT_FRAME_RATE, u64::from(config.fps) << 32 | 1)
			.map_err(|_| UNAVAILABLE)?;
		media_type
			.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, 1_u64 << 32 | 1)
			.map_err(|_| UNAVAILABLE)?;
		media_type
			.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
			.map_err(|_| UNAVAILABLE)?;
		Ok(media_type)
	}
}

fn sample_bytes(sample: &IMFSample, max_bytes: usize) -> Result<Vec<u8>, &'static str> {
	// SAFETY: Length is bounded before allocation; Lock's pointer is borrowed until Unlock.
	unsafe {
		if sample.GetTotalLength().map_err(|_| FAILED)? as usize > max_bytes {
			return Err(FAILED);
		}
		let buffer = sample.ConvertToContiguousBuffer().map_err(|_| FAILED)?;
		let mut data = std::ptr::null_mut();
		let mut capacity = 0;
		let mut length = 0;
		buffer
			.Lock(&mut data, Some(&mut capacity), Some(&mut length))
			.map_err(|_| FAILED)?;
		let result = if data.is_null() || length > capacity || length as usize > max_bytes {
			Err(FAILED)
		} else {
			Ok(std::slice::from_raw_parts(data, length as usize).to_vec())
		};
		buffer.Unlock().map_err(|_| FAILED)?;
		result
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn keyframe_control_uses_the_documented_unsigned_variant() {
		let value = force_keyframe_value();
		assert_eq!(value.vt(), windows::Win32::System::Variant::VT_UI4);
		assert_eq!(u32::try_from(&value).unwrap(), 1);
	}
	#[test]
	fn hardware_output_capacity_is_separate_from_compressed_frame_limit() {
		let mut info = MFT_OUTPUT_STREAM_INFO {
			dwFlags: 0,
			cbSize: 3840 * 2160 * 3 / 2,
			cbAlignment: 0,
		};
		assert!(info.cbSize as usize > crate::camera::MAX_ENCODED_BYTES);
		assert_eq!(
			output_buffer_size(&info, 3840 * 2160 * 4).unwrap(),
			info.cbSize
		);
		assert!(output_buffer_size(&info, 1024).is_err());
		info.cbSize = 0;
		assert!(output_buffer_size(&info, 3840 * 2160 * 4).is_err());
		info.cbSize = u32::MAX;
		assert!(output_buffer_size(&info, 3840 * 2160 * 4).is_err());
	}

	#[test]
	fn larger_native_buffer_does_not_relax_encoded_sample_limit() {
		let _runtime = Runtime::open().unwrap();
		let limit = crate::camera::MAX_ENCODED_BYTES;
		// Synthetic memory only: no transform, GPU, capture device or transport.
		unsafe {
			let capacity = 1920 * 1080 * 3 / 2;
			assert!(capacity > limit);
			let buffer = MFCreateMemoryBuffer(capacity as u32).unwrap();
			let mut data = std::ptr::null_mut();
			buffer.Lock(&mut data, None, None).unwrap();
			std::ptr::write_bytes(data, 0x2a, capacity);
			buffer.Unlock().unwrap();
			buffer.SetCurrentLength(limit as u32).unwrap();
			let sample = MFCreateSample().unwrap();
			sample.AddBuffer(&buffer).unwrap();
			let encoded = sample_bytes(&sample, limit).unwrap();
			assert_eq!(encoded.len(), limit);
			assert!(encoded.iter().all(|&byte| byte == 0x2a));
			buffer.SetCurrentLength(limit as u32 + 1).unwrap();
			assert!(sample_bytes(&sample, limit).is_err());
		}
	}
}
