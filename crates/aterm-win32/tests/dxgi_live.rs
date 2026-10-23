// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Live Windows DXGI output enumeration tests against `user32`.
//!
//! STOCK EXCEPTION: Windows only, and Trust has no Windows std, so only stock Rust builds this (the
//! Windows lane's guest pins its version: `aterm-lab/src/windows_guest.rs`).

#![cfg(windows)]
#![allow(non_snake_case)]

use aterm_win32::com::Com;
use aterm_win32::dxgi::{
    CreateDXGIFactory1, DXGI_OUTPUT_DESC1, IDXGIAdapter1_Vtbl, IDXGIFactory1_Vtbl,
    IDXGIOutput6_Vtbl, IID_IDXGIFactory1, IID_IDXGIOutput6,
};
use core::ffi::c_void;
use core::ptr;

/// user32's and kernel32's own declarations, independent of aterm-win32's.
mod user32 {
    use core::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[repr(C)]
    pub struct MonitorInfoExW {
        pub size: u32,
        pub monitor: Rect,
        pub work: Rect,
        pub flags: u32,
        pub device: [u16; 32],
    }

    pub type MonitorEnumProc =
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut Rect, isize) -> i32;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetCurrentProcessId() -> u32;
        pub fn ProcessIdToSessionId(process: u32, session: *mut u32) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn EnumDisplayMonitors(
            hdc: *mut c_void,
            clip: *const Rect,
            callback: MonitorEnumProc,
            data: isize,
        ) -> i32;
        pub fn GetMonitorInfoW(monitor: *mut c_void, info: *mut MonitorInfoExW) -> i32;
    }
}

unsafe extern "system" fn collect(
    monitor: *mut c_void,
    _: *mut c_void,
    _: *mut user32::Rect,
    data: isize,
) -> i32 {
    // SAFETY: `data` is the `Vec` `monitors` lends for this enumeration.
    let monitors = unsafe { &mut *(data as *mut Vec<usize>) };
    monitors.push(monitor as usize);
    1
}

/// user32's monitors in this session, by handle.
fn monitors() -> Vec<usize> {
    let mut monitors = Vec::new();
    // SAFETY: The callback writes only into `monitors`, which outlives the call.
    let ok = unsafe {
        user32::EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            collect,
            ptr::from_mut(&mut monitors) as isize,
        )
    };
    assert_ne!(ok, 0, "EnumDisplayMonitors");
    monitors
}

/// The reference a successful call wrote to `out`, owned.
///
/// # Safety
///
/// `out` is what the call whose result is `hr` wrote.
unsafe fn owned(hr: i32, out: *mut c_void) -> Option<Com> {
    // SAFETY: The caller's contract.
    (hr >= 0).then(|| unsafe { Com::from_raw(out) }).flatten()
}

/// Every output's `GetDesc1`, through aterm-win32 alone.
fn outputs() -> Vec<DXGI_OUTPUT_DESC1> {
    let mut out = ptr::null_mut();
    // SAFETY: The factory's IID and an out pointer written on success.
    let factory = unsafe { owned(CreateDXGIFactory1(&IID_IDXGIFactory1, &mut out), out) }
        .expect("CreateDXGIFactory1");
    // SAFETY: An IDXGIFactory1.
    let factory_vt = unsafe { factory.vtable::<IDXGIFactory1_Vtbl>() };
    let mut descriptions = Vec::new();
    for adapter in 0u32.. {
        let mut out = ptr::null_mut();
        // SAFETY: EnumAdapters1 hands back an owned IDXGIAdapter1, or fails past the last adapter.
        let hr = unsafe { (factory_vt.EnumAdapters1)(factory.as_raw(), adapter, &mut out) };
        // SAFETY: `out` is that call's.
        let Some(adapter) = (unsafe { owned(hr, out) }) else {
            break;
        };
        // SAFETY: An IDXGIAdapter1.
        let adapter_vt = unsafe { adapter.vtable::<IDXGIAdapter1_Vtbl>() };
        for output in 0u32.. {
            let mut out = ptr::null_mut();
            // SAFETY: EnumOutputs hands back an owned IDXGIOutput, or fails past the last output.
            let hr = unsafe { (adapter_vt.EnumOutputs)(adapter.as_raw(), output, &mut out) };
            // SAFETY: `out` is that call's.
            let Some(output) = (unsafe { owned(hr, out) }) else {
                break;
            };
            let output6 = output
                .query(&IID_IDXGIOutput6)
                .expect("Windows 11's outputs are IDXGIOutput6");
            let mut desc = DXGI_OUTPUT_DESC1::default();
            // SAFETY: An IDXGIOutput6; GetDesc1 fills `desc`.
            let hr = unsafe {
                (output6.vtable::<IDXGIOutput6_Vtbl>().GetDesc1)(output6.as_raw(), &mut desc)
            };
            assert!(hr >= 0, "GetDesc1: {hr:#x}");
            descriptions.push(desc);
        }
    }
    descriptions
}

/// This process's Terminal Services session; 0 has no display.
fn session() -> u32 {
    let mut session = 0;
    // SAFETY: This process's id and an out pointer.
    let ok = unsafe { user32::ProcessIdToSessionId(user32::GetCurrentProcessId(), &mut session) };
    assert_ne!(ok, 0, "ProcessIdToSessionId");
    session
}

#[test]
fn every_output_is_a_monitor_user32_describes_the_same_way() {
    let monitors = monitors();
    let outputs = outputs();
    let session = session();
    eprintln!(
        "session {session}; user32: {} monitor(s); DXGI: {} output(s)",
        monitors.len(),
        outputs.len()
    );
    if session == 0 {
        panic!(
            "aterm-gate: COULD NOT RUN — session 0 has no display, so there is no \
             output to compare; run it in a desktop session (`lab windows exec --desktop`)"
        );
    }
    assert!(!monitors.is_empty(), "a desktop session has a monitor");
    let mut seen = Vec::new();
    for desc in &outputs {
        let monitor = desc.Monitor as usize;
        assert!(monitors.contains(&monitor), "{monitor:#x} is user32's");
        let mut info = user32::MonitorInfoExW {
            size: u32::try_from(size_of::<user32::MonitorInfoExW>()).expect("fits"),
            monitor: user32::Rect::default(),
            work: user32::Rect::default(),
            flags: 0,
            device: [0; 32],
        };
        // SAFETY: A live monitor handle and a sized MONITORINFOEXW.
        assert_ne!(
            unsafe { user32::GetMonitorInfoW(desc.Monitor, &mut info) },
            0
        );
        let rect = desc.DesktopCoordinates;
        assert_eq!(
            user32::Rect {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            },
            info.monitor
        );
        assert_eq!(desc.DeviceName, info.device);
        assert_ne!(desc.AttachedToDesktop, 0);
        eprintln!(
            "{}: {:?}, {} bits, colour space {}, {}-{} nits",
            String::from_utf16_lossy(&desc.DeviceName).trim_end_matches('\0'),
            info.monitor,
            desc.BitsPerColor,
            desc.ColorSpace,
            desc.MinLuminance,
            desc.MaxLuminance,
        );
        seen.push(monitor);
    }
    seen.sort_unstable();
    let mut monitors = monitors;
    monitors.sort_unstable();
    assert_eq!(seen, monitors, "each monitor is one output");
}
