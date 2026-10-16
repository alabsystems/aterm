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
];
