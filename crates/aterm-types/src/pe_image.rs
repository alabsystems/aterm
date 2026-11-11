// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PE `Subsystem` field reader (`IMAGE_SUBSYSTEM_WINDOWS_GUI` vs console).

use std::path::Path;

/// `IMAGE_SUBSYSTEM_WINDOWS_GUI` (`WinNT.h`): GUI subsystem (`2`; console is `3`).
pub const IMAGE_SUBSYSTEM_WINDOWS_GUI: u16 = 2;

/// Number of leading image bytes read when probing the PE header.
pub const HEAD_BYTES: u64 = 4096;

/// Read the PE `Subsystem` field from `head`, or `None` if `head` is not a complete PE header.
#[must_use]
pub fn subsystem(head: &[u8]) -> Option<u16> {
    let bytes = |at: usize, len: usize| head.get(at..at.checked_add(len)?);
    if bytes(0, 2)? != b"MZ" {
        return None;
    }
    let nt = u32::from_le_bytes(bytes(0x3C, 4)?.try_into().ok()?);
    let nt = usize::try_from(nt).ok()?;
    if bytes(nt, 4)? != b"PE\0\0" {
        return None;
    }
    let field = bytes(nt.checked_add(4 + 20 + 68)?, 2)?;
    Some(u16::from_le_bytes(field.try_into().ok()?))
}

/// Read the PE `Subsystem` field from the first [`HEAD_BYTES`] of `path`.
#[must_use]
pub fn file_subsystem(path: &Path) -> Option<u16> {
    use std::io::Read as _;
    let (file, meta) = crate::fs_restricted::open_regular_nonblock_with_meta(path).ok()?;
    let cap = usize::try_from(meta.len().min(HEAD_BYTES).saturating_add(1)).unwrap_or(0);
    let mut head = Vec::with_capacity(cap);
    file.take(HEAD_BYTES).read_to_end(&mut head).ok()?;
    subsystem(&head)
}

#[cfg(test)]
#[path = "pe_image_tests.rs"]
mod tests;
