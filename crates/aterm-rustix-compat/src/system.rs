// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `rustix::system`: `uname`.

use std::ffi::CStr;

/// The six `struct utsname` strings, as bytes.
type Field = [u8; 65];

// `utsname`'s fields are what `Field` copies.
const _: () = {
    assert!(core::mem::size_of::<libc::utsname>() == 6 * core::mem::size_of::<Field>());
};

/// What `uname(2)` reported: system name, host name, release, version,
/// machine and NIS domain name.
#[doc(alias = "utsname")]
pub struct Uname {
    sysname: Field,
    nodename: Field,
    release: Field,
    version: Field,
    machine: Field,
    domainname: Field,
}

/// `uname()`: names this system.
#[inline]
pub fn uname() -> Uname {
    let empty = [0; 65];
    let mut raw = libc::utsname {
        sysname: empty,
        nodename: empty,
        release: empty,
        version: empty,
        machine: empty,
        domainname: empty,
    };
    // SAFETY: `raw` is a live, writable `struct utsname` for the call. uname(2)
    // fails only for a bad pointer, so the result is ignored, as upstream
    // ignores it; on that impossible path the fields stay empty strings.
    let _ = unsafe { libc::uname(&mut raw) };
    Uname {
        sysname: bytes(&raw.sysname),
        nodename: bytes(&raw.nodename),
        release: bytes(&raw.release),
        version: bytes(&raw.version),
        machine: bytes(&raw.machine),
        domainname: bytes(&raw.domainname),
    }
}

/// A `c_char` array as bytes (`c_char` is `i8` on x86_64, `u8` on aarch64).
fn bytes(field: &[libc::c_char; 65]) -> Field {
    field.map(|c| u8::from_ne_bytes(c.to_ne_bytes()))
}

/// The string up to the first NUL; the kernel always writes one.
fn text(field: &Field) -> &CStr {
    CStr::from_bytes_until_nul(field).unwrap_or_default()
}

impl Uname {
    /// `sysname`: the kernel's name (`Linux`).
    #[inline]
    pub fn sysname(&self) -> &CStr {
        text(&self.sysname)
    }

    /// `nodename`: the host name.
    #[inline]
    pub fn nodename(&self) -> &CStr {
        text(&self.nodename)
    }

    /// `release`: the kernel release.
    #[inline]
    pub fn release(&self) -> &CStr {
        text(&self.release)
    }

    /// `version`: the kernel build.
    #[inline]
    pub fn version(&self) -> &CStr {
        text(&self.version)
    }

    /// `machine`: the hardware name.
    #[inline]
    pub fn machine(&self) -> &CStr {
        text(&self.machine)
    }

    /// `domainname`: the NIS domain name.
    #[inline]
    pub fn domainname(&self) -> &CStr {
        text(&self.domainname)
    }
}

impl core::fmt::Debug for Uname {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:?} {:?} {:?} {:?} {:?} {:?}",
            self.sysname(),
            self.nodename(),
            self.release(),
            self.version(),
            self.machine(),
            self.domainname(),
        )
    }
}
