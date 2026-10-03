//! GPU adapter selection for the window surface.
//!
//! wgpu's `LowPower` hint ranks the integrated GPU above the discrete one on every platform,
//! but the display is usually wired to the discrete card. On Wayland that combination fails
//! outright: the compositor cannot import buffers from a GPU that drives no output, which
//! surfaces as `importing the supplied dmabufs failed` and then a panic inside `egui-wgpu`
//! (`The surface isn't supported by this adapter`). Rank the adapters that can actually
//! present instead, ordered by the device preference, so a preference can never pick a
//! device the window cannot use.

use eframe::wgpu;
use model::GpuPreference;

/// Sort key for an adapter; lower is better.
fn rank(info: &wgpu::AdapterInfo, prefer_integrated: bool) -> (u8, u8) {
	let device = match info.device_type {
		wgpu::DeviceType::DiscreteGpu => u8::from(prefer_integrated),
		wgpu::DeviceType::IntegratedGpu => u8::from(!prefer_integrated),
		wgpu::DeviceType::VirtualGpu => 2,
		wgpu::DeviceType::Other => 3,
		wgpu::DeviceType::Cpu => 4,
	};
	// The GL backend is the compatibility fallback; native APIs present far better. Where
	// Windows enables both, Vulkan outranks DX12 (see `windows_backends`).
	let backend = match info.backend {
		wgpu::Backend::Vulkan | wgpu::Backend::Metal => 0,
		wgpu::Backend::Dx12 => 1,
		wgpu::Backend::Gl => 2,
		_ => 3,
	};
	(device, backend)
}

/// Whether the saved preference asks for the integrated GPU.
fn prefer_integrated(preference: GpuPreference) -> bool {
	match preference {
		GpuPreference::PowerSaving => true,
		GpuPreference::HighPerformance => false,
		// The diagnostic override only applies when the person did not choose explicitly.
		GpuPreference::Automatic => wgpu::PowerPreference::from_env()
			.is_some_and(|preference| preference == wgpu::PowerPreference::LowPower),
	}
}

/// PCI vendor ID of Intel GPUs, whose Vulkan drivers crashed at startup on Windows.
#[cfg(target_os = "windows")]
const INTEL: u32 = 0x8086;

/// Graphics APIs to try on Windows. DX12 alone stays where it is required: Intel GPUs
/// (startup crashes in their Vulkan drivers) and transparent windows. Elsewhere Vulkan draws
/// the same pixels with about half the driver memory (AMD RX 5700 XT, signed-in idle: 458 to
/// 193 MB working set in a call). If Vulkan cannot start, `main` relaunches with DX12.
/// `WGPU_BACKEND` overrides the choice.
#[cfg(target_os = "windows")]
pub fn windows_backends(transparent: bool) -> wgpu::Backends {
	if let Some(backends) = wgpu::Backends::from_env() {
		return backends;
	}
	if transparent {
		return wgpu::Backends::DX12;
	}
	// Vendors come from DXGI alone: creating Direct3D or Vulkan devices just to look would
	// load the very driver memory this choice saves (about 190 MB with DX12 kept enabled).
	match platform::gpu::adapter_vendors() {
		Some(vendors) => vulkan_allowed(vendors.into_iter()),
		None => wgpu::Backends::DX12,
	}
}

/// Vulkan is trusted only when adapters were found and none of them is Intel.
#[cfg(target_os = "windows")]
fn vulkan_allowed(mut vendors: impl Iterator<Item = u32>) -> wgpu::Backends {
	let mut any = false;
	if vendors.any(|vendor| {
		any = true;
		vendor == INTEL
	}) || !any
	{
		wgpu::Backends::DX12
	} else {
		wgpu::Backends::VULKAN
	}
}

/// Describes an adapter for settings and bug reports.
pub fn describe(info: &wgpu::AdapterInfo) -> String {
	format!("{} ({:?})", info.name, info.backend)
}

/// Picks the adapter used for the window surface, or explains why none of them works.
pub fn select(
	preference: GpuPreference,
	adapters: &[wgpu::Adapter],
	surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter, String> {
	let prefer_integrated = prefer_integrated(preference);
	// An adapter without surface formats cannot configure the swapchain; egui would panic later.
	let mut usable: Vec<&wgpu::Adapter> = adapters
		.iter()
		.filter(|adapter| {
			surface.is_none_or(|surface| !surface.get_capabilities(adapter).formats.is_empty())
		})
		.collect();
	usable.sort_by_key(|adapter| rank(&adapter.get_info(), prefer_integrated));
	let Some(adapter) = usable.first() else {
		let available = adapters
			.iter()
			.map(|adapter| describe(&adapter.get_info()))
			.collect::<Vec<_>>()
			.join(", ");
		return Err(if available.is_empty() {
			"no GPU adapter found; install a Vulkan, Metal, DirectX or OpenGL driver".to_owned()
		} else {
			format!("no GPU adapter can draw this window; found {available}")
		});
	};
	eprintln!(
		"[AscendCord] GPU adapter: {}",
		describe(&adapter.get_info())
	);
	Ok((*adapter).clone())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn info(device_type: wgpu::DeviceType, backend: wgpu::Backend) -> wgpu::AdapterInfo {
		wgpu::AdapterInfo::new(device_type, backend)
	}

	#[test]
	fn discrete_gpu_wins_by_default() {
		let discrete = info(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Vulkan);
		let integrated = info(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Vulkan);
		assert!(rank(&discrete, false) < rank(&integrated, false));
		assert!(!prefer_integrated(GpuPreference::HighPerformance));
	}

	#[test]
	fn power_saving_prefers_integrated() {
		let discrete = info(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Vulkan);
		let integrated = info(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Vulkan);
		assert!(prefer_integrated(GpuPreference::PowerSaving));
		assert!(rank(&integrated, true) < rank(&discrete, true));
	}

	#[test]
	fn software_and_gl_adapters_come_last() {
		let vulkan = info(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Vulkan);
		let gl = info(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Gl);
		let llvmpipe = info(wgpu::DeviceType::Cpu, wgpu::Backend::Vulkan);
		assert!(rank(&vulkan, false) < rank(&gl, false));
		assert!(rank(&gl, false) < rank(&llvmpipe, false));
		assert!(rank(&gl, true) < rank(&llvmpipe, true));
	}

	#[test]
	fn vulkan_outranks_dx12_on_the_same_gpu() {
		let vulkan = info(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Vulkan);
		let dx12 = info(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Dx12);
		let gl = info(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Gl);
		assert!(rank(&vulkan, false) < rank(&dx12, false));
		assert!(rank(&dx12, false) < rank(&gl, false));
		// Device preference still comes first.
		let integrated = info(wgpu::DeviceType::IntegratedGpu, wgpu::Backend::Vulkan);
		assert!(rank(&dx12, false) < rank(&integrated, false));
	}

	#[cfg(target_os = "windows")]
	#[test]
	fn intel_or_no_adapters_keep_dx12() {
		let vulkan = wgpu::Backends::VULKAN;
		assert_eq!(vulkan_allowed([0x1002].into_iter()), vulkan);
		assert_eq!(vulkan_allowed([0x10de].into_iter()), vulkan);
		assert_eq!(
			vulkan_allowed([0x10de, INTEL].into_iter()),
			wgpu::Backends::DX12
		);
		assert_eq!(vulkan_allowed([INTEL].into_iter()), wgpu::Backends::DX12);
		assert_eq!(vulkan_allowed(std::iter::empty()), wgpu::Backends::DX12);
	}

	#[test]
	fn describes_adapters_for_bug_reports() {
		let described = describe(&info(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Vulkan));
		assert!(described.ends_with("(Vulkan)"), "{described}");
	}
}
