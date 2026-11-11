// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/dpi.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::{dc_dpi, hwnd_dpi};
use crate::platform_impl::platform::util::get_instance_handle;
use std::{mem, ptr};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{GetDC, ReleaseDC};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, CS_HREDRAW, CS_VREDRAW,
    WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};

/// user32's and kernel32's own declarations (winuser.h, processthreadsapi.h) for the count,
/// which aterm itself never reads.
mod witness {
    /// `GR_GDIOBJECTS`.
    pub const GR_GDIOBJECTS: u32 = 0;

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn GetGuiResources(process: isize, flags: u32) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetCurrentProcess() -> isize;
    }
}

/// The reads each count spans.
const READS: u32 = 200;

/// This process's GDI objects, a common DC checked out of the cache among them.
fn gdi_objects() -> u32 {
    unsafe { witness::GetGuiResources(witness::GetCurrentProcess(), witness::GR_GDIOBJECTS) }
}

/// A hidden top-level window of a class like aterm's (`CS_HREDRAW | CS_VREDRAW`, no DC of
/// its own), destroyed on drop.
struct Window(HWND);

impl Window {
    fn new() -> Self {
        let name: Vec<u16> = "aterm-window-dpi-test\0".encode_utf16().collect();
        let instance = get_instance_handle();
        let class = WNDCLASSEXW {
            cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: instance,
            lpszClassName: name.as_ptr(),
            ..Default::default()
        };
        // A second test's registration answers ERROR_CLASS_ALREADY_EXISTS, and the window
        // below still opens.
        unsafe { RegisterClassExW(&class) };
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                name.as_ptr(),
                name.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                40,
                40,
                400,
                260,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                ptr::null(),
            )
        };
        assert!(!hwnd.is_null(), "the test window opens");
        Self(hwnd)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}

/// A window's DPI read keeps no device context: `InitData::create_window` reads it once
/// for every window aterm opens. Both reads, the one this Windows takes (`GetDpiForWindow`)
/// and the `GetDeviceCaps` fallback's, `READS` times on one window, leave this process's
/// GDI objects where they were. The control: `READS` common DCs checked out of the cache
/// on the same window and kept move the count by at least `READS / 2` (they are released
/// after), so the count sees a kept DC.
#[test]
fn reading_a_windows_dpi_keeps_no_device_context() {
    let window = Window::new();
    let start = gdi_objects();
    let kept: Vec<_> = (0..READS).map(|_| unsafe { GetDC(window.0) }).collect();
    assert!(kept.iter().all(|dc| !dc.is_null()), "the cache hands out each DC");
    let holding = gdi_objects();
    for dc in kept {
        unsafe { ReleaseDC(window.0, dc) };
    }
    let before = gdi_objects();
    let dpi = (0..READS).map(|_| unsafe { hwnd_dpi(window.0) }).last();
    let after_read = gdi_objects();
    let dc = (0..READS).map(|_| unsafe { dc_dpi(window.0) }).last();
    let after_fallback = gdi_objects();
    eprintln!(
        "GDI objects: {start} -> {holding} holding {READS} DCs, {before} released; \
         {READS} DPI reads ({dpi:?}) -> {after_read}; {READS} fallback reads ({dc:?}) -> \
         {after_fallback}"
    );
    assert!(
        holding >= start + READS / 2,
        "the control: {READS} kept DCs move the count ({start} -> {holding})"
    );
    assert!(
        after_read < before + READS / 2,
        "{READS} DPI reads: GDI objects {before} -> {after_read}"
    );
    assert!(
        after_fallback < after_read + READS / 2,
        "{READS} fallback reads: GDI objects {after_read} -> {after_fallback}"
    );
}
