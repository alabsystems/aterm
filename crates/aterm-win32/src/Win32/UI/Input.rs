// Copyright (c) Microsoft Corporation.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from windows-sys 0.61.2 src/Windows/Win32/UI/Input/mod.rs. GENERATED
// by aterm-win32-oracle: the 35 of its 67 names that aterm's Windows cells read
// or that those name, with their feature gates dropped and their paths pointed
// into this crate. Do not edit; regenerate. See ../../../LICENSE-MIT and
// ../../../LICENSE-APACHE for the upstream licenses.

//! `Win32::UI::Input`.

pub mod Ime;
pub mod KeyboardAndMouse;
pub mod Pointer;
pub mod Touch;

link!("user32.dll" "system" fn GetRawInputData(hrawinput : HRAWINPUT, uicommand : RAW_INPUT_DATA_COMMAND_FLAGS, pdata : *mut core::ffi::c_void, pcbsize : *mut u32, cbsizeheader : u32) -> u32);
link!("user32.dll" "system" fn GetRawInputDeviceInfoW(hdevice : super::super::Foundation:: HANDLE, uicommand : RAW_INPUT_DEVICE_INFO_COMMAND, pdata : *mut core::ffi::c_void, pcbsize : *mut u32) -> u32);
link!("user32.dll" "system" fn GetRawInputDeviceList(prawinputdevicelist : *mut RAWINPUTDEVICELIST, puinumdevices : *mut u32, cbsize : u32) -> u32);
link!("user32.dll" "system" fn RegisterRawInputDevices(prawinputdevices : *const RAWINPUTDEVICE, uinumdevices : u32, cbsize : u32) -> crate::core::BOOL);
pub type HRAWINPUT = *mut core::ffi::c_void;
pub const MOUSE_MOVE_RELATIVE: MOUSE_STATE = 0u16;
pub type MOUSE_STATE = u16;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RAWHID {
    pub dwSizeHid: u32,
    pub dwCount: u32,
    pub bRawData: [u8; 1],
}
impl Default for RAWHID {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RAWINPUT {
    pub header: RAWINPUTHEADER,
    pub data: RAWINPUT_0,
}
impl Default for RAWINPUT {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub union RAWINPUT_0 {
    pub mouse: RAWMOUSE,
    pub keyboard: RAWKEYBOARD,
    pub hid: RAWHID,
}
impl Default for RAWINPUT_0 {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RAWINPUTDEVICE {
    pub usUsagePage: u16,
    pub usUsage: u16,
    pub dwFlags: RAWINPUTDEVICE_FLAGS,
    pub hwndTarget: super::super::Foundation::HWND,
}
impl Default for RAWINPUTDEVICE {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RAWINPUTDEVICELIST {
    pub hDevice: super::super::Foundation::HANDLE,
    pub dwType: RID_DEVICE_INFO_TYPE,
}
impl Default for RAWINPUTDEVICELIST {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
pub type RAWINPUTDEVICE_FLAGS = u32;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RAWINPUTHEADER {
    pub dwType: u32,
    pub dwSize: u32,
    pub hDevice: super::super::Foundation::HANDLE,
    pub wParam: super::super::Foundation::WPARAM,
}
impl Default for RAWINPUTHEADER {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RAWKEYBOARD {
    pub MakeCode: u16,
    pub Flags: u16,
    pub Reserved: u16,
    pub VKey: u16,
    pub Message: u32,
    pub ExtraInformation: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RAWMOUSE {
    pub usFlags: MOUSE_STATE,
    pub Anonymous: RAWMOUSE_0,
    pub ulRawButtons: u32,
    pub lLastX: i32,
    pub lLastY: i32,
    pub ulExtraInformation: u32,
}
impl Default for RAWMOUSE {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub union RAWMOUSE_0 {
    pub ulButtons: u32,
    pub Anonymous: RAWMOUSE_0_0,
}
impl Default for RAWMOUSE_0 {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RAWMOUSE_0_0 {
    pub usButtonFlags: u16,
    pub usButtonData: u16,
}
pub type RAW_INPUT_DATA_COMMAND_FLAGS = u32;
pub type RAW_INPUT_DEVICE_INFO_COMMAND = u32;
pub const RIDEV_DEVNOTIFY: RAWINPUTDEVICE_FLAGS = 8192u32;
pub const RIDEV_INPUTSINK: RAWINPUTDEVICE_FLAGS = 256u32;
pub const RIDEV_REMOVE: RAWINPUTDEVICE_FLAGS = 1u32;
pub const RIDI_DEVICEINFO: RAW_INPUT_DEVICE_INFO_COMMAND = 536870923u32;
pub const RIDI_DEVICENAME: RAW_INPUT_DEVICE_INFO_COMMAND = 536870919u32;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RID_DEVICE_INFO {
    pub cbSize: u32,
    pub dwType: RID_DEVICE_INFO_TYPE,
    pub Anonymous: RID_DEVICE_INFO_0,
}
impl Default for RID_DEVICE_INFO {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub union RID_DEVICE_INFO_0 {
    pub mouse: RID_DEVICE_INFO_MOUSE,
    pub keyboard: RID_DEVICE_INFO_KEYBOARD,
    pub hid: RID_DEVICE_INFO_HID,
}
impl Default for RID_DEVICE_INFO_0 {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RID_DEVICE_INFO_HID {
    pub dwVendorId: u32,
    pub dwProductId: u32,
    pub dwVersionNumber: u32,
    pub usUsagePage: u16,
    pub usUsage: u16,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RID_DEVICE_INFO_KEYBOARD {
    pub dwType: u32,
    pub dwSubType: u32,
    pub dwKeyboardMode: u32,
    pub dwNumberOfFunctionKeys: u32,
    pub dwNumberOfIndicators: u32,
    pub dwNumberOfKeysTotal: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RID_DEVICE_INFO_MOUSE {
    pub dwId: u32,
    pub dwNumberOfButtons: u32,
    pub dwSampleRate: u32,
    pub fHasHorizontalWheel: crate::core::BOOL,
}
pub type RID_DEVICE_INFO_TYPE = u32;
pub const RID_INPUT: RAW_INPUT_DATA_COMMAND_FLAGS = 268435459u32;
pub const RIM_TYPEHID: RID_DEVICE_INFO_TYPE = 2u32;
pub const RIM_TYPEKEYBOARD: RID_DEVICE_INFO_TYPE = 1u32;
pub const RIM_TYPEMOUSE: RID_DEVICE_INFO_TYPE = 0u32;
