// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one field of a Windows PE image aterm reads: its `Subsystem`, which says
//! whether Windows starts the program with a console. aterm-gui reads it to tell
//! its windowed image from its console image (OS-restart registration);
//! aterm-pty reads it because a GUI-subsystem program never connects to the
//! console of the tab it runs in, so its console host never answers it. Pure
//! over bytes, so its table runs on every host.

use std::path::Path;

/// `IMAGE_SUBSYSTEM_WINDOWS_GUI` (WinNT.h): a program Windows starts with no
/// console. A console program's is `IMAGE_SUBSYSTEM_WINDOWS_CUI` (3).
pub const IMAGE_SUBSYSTEM_WINDOWS_GUI: u16 = 2;

/// How much of an image file is read for its header. The field sits at the NT
/// headers' offset plus 92; every aterm image and `notepad.exe` measured put
/// those headers at 0xF8 or 0x100 (2026-09-30), and the linkers place them right
/// after the DOS stub, so 4 KiB is ample.
pub const HEAD_BYTES: u64 = 4096;

/// The PE `Subsystem` field out of the first bytes of an image file, or `None`
/// when `head` is no PE image or ends before the field: `MZ`, the NT headers'
/// offset at 0x3C, `PE\0\0` there, then the 20-byte file header and the optional
/// header, whose `Subsystem` sits 68 bytes in — the same offset in PE32 and PE32+.
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

/// The `Subsystem` of the image file at `path`, by its first [`HEAD_BYTES`];
/// `None` when it cannot be read (no such file, an app execution alias, which
/// cannot be opened) or holds no PE header.
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

    // The field is read at the NT headers' offset plus 92 wherever they sit,
    // and a file is read up to it.
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
