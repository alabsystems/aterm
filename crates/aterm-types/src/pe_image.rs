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
    let mut head = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(HEAD_BYTES).read_to_end(&mut head))
        .ok()?;
    subsystem(&head)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest header the reader accepts: `MZ`, the NT offset, `PE\0\0`
    /// there, and `subsystem` at its offset.
    fn header(nt: usize, subsystem: u16) -> Vec<u8> {
        let mut head = vec![0u8; nt + 4 + 20 + 70];
        head[..2].copy_from_slice(b"MZ");
        head[0x3C..0x40].copy_from_slice(&u32::try_from(nt).expect("small").to_le_bytes());
        head[nt..nt + 4].copy_from_slice(b"PE\0\0");
        head[nt + 92..nt + 94].copy_from_slice(&subsystem.to_le_bytes());
        head
    }

    #[test]
    fn the_subsystem_is_read_at_the_nt_headers_offset() {
        assert_eq!(
            subsystem(&header(0x80, 2)),
            Some(IMAGE_SUBSYSTEM_WINDOWS_GUI)
        );
        assert_eq!(subsystem(&header(0xF8, 3)), Some(3));
        assert_eq!(subsystem(b"#!/bin/sh\n"), None);
        let dir = std::env::temp_dir().join(format!("aterm-pe-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let file = dir.join("image.exe");
        std::fs::write(&file, header(0x100, IMAGE_SUBSYSTEM_WINDOWS_GUI)).expect("write");
        assert_eq!(file_subsystem(&file), Some(IMAGE_SUBSYSTEM_WINDOWS_GUI));
        assert_eq!(file_subsystem(&dir.join("missing.exe")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
