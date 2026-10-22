// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The protocol tables libwayland reads: `struct wl_interface` and
//! `struct wl_message` (wayland-util.h), laid out as C lays them out, built
//! as statics by the generated protocol modules. libwayland marshals and
//! demarshals every message by its table's signature, creates the objects an
//! event announces from its table's types, and checks an object argument
//! against the expected interface by name (`wl_interface_equal`), so a table
//! here and libwayland's own copy of the same interface interoperate.

use std::ffi::{CStr, c_char, c_int};
use std::ptr::null;

/// `struct wl_interface`.
#[repr(C)]
pub struct Interface {
    name: *const c_char,
    version: c_int,
    method_count: c_int,
    methods: *const Message,
    event_count: c_int,
    events: *const Message,
}

// SAFETY: immutable after construction; every pointer is to static data.
unsafe impl Sync for Interface {}

/// `struct wl_message`.
#[repr(C)]
pub struct Message {
    name: *const c_char,
    signature: *const c_char,
    types: *const TypeRef,
}

// SAFETY: as for Interface.
unsafe impl Sync for Message {}

/// One entry of a message's `types` array: the interface of an object or
/// new_id argument, or null.
#[repr(transparent)]
pub struct TypeRef(*const Interface);

// SAFETY: as for Interface.
unsafe impl Sync for TypeRef {}

impl TypeRef {
    /// No interface (a non-object argument, or an object of any type).
    pub const NULL: TypeRef = TypeRef(null());

    /// The argument's interface.
    #[inline]
    pub const fn to(interface: &'static Interface) -> TypeRef {
        TypeRef(interface)
    }
}

/// Nulls enough for any message whose arguments name no interface: one per
/// signature argument, as wayland-scanner's `types_null`.
pub static NULLS: [TypeRef; 8] = [
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
    TypeRef::NULL,
];

#[inline]
const fn count(n: usize) -> c_int {
    assert!(n <= c_int::MAX as usize);
    n as c_int
}

impl Interface {
    /// A table: `requests` and `events` in opcode order.
    #[inline]
    pub const fn new(
        name: &'static CStr,
        version: u32,
        requests: &'static [Message],
        events: &'static [Message],
    ) -> Interface {
        Interface {
            name: name.as_ptr(),
            version: count(version as usize),
            method_count: count(requests.len()),
            methods: if requests.is_empty() {
                null()
            } else {
                requests.as_ptr()
            },
            event_count: count(events.len()),
            events: if events.is_empty() {
                null()
            } else {
                events.as_ptr()
            },
        }
    }

    /// The interface's name.
    #[inline]
    pub fn name(&self) -> &'static CStr {
        // SAFETY: built from a `&'static CStr` by `new`.
        unsafe { CStr::from_ptr(self.name) }
    }

    /// The highest version this table describes.
    #[inline]
    pub fn version(&self) -> u32 {
        u32::try_from(self.version).unwrap_or(0)
    }

    /// The requests, in opcode order.
    #[inline]
    pub fn requests(&self) -> &'static [Message] {
        // SAFETY: `new` set the pointer and count from one static slice.
        unsafe { messages(self.methods, self.method_count) }
    }

    /// The events, in opcode order.
    #[inline]
    pub fn events(&self) -> &'static [Message] {
        // SAFETY: as for `requests`.
        unsafe { messages(self.events, self.event_count) }
    }
}

/// # Safety
/// `p` and `n` come from one static slice (or null and 0).
#[inline]
unsafe fn messages(p: *const Message, n: c_int) -> &'static [Message] {
    match usize::try_from(n) {
        Ok(n) if n > 0 && !p.is_null() => {
            // SAFETY: the caller's contract.
            unsafe { std::slice::from_raw_parts(p, n) }
        }
        _ => &[],
    }
}

impl Message {
    /// A message: `types` holds one entry per signature argument (`new_id`
    /// without an interface counts three: `s`, `u`, `n`). Extra type slots
    /// are allowed; a shorter slice is refused before its pointer is stored.
    ///
    /// # Panics
    /// Panics when `types` has fewer slots than the signature has arguments.
    #[inline]
    pub const fn new(
        name: &'static CStr,
        signature: &'static CStr,
        types: &'static [TypeRef],
    ) -> Message {
        assert!(
            types.len() >= signature_args(signature.to_bytes()),
            "a type slot is required for every signature argument"
        );
        Message {
            name: name.as_ptr(),
            signature: signature.as_ptr(),
            types: types.as_ptr(),
        }
    }

    /// The message's name.
    #[inline]
    pub fn name(&self) -> &'static CStr {
        // SAFETY: built from a `&'static CStr` by `new`.
        unsafe { CStr::from_ptr(self.name) }
    }

    /// The signature: an optional since-version prefix, then one type code
    /// per argument (`i u f s o n a h`), each optionally after `?`.
    #[inline]
    pub fn signature(&self) -> &'static CStr {
        // SAFETY: as for `name`.
        unsafe { CStr::from_ptr(self.signature) }
    }

    /// The interface of signature argument `i`, if it names one.
    #[inline]
    pub fn arg_interface(&self, i: usize) -> Option<&'static Interface> {
        if i >= signature_args(self.signature().to_bytes()) {
            return None;
        }
        // SAFETY: `types` holds one entry per signature argument (`new`'s
        // contract), each null or a static interface.
        unsafe { (*self.types.add(i)).0.as_ref() }
    }
}

/// The number of arguments a signature describes.
#[inline]
pub const fn signature_args(sig: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < sig.len() {
        if matches!(
            sig[i],
            b'i' | b'u' | b'f' | b's' | b'o' | b'n' | b'a' | b'h'
        ) {
            count += 1;
        }
        i += 1;
    }
    count
}
