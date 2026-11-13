// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::ffi::{self, Device, Obj};

/// A device for GPU tests; a machine without one ends the test COULD NOT RUN.
pub(crate) fn device() -> Device {
    Device::preferred_listed()
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no listed Metal device"))
}

/// `cell.metal`'s 16-byte `Uniforms` block: `float2 screen`, `float text_blend`, `float pad`, laid out identically in `repr(C)`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct CellUniforms {
    pub(crate) screen: [f32; 2],
    pub(crate) text_blend: f32,
    pub(crate) pad: f32,
}

/// View `v` as a byte slice for GPU upload.
///
/// # Safety
/// `T` must be `repr(C)` and free of padding-sensitive invariants; the bytes are copied
/// straight into a GPU-visible buffer.
pub(crate) unsafe fn as_bytes<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: `T: Copy` + `repr(C)`; read-only, lives only for the copy.
    unsafe { std::slice::from_raw_parts(std::ptr::from_ref(v).cast::<u8>(), size_of::<T>()) }
}

/// A bare `CALayer` (+1).
pub(crate) fn plain_calayer() -> Obj {
    let _pool = ffi::AutoreleasePool::new();
    // SAFETY: alloc/init, +1, exactly as `Swapchain::new_layer`.
    unsafe {
        let alloc: unsafe extern "C-unwind" fn(ffi::ClassPtr, ffi::Sel) -> ffi::Id =
            ffi::checked_msg();
        let raw = alloc(ffi::class(c"CALayer"), ffi::sel(c"alloc"));
        let init: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::checked_msg();
        Obj::from_owned(init(raw, ffi::sel(c"init"))).expect("CALayer init")
    }
}

/// The two strings a C-string read of an `NSString` gets wrong, as UTF-16 code units and the text they must read back as.
pub(crate) const C_STRING_HOLES: [(&[u16], &str); 2] = [
    (&[0x61, 0x0000, 0x62], "a\u{0}b"),
    (&[0x61, 0xD800, 0x62], "a\u{FFFD}b"),
];

/// A +1 `NSString` of exactly `units`, built without a C string so it may hold U+0000 or an unpaired surrogate, which no Rust `str` can spell.
pub(crate) fn ns_string_of_units(units: &[u16]) -> Obj {
    // SAFETY: `+alloc` returns a +1 instance that `-initWithCharacters:length:` (`@32@0:8r^S16Q24`) consumes, reading `units.len()` code units during the call and returning +1 or nil.
    unsafe {
        let alloc: unsafe extern "C-unwind" fn(ffi::ClassPtr, ffi::Sel) -> ffi::Id =
            ffi::checked_msg();
        let raw = alloc(ffi::class(c"NSString"), ffi::sel(c"alloc"));
        let init: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, *const u16, usize) -> ffi::Id =
            ffi::checked_msg();
        Obj::from_owned(init(
            raw,
            ffi::sel(c"initWithCharacters:length:"),
            units.as_ptr(),
            units.len(),
        ))
    }
    .expect("NSString from UTF-16")
}

/// `-[CALayer sublayers].count` (0 when the property is nil).
pub(crate) fn sublayer_count(layer: &Obj) -> usize {
    let _pool = ffi::AutoreleasePool::new();
    // SAFETY: `sublayers` returns a +0 NSArray (or nil) owned by the pool; `count` is an NSUInteger getter on it.
    unsafe {
        let get: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::checked_msg();
        let arr = get(layer.id(), ffi::sel(c"sublayers"));
        if arr.is_null() {
            return 0;
        }
        let count: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> usize = ffi::checked_msg();
        count(arr, ffi::sel(c"count"))
    }
}
