// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! NVIDIA's proprietary driver's device-wide busy figure, through NVML.
//!
//! The driver publishes no busy figure in sysfs; its own tools
//! (`nvidia-smi`) read NVML, the `libnvidia-ml.so.1` the driver installs.
//! This module opens it at run time through aterm-dl, so a machine without
//! the driver links and runs the same binary.
//!
//! * The library is opened, its three entry points read and `nvmlInit_v2`
//!   called once per process, on the first ask; the outcome, success or its
//!   [`Why`], is kept in a `OnceLock` and not retried. `nvmlInit_v2`
//!   initialises no GPU (the handle lookups do). NVML is never shut down:
//!   aterm-dl never closes a library, and the init is held for the
//!   process's life, as a poller of the driver's own holds it.
//! * One card is read by its PCI slot name (sysfs's `PCI_SLOT_NAME`,
//!   `000f:01:00.0`, a form `nvmlDeviceGetHandleByPciBusId_v2` accepts):
//!   the `gpu` field of `nvmlDeviceGetUtilizationRates`, the percent of the
//!   driver's last sample period in which a kernel ran on it.
//! * Only [`crate::linux::Root::system`] carries this reader, and
//!   [`crate::linux::Root::gpu`] asks it only for an awake card whose
//!   runtime power management a read cannot move: the handle lookup
//!   initialises the GPU, which would wake a hybrid laptop's sleeping dGPU
//!   and, polled, hold it awake.
//!
//! What a failure answers, the init's and each call's alike:
//!
//! * No library, or one without the three symbols: [`Why::NotRead`]. The
//!   card is there (its DRM device names the driver), so [`Why::Absent`]
//!   would say a present GPU is missing; the driver has a public source and
//!   this process cannot read it (a container without the driver's
//!   user-space libraries).
//! * `NVML_ERROR_NOT_SUPPORTED`: [`Why::Unsupported`], the device keeps no
//!   such figure (a MIG-partitioned GPU's utilization is one).
//! * `NVML_ERROR_NO_PERMISSION`: [`Why::NeedsPrivilege`]`("nvml")`, naming
//!   the gate that refused as `cap_perfmon` does for i915: NVML's access to
//!   the driver or the device (`/dev/nvidia*`'s mode), which aterm never
//!   asks for.
//! * Any other code (the driver not loaded, a library a version behind its
//!   kernel module, the GPU lost): [`Why::NotRead`].

use std::ffi::{CStr, CString, c_char, c_int, c_uint};
use std::sync::OnceLock;

use crate::Why;

/// `nvmlReturn_t`, a C enum.
type NvmlReturn = c_int;

/// `NVML_SUCCESS`.
const SUCCESS: NvmlReturn = 0;
/// `NVML_ERROR_NOT_SUPPORTED`.
const ERROR_NOT_SUPPORTED: NvmlReturn = 3;
/// `NVML_ERROR_NO_PERMISSION`.
const ERROR_NO_PERMISSION: NvmlReturn = 4;

/// `struct nvmlDevice_st`, opaque: `nvmlDevice_t` is a pointer to it.
#[repr(C)]
struct NvmlDevice {
    _opaque: [u8; 0],
}

/// `nvmlUtilization_t`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Utilization {
    /// Percent of the last sample period in which a kernel ran.
    gpu: c_uint,
    /// Percent of it in which device memory was read or written.
    memory: c_uint,
}

aterm_dl::table! {
    /// libnvidia-ml: what the device-wide busy figure takes.
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

/// The [`Why`] of an NVML return code other than `NVML_SUCCESS` (the
/// module's table).
pub(crate) const fn why(code: NvmlReturn) -> Why {
    match code {
        ERROR_NOT_SUPPORTED => Why::Unsupported,
        ERROR_NO_PERMISSION => Why::NeedsPrivilege("nvml"),
        _ => Why::NotRead,
    }
}

/// `Ok(())` for `NVML_SUCCESS`, else its [`Why`].
const fn ok(code: NvmlReturn) -> Result<(), Why> {
    if code == SUCCESS {
        Ok(())
    } else {
        Err(why(code))
    }
}

/// NVML, opened and initialised once per process; never shut down.
fn nvml() -> Result<&'static Nvml, Why> {
    static NVML: OnceLock<Result<Nvml, Why>> = OnceLock::new();
    NVML.get_or_init(|| {
        let lib = aterm_dl::Library::open(SONAMES).map_err(|_| Why::NotRead)?;
        // SAFETY: every row transcribes nvml.h's prototype (`nvmlReturn_t`
        // is a C enum, an int; `nvmlDevice_t` a pointer to an opaque struct).
        let nvml = unsafe { Nvml::load(&lib) }.map_err(|_| Why::NotRead)?;
        // SAFETY: nvmlInit_v2 takes nothing and is thread-safe; it runs once
        // per process (this OnceLock) and is never undone, so every handle
        // and call after it stands on a live init.
        ok(unsafe { (nvml.init)() })?;
        Ok(nvml)
    })
    .as_ref()
    .map_err(|why| *why)
}

/// The device-wide busy percent, at most 100, of the NVIDIA card in PCI
/// slot `slot` (`000f:01:00.0`), or why it cannot be read (the module's
/// table). The first call opens and initialises NVML.
pub(crate) fn device_pct(slot: &str) -> Result<u8, Why> {
    let nvml = nvml()?;
    let bus_id = CString::new(slot).map_err(|_| Why::NotRead)?;
    let mut device: *mut NvmlDevice = std::ptr::null_mut();
    // SAFETY: a live init (`nvml`), a NUL-terminated bus id that outlives the
    // call, and a place for the handle.
    ok(unsafe { (nvml.handle_by_pci_bus_id)(bus_id.as_ptr(), &raw mut device) })?;
    if device.is_null() {
        return Err(Why::NotRead);
    }
    let mut rates = Utilization::default();
    // SAFETY: a handle NVML just gave under a live init, and a place laid out
    // as `nvmlUtilization_t`.
    ok(unsafe { (nvml.utilization_rates)(device, &raw mut rates) })?;
    Ok(u8::try_from(rates.gpu.min(100)).unwrap_or(100))
}
