// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The running image's link identity: the bytes its linker stamped into the executable this code
//! is linked into, read from that executable's mapping in memory, never from a file on disk that
//! a later build may have replaced.
//!
//! - **macOS**: the `LC_UUID` load command of the Mach-O image that holds this module's code,
//!   found through `dladdr` ([`mapped`], which the stall watchdog's stack report reads too).
//! - **Linux**: the `NT_GNU_BUILD_ID` note of the main program, the first object
//!   `dl_iterate_phdr` visits.
//! - **Windows**: the CodeView `RSDS` record (the PDB's GUID and age) in the debug directory of
//!   the executable's in-memory image (`GetModuleHandleW(NULL)`).
//!
//! Each changes whenever the linked code does, a build of an uncommitted tree included, which a
//! version stamp does not. An image that carries none (a linker told not to write one) and every
//! other platform answer `None`. The parsers ([`lc_uuid`], [`gnu_build_id`], [`codeview`]) are
//! pure functions of the bytes they are handed, compiled and tested on every host.
//!
//! Of the readers of the live image, only macOS's ([`mapped`]) is held to anything by a test,
//! which compares what it reads with the `LC_UUID` of the test binary's file. No test reads the
//! live image through the Linux or Windows reader (`elf::main_build_id`, `pe::main_codeview`):
//! on those hosts this crate's tests hold only the parsers, so that the readers find the main
//! program's identity there is not shown. Each answers `None` where it finds none; one that
//! answered bytes that do not move with the build would let a caller keyed on [`identity`] keep
//! what an earlier build made.

use std::sync::OnceLock;

/// The link identity of the image this code runs in, read once per process: the `LC_UUID` on
/// macOS (16 bytes), the GNU build id on Linux (as many bytes as its note holds), the CodeView
/// GUID followed by its age on Windows (20 bytes). `None` where the image carries none or this
/// platform has no reader here.
#[must_use]
pub fn identity() -> Option<&'static [u8]> {
    static IDENTITY: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    IDENTITY.get_or_init(read).as_deref()
}

#[cfg(target_os = "macos")]
fn read() -> Option<Vec<u8>> {
    mapped()?.uuid.map(|uuid| uuid.to_vec())
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn read() -> Option<Vec<u8>> {
    elf::main_build_id()
}

#[cfg(all(windows, target_pointer_width = "64"))]
fn read() -> Option<Vec<u8>> {
    pe::main_codeview().map(|record| record.to_vec())
}

#[cfg(not(any(
    target_os = "macos",
    all(target_os = "linux", target_pointer_width = "64"),
    all(windows, target_pointer_width = "64")
)))]
const fn read() -> Option<Vec<u8>> {
    None
}

/// `LC_UUID`, the load command that carries a Mach-O image's UUID.
const LC_UUID: u32 = 0x1b;

/// The `LC_UUID` among `ncmds` load commands laid out in `commands` (the bytes that follow a
/// 64-bit Mach-O header, `sizeofcmds` of them): `None` where no command is one, or where a
/// command's size is short of its own header or runs past `commands`.
#[must_use]
pub fn lc_uuid(commands: &[u8], ncmds: u32) -> Option<[u8; 16]> {
    let mut at = 0usize;
    for _ in 0..ncmds {
        let header = commands.get(at..at.checked_add(8)?)?;
        let cmd = u32::from_ne_bytes(header[..4].try_into().ok()?);
        let size = u32::from_ne_bytes(header[4..].try_into().ok()?) as usize;
        if size < 8 {
            return None;
        }
        let command = commands.get(at..at.checked_add(size)?)?;
        if cmd == LC_UUID {
            return command.get(8..24)?.try_into().ok();
        }
        at += size;
    }
    None
}

/// The image of this process that holds this module's code, as the loader mapped it.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapped {
    /// Where its Mach-O header is mapped: its load address.
    pub base: usize,
    /// Its `LC_UUID`; `None` where it carries none, or its header is not a 64-bit Mach-O one or
    /// names load commands past what one may hold.
    pub uuid: Option<[u8; 16]>,
}

/// The image of this process that holds this module's code: the executable it is linked into.
/// `None` where the loader does not know the address.
#[cfg(target_os = "macos")]
#[must_use]
pub fn mapped() -> Option<Mapped> {
    use std::ffi::{c_char, c_void};

    #[repr(C)]
    struct DlInfo {
        fname: *const c_char,
        fbase: *mut c_void,
        sname: *const c_char,
        saddr: *mut c_void,
    }
    unsafe extern "C" {
        fn dladdr(address: *const c_void, info: *mut DlInfo) -> i32;
    }

    /// `MH_MAGIC_64`: a 64-bit Mach-O header in this host's byte order.
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    /// The most load-command bytes a header is believed to name: the loader maps far fewer.
    const MAX_COMMANDS: usize = 1 << 20;
    let mut info = DlInfo {
        fname: std::ptr::null(),
        fbase: std::ptr::null_mut(),
        sname: std::ptr::null(),
        saddr: std::ptr::null_mut(),
    };
    // SAFETY: dladdr searches the loader's tables for the address of this function, code of this
    // image, without reading memory at it; `info` is a live, correctly laid-out output record.
    if unsafe { dladdr(mapped as *const c_void, &raw mut info) } == 0 || info.fbase.is_null() {
        return None;
    }
    let base = info.fbase.cast::<u8>();
    // SAFETY: dladdr names the mapped Mach-O header of the image holding this code, which stays
    // loaded while its code runs, and a Mach-O header is at least 32 bytes.
    let (magic, ncmds, size) = unsafe {
        (
            base.cast::<u32>().read_unaligned(),
            base.add(16).cast::<u32>().read_unaligned(),
            base.add(20).cast::<u32>().read_unaligned(),
        )
    };
    let size = size as usize;
    let uuid = if magic == MH_MAGIC_64 && size <= MAX_COMMANDS {
        // SAFETY: a 64-bit Mach-O header is followed by its `sizeofcmds` bytes of load commands,
        // mapped with it, and `size` is that field, bounded above.
        let commands = unsafe { std::slice::from_raw_parts(base.add(32), size) };
        lc_uuid(commands, ncmds)
    } else {
        None
    };
    Some(Mapped {
        base: base as usize,
        uuid,
    })
}

/// `NT_GNU_BUILD_ID`, the note type of a GNU build id.
const NT_GNU_BUILD_ID: u32 = 3;

/// The GNU build id among the ELF notes laid out in `notes` (one `PT_NOTE` segment's bytes):
/// the descriptor of the first note named `GNU` of type `NT_GNU_BUILD_ID`. `None` where no note
/// is one, or where a note's sizes run past `notes`.
#[must_use]
pub fn gnu_build_id(notes: &[u8]) -> Option<&[u8]> {
    let pad = |n: usize| n.checked_add(3).map(|n| n & !3);
    let mut at = 0usize;
    while let Some(header) = notes.get(at..at.checked_add(12)?) {
        let word = |i: usize| -> Option<usize> {
            Some(u32::from_ne_bytes(header[i..i + 4].try_into().ok()?) as usize)
        };
        let (namesz, descsz, kind) = (word(0)?, word(4)?, word(8)?);
        let name_at = at + 12;
        let desc_at = name_at.checked_add(pad(namesz)?)?;
        let next = desc_at.checked_add(pad(descsz)?)?;
        let name = notes.get(name_at..name_at.checked_add(namesz)?)?;
        let desc = notes.get(desc_at..desc_at.checked_add(descsz)?)?;
        if kind == NT_GNU_BUILD_ID as usize && name == b"GNU\0" {
            return Some(desc);
        }
        at = next;
    }
    None
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
mod elf {
    use std::ffi::{c_char, c_int, c_void};

    /// `PT_NOTE`: a segment of ELF notes.
    const PT_NOTE: u32 = 4;

    /// The leading fields of glibc's and musl's `struct dl_phdr_info` on a 64-bit target: the
    /// loader hands a pointer to one, and only these are read.
    #[repr(C)]
    struct DlPhdrInfo {
        addr: usize,
        name: *const c_char,
        phdr: *const Phdr,
        phnum: u16,
    }

    /// `Elf64_Phdr`.
    #[repr(C)]
    struct Phdr {
        kind: u32,
        flags: u32,
        offset: u64,
        vaddr: u64,
        paddr: u64,
        filesz: u64,
        memsz: u64,
        align: u64,
    }

    type Visit = unsafe extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> c_int;

    unsafe extern "C" {
        fn dl_iterate_phdr(callback: Option<Visit>, data: *mut c_void) -> c_int;
    }

    /// The first object visited, the main program: its build id into `data`, then stop.
    unsafe extern "C" fn visit(info: *mut DlPhdrInfo, _size: usize, data: *mut c_void) -> c_int {
        // SAFETY: the loader hands a live `dl_phdr_info` for the duration of the call, and
        // `data` is the `Option<Vec<u8>>` `main_build_id` passed, borrowed for the call alone.
        let (info, found) = unsafe { (&*info, &mut *data.cast::<Option<Vec<u8>>>()) };
        if !info.phdr.is_null() {
            // SAFETY: `phdr` points at the object's `phnum` program headers, mapped with it.
            let headers = unsafe { std::slice::from_raw_parts(info.phdr, info.phnum.into()) };
            for header in headers.iter().filter(|header| header.kind == PT_NOTE) {
                let (Ok(vaddr), Ok(size)) = (
                    usize::try_from(header.vaddr),
                    usize::try_from(header.filesz),
                ) else {
                    continue;
                };
                let Some(at) = info.addr.checked_add(vaddr) else {
                    continue;
                };
                // SAFETY: a `PT_NOTE` segment of a loaded object lies in a `PT_LOAD` one, mapped
                // at the object's load bias plus its address, `filesz` bytes of it from the file.
                let notes = unsafe { std::slice::from_raw_parts(at as *const u8, size) };
                if let Some(id) = super::gnu_build_id(notes) {
                    *found = Some(id.to_vec());
                    break;
                }
            }
        }
        1
    }

    pub(super) fn main_build_id() -> Option<Vec<u8>> {
        let mut found: Option<Vec<u8>> = None;
        // SAFETY: `visit` reads only what the loader hands it and writes only `found`, which
        // outlives the call; it stops the walk after the first object.
        unsafe { dl_iterate_phdr(Some(visit), (&raw mut found).cast()) };
        found
    }
}

/// `IMAGE_DEBUG_TYPE_CODEVIEW`.
const DEBUG_CODEVIEW: u32 = 2;

/// The CodeView `RSDS` record's GUID and age (20 bytes) in the debug directory of `image`, a PE32+
/// image as its loader maps it (sections at their virtual addresses, `SizeOfImage` bytes): `None`
/// where it is no PE32+ image, has no debug directory or no CodeView entry, or where an offset
/// runs past `image`.
#[must_use]
pub fn codeview(image: &[u8]) -> Option<[u8; 20]> {
    let u16_at = |at: usize| -> Option<usize> {
        Some(u16::from_le_bytes(image.get(at..at.checked_add(2)?)?.try_into().ok()?).into())
    };
    let u32_at = |at: usize| -> Option<usize> {
        Some(u32::from_le_bytes(image.get(at..at.checked_add(4)?)?.try_into().ok()?) as usize)
    };
    if image.get(..2)? != b"MZ" {
        return None;
    }
    let nt = u32_at(0x3c)?;
    if image.get(nt..nt.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    // The optional header follows the 4-byte signature and the 20-byte file header.
    let optional = nt.checked_add(24)?;
    if u16_at(optional)? != 0x20b {
        return None;
    }
    // PE32+: `NumberOfRvaAndSizes` at 108, the data directories from 112, the debug one sixth.
    if u32_at(optional.checked_add(108)?)? <= 6 {
        return None;
    }
    let directory = optional.checked_add(112 + 6 * 8)?;
    let (rva, size) = (u32_at(directory)?, u32_at(directory.checked_add(4)?)?);
    for entry in 0..size / 28 {
        let at = rva.checked_add(entry * 28)?;
        if u32_at(at.checked_add(12)?)? != DEBUG_CODEVIEW as usize {
            continue;
        }
        let (length, data) = (u32_at(at.checked_add(16)?)?, u32_at(at.checked_add(20)?)?);
        if length < 24 || image.get(data..data.checked_add(4)?)? != b"RSDS" {
            continue;
        }
        return image.get(data + 4..data.checked_add(24)?)?.try_into().ok();
    }
    None
}

#[cfg(all(windows, target_pointer_width = "64"))]
mod pe {
    #[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
    unsafe extern "system" {
        fn GetModuleHandleW(lpModuleName: *const u16) -> *mut std::ffi::c_void;
    }

    /// The executable's CodeView record, read from its own mapped image.
    pub(super) fn main_codeview() -> Option<[u8; 20]> {
        // SAFETY: a null name asks for the executable's module, which stays mapped for the life
        // of the process; the call reads nothing of ours.
        let base = unsafe { GetModuleHandleW(std::ptr::null()) }.cast::<u8>();
        if base.is_null() {
            return None;
        }
        // SAFETY: a mapped PE image begins with its DOS header, 64 bytes, whose `e_lfanew` (at
        // 0x3c) is the offset of its NT headers, which lie in the same mapped header page.
        let nt = unsafe { base.add(0x3c).cast::<u32>().read_unaligned() } as usize;
        if nt > 4096 - 24 - 60 {
            return None;
        }
        // SAFETY: `SizeOfImage` is at 56 in the optional header that follows the NT signature and
        // file header (24 bytes), inside the header page bounded just above.
        let size = unsafe { base.add(nt + 24 + 56).cast::<u32>().read_unaligned() } as usize;
        // SAFETY: the loader maps the whole image, `SizeOfImage` bytes from its base, for the
        // life of the module; `codeview` only reads it.
        let image = unsafe { std::slice::from_raw_parts(base, size) };
        super::codeview(image)
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
