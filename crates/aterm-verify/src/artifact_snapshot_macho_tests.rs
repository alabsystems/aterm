// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Write as _;

fn image(commands: &[Vec<u8>]) -> Vec<u8> {
    let mut header = vec![0; 32];
    header[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
    header[4..8].copy_from_slice(&0x0100_000c_u32.to_le_bytes());
    header[12..16].copy_from_slice(&2_u32.to_le_bytes());
    header[16..20].copy_from_slice(&(commands.len() as u32).to_le_bytes());
    header[20..24]
        .copy_from_slice(&(commands.iter().map(Vec::len).sum::<usize>() as u32).to_le_bytes());
    for command in commands {
        header.extend(command);
    }
    header
}

fn load(cmd: u32, text: &[u8]) -> Vec<u8> {
    let minimum = if cmd == 0xe || cmd == 0x27 || cmd == 0x8000_001c {
        12
    } else {
        24
    };
    let mut bytes = vec![0; (minimum + text.len() + 1).div_ceil(8) * 8];
    let len = bytes.len() as u32;
    bytes[..4].copy_from_slice(&cmd.to_le_bytes());
    bytes[4..8].copy_from_slice(&len.to_le_bytes());
    bytes[8..12].copy_from_slice(&(minimum as u32).to_le_bytes());
    bytes[minimum..minimum + text.len()].copy_from_slice(text);
    bytes
}

fn admits(bytes: &[u8]) -> bool {
    let mut file = aterm_tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    relocatable(&file).is_ok()
}

#[test]
fn appkit_image_relocation_rejects_every_location_sensitive_loader_variant() {
    for cmd in [0xc, 0x8000_0018, 0x8000_001f, 0x20, 0x8000_0023] {
        assert!(admits(&image(&[load(cmd, b"/usr/lib/libSystem.B.dylib")])));
        for name in [
            b"@rpath/a.dylib".as_slice(),
            b"@loader_path/a",
            b"@executable_path/a",
            b"relative",
            b"/tmp/a",
            b"/usr/lib/../../tmp/a",
        ] {
            assert!(!admits(&image(&[load(cmd, name)])), "{cmd:x} {name:?}");
        }
    }
    assert!(admits(&image(&[
        load(0xe, b"/usr/lib/dyld"),
        load(0xc, b"/System/Library/Frameworks/AppKit.framework/AppKit")
    ])));
    for command in [
        load(0xe, b"/tmp/dyld"),
        load(0x27, b"DYLD_LIBRARY_PATH=/tmp"),
        load(0x8000_001c, b"/usr/lib"),
    ] {
        assert!(!admits(&image(&[command])));
    }
    let mut bad = load(0xc, b"/usr/lib/libSystem.B.dylib");
    bad[8..12].copy_from_slice(&8_u32.to_le_bytes());
    assert!(!admits(&image(&[bad])));
    let mut bad = load(0xc, b"/usr/lib/a");
    bad[24..].fill(b'x');
    assert!(!admits(&image(&[bad])));
}

#[test]
fn appkit_image_relocation_checks_all_universal_slices_and_their_bounds() {
    let good = image(&[load(0xc, b"/usr/lib/libSystem.B.dylib")]);
    let bad = image(&[load(0xc, b"@loader_path/foreign")]);
    let fat = |second: &[u8]| {
        let mut second = second.to_vec();
        second[4..8].copy_from_slice(&0x0100_0007_u32.to_le_bytes());
        let mut bytes = vec![0; 48];
        bytes[..4].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe]);
        bytes[4..8].copy_from_slice(&2_u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&0x0100_000c_u32.to_be_bytes());
        bytes[16..20].copy_from_slice(&48_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&(good.len() as u32).to_be_bytes());
        bytes[28..32].copy_from_slice(&0x0100_0007_u32.to_be_bytes());
        bytes[36..40].copy_from_slice(&(48_u32 + good.len() as u32).to_be_bytes());
        bytes[40..44].copy_from_slice(&(second.len() as u32).to_be_bytes());
        bytes.extend(&good);
        bytes.extend(&second);
        bytes
    };
    assert!(admits(&fat(&good)));
    assert!(
        !admits(&fat(&bad)),
        "a native safe slice cannot hide a foreign unsafe slice"
    );
    let mut bytes = fat(&good);
    bytes[36..40].copy_from_slice(&48_u32.to_be_bytes());
    assert!(
        !admits(&bytes),
        "overlapping slices are not independently bounded"
    );
    let bytes = fat(&good);
    assert!(!admits(&bytes[..bytes.len() - 1]));
}
