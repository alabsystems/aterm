// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
use crate::oracle::Plant;
pub const PLANTS: &[Plant] = &[
    Plant {
        name: r###"alias-signedness"###,
        phase: r###"conformance"###,
        file: r###"src/core.rs"###,
        old: r###"pub type HRESULT = i32;"###,
        new: r###"pub type HRESULT = u32;"###,
    },
    Plant {
        name: r###"alias-same-width"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/Foundation.rs"###,
        old: r###"pub type WPARAM = usize;"###,
        new: r###"pub type WPARAM = u64;"###,
    },
    Plant {
        name: r###"alias-mutability"###,
        phase: r###"conformance"###,
        file: r###"src/core.rs"###,
        old: r###"pub type PCWSTR = *const u16;"###,
        new: r###"pub type PCWSTR = *mut u16;"###,
    },
    Plant {
        name: r###"field-type"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"pub struct MSG {
    pub hwnd: super::super::Foundation::HWND,
    pub message: u32,"###,
        new: r###"pub struct MSG {
    pub hwnd: super::super::Foundation::HWND,
    pub message: i32,"###,
    },
    Plant {
        name: r###"field-order"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/Foundation.rs"###,
        old: r###"pub struct RECT {
    pub left: i32,
    pub top: i32,"###,
        new: r###"pub struct RECT {
    pub top: i32,
    pub left: i32,"###,
    },
    Plant {
        name: r###"field-dropped"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"    pub lParam: super::super::Foundation::LPARAM,
    pub time: u32,
    pub pt: super::super::Foundation::POINT,
}"###,
        new: r###"    pub lParam: super::super::Foundation::LPARAM,
    pub pt: super::super::Foundation::POINT,
}"###,
    },
    Plant {
        name: r###"packing"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/Graphics/Dwm.rs"###,
        old: r###"#[repr(C, packed(1))]
#[derive(Clone, Copy)]
pub struct DWM_BLURBEHIND {"###,
        new: r###"#[repr(C)]
#[derive(Clone, Copy)]
pub struct DWM_BLURBEHIND {"###,
    },
    Plant {
        name: r###"const-value"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"pub const WM_PAINT: u32 = 15u32;"###,
        new: r###"pub const WM_PAINT: u32 = 16u32;"###,
    },
    Plant {
        name: r###"const-type"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"pub const WM_PAINT: u32 = 15u32;"###,
        new: r###"pub const WM_PAINT: i32 = 15i32;"###,
    },
    Plant {
        name: r###"pointer-const-value"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"pub const IDC_ARROW: crate::core::PCWSTR = 32512u16 as _;"###,
        new: r###"pub const IDC_ARROW: crate::core::PCWSTR = 32513u16 as _;"###,
    },
    Plant {
        name: r###"cstr-value"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/Security/Cryptography.rs"###,
        old: r###""1.3.6.1.5.5.7.3.1""###,
        new: r###""1.3.6.1.5.5.7.3.2""###,
    },
    Plant {
        name: r###"missing-item"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"pub const WM_PAINT: u32 = 15u32;
"###,
        new: r###""###,
    },
    Plant {
        name: r###"no-default"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"impl Default for MSG {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}
"###,
        new: r###""###,
    },
    Plant {
        name: r###"fn-argument"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"fn PeekMessageW(lpmsg : *mut MSG,"###,
        new: r###"fn PeekMessageW(lpmsg : *const MSG,"###,
    },
    Plant {
        name: r###"fn-return"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"wremovemsg : PEEK_MESSAGE_REMOVE_TYPE) -> crate::core::BOOL);"###,
        new: r###"wremovemsg : PEEK_MESSAGE_REMOVE_TYPE) -> u32);"###,
    },
    Plant {
        name: r###"fn-abi"###,
        phase: r###"conformance"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###""system" fn PeekMessageW("###,
        new: r###""C" fn PeekMessageW("###,
    },
    Plant {
        name: r###"link-name"###,
        phase: r###"import"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"link!("user32.dll" "system" fn PeekMessageW("###,
        new: r###"link!("user32.dll" "system" "PeekMessageA" fn PeekMessageW("###,
    },
    Plant {
        name: r###"dll"###,
        phase: r###"import"###,
        file: r###"src/Win32/UI/WindowsAndMessaging.rs"###,
        old: r###"link!("user32.dll" "system" fn PeekMessageW("###,
        new: r###"link!("kernel32.dll" "system" fn PeekMessageW("###,
    },
    // The hand-written COM vocabulary and DXGI slice (WB.2), held to
    // `windows` 0.62.2 by win32-oracle/conformance/src/com.rs.
    Plant {
        name: r###"com-slot-inserted"###,
        phase: r###"conformance"###,
        file: r###"src/dxgi.rs"###,
        old: r###"    // IDXGIOutput6
    pub GetDesc1:"###,
        new: r###"    // IDXGIOutput6
    pub Reserved: usize,
    pub GetDesc1:"###,
    },
    Plant {
        name: r###"com-slot-order"###,
        phase: r###"conformance"###,
        file: r###"src/dxgi.rs"###,
        old: r###"    // IDXGIOutput
    pub GetDesc: usize,
    pub GetDisplayModeList: usize,"###,
        new: r###"    // IDXGIOutput
    pub GetDisplayModeList: usize,
    pub GetDesc: usize,"###,
    },
    Plant {
        name: r###"com-iunknown-order"###,
        phase: r###"conformance"###,
        file: r###"src/com.rs"###,
        old: r###"    pub AddRef: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub Release: unsafe extern "system" fn(this: *mut c_void) -> u32,"###,
        new: r###"    pub Release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub AddRef: unsafe extern "system" fn(this: *mut c_void) -> u32,"###,
    },
    Plant {
        name: r###"com-slot-type"###,
        phase: r###"conformance"###,
        file: r###"src/dxgi.rs"###,
        old: r###"fn(*mut c_void, DXGI_COLOR_SPACE_TYPE, *mut u32) -> HRESULT,"###,
        new: r###"fn(*mut c_void, DXGI_COLOR_SPACE_TYPE, *mut i32) -> HRESULT,"###,
    },
    Plant {
        name: r###"com-iid"###,
        phase: r###"conformance"###,
        file: r###"src/dxgi.rs"###,
        old: r###"0x068346e8_aaec_4b84_add7_137f513f77a1"###,
        new: r###"0x068346e8_aaec_4b84_add7_137f513f77a2"###,
    },
    Plant {
        name: r###"com-field-order"###,
        phase: r###"conformance"###,
        file: r###"src/dxgi.rs"###,
        old: r###"    pub MinLuminance: f32,
    pub MaxLuminance: f32,"###,
        new: r###"    pub MaxLuminance: f32,
    pub MinLuminance: f32,"###,
    },
    Plant {
        name: r###"com-const-value"###,
        phase: r###"conformance"###,
        file: r###"src/dxgi.rs"###,
        old: r###"DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020: DXGI_COLOR_SPACE_TYPE = 12i32;"###,
        new: r###"DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020: DXGI_COLOR_SPACE_TYPE = 13i32;"###,
    },
    Plant {
        name: r###"com-dll"###,
        phase: r###"import"###,
        file: r###"src/dxgi.rs"###,
        old: r###"link!("dxgi.dll" "system" fn CreateDXGIFactory1("###,
        new: r###"link!("d3d11.dll" "system" fn CreateDXGIFactory1("###,
    },
];
