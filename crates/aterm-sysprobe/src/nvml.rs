// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! NVIDIA's proprietary driver, through NVML: a card's device-wide busy figure, its name, and
//! whether it works in the system's memory.

use std::ffi::{CStr, CString, c_char, c_int, c_uint};
use std::sync::{Mutex, OnceLock};

use crate::Why;
use crate::linux::NvidiaCard;

type NvmlReturn = c_int;

const SUCCESS: NvmlReturn = 0;
const ERROR_NOT_SUPPORTED: NvmlReturn = 3;
const ERROR_NO_PERMISSION: NvmlReturn = 4;

/// `NVML_DEVICE_NAME_V2_BUFFER_SIZE`: room for any name `nvmlDeviceGetName` gives, its NUL
/// included.
const NAME_BUFFER: usize = 96;

/// `NVML_DEVICE_ADDRESSING_MODE_ATS`: the card and the CPUs share one set of page tables.
const ADDRESSING_MODE_ATS: c_uint = 2;

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

/// `nvmlMemory_t`: the card's own memory.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

/// `nvmlDeviceAddressingMode_v1_t`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AddressingMode {
    version: c_uint,
    value: c_uint,
}

/// `nvmlDeviceAddressingMode_v1`: `NVML_STRUCT_VERSION(DeviceAddressingMode, 1)`, the struct's
/// size with the version in the top byte.
const ADDRESSING_MODE_V1: c_uint = size_of::<AddressingMode>() as c_uint | (1 << 24);

aterm_dl::table! {
    struct Nvml {
        init = c"nvmlInit_v2": unsafe extern "C" fn() -> NvmlReturn;
        handle_by_pci_bus_id = c"nvmlDeviceGetHandleByPciBusId_v2":
            unsafe extern "C" fn(*const c_char, *mut *mut NvmlDevice) -> NvmlReturn;
        utilization_rates = c"nvmlDeviceGetUtilizationRates":
            unsafe extern "C" fn(*mut NvmlDevice, *mut Utilization) -> NvmlReturn;
        name = c"nvmlDeviceGetName":
            unsafe extern "C" fn(*mut NvmlDevice, *mut c_char, c_uint) -> NvmlReturn;
        memory_info = c"nvmlDeviceGetMemoryInfo":
            unsafe extern "C" fn(*mut NvmlDevice, *mut Memory) -> NvmlReturn;
    }
    optional {
        /// A newer call than the rest, which an older driver's library lacks.
        addressing_mode = c"nvmlDeviceGetAddressingMode":
            unsafe extern "C" fn(*mut NvmlDevice, *mut AddressingMode) -> NvmlReturn;
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
        // `nvmlDevice_t` a pointer to an opaque struct; `nvmlMemory_t` three `unsigned long
        // long`s; `nvmlDeviceAddressingMode_t` two `unsigned int`s).
        let nvml = unsafe { Nvml::load(&lib) }.map_err(|_| Why::NotRead)?;
        // SAFETY: nvmlInit_v2 takes nothing and is thread-safe; it runs once per process (this
        // OnceLock) and is never undone, so every handle and call after it stands on a live init.
        ok(unsafe { (nvml.init)() })?;
        Ok(nvml)
    })
    .as_ref()
    .map_err(|why| *why)
}

/// The NVIDIA card in PCI slot `slot`, read through NVML: its device-wide busy percent (at most
/// 100) every call, and its name and whether it works in the system's memory, which do not
/// change while the driver runs, read once per process ([`facts`]).
pub(crate) fn card(slot: &str) -> NvidiaCard {
    let nvml = match nvml() {
        Ok(nvml) => nvml,
        Err(why) => return NvidiaCard::unread(why),
    };
    let device = match handle(nvml, slot) {
        Ok(device) => device,
        Err(why) => return NvidiaCard::unread(why),
    };
    let (name, unified) = facts(nvml, slot, device);
    let mut rates = Utilization::default();
    // SAFETY: a handle NVML just gave under a live init, and a place laid out as
    // `nvmlUtilization_t`.
    let busy = ok(unsafe { (nvml.utilization_rates)(device, &raw mut rates) })
        .map(|()| u8::try_from(rates.gpu.min(100)).unwrap_or(100));
    NvidiaCard {
        busy,
        name,
        unified,
    }
}

/// The handle of the card in PCI slot `slot`.
fn handle(nvml: &Nvml, slot: &str) -> Result<*mut NvmlDevice, Why> {
    let bus_id = CString::new(slot).map_err(|_| Why::NotRead)?;
    let mut device: *mut NvmlDevice = std::ptr::null_mut();
    // SAFETY: a live init (`nvml`), a NUL-terminated bus id that outlives the call, and a place for
    // the handle.
    ok(unsafe { (nvml.handle_by_pci_bus_id)(bus_id.as_ptr(), &raw mut device) })?;
    if device.is_null() {
        return Err(Why::NotRead);
    }
    Ok(device)
}

/// The card's name and whether it works in the system's memory, kept per slot once its name
/// reads: a card NVML names keeps both for the process, and one it fails to name asks again at
/// the next call. It works in the system's memory where NVML has no memory of its own to report
/// for it (`nvmlDeviceGetMemoryInfo` answers not supported) and it addresses system memory through
/// ATS (`nvmlDeviceGetAddressingMode`), as a GB10 does; a card with memory of its own (a discrete
/// card, a Grace Hopper's HBM) or a driver without the addressing call does not.
fn facts(nvml: &Nvml, slot: &str, device: *mut NvmlDevice) -> (Option<String>, bool) {
    static KEPT: Mutex<Vec<(String, String, bool)>> = Mutex::new(Vec::new());
    let mut kept = KEPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, name, unified)) = kept.iter().find(|(s, ..)| s == slot) {
        return (Some(name.clone()), *unified);
    }
    let mut buf = [0 as c_char; NAME_BUFFER];
    // SAFETY: a handle NVML gave under a live init, and a buffer of the length passed.
    let named = ok(unsafe { (nvml.name)(device, buf.as_mut_ptr(), NAME_BUFFER as c_uint) });
    let bytes: Vec<u8> = buf.iter().map(|c| c.to_ne_bytes()[0]).collect();
    let name = named
        .ok()
        .and_then(|()| CStr::from_bytes_until_nul(&bytes).ok())
        .map(|text| text.to_string_lossy().trim().to_owned())
        .filter(|text| !text.is_empty());
    let Some(name) = name else {
        return (None, false);
    };
    let mut memory = Memory::default();
    // SAFETY: a handle NVML gave under a live init, and a place laid out as `nvmlMemory_t`.
    let own = unsafe { (nvml.memory_info)(device, &raw mut memory) };
    let ats = nvml.addressing_mode.is_some_and(|mode_of| {
        let mut mode = AddressingMode {
            version: ADDRESSING_MODE_V1,
            value: 0,
        };
        // SAFETY: a handle NVML gave under a live init, and a version-stamped place laid out as
        // `nvmlDeviceAddressingMode_v1_t`.
        ok(unsafe { mode_of(device, &raw mut mode) }).is_ok() && mode.value == ADDRESSING_MODE_ATS
    });
    let unified = own == ERROR_NOT_SUPPORTED && ats;
    kept.push((slot.to_owned(), name.clone(), unified));
    (Some(name), unified)
}
