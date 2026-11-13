// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{DYLIB_COMMAND, RPATH, RPATH_COMMAND};

/// `LC_LOAD_DYLIB`.
pub(crate) const LOAD_DYLIB: u32 = 0xC;
/// `LC_ID_DYLIB`.
pub(crate) const ID_DYLIB: u32 = super::DYLIB_ID;

fn words(ws: &[u32], little: bool) -> Vec<u8> {
    ws.iter()
        .flat_map(|w| {
            if little {
                w.to_le_bytes()
            } else {
                w.to_be_bytes()
            }
        })
        .collect()
}

/// One `dylib_command` naming `name`, padded to a multiple of 8 (a 64-bit file's).
pub(crate) fn dylib_command(cmd: u32, name: &str, little: bool) -> Vec<u8> {
    dylib_command_aligned(cmd, name, little, 8)
}

/// One `dylib_command` naming `name`, padded to a multiple of 4 only (a 32-bit
/// file's alignment).
pub(crate) fn dylib_command32(cmd: u32, name: &str, little: bool) -> Vec<u8> {
    dylib_command_aligned(cmd, name, little, 4)
}

fn dylib_command_aligned(cmd: u32, name: &str, little: bool, align: usize) -> Vec<u8> {
    let size = (DYLIB_COMMAND + name.len() + 1).div_ceil(align) * align;
    let fixed = [
        cmd,
        size as u32,
        DYLIB_COMMAND as u32,
        2,
        0x1_0000,
        0x1_0000,
    ];
    let mut c = words(&fixed, little);
    c.extend_from_slice(name.as_bytes());
    c.resize(size, 0);
    c
}

/// One `rpath_command` naming `path`, padded to a multiple of 8.
pub(crate) fn rpath_command(path: &str, little: bool) -> Vec<u8> {
    let size = (RPATH_COMMAND + path.len() + 1).div_ceil(8) * 8;
    let mut c = words(&[RPATH, size as u32, RPATH_COMMAND as u32], little);
    c.extend_from_slice(path.as_bytes());
    c.resize(size, 0);
    c
}

/// A `cmd`/`cmdsize` pair padded to `size` bytes: a load command the reader skips.
pub(crate) fn other_command(cmd: u32, size: u32, little: bool) -> Vec<u8> {
    let mut c = words(&[cmd, size], little);
    c.resize(size as usize, 0);
    c
}

/// A thin 64-bit arm64 executable whose load commands are `commands`, in that order.
pub(crate) fn thin(commands: &[Vec<u8>], little: bool) -> Vec<u8> {
    let body: Vec<u8> = commands.concat();
    let magic = if little { 0xCFFA_EDFEu32 } else { 0xFEED_FACF };
    let mut m = magic.to_be_bytes().to_vec();
    let header = [
        0x0100_000C,
        0,
        2,
        commands.len() as u32,
        body.len() as u32,
        0,
        0,
    ];
    m.extend(words(&header, little));
    m.extend_from_slice(&body);
    m.extend_from_slice(&[0u8; 64]);
    m
}

/// A thin 32-bit arm64_32 dylib (a watchOS slice's shape) whose load commands are
/// `commands`, in that order: a 28-byte header, no reserved word.
pub(crate) fn thin32(commands: &[Vec<u8>], little: bool) -> Vec<u8> {
    let body: Vec<u8> = commands.concat();
    let magic = if little { 0xCEFA_EDFEu32 } else { 0xFEED_FACE };
    let mut m = magic.to_be_bytes().to_vec();
    let header = [
        0x0200_000C,
        1,
        6,
        commands.len() as u32,
        body.len() as u32,
        0,
    ];
    m.extend(words(&header, little));
    m.extend_from_slice(&body);
    m.extend_from_slice(&[0u8; 64]);
    m
}

/// A static archive (`ar`'s magic, then one member header): what a universal
/// file's archive slice holds, object files that load no dylib.
pub(crate) fn archive() -> Vec<u8> {
    let mut a = super::ARCHIVE_MAGIC.to_vec();
    a.extend_from_slice(b"absvdi2.c.o/    0           0     0     644     8         `\n");
    a.extend_from_slice(&[0xCF, 0xFA, 0xED, 0xFE, 0, 0, 0, 0]);
    a
}

/// A universal file of `slices`, with a 32- or 64-bit arch table.
pub(crate) fn fat(slices: &[Vec<u8>], is_wide: bool) -> Vec<u8> {
    let entry = if is_wide { 32 } else { 20 };
    let mut offset = (8 + entry * slices.len()).div_ceil(16) * 16;
    let magic = if is_wide { 0xCAFE_BABFu32 } else { 0xCAFE_BABE };
    let mut f = magic.to_be_bytes().to_vec();
    f.extend_from_slice(&(slices.len() as u32).to_be_bytes());
    for s in slices {
        f.extend_from_slice(&0x0100_000Cu32.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());
        if is_wide {
            f.extend_from_slice(&(offset as u64).to_be_bytes());
            f.extend_from_slice(&(s.len() as u64).to_be_bytes());
            f.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0]);
        } else {
            f.extend_from_slice(&(offset as u32).to_be_bytes());
            f.extend_from_slice(&(s.len() as u32).to_be_bytes());
            f.extend_from_slice(&4u32.to_be_bytes());
        }
        offset += s.len().div_ceil(16) * 16;
    }
    for s in slices {
        f.resize(f.len().div_ceil(16) * 16, 0);
        f.extend_from_slice(s);
    }
    f
}
