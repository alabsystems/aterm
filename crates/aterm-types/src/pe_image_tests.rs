// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
    let scratch = crate::test_scratch_dir("éééééé");
    let dir = scratch.path();
    assert_eq!(
        dir.extension().and_then(|ext| ext.to_str()),
        Some("noindex")
    );
    let file = dir.join("image.exe");
    std::fs::write(&file, header(0x100, IMAGE_SUBSYSTEM_WINDOWS_GUI)).expect("write");
    assert_eq!(file_subsystem(&file), Some(IMAGE_SUBSYSTEM_WINDOWS_GUI));
    assert_eq!(file_subsystem(&dir.join("missing.exe")), None);
}
