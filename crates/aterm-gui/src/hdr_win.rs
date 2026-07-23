// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Windows EDR headroom + SDR-white query for the cursor aurora (M3 phase B on
//! Windows). Two sources, both keyed off the window's monitor:
//!
//! * **DXGI** (`IDXGIOutput6::GetDesc1` → `MaxLuminance`) for the panel's peak nits.
//!   DXGI is COM (vtable dispatch), so this uses the `windows` crate.
//! * **DisplayConfig** (`DisplayConfigGetDeviceInfo` advanced-color + SDR-white) for
//!   the "Use HDR" state and the desktop's SDR-white level. This is a FLAT C API, so
//!   it is hand-rolled here (house style, mirroring `aterm-pty`'s `ffi.rs`), with the
//!   exact `#[repr(C)]` layouts validated against a live display.
//!
//! Everything fails SAFE to `(edr_max 1.0, sdr_white_scale 1.0)` — no headroom, no
//! scaling — so a query failure or an HDR-off display leaves the aurora inert and the
//! present byte-identical to SDR. `edr_max = MaxLuminance / SDR-white` (relative, like
//! macOS's `NSScreen` value); `sdr_white_scale = SDR-white / 80` (scRGB reference).
#![cfg(windows)]

use core::ffi::c_void;
use winit::platform::windows::MonitorHandleExtWindows;
use winit::window::Window;

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
    // Fail-safe: if the DXGI peak query fails, fall back to SDR-white (headroom 0 =
    // the aurora glow adds nothing) while STILL scaling the grid to SDR-white.
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

// ---- DXGI: panel MaxLuminance for a monitor (COM, via the `windows` crate) --------

fn dxgi_max_luminance(hmon: isize) -> Option<f32> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
    use windows::core::Interface;
    // SAFETY: standard DXGI enumeration; every handle is COM-reference-counted by the
    // `windows` crate and dropped at scope end. No raw pointers escape.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut ai = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            ai += 1;
            let mut oi = 0u32;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                oi += 1;
                let Ok(o6) = output.cast::<IDXGIOutput6>() else {
                    continue;
                };
                if let Ok(desc) = o6.GetDesc1()
                    && desc.Monitor.0 as isize == hmon
                {
                    return Some(desc.MaxLuminance);
                }
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
#[derive(Clone, Copy)]
struct ModeInfo([u32; 16]);
impl Default for ModeInfo {
    fn default() -> Self {
        Self([0; 16])
    }
}
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

/// The first active display path's `(SDR-white nits, HDR-enabled)`. Single-display
/// exact; on multi-monitor it reports the first active path (a documented first-cut
/// limitation — DXGI above already matches the window's exact HMONITOR for the peak).
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
