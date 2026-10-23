// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/raw_input.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::mem::{self, size_of};
use std::ptr;

use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HID_USAGE_GENERIC_KEYBOARD, HID_USAGE_GENERIC_MOUSE, HID_USAGE_PAGE_GENERIC,
};
use windows_sys::Win32::Foundation::{HANDLE, HWND};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, MAPVK_VK_TO_VSC_EX, VK_NUMLOCK, VK_SHIFT,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, RegisterRawInputDevices,
    HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RAWKEYBOARD,
    RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_DEVICEINFO, RIDI_DEVICENAME,
    RID_DEVICE_INFO, RID_DEVICE_INFO_HID, RID_DEVICE_INFO_KEYBOARD, RID_DEVICE_INFO_MOUSE,
    RID_INPUT, RIM_TYPEHID, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    RI_KEY_E0, RI_KEY_E1, RI_MOUSE_BUTTON_1_DOWN, RI_MOUSE_BUTTON_1_UP, RI_MOUSE_BUTTON_2_DOWN,
    RI_MOUSE_BUTTON_2_UP, RI_MOUSE_BUTTON_3_DOWN, RI_MOUSE_BUTTON_3_UP, RI_MOUSE_BUTTON_4_DOWN,
    RI_MOUSE_BUTTON_4_UP, RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP,
};

use super::scancode_to_physicalkey;
use crate::event::ElementState;
use crate::event_loop::DeviceEvents;
use crate::keyboard::{KeyCode, PhysicalKey};
use crate::platform_impl::platform::util;

#[allow(dead_code)]
pub fn get_raw_input_device_list() -> Option<Vec<RAWINPUTDEVICELIST>> {
    let list_size = size_of::<RAWINPUTDEVICELIST>() as u32;

    let mut num_devices = 0;
    let status = unsafe { GetRawInputDeviceList(ptr::null_mut(), &mut num_devices, list_size) };

    if status == u32::MAX {
        return None;
    }

    let mut buffer = Vec::with_capacity(num_devices as _);

    let num_stored =
        unsafe { GetRawInputDeviceList(buffer.as_mut_ptr(), &mut num_devices, list_size) };

    if num_stored == u32::MAX {
        return None;
    }

    debug_assert_eq!(num_devices, num_stored);

    unsafe { buffer.set_len(num_devices as _) };

    Some(buffer)
}

#[allow(dead_code)]
pub enum RawDeviceInfo {
    Mouse(RID_DEVICE_INFO_MOUSE),
    Keyboard(RID_DEVICE_INFO_KEYBOARD),
    Hid(RID_DEVICE_INFO_HID),
}

impl From<RID_DEVICE_INFO> for RawDeviceInfo {
    fn from(info: RID_DEVICE_INFO) -> Self {
        unsafe {
            match info.dwType {
                RIM_TYPEMOUSE => RawDeviceInfo::Mouse(info.Anonymous.mouse),
                RIM_TYPEKEYBOARD => RawDeviceInfo::Keyboard(info.Anonymous.keyboard),
                RIM_TYPEHID => RawDeviceInfo::Hid(info.Anonymous.hid),
                _ => unreachable!(),
            }
        }
    }
}

#[allow(dead_code)]
pub fn get_raw_input_device_info(handle: HANDLE) -> Option<RawDeviceInfo> {
    let mut info: RID_DEVICE_INFO = unsafe { mem::zeroed() };
    let info_size = size_of::<RID_DEVICE_INFO>() as u32;

    info.cbSize = info_size;

    let mut minimum_size = 0;
    let status = unsafe {
        GetRawInputDeviceInfoW(handle, RIDI_DEVICEINFO, &mut info as *mut _ as _, &mut minimum_size)
    };

    if status == u32::MAX || status == 0 {
        return None;
    }

    debug_assert_eq!(info_size, status);

    Some(info.into())
}

pub fn get_raw_input_device_name(handle: HANDLE) -> Option<String> {
    let mut minimum_size = 0;
    let status = unsafe {
        GetRawInputDeviceInfoW(handle, RIDI_DEVICENAME, ptr::null_mut(), &mut minimum_size)
    };

    if status != 0 {
        return None;
    }

    let mut name: Vec<u16> = Vec::with_capacity(minimum_size as _);

    let status = unsafe {
        GetRawInputDeviceInfoW(handle, RIDI_DEVICENAME, name.as_ptr() as _, &mut minimum_size)
    };

    if status == u32::MAX || status == 0 {
        return None;
    }

    debug_assert_eq!(minimum_size, status);

    unsafe { name.set_len(minimum_size as _) };

    util::decode_wide(&name).into_string().ok()
}

pub fn register_raw_input_devices(devices: &[RAWINPUTDEVICE]) -> bool {
    let device_size = size_of::<RAWINPUTDEVICE>() as u32;

    unsafe {
        RegisterRawInputDevices(devices.as_ptr(), devices.len() as u32, device_size) == true.into()
    }
}

pub fn register_all_mice_and_keyboards_for_raw_input(
    mut window_handle: HWND,
    filter: DeviceEvents,
) -> bool {
    let flags = match filter {
        DeviceEvents::Never => {
            window_handle = std::ptr::null_mut();
            RIDEV_REMOVE
        },
        DeviceEvents::WhenFocused => RIDEV_DEVNOTIFY,
        DeviceEvents::Always => RIDEV_DEVNOTIFY | RIDEV_INPUTSINK,
    };

    let devices: [RAWINPUTDEVICE; 2] = [
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: HID_USAGE_GENERIC_MOUSE,
            dwFlags: flags,
            hwndTarget: window_handle,
        },
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: HID_USAGE_GENERIC_KEYBOARD,
            dwFlags: flags,
            hwndTarget: window_handle,
        },
    ];

    register_raw_input_devices(&devices)
}

pub fn get_raw_input_data(handle: HRAWINPUT) -> Option<RAWINPUT> {
    let mut data: RAWINPUT = unsafe { mem::zeroed() };
    let mut data_size = size_of::<RAWINPUT>() as u32;
    let header_size = size_of::<RAWINPUTHEADER>() as u32;

    let status = unsafe {
        GetRawInputData(handle, RID_INPUT, &mut data as *mut _ as _, &mut data_size, header_size)
    };

    if status == u32::MAX || status == 0 {
        return None;
    }

    Some(data)
}

fn button_flags_to_element_state(
    button_flags: u32,
    down_flag: u32,
    up_flag: u32,
) -> Option<ElementState> {
    if util::has_flag(button_flags, down_flag) {
        Some(ElementState::Pressed)
    } else if util::has_flag(button_flags, up_flag) {
        Some(ElementState::Released)
    } else {
        None
    }
}

pub fn get_raw_mouse_button_state(button_flags: u32) -> [Option<ElementState>; 5] {
    [
        button_flags_to_element_state(button_flags, RI_MOUSE_BUTTON_1_DOWN, RI_MOUSE_BUTTON_1_UP),
        button_flags_to_element_state(button_flags, RI_MOUSE_BUTTON_2_DOWN, RI_MOUSE_BUTTON_2_UP),
        button_flags_to_element_state(button_flags, RI_MOUSE_BUTTON_3_DOWN, RI_MOUSE_BUTTON_3_UP),
        button_flags_to_element_state(button_flags, RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP),
        button_flags_to_element_state(button_flags, RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP),
    ]
}

pub fn get_keyboard_physical_key(keyboard: RAWKEYBOARD) -> Option<PhysicalKey> {
    let extension = {
        if util::has_flag(keyboard.Flags, RI_KEY_E0 as _) {
            0xe000
        } else if util::has_flag(keyboard.Flags, RI_KEY_E1 as _) {
            0xe100
        } else {
            0x0000
        }
    };
    let scancode = if keyboard.MakeCode == 0 {
        unsafe { MapVirtualKeyW(keyboard.VKey as u32, MAPVK_VK_TO_VSC_EX) as u16 }
    } else {
        keyboard.MakeCode | extension
    };
    if scancode == 0xe11d || scancode == 0xe02a {
        // Ignore prefix scancodes emitted before Pause (0xE11D) and PrintScreen (0xE02A).
        return None;
    }
    let physical_key = if keyboard.VKey == VK_NUMLOCK {
        PhysicalKey::Code(KeyCode::NumLock)
    } else {
        scancode_to_physicalkey(scancode as u32)
    };
    if keyboard.VKey == VK_SHIFT {
        if let PhysicalKey::Code(
            KeyCode::NumpadDecimal
            | KeyCode::Numpad0
            | KeyCode::Numpad1
            | KeyCode::Numpad2
            | KeyCode::Numpad3
            | KeyCode::Numpad4
            | KeyCode::Numpad5
            | KeyCode::Numpad6
            | KeyCode::Numpad7
            | KeyCode::Numpad8
            | KeyCode::Numpad9,
        ) = physical_key
        {
            // Ignore synthetic Shift events emitted by Windows when Shift overrides NumLock.
            return None;
        }
    }

    Some(physical_key)
}
