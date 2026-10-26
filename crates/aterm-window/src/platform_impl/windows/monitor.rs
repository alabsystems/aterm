// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/monitor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::collections::VecDeque;
use std::hash::Hash;
use std::{io, mem, ptr};

use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsExW, GetMonitorInfoW, MonitorFromPoint,
    MonitorFromWindow, DEVMODEW, ENUM_CURRENT_SETTINGS, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};

use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::platform_impl::platform::dpi::{dpi_to_scale_factor, get_monitor_dpi};
use crate::platform_impl::platform::window::Window;

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct MonitorHandle(HMONITOR);

unsafe impl Send for MonitorHandle {}
unsafe impl Sync for MonitorHandle {}

unsafe extern "system" fn monitor_enum_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _place: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let monitors = data as *mut VecDeque<MonitorHandle>;
    unsafe { (*monitors).push_back(MonitorHandle::new(hmonitor)) };
    true.into() // continue enumeration
}

pub fn available_monitors() -> VecDeque<MonitorHandle> {
    let mut monitors: VecDeque<MonitorHandle> = VecDeque::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            ptr::null(),
            Some(monitor_enum_proc),
            &mut monitors as *mut _ as LPARAM,
        );
    }
    monitors
}

pub fn primary_monitor() -> MonitorHandle {
    const ORIGIN: POINT = POINT { x: 0, y: 0 };
    let hmonitor = unsafe { MonitorFromPoint(ORIGIN, MONITOR_DEFAULTTOPRIMARY) };
    MonitorHandle::new(hmonitor)
}

pub fn current_monitor(hwnd: HWND) -> MonitorHandle {
    let hmonitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    MonitorHandle::new(hmonitor)
}

impl Window {
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        available_monitors()
    }

    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        let monitor = primary_monitor();
        Some(monitor)
    }
}

pub(crate) fn get_monitor_info(hmonitor: HMONITOR) -> Result<MONITORINFOEXW, io::Error> {
    let mut monitor_info: MONITORINFOEXW = unsafe { mem::zeroed() };
    monitor_info.monitorInfo.cbSize = mem::size_of::<MONITORINFOEXW>() as u32;
    let status = unsafe {
        GetMonitorInfoW(hmonitor, &mut monitor_info as *mut MONITORINFOEXW as *mut MONITORINFO)
    };
    if status == false.into() {
        Err(io::Error::last_os_error())
    } else {
        Ok(monitor_info)
    }
}

impl MonitorHandle {
    pub(crate) fn new(hmonitor: HMONITOR) -> Self {
        MonitorHandle(hmonitor)
    }

    #[inline]
    pub fn hmonitor(&self) -> HMONITOR {
        self.0
    }

    #[inline]
    pub fn size(&self) -> PhysicalSize<u32> {
        let rc_monitor = get_monitor_info(self.0).unwrap().monitorInfo.rcMonitor;
        PhysicalSize {
            width: (rc_monitor.right - rc_monitor.left) as u32,
            height: (rc_monitor.bottom - rc_monitor.top) as u32,
        }
    }

    #[inline]
    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        let monitor_info = get_monitor_info(self.0).ok()?;
        let device_name = monitor_info.szDevice.as_ptr();
        unsafe {
            let mut mode: DEVMODEW = mem::zeroed();
            mode.dmSize = mem::size_of_val(&mode) as u16;
            if EnumDisplaySettingsExW(device_name, ENUM_CURRENT_SETTINGS, &mut mode, 0)
                == false.into()
            {
                None
            } else {
                Some(mode.dmDisplayFrequency * 1000)
            }
        }
    }

    #[inline]
    pub fn position(&self) -> PhysicalPosition<i32> {
        get_monitor_info(self.0)
            .map(|info| {
                let rc_monitor = info.monitorInfo.rcMonitor;
                PhysicalPosition { x: rc_monitor.left, y: rc_monitor.top }
            })
            .unwrap_or(PhysicalPosition { x: 0, y: 0 })
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        dpi_to_scale_factor(get_monitor_dpi(self.0).unwrap_or(96))
    }
}
