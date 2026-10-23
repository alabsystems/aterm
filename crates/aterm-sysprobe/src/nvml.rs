// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! NVIDIA's proprietary driver's device-wide busy figure, through NVML.

use std::ffi::{CStr, CString, c_char, c_int, c_uint};
use std::sync::OnceLock;

use crate::Why;

type NvmlReturn = c_int;

const SUCCESS: NvmlReturn = 0;
const ERROR_NOT_SUPPORTED: NvmlReturn = 3;
const ERROR_NO_PERMISSION: NvmlReturn = 4;

#[repr(C)]
struct NvmlDevice {
    _opaque: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

aterm_dl::table! {
    struct Nvml {
        init = c"nvmlInit_v2": unsafe extern "C" fn() -> NvmlReturn;
        handle_by_pci_bus_id = c"nvmlDeviceGetHandleByPciBusId_v2":
            unsafe extern "C" fn(*const c_char, *mut *mut NvmlDevice) -> NvmlReturn;
        utilization_rates = c"nvmlDeviceGetUtilizationRates":
            unsafe extern "C" fn(*mut NvmlDevice, *mut Utilization) -> NvmlReturn;
    }
}

/// The soname the driver's runtime package installs.
pub(crate) static SONAMES: &[&CStr] = &[c"libnvidia-ml.so.1"];

/// The [`Why`] of an NVML return code other than `NVML_SUCCESS`.
pub(crate) const fn why(code: NvmlReturn) -> Why {
    match code {
        ERROR_NOT_SUPPORTED => Why::Unsupported,
        ERROR_NO_PERMISSION => Why::NeedsPrivilege("nvml"),
        _ => Why::NotRead,
    }
}

const fn ok(code: NvmlReturn) -> Result<(), Why> {
    if code == SUCCESS {
        Ok(())
    } else {
        Err(why(code))
    }
}

fn nvml() -> Result<&'static Nvml, Why> {
    static NVML: OnceLock<Result<Nvml, Why>> = OnceLock::new();
    NVML.get_or_init(|| {
        let lib = aterm_dl::Library::open(SONAMES).map_err(|_| Why::NotRead)?;
        // SAFETY: every row transcribes nvml.h's prototype (`nvmlReturn_t` is a C enum, an int;
        // `nvmlDevice_t` a pointer to an opaque struct).
        let nvml = unsafe { Nvml::load(&lib) }.map_err(|_| Why::NotRead)?;
        // SAFETY: nvmlInit_v2 takes nothing and is thread-safe; it runs once per process (this
        // OnceLock) and is never undone, so every handle and call after it stands on a live init.
        ok(unsafe { (nvml.init)() })?;
        Ok(nvml)
    })
    .as_ref()
    .map_err(|why| *why)
}

/// Device-wide busy percent (at most 100) of the NVIDIA card in PCI slot `slot`, or [`Why`].
pub(crate) fn device_pct(slot: &str) -> Result<u8, Why> {
    let nvml = nvml()?;
    let bus_id = CString::new(slot).map_err(|_| Why::NotRead)?;
    let mut device: *mut NvmlDevice = std::ptr::null_mut();
    // SAFETY: a live init (`nvml`), a NUL-terminated bus id that outlives the call, and a place for
    // the handle.
    ok(unsafe { (nvml.handle_by_pci_bus_id)(bus_id.as_ptr(), &raw mut device) })?;
    if device.is_null() {
        return Err(Why::NotRead);
    }
    let mut rates = Utilization::default();
    // SAFETY: a handle NVML just gave under a live init, and a place laid out as
    // `nvmlUtilization_t`.
    ok(unsafe { (nvml.utilization_rates)(device, &raw mut rates) })?;
    Ok(u8::try_from(rates.gpu.min(100)).unwrap_or(100))
}
