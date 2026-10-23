// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/dpi.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by Andrew Yates for the aterm project in 2026; see the repository NOTICE.

#![allow(non_snake_case, unused_unsafe)]

use std::sync::Once;

use windows_sys::Win32::Foundation::{HWND, S_OK};
use windows_sys::Win32::Graphics::Gdi::{
    GetDC, GetDeviceCaps, MonitorFromWindow, HMONITOR, LOGPIXELSX, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI, PROCESS_PER_MONITOR_DPI_AWARE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::IsProcessDPIAware;

use crate::platform_impl::platform::util::{
    ENABLE_NON_CLIENT_DPI_SCALING, GET_DPI_FOR_MONITOR, GET_DPI_FOR_WINDOW, SET_PROCESS_DPI_AWARE,
    SET_PROCESS_DPI_AWARENESS, SET_PROCESS_DPI_AWARENESS_CONTEXT,
};

pub fn become_dpi_aware() {
    static ENABLE_DPI_AWARENESS: Once = Once::new();
    ENABLE_DPI_AWARENESS.call_once(|| {
        unsafe {
            if let Some(SetProcessDpiAwarenessContext) = *SET_PROCESS_DPI_AWARENESS_CONTEXT {
                // We are on Windows 10 Anniversary Update (1607) or later.
                if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
                    == false.into()
                {
                    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE);
                }
            } else if let Some(SetProcessDpiAwareness) = *SET_PROCESS_DPI_AWARENESS {
                SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);
            } else if let Some(SetProcessDPIAware) = *SET_PROCESS_DPI_AWARE {
                SetProcessDPIAware();
            }
        }
    });
}

pub fn enable_non_client_dpi_scaling(hwnd: HWND) {
    unsafe {
        if let Some(EnableNonClientDpiScaling) = *ENABLE_NON_CLIENT_DPI_SCALING {
            EnableNonClientDpiScaling(hwnd);
        }
    }
}

pub fn get_monitor_dpi(hmonitor: HMONITOR) -> Option<u32> {
    unsafe {
        if let Some(GetDpiForMonitor) = *GET_DPI_FOR_MONITOR {
            let mut dpi_x = 0;
            let mut dpi_y = 0;
            if GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) == S_OK {
                return Some(dpi_x);
            }
        }
    }
    None
}

pub const BASE_DPI: u32 = 96;
pub fn dpi_to_scale_factor(dpi: u32) -> f64 {
    dpi as f64 / BASE_DPI as f64
}

pub unsafe fn hwnd_dpi(hwnd: HWND) -> u32 {
    let hdc = unsafe { GetDC(hwnd) };
    if hdc.is_null() {
        panic!("[winit] `GetDC` returned null!");
    }
    if let Some(GetDpiForWindow) = *GET_DPI_FOR_WINDOW {
        match unsafe { GetDpiForWindow(hwnd) } {
            0 => BASE_DPI, // 0 is returned if hwnd is invalid
            dpi => dpi,
        }
    } else if let Some(GetDpiForMonitor) = *GET_DPI_FOR_MONITOR {
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if monitor.is_null() {
            return BASE_DPI;
        }

        let mut dpi_x = 0;
        let mut dpi_y = 0;
        if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } == S_OK {
            dpi_x
        } else {
            BASE_DPI
        }
    } else {
        // We are on Vista or later.
        if unsafe { IsProcessDPIAware() } != false.into() {
            unsafe { GetDeviceCaps(hdc, LOGPIXELSX as i32) as u32 }
        } else {
            BASE_DPI
        }
    }
}
