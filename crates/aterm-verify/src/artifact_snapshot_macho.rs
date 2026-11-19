// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private image may move only when every slice has location-independent system imports.

use std::fs::File;
use std::os::unix::fs::FileExt as _;

// Match the existing compile-facts Mach-O reader's format and table limits.
const TABLE_LIMIT: u64 = 1 << 20;

fn read(file: &File, at: u64, n: usize, end: u64) -> Result<Vec<u8>, String> {
    if at.checked_add(n as u64).is_none_or(|v| v > end) {
        return Err("Mach-O range leaves its image or slice".into());
    }
    let mut bytes = vec![0; n];
    file.read_exact_at(&mut bytes, at)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn word(bytes: &[u8], offset: usize, little: bool) -> Result<u32, String> {
    let bytes: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or("short Mach-O word")?
        .try_into()
        .map_err(|_| "Mach-O word")?;
    Ok(if little {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
}

fn wide(bytes: &[u8], offset: usize, little: bool) -> Result<u64, String> {
    let bytes: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or("short Mach-O wide word")?
        .try_into()
        .map_err(|_| "Mach-O wide word")?;
    Ok(if little {
        u64::from_le_bytes(bytes)
    } else {
        u64::from_be_bytes(bytes)
    })
}

fn name(command: &[u8], little: bool, minimum: usize) -> Result<&str, String> {
    let start = usize::try_from(word(command, 8, little)?).map_err(|_| "Mach-O name offset")?;
    if start < minimum {
        return Err("Mach-O name overlaps its command".into());
    }
    let tail = command.get(start..).ok_or("Mach-O name outside command")?;
    let nul = tail
        .iter()
        .position(|b| *b == 0)
        .ok_or("unterminated Mach-O name")?;
    std::str::from_utf8(&tail[..nul]).map_err(|_| "non-UTF8 Mach-O name".into())
}

fn system(name: &str) -> bool {
    (name.starts_with("/System/Library/") || name.starts_with("/usr/lib/"))
        && !name.split('/').any(|c| c == "." || c == "..")
        && !name.chars().any(char::is_control)
}

fn slice(file: &File, at: u64, end: u64, expected: Option<(u32, u32)>) -> Result<(), String> {
    let header = read(file, at, 32, end)?;
    let little = true;
    let cpu = word(&header, 4, little)?;
    let subtype = word(&header, 8, little)?;
    if header[..4] != [0xcf, 0xfa, 0xed, 0xfe]
        || !matches!(cpu, 0x0100_000c | 0x0100_0007)
        || expected.is_some_and(|identity| identity != (cpu, subtype))
    {
        return Err("AppKit image is not a matching supported 64-bit Mach-O slice".into());
    }
    if word(&header, 12, little)? != 2 {
        return Err("AppKit Mach-O is not MH_EXECUTE".into());
    }
    let count = word(&header, 16, little)?;
    let size = u64::from(word(&header, 20, little)?);
    if size > TABLE_LIMIT || u64::from(count) > size / 8 {
        return Err("Mach-O load table exceeds its bound".into());
    }
    let commands = read(file, at + 32, size as usize, end)?;
    let mut offset = 0_usize;
    for _ in 0..count {
        let remaining = commands
            .get(offset..)
            .ok_or("Mach-O command outside table")?;
        let cmd = word(remaining, 0, little)?;
        let size = word(remaining, 4, little)? as usize;
        if size < 8 || size % 4 != 0 {
            return Err("invalid Mach-O load command size".into());
        }
        let command = remaining.get(..size).ok_or("short Mach-O load command")?;
        match cmd {
            // All dylib-loading variants, including the obsolete lazy variant.
            0xc | 0x8000_0018 | 0x8000_001f | 0x20 | 0x8000_0023 => {
                if command.len() < 24 || !system(name(command, little, 24)?) {
                    return Err("AppKit image has a non-system or relative dynamic import".into());
                }
            }
            0xe => {
                if name(command, little, 12)? != "/usr/lib/dyld" {
                    return Err("AppKit image has a non-system dynamic loader".into());
                }
            }
            // Search paths and embedded environments change relocation semantics. Older
            // fixed-VM/prebound/subframework commands have no admitted relocation contract.
            0x8000_001c | 0x27 | 0x6 | 0x7 | 0xd | 0xf | 0x10 | 0x12..=0x15 => {
                return Err("AppKit image has unbound loader/search-path metadata".into());
            }
            // Commands with no loader/search-path names. Unknown future commands have no
            // relocation contract here and must be reviewed before admission.
            0x1..=0x5
            | 0x8
            | 0xa
            | 0xb
            | 0x11
            | 0x16
            | 0x17
            | 0x19..=0x1b
            | 0x1d
            | 0x1e
            | 0x21
            | 0x22
            | 0x8000_0022
            | 0x24..=0x26
            | 0x8000_0028
            | 0x29..=0x2e
            | 0x2f..=0x32
            | 0x8000_0033
            | 0x8000_0034 => {}
            _ => return Err("AppKit image has an unknown relocation command".into()),
        }
        offset = offset.checked_add(size).ok_or("Mach-O command overflow")?;
    }
    if offset != commands.len() {
        return Err("unclaimed Mach-O load table bytes".into());
    }
    Ok(())
}

pub(super) fn relocatable(file: &File) -> Result<(), String> {
    let end = file.metadata().map_err(|e| e.to_string())?.len();
    let header = read(file, 0, 8, end)?;
    let wide_arch = match header[..4] {
        [0xca, 0xfe, 0xba, 0xbe] => false,
        [0xca, 0xfe, 0xba, 0xbf] => true,
        _ => return slice(file, 0, end, None),
    };
    let little = false;
    let count = word(&header, 4, little)? as usize;
    if count == 0 || count > 16 {
        return Err("invalid universal Mach-O slice count".into());
    }
    let row = if wide_arch { 32 } else { 20 };
    let table_end = 8 + (count * row) as u64;
    let arches = read(file, 8, count * row, end)?;
    let mut ranges = Vec::with_capacity(count);
    for arch in arches.chunks_exact(row) {
        let cpu = word(arch, 0, little)?;
        let subtype = word(arch, 4, little)?;
        let (offset, size) = if wide_arch {
            (wide(arch, 8, little)?, wide(arch, 16, little)?)
        } else {
            (
                u64::from(word(arch, 8, little)?),
                u64::from(word(arch, 12, little)?),
            )
        };
        let finish = offset
            .checked_add(size)
            .ok_or("universal Mach-O range overflow")?;
        let align = word(arch, if wide_arch { 24 } else { 16 }, little)?;
        if offset < table_end
            || size < 32
            || finish > end
            || align > 63
            || offset % (1_u64 << align.min(63)) != 0
            || wide_arch && word(arch, 28, little)? != 0
            || ranges
                .iter()
                .any(|&(other, a, b)| other == cpu || offset < b && a < finish)
        {
            return Err("universal Mach-O slices overlap or leave the image".into());
        }
        slice(file, offset, finish, Some((cpu, subtype)))?;
        ranges.push((cpu, offset, finish));
    }
    Ok(())
}

#[cfg(test)]
#[path = "artifact_snapshot_macho_tests.rs"]
mod tests;
