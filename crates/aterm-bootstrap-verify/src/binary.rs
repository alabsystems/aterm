// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The candidate: the file install.sh downloaded, held to the signed name's size, ELF
//! header and digest before anything runs it ([`verify_binary`]).

use std::ffi::OsStr;
use std::io::{Read as _, Seek as _, SeekFrom};

use crate::chain::open_file;
use crate::py::{PyPath, bare_os_error, os_error};
use crate::{Refusal, Verdict, require};

/// Hold the file at `path` (not a symbolic link) to the signed `size`, a 64-bit
/// little-endian ELF executable of `arch`'s machine, and the signed `digest`.
///
/// # Errors
/// The first of those it is not.
pub fn verify_binary(path: &OsStr, arch: &str, digest: &str, size: u64) -> Verdict<()> {
    let named = PyPath::new(path).to_path();
    let meta = std::fs::symlink_metadata(&named).map_err(|e| os_error(&e, &named))?;
    require(
        meta.file_type().is_file() && meta.len() == size,
        "Linux artifact size/type mismatch",
    )?;
    // Opened by the spelling it was given, as the embedded verifier opened it.
    let mut file = open_file(std::path::Path::new(path))?;
    let mut header = Vec::with_capacity(64);
    (&mut file)
        .take(64)
        .read_to_end(&mut header)
        .map_err(|e| bare_os_error(&e))?;
    require(
        header.len() == 64 && header[..7] == *b"\x7fELF\x02\x01\x01",
        "not a 64-bit little-endian ELF",
    )?;
    let half = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
    let word = |at: usize| {
        u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    require(matches!(half(16), 2 | 3), "ELF is not an executable")?;
    require(
        word(20) == 1 && half(52) == 64,
        "invalid ELF version/header size",
    )?;
    let machine = match arch {
        "x86_64" => 62,
        "aarch64" => 183,
        _ => return Err(Refusal::new("unsupported Linux architecture")),
    };
    require(
        half(18) == machine,
        "Linux artifact ELF architecture mismatch",
    )?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| bare_os_error(&e))?;
    let mut hasher = aterm_digest::Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buffer).map_err(|e| bare_os_error(&e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    require(
        aterm_codec::hex::encode_lower(&hasher.finalize()) == digest,
        "Linux artifact SHA-256 mismatch",
    )
}
