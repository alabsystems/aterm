// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native Targo image structure, independent of producer observations and paths.

use std::collections::BTreeSet;

const HEADER_SIZE: usize = 32;
const MAX_COMMANDS: usize = 4096;
const MAX_COMMAND_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Eq, PartialEq)]
pub(super) struct NativeImage {
    pub(super) system_libraries: Vec<String>,
    pub(super) code_signature_present: bool,
}

fn need(valid: bool, why: &str) -> Result<(), String> {
    if valid { Ok(()) } else { Err(why.to_owned()) }
}

fn word(raw: &[u8], at: usize) -> Result<usize, String> {
    let end = at.checked_add(4).ok_or("Mach-O word offset overflow")?;
    let bytes = raw.get(at..end).ok_or("truncated Mach-O word")?;
    let bytes: [u8; 4] = bytes.try_into().map_err(|_| "invalid Mach-O word")?;
    usize::try_from(u32::from_le_bytes(bytes))
        .map_err(|_| "Mach-O word exceeds the address space".to_owned())
}

fn load_string(raw: &[u8], offset: usize, next: usize, minimum: usize) -> Result<&str, String> {
    let length = next - offset;
    need(length >= minimum, "truncated Mach-O string command")?;
    let start = word(raw, offset + 8)?;
    need(
        start >= minimum && start < length,
        "invalid Mach-O string offset",
    )?;
    let data = raw
        .get(offset + start..next)
        .ok_or("invalid Mach-O string bounds")?;
    let zero = data
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("unterminated Mach-O string")?;
    std::str::from_utf8(&data[..zero]).map_err(|_| "Mach-O string is not UTF-8".to_owned())
}

fn system_library(name: &str) -> bool {
    (name.starts_with("/usr/lib/") || name.starts_with("/System/Library/Frameworks/"))
        && !name
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
        && !name
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
}

/// Inspect bytes only. Signature presence is structural; the separate actual
/// codesign phase establishes signature validity on these same artifact bytes.
pub(super) fn native_targo_image(raw: &[u8], host: &str) -> Result<NativeImage, String> {
    need(
        host == "aarch64-apple-darwin",
        "unsupported native Targo host",
    )?;
    need(
        raw.len() >= HEADER_SIZE && raw.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]),
        "native Targo is not thin little-endian Mach-O64",
    )?;
    need(
        word(raw, 4)? == 0x0100_000c,
        "native Targo CPU differs from its host",
    )?;
    need(
        word(raw, 12)? == 2,
        "native Targo is not a Mach-O executable",
    )?;
    let count = word(raw, 16)?;
    let size = word(raw, 20)?;
    need(
        (1..=MAX_COMMANDS).contains(&count)
            && (8..=MAX_COMMAND_BYTES).contains(&size)
            && size <= raw.len() - HEADER_SIZE,
        "invalid native Targo load-command bounds",
    )?;
    let end = HEADER_SIZE
        .checked_add(size)
        .ok_or("Mach-O command offset overflow")?;
    let mut offset = HEADER_SIZE;
    let mut libraries = BTreeSet::new();
    let mut signature = false;
    for _ in 0..count {
        need(
            offset <= end.saturating_sub(8),
            "truncated native Targo load command",
        )?;
        let command = word(raw, offset)? & 0x7fff_ffff;
        let length = word(raw, offset + 4)?;
        let next = offset
            .checked_add(length)
            .ok_or("Mach-O command size overflow")?;
        need(
            length >= 8 && length.is_multiple_of(8) && next <= end,
            "invalid native Targo load-command size",
        )?;
        match command {
            0x1c => return Err("native Targo embeds a loader rpath".to_owned()),
            0x27 => return Err("native Targo embeds loader environment".to_owned()),
            0xc | 0x18 | 0x1f | 0x20 | 0x23 => {
                let name = load_string(raw, offset, next, 24)?;
                need(
                    system_library(name),
                    "native Targo links a non-system library",
                )?;
                libraries.insert(name.to_owned());
            }
            0xe => {
                need(
                    load_string(raw, offset, next, 12)? == "/usr/lib/dyld",
                    "native Targo selects a non-system loader",
                )?;
            }
            0x1d => {
                need(
                    !signature && length == 16,
                    "invalid or duplicate native code signature command",
                )?;
                let start = word(raw, offset + 8)?;
                let bytes = word(raw, offset + 12)?;
                let signature_end = start
                    .checked_add(bytes)
                    .ok_or("code signature offset overflow")?;
                need(
                    bytes > 0 && start >= end && signature_end <= raw.len(),
                    "native Targo code signature lies outside its payload",
                )?;
                signature = true;
            }
            _ => {}
        }
        offset = next;
    }
    need(
        offset == end,
        "native Targo load-command count and size differ",
    )?;
    Ok(NativeImage {
        system_libraries: libraries.into_iter().collect(),
        code_signature_present: signature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "aarch64-apple-darwin";

    fn put(raw: &mut [u8], offset: usize, value: u32) {
        raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn string_command(kind: u32, minimum: usize, name: &[u8]) -> Vec<u8> {
        let length = (minimum + name.len() + 1 + 7) & !7;
        let mut raw = vec![0; length];
        put(&mut raw, 0, kind);
        put(&mut raw, 4, u32::try_from(length).unwrap());
        put(&mut raw, 8, u32::try_from(minimum).unwrap());
        raw[minimum..minimum + name.len()].copy_from_slice(name);
        raw
    }

    fn image(commands: &[Vec<u8>], payload: &[u8]) -> Vec<u8> {
        let size: usize = commands.iter().map(Vec::len).sum();
        let mut raw = vec![0; HEADER_SIZE];
        put(&mut raw, 0, 0xfeed_facf);
        put(&mut raw, 4, 0x0100_000c);
        put(&mut raw, 12, 2);
        put(&mut raw, 16, u32::try_from(commands.len()).unwrap());
        put(&mut raw, 20, u32::try_from(size).unwrap());
        for command in commands {
            raw.extend(command);
        }
        raw.extend(payload);
        raw
    }

    fn library(name: &[u8]) -> Vec<u8> {
        string_command(0xc, 24, name)
    }

    #[test]
    fn native_structure_sorts_and_deduplicates_system_dependencies() {
        let raw = image(
            &[
                library(b"/usr/lib/libz.1.dylib"),
                string_command(0xe, 12, b"/usr/lib/dyld"),
                library(b"/usr/lib/libSystem.B.dylib"),
                library(b"/usr/lib/libz.1.dylib"),
            ],
            &[],
        );
        assert_eq!(
            native_targo_image(&raw, HOST).unwrap(),
            NativeImage {
                system_libraries: vec![
                    "/usr/lib/libSystem.B.dylib".into(),
                    "/usr/lib/libz.1.dylib".into(),
                ],
                code_signature_present: false,
            }
        );
    }

    #[test]
    fn native_signature_presence_requires_a_bounded_separate_blob() {
        let dylib = library(b"/usr/lib/libSystem.B.dylib");
        let mut signature = vec![0; 16];
        put(&mut signature, 0, 0x1d);
        put(&mut signature, 4, 16);
        put(
            &mut signature,
            8,
            u32::try_from(HEADER_SIZE + dylib.len() + 16).unwrap(),
        );
        put(&mut signature, 12, 4);
        let raw = image(&[dylib, signature], &[1, 2, 3, 4]);
        assert!(
            native_targo_image(&raw, HOST)
                .unwrap()
                .code_signature_present
        );
    }

    #[test]
    fn every_truncated_prefix_refuses_without_an_image_observation() {
        let raw = image(&[library(b"/usr/lib/libSystem.B.dylib")], &[]);
        for length in 0..raw.len() {
            assert!(
                native_targo_image(&raw[..length], HOST).is_err(),
                "{length}"
            );
        }
    }

    #[test]
    fn foreign_format_cpu_file_type_and_host_refuse() {
        let raw = image(&[library(b"/usr/lib/libSystem.B.dylib")], &[]);
        for (offset, value) in [
            (0, 0xcafe_babe),
            (0, 0xcffa_edfe),
            (4, 0x0100_0007),
            (12, 6),
        ] {
            let mut bad = raw.clone();
            put(&mut bad, offset, value);
            assert!(native_targo_image(&bad, HOST).is_err());
        }
        assert!(native_targo_image(&raw, "x86_64-apple-darwin").is_err());
    }

    #[test]
    fn malformed_command_counts_sizes_and_strings_refuse() {
        let raw = image(&[library(b"/usr/lib/libSystem.B.dylib")], &[]);
        for (offset, value) in [
            (16, 0),
            (16, 2),
            (16, 4097),
            (20, u32::MAX),
            (HEADER_SIZE + 4, 0),
            (HEADER_SIZE + 4, 4),
            (HEADER_SIZE + 4, 12),
            (HEADER_SIZE + 4, u32::MAX),
            (HEADER_SIZE + 8, 0),
            (HEADER_SIZE + 8, u32::MAX),
        ] {
            let mut bad = raw.clone();
            put(&mut bad, offset, value);
            assert!(native_targo_image(&bad, HOST).is_err(), "{offset}:{value}");
        }
        let mut no_terminator = raw.clone();
        no_terminator[HEADER_SIZE + 24..].fill(b'x');
        assert!(native_targo_image(&no_terminator, HOST).is_err());
        let mut non_utf8 = raw;
        non_utf8[HEADER_SIZE + 24] = 0xff;
        assert!(native_targo_image(&non_utf8, HOST).is_err());
    }

    #[test]
    fn all_library_load_variants_refuse_non_system_and_noncanonical_paths() {
        for kind in [0xc, 0x8000_0018, 0x8000_001f, 0x20, 0x8000_0023] {
            for name in [
                "@rpath/libforeign.dylib",
                "relative.dylib",
                "/tmp/libforeign.dylib",
                "/usr/lib/../foreign.dylib",
                "/usr/lib//foreign.dylib",
                "/usr/lib/",
                "/usr/lib/control\n.dylib",
                "/System/Library/Frameworks/../foreign.dylib",
            ] {
                let raw = image(&[string_command(kind, 24, name.as_bytes())], &[]);
                assert!(native_targo_image(&raw, HOST).is_err(), "{kind}:{name}");
            }
        }
    }

    #[test]
    fn rpath_environment_and_foreign_loader_refuse() {
        for (kind, name) in [
            (0x8000_001c, "/usr/lib"),
            (0x27, "DYLD_INSERT_LIBRARIES=/usr/lib/libSystem.B.dylib"),
            (0xe, "/tmp/dyld"),
        ] {
            let raw = image(&[string_command(kind, 12, name.as_bytes())], &[]);
            assert!(native_targo_image(&raw, HOST).is_err());
        }
    }

    #[test]
    fn empty_overlapping_outside_and_duplicate_signature_blobs_refuse() {
        let dylib = library(b"/usr/lib/libSystem.B.dylib");
        let end = HEADER_SIZE + dylib.len() + 16;
        for (start, size) in [
            (0, 4),
            (u32::try_from(end).unwrap(), 0),
            (u32::MAX, 4),
            (u32::try_from(end).unwrap(), 5),
        ] {
            let mut signature = vec![0; 16];
            put(&mut signature, 0, 0x1d);
            put(&mut signature, 4, 16);
            put(&mut signature, 8, start);
            put(&mut signature, 12, size);
            let raw = image(&[dylib.clone(), signature], &[1, 2, 3, 4]);
            assert!(native_targo_image(&raw, HOST).is_err());
        }
        let mut signature = vec![0; 16];
        put(&mut signature, 0, 0x1d);
        put(&mut signature, 4, 16);
        put(&mut signature, 8, u32::try_from(end + 16).unwrap());
        put(&mut signature, 12, 4);
        let raw = image(&[dylib, signature.clone(), signature], &[1, 2, 3, 4]);
        assert!(native_targo_image(&raw, HOST).is_err());
    }
}
