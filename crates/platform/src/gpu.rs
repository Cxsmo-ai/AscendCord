//! Display adapter facts needed before the first window exists.

/// PCI vendor IDs of the hardware display adapters, read through DXGI only so no Direct3D
/// device (and none of its driver memory) is created. `None` when DXGI is unavailable.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub fn adapter_vendors() -> Option<Vec<u32>> {
	use windows::Win32::Graphics::Dxgi::{
		CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
	};
	// SAFETY: plain COM factory and adapter descriptor queries; the returned interfaces are
	// released when dropped and no pointers outlive this function.
	unsafe {
		let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
		let mut vendors = Vec::new();
		for index in 0..16 {
			let Ok(adapter) = factory.EnumAdapters1(index) else {
				break;
			};
			let Ok(description) = adapter.GetDesc1() else {
				continue;
			};
			// The Microsoft Basic Render Driver is software and says nothing about drivers.
			if description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
				continue;
			}
			vendors.push(description.VendorId);
		}
		Some(vendors)
	}
}
