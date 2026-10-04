//! Compatibility path for virtual cameras registered only with DirectShow.
#![allow(non_snake_case)] // Custom COM interfaces preserve the Qedit.h method names.
use super::{INVALID, Shared, TIMEOUT, UNAVAILABLE, invalid};
use model::CameraQuality;
use std::{
	ffi::c_void,
	mem::ManuallyDrop,
	sync::Mutex,
	sync::atomic::Ordering,
	sync::mpsc,
	time::{Duration, Instant},
};
use windows::{
	Win32::{
		Media::{DirectShow::*, MediaFoundation::*},
		System::{
			Com::{StructuredStorage::IPropertyBag, *},
			Variant::{VARIANT, VT_BSTR, VariantClear},
		},
	},
	core::{BOOL, GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, implement, interface, w},
};

const SAMPLE_GRABBER: GUID = GUID::from_u128(0xc1f400a0_3f08_11d3_9f0b_006008039e37);
const NULL_RENDERER: GUID = GUID::from_u128(0xc1f400a4_3f08_11d3_9f0b_006008039e37);
const MAX_BYTES: usize = 3840 * 2160 * 4;
// The graph's allocator is bounded by its total memory, not an assumed buffer count: virtual
// cameras and colour converters commonly negotiate four or more samples.
const MAX_ALLOCATOR_BYTES: usize = MAX_BYTES * 8;
const FORMAT_CHANGED: &str = "Camera changed its video format; restart the camera";

fn allocator_fits(buffers: i32, buffer_bytes: i32, prefix: i32, frame_bytes: usize) -> bool {
	let (Ok(buffers), Ok(buffer_bytes)) = (usize::try_from(buffers), usize::try_from(buffer_bytes))
	else {
		return false;
	};
	buffers >= 1
		&& (0..=4096).contains(&prefix)
		&& (frame_bytes..=MAX_BYTES).contains(&buffer_bytes)
		&& buffers
			.checked_mul(buffer_bytes)
			.is_some_and(|total| total <= MAX_ALLOCATOR_BYTES)
}

// Qedit.h interfaces are absent from windows-rs metadata. Keep the SDK ABI order.
#[interface("6b652fff-11fe-4fce-92ad-0266b5d7c78f")]
unsafe trait ISampleGrabber: IUnknown {
	fn SetOneShot(&self, value: BOOL) -> HRESULT;
	fn SetMediaType(&self, media: *const AM_MEDIA_TYPE) -> HRESULT;
	fn GetConnectedMediaType(&self, media: *mut AM_MEDIA_TYPE) -> HRESULT;
	fn SetBufferSamples(&self, value: BOOL) -> HRESULT;
	fn GetCurrentBuffer(&self, length: *mut i32, buffer: *mut i32) -> HRESULT;
	fn GetCurrentSample(&self, sample: *mut *mut c_void) -> HRESULT;
	fn SetCallback(&self, callback: *mut c_void, method: i32) -> HRESULT;
}

#[interface("0579154a-2b53-4994-b0d0-e773148eff85")]
unsafe trait ISampleGrabberCB: IUnknown {
	fn SampleCB(&self, time: f64, sample: *mut c_void) -> HRESULT;
	fn BufferCB(&self, time: f64, buffer: *mut u8, length: i32) -> HRESULT;
}

#[implement(ISampleGrabberCB)]
struct Callback {
	send: mpsc::SyncSender<Result<Vec<u8>, &'static str>>,
	last: Mutex<(Instant, bool)>,
	bytes: usize,
	dimensions: (usize, usize, bool),
	interval: Duration,
}
impl ISampleGrabberCB_Impl for Callback_Impl {
	unsafe fn SampleCB(&self, _: f64, sample: *mut c_void) -> HRESULT {
		let Ok(mut last) = self.last.try_lock() else {
			return HRESULT(0);
		};
		// SAFETY: Borrowed sample stays valid until this callback returns. Never
		// retain COM samples or encode on the native streaming thread.
		let result = unsafe {
			(|| {
				if last.1 {
					return Err("after an earlier error");
				}
				let sample = IMediaSample::from_raw_borrowed(&sample).ok_or("sample pointer")?;
				let length = sample.GetActualDataLength();
				let capacity = sample.GetSize();
				// A sample may carry tail bytes after the picture; only the picture is copied.
				if length < 0
					|| capacity < length
					|| capacity as usize > MAX_BYTES
					|| (length as usize) < self.bytes
				{
					return Err("sample size");
				}
				let changed = sample.GetMediaType().map_err(|_| "sample media type")?;
				if !changed.is_null() {
					let media = Media::take(changed);
					if rgb_dimensions(&media.0).ok() != Some(self.dimensions) {
						return Err(FORMAT_CHANGED);
					}
				}
				if last.0.elapsed() < self.interval {
					return Ok(None);
				}
				last.0 = Instant::now();
				let pointer = sample.GetPointer().map_err(|_| "sample pointer")?;
				if pointer.is_null() {
					return Err("sample pointer");
				}
				Ok(Some(
					std::slice::from_raw_parts(pointer, self.bytes).to_vec(),
				))
			})()
		};
		// Exactly one frame, at most MAX_BYTES, waits for the worker.
		match result {
			Ok(Some(bytes)) => {
				let _ = self.send.try_send(Ok(bytes));
			}
			Ok(None) => {}
			Err(error) => {
				last.1 = true;
				let _ = self.send.try_send(Err(error));
			}
		}
		HRESULT(0)
	}
	unsafe fn BufferCB(&self, _: f64, _: *mut u8, _: i32) -> HRESULT {
		HRESULT(0)
	}
}

struct Media(AM_MEDIA_TYPE);
impl Media {
	unsafe fn take(pointer: *mut AM_MEDIA_TYPE) -> Self {
		// SAFETY: Caller obtained this owned CoTaskMem block from DirectShow.
		unsafe {
			let media = pointer.read();
			CoTaskMemFree(Some(pointer.cast()));
			Self(media)
		}
	}
}
impl Drop for Media {
	fn drop(&mut self) {
		// SAFETY: DirectShow transfers these two owned fields to the caller.
		unsafe {
			CoTaskMemFree(Some(self.0.pbFormat.cast()));
			ManuallyDrop::drop(&mut self.0.pUnk);
		}
	}
}

fn monikers() -> Result<Vec<(String, String, IMoniker)>, &'static str> {
	// SAFETY: Parent initializes COM on this worker. Binding storage reads only
	// registration metadata; BindToObject is reserved for explicit capture below.
	unsafe {
		let system: ICreateDevEnum =
			CoCreateInstance(&CLSID_SystemDeviceEnum, None, CLSCTX_INPROC_SERVER)
				.map_err(|_| UNAVAILABLE)?;
		let mut enumeration = None;
		system
			.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory, &mut enumeration, 0)
			.map_err(|_| UNAVAILABLE)?;
		let Some(enumeration) = enumeration else {
			return Ok(Vec::new());
		};
		let mut devices = Vec::new();
		for _ in 0..32 {
			let mut entry = [None];
			if enumeration.Next(&mut entry, None) != HRESULT(0) {
				break;
			}
			let Some(moniker) = entry[0].take() else {
				break;
			};
			let Ok(display) = moniker.GetDisplayName(None, None) else {
				continue;
			};
			let id = if display.is_null() {
				None
			} else {
				let mut length = 0;
				while length <= 4096 && *display.0.add(length) != 0 {
					length += 1;
				}
				(length <= 4096).then(|| {
					String::from_utf16_lossy(std::slice::from_raw_parts(display.0, length))
				})
			};
			CoTaskMemFree(Some(display.0.cast()));
			let Some(id) = id.filter(|id| !id.is_empty() && id.len() <= 4090) else {
				continue;
			};
			let Ok(bag) = moniker.BindToStorage::<_, _, IPropertyBag>(None, None) else {
				continue;
			};
			let mut value = VARIANT::default();
			let read = bag.Read(w!("FriendlyName"), &mut value, None);
			let name = if read.is_ok() && value.Anonymous.Anonymous.vt == VT_BSTR {
				let bstr = &value.Anonymous.Anonymous.Anonymous.bstrVal;
				(bstr.len() <= 256).then(|| bstr.to_string())
			} else {
				None
			};
			let _ = VariantClear(&mut value);
			let name = name
				.filter(|name| !name.is_empty() && name.len() <= 256 && !name.contains('\0'))
				.unwrap_or_else(|| "Windows camera".into());
			devices.push((format!("dshow:{id}"), name, moniker));
		}
		Ok(devices)
	}
}

pub(super) fn devices() -> Result<Vec<(String, String)>, &'static str> {
	Ok(monikers()?
		.into_iter()
		.map(|(id, name, _)| (id, name))
		.collect())
}

fn dimensions(media: &AM_MEDIA_TYPE) -> Result<(usize, usize, bool), &'static str> {
	if media.majortype != MEDIATYPE_Video
		|| media.pbFormat.is_null()
		|| media.cbFormat as usize > 65536
	{
		return Err(INVALID);
	}
	// SAFETY: Validate the advertised format block size before reading its header;
	// read_unaligned also handles filters which do not align their format blocks.
	let bitmap = unsafe {
		if media.formattype == FORMAT_VideoInfo
			&& media.cbFormat as usize >= size_of::<VIDEOINFOHEADER>()
		{
			media
				.pbFormat
				.cast::<VIDEOINFOHEADER>()
				.read_unaligned()
				.bmiHeader
		} else if media.formattype == FORMAT_VideoInfo2
			&& media.cbFormat as usize >= size_of::<VIDEOINFOHEADER2>()
		{
			media
				.pbFormat
				.cast::<VIDEOINFOHEADER2>()
				.read_unaligned()
				.bmiHeader
		} else {
			return Err(INVALID);
		}
	};
	if !(1..=3840).contains(&bitmap.biWidth)
		|| !(1..=2160).contains(&bitmap.biHeight.unsigned_abs())
		|| bitmap.biSizeImage as usize > MAX_BYTES
	{
		return Err(INVALID);
	}
	Ok((
		bitmap.biWidth as usize,
		bitmap.biHeight.unsigned_abs() as usize,
		bitmap.biHeight > 0,
	))
}

fn rgb_dimensions(media: &AM_MEDIA_TYPE) -> Result<(usize, usize, bool), &'static str> {
	let dimensions = dimensions(media)?;
	if media.subtype != MEDIASUBTYPE_RGB24 || media.formattype != FORMAT_VideoInfo {
		return Err(INVALID);
	}
	// SAFETY: dimensions validated the complete VIDEOINFOHEADER allocation.
	let bitmap = unsafe {
		media
			.pbFormat
			.cast::<VIDEOINFOHEADER>()
			.read_unaligned()
			.bmiHeader
	};
	if bitmap.biBitCount != 24 || bitmap.biPlanes != 1 || bitmap.biCompression != 0 {
		return Err(INVALID);
	}
	Ok(dimensions)
}

/// FourCC subtypes print as their four letters, others as the GUID.
fn subtype_name(subtype: &GUID) -> String {
	let code = subtype.data1.to_le_bytes();
	if code.iter().all(u8::is_ascii_alphanumeric) {
		String::from_utf8_lossy(&code).into_owned()
	} else if *subtype == MEDIASUBTYPE_RGB24 {
		"RGB24".into()
	} else if *subtype == MEDIASUBTYPE_RGB32 {
		"RGB32".into()
	} else {
		format!("{subtype:?}")
	}
}

/// Names of the filters DirectShow connected, for reports about a camera that stays silent.
fn filter_names(graph: &IGraphBuilder) -> String {
	let mut names = Vec::new();
	// SAFETY: Worker-owned graph. Each FILTER_INFO graph reference is released after reading.
	unsafe {
		let Ok(filters) = graph.EnumFilters() else {
			return "unknown filters".into();
		};
		for _ in 0..16 {
			let mut filter = [None];
			if filters.Next(&mut filter, None) != HRESULT(0) {
				break;
			}
			let Some(filter) = filter[0].take() else {
				break;
			};
			let mut info = FILTER_INFO::default();
			if filter.QueryFilterInfo(&mut info).is_ok() {
				let length = info.achName.iter().position(|&c| c == 0).unwrap_or(128);
				names.push(String::from_utf16_lossy(&info.achName[..length]));
				ManuallyDrop::drop(&mut info.pGraph);
			}
		}
	}
	names.join(" / ")
}

fn configure(
	builder: &ICaptureGraphBuilder2,
	source: &IBaseFilter,
	quality: CameraQuality,
) -> Result<String, &'static str> {
	let (target_width, target_height) = quality.dimensions();
	// SAFETY: Owned worker graph, only bounded native formats may be selected.
	unsafe {
		let mut pointer = std::ptr::null_mut();
		builder
			.FindInterface(
				Some(&PIN_CATEGORY_CAPTURE),
				Some(&MEDIATYPE_Video),
				source,
				&IAMStreamConfig::IID,
				&mut pointer,
			)
			.map_err(|_| INVALID)?;
		if pointer.is_null() {
			return Err(INVALID);
		}
		let config = IAMStreamConfig::from_raw(pointer);
		let (mut count, mut size) = (0, 0);
		config
			.GetNumberOfCapabilities(&mut count, &mut size)
			.map_err(|_| INVALID)?;
		if !(1..=256).contains(&count) || !(1..=4096).contains(&size) {
			return Err(INVALID);
		}
		let mut capabilities = vec![0; size as usize];
		let mut choices = Vec::new();
		for index in 0..count {
			let mut pointer = std::ptr::null_mut();
			let result = config.GetStreamCaps(index, &mut pointer, capabilities.as_mut_ptr());
			if pointer.is_null() {
				continue;
			}
			let media = Media::take(pointer);
			if result.is_err() {
				continue;
			}
			if let Ok((width, height, _)) = dimensions(&media.0) {
				let (target_width, target_height) = (target_width as usize, target_height as usize);
				let exact = width == target_width && height == target_height;
				let covers = width >= target_width && height >= target_height;
				let same_shape = width * target_height == height * target_width;
				// Without the exact mode, prefer the smallest one that covers the selected size
				// (scaled down), else the largest smaller one (scaled up); same shape first.
				let area = if covers {
					width * height
				} else {
					usize::MAX - width * height
				};
				choices.push(((!exact, !covers, !same_shape, area), media));
			}
		}
		choices.sort_by_key(|(rank, _)| *rank);
		for (_, media) in choices {
			if config.SetFormat(&media.0).is_ok() {
				let (width, height, _) = dimensions(&media.0)?;
				return Ok(format!(
					"{} {width}x{height}",
					subtype_name(&media.0.subtype)
				));
			}
		}
		Err("Camera does not offer a supported capture mode up to 3840×2160")
	}
}

struct Capture {
	control: IMediaControl,
	grabber: ISampleGrabber,
	callback: Option<ISampleGrabberCB>,
}
impl Drop for Capture {
	fn drop(&mut self) {
		// SAFETY: Stop drains streaming callbacks before the worker releases graph
		// references. The callback itself owns all state needed by an in-flight call.
		unsafe {
			let _ = self.control.Stop();
			let _ = self.grabber.SetCallback(std::ptr::null_mut(), 0);
		}
	}
}

pub(super) fn run(
	shared: &Shared,
	device: Option<&str>,
	quality: CameraQuality,
	emit: &mut dyn FnMut(Vec<u8>) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
	if shared.stopped.load(Ordering::Acquire) {
		return Ok(());
	}
	let (_, _, moniker) = monikers()?
		.into_iter()
		.find(|(id, _, _)| device.is_none_or(|selected| selected == id))
		.ok_or("Selected Windows camera is no longer available")?;
	// SAFETY: This is the only activation path, called after an explicit camera-on
	// gesture. Graph, format negotiation and teardown all remain on this worker.
	let (capture, receive, width, height, bottom_up, mode, filters) = unsafe {
		let source: IBaseFilter = moniker.BindToObject(None, None).map_err(|_| UNAVAILABLE)?;
		let graph: IGraphBuilder = CoCreateInstance(&CLSID_FilterGraph, None, CLSCTX_INPROC_SERVER)
			.map_err(|_| UNAVAILABLE)?;
		let builder: ICaptureGraphBuilder2 =
			CoCreateInstance(&CLSID_CaptureGraphBuilder2, None, CLSCTX_INPROC_SERVER)
				.map_err(|_| UNAVAILABLE)?;
		builder.SetFiltergraph(&graph).map_err(|_| UNAVAILABLE)?;
		graph
			.AddFilter(&source, w!("Camera"))
			.map_err(|_| UNAVAILABLE)?;
		let mode = configure(&builder, &source, quality)?;
		let filter: IBaseFilter = CoCreateInstance(&SAMPLE_GRABBER, None, CLSCTX_INPROC_SERVER)
			.map_err(|_| UNAVAILABLE)?;
		let grabber: ISampleGrabber = filter.cast().map_err(|_| UNAVAILABLE)?;
		let mut capture = Capture {
			control: graph.cast().map_err(|_| UNAVAILABLE)?,
			grabber,
			callback: None,
		};
		let requested = AM_MEDIA_TYPE {
			majortype: MEDIATYPE_Video,
			subtype: MEDIASUBTYPE_RGB24,
			formattype: FORMAT_VideoInfo,
			..Default::default()
		};
		capture
			.grabber
			.SetMediaType(&requested)
			.ok()
			.map_err(|_| INVALID)?;
		capture
			.grabber
			.SetBufferSamples(BOOL(0))
			.ok()
			.map_err(|_| INVALID)?;
		graph
			.AddFilter(&filter, w!("Camera frames"))
			.map_err(|_| UNAVAILABLE)?;
		let sink: IBaseFilter = CoCreateInstance(&NULL_RENDERER, None, CLSCTX_INPROC_SERVER)
			.map_err(|_| UNAVAILABLE)?;
		graph
			.AddFilter(&sink, w!("Camera sink"))
			.map_err(|_| UNAVAILABLE)?;
		builder
			.RenderStream(
				Some(&PIN_CATEGORY_CAPTURE),
				&MEDIATYPE_Video,
				&source,
				&filter,
				&sink,
			)
			.map_err(|_| "Windows could not convert this camera to RGB video")?;
		// Deliver samples as they arrive. Some virtual cameras stamp samples with their own
		// clock, and a clocked graph would hold every one of them back.
		if let Ok(filter) = graph.cast::<IMediaFilter>() {
			let _ = filter.SetSyncSource(None::<&windows::Win32::Media::IReferenceClock>);
		}
		let filters = filter_names(&graph);
		let mut media = Media(AM_MEDIA_TYPE::default());
		capture
			.grabber
			.GetConnectedMediaType(&mut media.0)
			.ok()
			.map_err(|_| INVALID)?;
		let (width, height, bottom_up) = rgb_dimensions(&media.0).map_err(|_| {
			invalid(
				shared,
				format!(
					"converted format {:?} / {:?}",
					media.0.subtype, media.0.formattype
				),
			)
		})?;
		// rgb_frame scales a camera without the exact mode to the selected size.
		let bytes = (width * 3).next_multiple_of(4) * height;
		// Check the actual negotiated allocator before any filter starts delivering.
		let pins = filter.EnumPins().map_err(|_| INVALID)?;
		let mut bounded_allocator = false;
		for _ in 0..16 {
			let mut pin = [None];
			if pins.Next(&mut pin, None) != HRESULT(0) {
				break;
			}
			let Some(pin) = pin[0].take() else { break };
			if let Ok(input) = pin.cast::<IMemInputPin>() {
				let properties = input
					.GetAllocator()
					.and_then(|allocator| allocator.GetProperties())
					.map_err(|error| invalid(shared, format!("allocator properties: {error}")))?;
				if !allocator_fits(
					properties.cBuffers,
					properties.cbBuffer,
					properties.cbPrefix,
					bytes,
				) {
					return Err(invalid(
						shared,
						format!(
							"allocator: {} buffers of {} bytes, prefix {}; {width}x{height} needs {bytes}",
							properties.cBuffers, properties.cbBuffer, properties.cbPrefix
						),
					));
				}
				bounded_allocator = true;
			}
		}
		if !bounded_allocator {
			return Err(invalid(shared, "no frame grabber allocator".into()));
		}
		let (send, receive) = mpsc::sync_channel(1);
		let interval = Duration::from_secs_f64(1.0 / f64::from(quality.frames_per_second));
		let callback: ISampleGrabberCB = Callback {
			send,
			last: Mutex::new((Instant::now() - interval, false)),
			bytes,
			dimensions: (width, height, bottom_up),
			interval,
		}
		.into();
		capture
			.grabber
			.SetCallback(callback.as_raw(), 0)
			.ok()
			.map_err(|_| INVALID)?;
		capture.callback = Some(callback);
		if !shared.stopped.load(Ordering::Acquire) {
			capture.control.Run().map_err(|_| UNAVAILABLE)?;
		}
		(capture, receive, width, height, bottom_up, mode, filters)
	};
	let mut last = Instant::now();
	while !shared.stopped.load(Ordering::Acquire) {
		match receive.recv_timeout(Duration::from_millis(50)) {
			Ok(bytes) => {
				let bytes = bytes.map_err(|reason| match reason {
					FORMAT_CHANGED => reason,
					reason => invalid(shared, format!("{reason}, {width}x{height} RGB24")),
				})?;
				let target = quality.dimensions();
				let rgb = rgb_frame(
					&bytes,
					width,
					height,
					bottom_up,
					(target.0 as usize, target.1 as usize),
				)?;
				last = Instant::now();
				if !shared.stopped.load(Ordering::Acquire) {
					emit(rgb)?;
				}
			}
			Err(mpsc::RecvTimeoutError::Timeout) if last.elapsed() < Duration::from_secs(5) => {}
			_ => {
				shared.set_error_detail(format!(
					"{TIMEOUT} (no frames from {mode} through {filters})"
				));
				return Err(TIMEOUT);
			}
		}
	}
	drop(capture);
	Ok(())
}

fn rgb_frame(
	bytes: &[u8],
	width: usize,
	height: usize,
	bottom_up: bool,
	target: (usize, usize),
) -> Result<Vec<u8>, &'static str> {
	let (target_width, target_height) = target;
	if !(1..=3840).contains(&width)
		|| !(1..=2160).contains(&height)
		|| !(640..=3840).contains(&target_width)
		|| !(480..=2160).contains(&target_height)
	{
		return Err(INVALID);
	}
	let pitch = (width * 3).next_multiple_of(4);
	if bytes.len() != pitch * height || bytes.len() > MAX_BYTES {
		return Err(INVALID);
	}
	let mut rgb = vec![0; target_width * target_height * 3];
	// Preserve aspect ratio with bounded nearest-neighbor scaling. The selected
	// capture and encode dimensions remain the output dimensions.
	let (draw_width, draw_height) = if width * target_height > height * target_width {
		(target_width, (height * target_width / width).max(1))
	} else {
		((width * target_height / height).max(1), target_height)
	};
	let left = (target_width - draw_width) / 2;
	let top = (target_height - draw_height) / 2;
	for y in 0..draw_height {
		let source_y = y * height / draw_height;
		let source_y = if bottom_up {
			height - 1 - source_y
		} else {
			source_y
		};
		for x in 0..draw_width {
			let source = source_y * pitch + (x * width / draw_width) * 3;
			let dest = ((top + y) * target_width + left + x) * 3;
			rgb[dest..dest + 3].copy_from_slice(&[
				bytes[source + 2],
				bytes[source + 1],
				bytes[source],
			]);
		}
	}
	Ok(rgb)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::camera::{HEIGHT, WIDTH};
	#[test]
	fn rgb_conversion_bounds_padding_orientation_and_resize() {
		let mut header = VIDEOINFOHEADER::default();
		header.bmiHeader.biWidth = WIDTH as i32;
		header.bmiHeader.biHeight = HEIGHT as i32;
		header.bmiHeader.biPlanes = 1;
		header.bmiHeader.biBitCount = 24;
		let mut media = AM_MEDIA_TYPE {
			majortype: MEDIATYPE_Video,
			subtype: MEDIASUBTYPE_RGB24,
			formattype: FORMAT_VideoInfo,
			cbFormat: size_of::<VIDEOINFOHEADER>() as u32,
			pbFormat: (&mut header as *mut VIDEOINFOHEADER).cast(),
			..Default::default()
		};
		assert_eq!(rgb_dimensions(&media).unwrap(), (WIDTH, HEIGHT, true));
		media.cbFormat -= 1;
		assert!(rgb_dimensions(&media).is_err());
		media.cbFormat += 1;
		media.subtype = MEDIASUBTYPE_RGB32;
		assert!(rgb_dimensions(&media).is_err());
		let bytes = [1, 2, 3, 0, 4, 5, 6, 0];
		let target = (WIDTH, HEIGHT);
		let top = rgb_frame(&bytes, 1, 2, false, target).unwrap();
		let bottom = rgb_frame(&bytes, 1, 2, true, target).unwrap();
		let center = WIDTH / 2 * 3;
		assert_eq!(&top[center..center + 3], &[3, 2, 1]);
		assert_eq!(&bottom[center..center + 3], &[6, 5, 4]);
		let end = (HEIGHT - 1) * WIDTH * 3 + center;
		assert_eq!(&top[end..end + 3], &[6, 5, 4]);
		assert_eq!(&top[..3], &[0, 0, 0]);
		let frame = 1920 * 1080 * 3;
		for buffers in [1, 3, 4, 8, 12] {
			assert!(allocator_fits(buffers, frame as i32, 0, frame));
		}
		assert!(!allocator_fits(0, frame as i32, 0, frame));
		assert!(!allocator_fits(-1, frame as i32, 0, frame));
		assert!(!allocator_fits(4, frame as i32 - 1, 0, frame));
		assert!(!allocator_fits(1, frame as i32, 4097, frame));
		assert!(!allocator_fits(9, MAX_BYTES as i32, 0, frame));
		let wide = rgb_frame(&vec![255; 16 * 3 * 9], 16, 9, false, target).unwrap();
		assert_eq!(&wide[..WIDTH * 60 * 3], &vec![0; WIDTH * 60 * 3]);
		assert_eq!(
			&wide[WIDTH * 60 * 3..WIDTH * 420 * 3],
			&vec![255; WIDTH * 360 * 3]
		);
		for (width, height) in [(0, 2), (1, 0), (1921, 1), (1, 1081), (usize::MAX, 1)] {
			assert!(rgb_frame(&bytes, width, height, false, target).is_err());
		}
		assert!(rgb_frame(&bytes[..7], 1, 2, false, target).is_err());
		assert!(rgb_frame(&bytes, 1, 1, false, target).is_err());
	}
}
