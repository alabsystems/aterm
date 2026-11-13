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
#[path = "component_image_tests.rs"]
mod tests;
