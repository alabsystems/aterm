// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every string a running app's windows carry, read through the Accessibility API as any process
//! the person granted Accessibility reads them: each element's string attributes ([`STRINGS`])
//! and each text element's characters (`AXStringForRange`), depth first.
//!
//! One file, two binaries: `lab reveal-tab` reads its instance's tree with it in-process
//! (`reveal_tab.rs`, `mod ax`), and the guest helper's `ax-texts` verb reads it in a `lab mac nux`
//! guest under the person base's grant (`guest/main.rs`, `mod tree`), so both read the same tree
//! the same way.

#![expect(
    unsafe_code,
    reason = "the Accessibility API: reading the tree another process can read"
)]

use std::ffi::c_void;

type CfRef = *const c_void;

#[repr(C)]
struct CfRange {
    location: isize,
    length: isize,
}

/// How deep a window's tree is read.
const DEPTH: usize = 32;
/// The most elements read in a pass.
const MOST: usize = 50_000;
/// The most characters read of one text element.
const CHARACTERS: i64 = 1 << 16;
/// `kAXValueCFRangeType`.
const RANGE: u32 = 4;
/// `kCFNumberSInt64Type`.
const SINT64: isize = 4;
const UTF8: u32 = 0x0800_0100;
/// Each element's string attributes read.
const STRINGS: [&str; 7] = [
    "AXTitle",
    "AXDescription",
    "AXValue",
    "AXHelp",
    "AXSelectedText",
    "AXPlaceholderValue",
    "AXRoleDescription",
];

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CfRef);
    fn CFGetTypeID(cf: CfRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFArrayGetCount(array: CfRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CfRef, index: isize) -> CfRef;
    fn CFStringGetLength(string: CfRef) -> isize;
    fn CFStringGetCharacters(string: CfRef, range: CfRange, buffer: *mut u16);
    fn CFNumberGetValue(number: CfRef, kind: isize, out: *mut c_void) -> u8;
    fn CFStringCreateWithBytes(
        alloc: CfRef,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: u8,
    ) -> CfRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> CfRef;
    fn AXUIElementCopyAttributeValue(el: CfRef, attr: CfRef, out: *mut CfRef) -> i32;
    fn AXUIElementCopyParameterizedAttributeValue(
        el: CfRef,
        attr: CfRef,
        param: CfRef,
        out: *mut CfRef,
    ) -> i32;
    fn AXUIElementSetMessagingTimeout(el: CfRef, secs: f32) -> i32;
    fn AXValueCreate(kind: u32, value: *const c_void) -> CfRef;
}

/// A CF object this code owns one reference to (null: none).
struct Owned(CfRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: non-null, and this value owns one reference to it.
            unsafe { CFRelease(self.0) }
        }
    }
}

/// Whether this process (its responsible app) holds Accessibility, with no prompt.
pub(super) fn trusted() -> bool {
    // SAFETY: a plain query.
    unsafe { AXIsProcessTrusted() }
}

fn cf_string(text: &str) -> Owned {
    // SAFETY: the bytes and length of a live UTF-8 string; the result is owned.
    Owned(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            text.as_ptr(),
            isize::try_from(text.len()).unwrap_or(0),
            UTF8,
            0,
        )
    })
}

fn text(cf: CfRef) -> Option<String> {
    if cf.is_null() {
        return None;
    }
    // SAFETY: `CFGetTypeID` reads any live CF object; a non-string is refused before it is
    // read as one, and the buffer holds the string's whole length.
    unsafe {
        if CFGetTypeID(cf) != CFStringGetTypeID() {
            return None;
        }
        let length = CFStringGetLength(cf);
        let mut units = vec![0u16; usize::try_from(length).ok()?];
        CFStringGetCharacters(
            cf,
            CfRange {
                location: 0,
                length,
            },
            units.as_mut_ptr(),
        );
        Some(String::from_utf16_lossy(&units))
    }
}

fn number(cf: CfRef) -> Option<i64> {
    if cf.is_null() {
        return None;
    }
    let mut out = 0i64;
    // SAFETY: a live CF object, read as a number only when it is one, into an i64.
    unsafe {
        (CFGetTypeID(cf) == CFNumberGetTypeID()
            && CFNumberGetValue(cf, SINT64, (&raw mut out).cast()) != 0)
            .then_some(out)
    }
}

fn items(array: CfRef) -> Vec<CfRef> {
    if array.is_null() {
        return Vec::new();
    }
    // SAFETY: a live CF object, read as an array only when it is one; its items are
    // borrowed while the array lives.
    unsafe {
        if CFGetTypeID(array) != CFArrayGetTypeID() {
            return Vec::new();
        }
        (0..CFArrayGetCount(array))
            .map(|index| CFArrayGetValueAtIndex(array, index))
            .collect()
    }
}

fn attr(el: CfRef, name: &str) -> Owned {
    let key = cf_string(name);
    let mut out: CfRef = std::ptr::null();
    // SAFETY: an AX element, an attribute name and an out pointer; the value is owned.
    let err = unsafe { AXUIElementCopyAttributeValue(el, key.0, &raw mut out) };
    Owned(if err == 0 { out } else { std::ptr::null() })
}

/// A text element's first [`CHARACTERS`] characters (`AXStringForRange`), as a reader asks
/// for the text it does not hold as a value.
fn characters(el: CfRef) -> Option<String> {
    let count = number(attr(el, "AXNumberOfCharacters").0)?.min(CHARACTERS);
    if count <= 0 {
        return None;
    }
    let range = CfRange {
        location: 0,
        length: isize::try_from(count).ok()?,
    };
    // SAFETY: a CFRange of the kind named, copied into an owned AXValue.
    let param = Owned(unsafe { AXValueCreate(RANGE, (&raw const range).cast()) });
    if param.0.is_null() {
        return None;
    }
    let key = cf_string("AXStringForRange");
    let mut out: CfRef = std::ptr::null();
    // SAFETY: an AX element, an attribute name, its parameter and an out pointer; the value
    // is owned.
    let err =
        unsafe { AXUIElementCopyParameterizedAttributeValue(el, key.0, param.0, &raw mut out) };
    let out = Owned(if err == 0 { out } else { std::ptr::null() });
    text(out.0)
}

/// Every string `pid`'s windows' elements carry, depth first.
pub(super) fn texts(pid: i32) -> Vec<String> {
    // SAFETY: an app element for a pid, owned.
    let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
    if app.0.is_null() {
        return Vec::new();
    }
    // SAFETY: the element this code created; a busy app answers within the timeout or not.
    unsafe { AXUIElementSetMessagingTimeout(app.0, 2.0) };
    let windows = attr(app.0, "AXWindows");
    let mut out = Vec::new();
    let mut read = 0usize;
    for window in items(windows.0) {
        walk(window, 0, &mut read, &mut out);
    }
    out
}

fn walk(el: CfRef, depth: usize, read: &mut usize, out: &mut Vec<String>) {
    if depth > DEPTH || *read >= MOST {
        return;
    }
    *read += 1;
    for name in STRINGS {
        out.extend(text(attr(el, name).0).filter(|text| !text.is_empty()));
    }
    out.extend(characters(el));
    let children = attr(el, "AXChildren");
    for child in items(children.0) {
        walk(child, depth + 1, read, out);
    }
}
