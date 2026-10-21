// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Windows EDR headroom and SDR-white query for the cursor aurora, keyed off the window's
//! monitor:
//!
//! * DXGI (`IDXGIOutput6::GetDesc1` → `MaxLuminance`) for the panel's peak nits; COM, through
//!   aterm-win32's `dxgi` vtables, which its oracle holds to `windows` 0.62.2's slots.
//! * DisplayConfig (`DisplayConfigGetDeviceInfo` advanced-color and SDR-white) for the "Use
//!   HDR" state and the desktop's SDR-white level; a flat C API, hand-rolled here like
//!   `aterm-pty`'s `windows/ffi.rs`, with `#[repr(C)]` layouts validated against a live
//!   display.
//!
//! Every failure yields `(1.0, 1.0)`, so a failed query or an HDR-off display leaves the aurora
//! inert and the present identical to SDR. `edr_max = MaxLuminance / SDR-white` (relative, like
//! macOS's `NSScreen` value); `sdr_white_scale = SDR-white / 80` (the scRGB reference).
//!
//! DWM composes SDR-drawn pixels (GDI, the caption tint) onto the scRGB desktop at the same
//! `sdr_white_scale` the present applies, so caption, grid and gutter match
//! (`aterm_render::hdr::scrgb_present_channel` has the curve and how to capture it).
#![cfg(windows)]

use aterm_win32::com::Com;
use aterm_window::platform::windows::MonitorHandleExtWindows;
use aterm_window::window::Window;
use core::ffi::c_void;

/// `(edr_max, sdr_white_scale)` for `window`'s monitor when Windows HDR is ON;
/// `(1.0, 1.0)` when HDR is off or any query fails.
pub(crate) fn query(window: &Window) -> (f32, f32) {
    let Some(mon) = window.current_monitor() else {
        return (1.0, 1.0);
    };
    let hmon = mon.hmonitor();
    let Some((sdr_nits, hdr_on)) = displayconfig_primary() else {
        return (1.0, 1.0);
    };
    if !hdr_on || sdr_nits < 1.0 {
        return (1.0, 1.0);
    }
    // Without a DXGI peak, headroom is 0 (the aurora adds nothing) but the grid still scales to
    // SDR-white.
    let max_nits = dxgi_max_luminance(hmon).unwrap_or(sdr_nits);
    let edr_max = (max_nits / sdr_nits).max(1.0);
    let scale = (sdr_nits / 80.0).max(1.0);
    (edr_max, scale)
}

/// The EDR maximum ratio for the aurora headroom. See [`query`].
pub(crate) fn edr_max(window: &Window) -> f32 {
    query(window).0
}

/// The reference-white scale (`SDR-white / 80`) for the scRGB present. See [`query`].
pub(crate) fn sdr_white_scale(window: &Window) -> f32 {
    query(window).1
}

// ---- DXGI: panel MaxLuminance for a monitor (COM, aterm-win32's vtables) ---------

/// The interface pointer one successful DXGI call wrote to `out`, owned; `None`
/// for a failure, which ends an enumeration (`DXGI_ERROR_NOT_FOUND`) as any
/// other does.
///
/// # Safety
///
/// `out` is what the call whose result is `hr` wrote: on success, a live
/// interface pointer whose one reference nothing else releases.
unsafe fn owned(hr: i32, out: *mut c_void) -> Option<Com> {
    // SAFETY: the caller's contract.
    (hr >= 0).then(|| unsafe { Com::from_raw(out) }).flatten()
}

fn dxgi_max_luminance(hmon: isize) -> Option<f32> {
    use aterm_win32::dxgi::{
        CreateDXGIFactory1, DXGI_OUTPUT_DESC1, IDXGIAdapter1_Vtbl, IDXGIFactory1_Vtbl,
        IDXGIOutput6_Vtbl, IID_IDXGIFactory1, IID_IDXGIOutput6,
    };
    let mut out = core::ptr::null_mut();
    // SAFETY: the factory's IID and an out pointer DXGI writes only on success,
    // when it hands over one reference.
    let factory = unsafe { owned(CreateDXGIFactory1(&IID_IDXGIFactory1, &mut out), out) }?;
    // SAFETY: `factory` is an IDXGIFactory1, whose vtable this is; every object
    // below is owned by a `Com` and released at scope end, and no pointer escapes.
    let factory_vt = unsafe { factory.vtable::<IDXGIFactory1_Vtbl>() };
    for ai in 0u32.. {
        let mut out = core::ptr::null_mut();
        // SAFETY: as above; EnumAdapters1 hands back an owned IDXGIAdapter1.
        let hr = unsafe { (factory_vt.EnumAdapters1)(factory.as_raw(), ai, &mut out) };
        // SAFETY: `out` is that call's.
        let Some(adapter) = (unsafe { owned(hr, out) }) else {
            break;
        };
        // SAFETY: `adapter` is an IDXGIAdapter1.
        let adapter_vt = unsafe { adapter.vtable::<IDXGIAdapter1_Vtbl>() };
        for oi in 0u32.. {
            let mut out = core::ptr::null_mut();
            // SAFETY: EnumOutputs hands back an owned IDXGIOutput.
            let hr = unsafe { (adapter_vt.EnumOutputs)(adapter.as_raw(), oi, &mut out) };
            // SAFETY: `out` is that call's.
            let Some(output) = (unsafe { owned(hr, out) }) else {
                break;
            };
            let Ok(o6) = output.query(&IID_IDXGIOutput6) else {
                continue;
            };
            let mut desc = DXGI_OUTPUT_DESC1::default();
            // SAFETY: `o6` is an IDXGIOutput6; GetDesc1 fills `desc`.
            let hr = unsafe { (o6.vtable::<IDXGIOutput6_Vtbl>().GetDesc1)(o6.as_raw(), &mut desc) };
            if hr >= 0 && desc.Monitor as isize == hmon {
                return Some(desc.MaxLuminance);
            }
        }
    }
    None
}

// ---- DisplayConfig: SDR-white + HDR-on state (flat C, hand-rolled) ----------------

const QDC_ONLY_ACTIVE_PATHS: u32 = 0x0000_0002;
const GET_ADVANCED_COLOR_INFO: u32 = 9;
const GET_SDR_WHITE_LEVEL: u32 = 11;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Luid {
    low: u32,
    high: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Header {
    kind: u32,
    size: u32,
    adapter_id: Luid,
    id: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Src {
    adapter_id: Luid,
    id: u32,
    mode_idx: u32,
    status: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rational {
    num: u32,
    den: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Tgt {
    adapter_id: Luid,
    id: u32,
    mode_idx: u32,
    out_tech: u32,
    rotation: u32,
    scaling: u32,
    refresh: Rational,
    scanline: u32,
    available: i32,
    status: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PathInfo {
    src: Src,
    tgt: Tgt,
    flags: u32,
}
/// `DISPLAYCONFIG_MODE_INFO` is a 64-byte tagged union; we never read it (only pass
/// the array to `QueryDisplayConfig`), so an opaque 4-byte-aligned blob suffices.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ModeInfo([u32; 16]);
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AdvColorInfo {
    header: Header,
    value: u32, // bit0 supported, bit1 enabled, ...
    color_encoding: i32,
    bits_per_channel: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SdrWhite {
    header: Header,
    sdr_white_level: u32, // nits = level / 1000 * 80
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetDisplayConfigBufferSizes(flags: u32, num_paths: *mut u32, num_modes: *mut u32) -> i32;
    fn QueryDisplayConfig(
        flags: u32,
        num_paths: *mut u32,
        paths: *mut PathInfo,
        num_modes: *mut u32,
        modes: *mut ModeInfo,
        current_topology: *mut c_void,
    ) -> i32;
    fn DisplayConfigGetDeviceInfo(packet: *mut Header) -> i32;
}

/// The first active display path's `(SDR-white nits, HDR-enabled)`. On multi-monitor this is
/// the first active path, not the window's (DXGI above matches the window's HMONITOR for the
/// peak).
fn displayconfig_primary() -> Option<(f32, bool)> {
    // SAFETY: hand-rolled DisplayConfig FFI with `#[repr(C)]` layouts validated
    // against a live display; sizes are passed by the API contract and every call is
    // return-code-checked. The two device-info packets are full structs whose first
    // field is the header the API writes through.
    unsafe {
        let mut num_paths = 0u32;
        let mut num_modes = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut num_paths, &mut num_modes) != 0 {
            return None;
        }
        if num_paths == 0 {
            return None;
        }
        let mut paths = vec![PathInfo::default(); num_paths as usize];
        let mut modes = vec![ModeInfo::default(); num_modes as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut num_paths,
            paths.as_mut_ptr(),
            &mut num_modes,
            modes.as_mut_ptr(),
            core::ptr::null_mut(),
        ) != 0
        {
            return None;
        }
        for path in paths.iter().take(num_paths as usize) {
            let t = &path.tgt;
            let mut aci = AdvColorInfo {
                header: Header {
                    kind: GET_ADVANCED_COLOR_INFO,
                    size: core::mem::size_of::<AdvColorInfo>() as u32,
                    adapter_id: t.adapter_id,
                    id: t.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut aci.header) != 0 {
                continue;
            }
            let hdr_on = aci.value & 0b10 != 0;
            let mut sdr = SdrWhite {
                header: Header {
                    kind: GET_SDR_WHITE_LEVEL,
                    size: core::mem::size_of::<SdrWhite>() as u32,
                    adapter_id: t.adapter_id,
                    id: t.id,
                },
                ..Default::default()
            };
            let _ = DisplayConfigGetDeviceInfo(&mut sdr.header);
            // A zero/absent level means the API didn't fill it (or SDR); treat as the
            // scRGB reference (80 nits → scale 1.0).
            let raw = if sdr.sdr_white_level == 0 {
                1000
            } else {
                sdr.sdr_white_level
            };
            let nits = raw as f32 / 1000.0 * 80.0;
            return Some((nits, hdr_on));
        }
    }
    None
}
