// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026; see the repository NOTICE.

//! Nonblocking clipboard pipe transfers and their completion policy.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// The hang detector for a required clipboard transfer.
pub(super) const TRANSFER_HANG: Duration = Duration::from_secs(60);

/// Bound memory even when a foreign source never finishes its text.
const MAX_PASTE_BYTES: usize = 16 << 20;

/// Read a nonblocking pipe to EOF, or to the explicit text size limit.
pub(super) fn read(pipe: impl Read) -> Option<String> {
    read_after(pipe, Duration::ZERO)
}

fn read_after(pipe: impl Read, elapsed: Duration) -> Option<String> {
    let started = Instant::now();
    read_with_elapsed(pipe, || elapsed.saturating_add(started.elapsed()))
}

fn read_with_elapsed(mut pipe: impl Read, mut elapsed: impl FnMut() -> Duration) -> Option<String> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        if elapsed() >= TRANSFER_HANG {
            return None;
        }
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                if bytes.len() >= MAX_PASTE_BYTES {
                    bytes.truncate(MAX_PASTE_BYTES);
                    truncated = true;
                    break;
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
            Err(_) => return None,
        }
    }
    decode_paste_bytes(bytes, truncated)
}

/// Send the entire selection on a nonblocking pipe.
///
/// # Errors
/// A hung or broken transfer, a zero-byte write, or a flush failure.
pub(super) fn write(pipe: impl Write, bytes: &[u8]) -> std::io::Result<()> {
    write_after(pipe, bytes, Duration::ZERO)
}

fn write_after(mut pipe: impl Write, bytes: &[u8], elapsed: Duration) -> std::io::Result<()> {
    let started = Instant::now();
    let mut at = 0;
    while at < bytes.len() {
        if elapsed.saturating_add(started.elapsed()) >= TRANSFER_HANG {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        match pipe.write(&bytes[at..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => at += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
            Err(error) => return Err(error),
        }
    }
    pipe.flush()
}

fn decode_paste_bytes(bytes: Vec<u8>, truncated: bool) -> Option<String> {
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid_len = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid_len);
            String::from_utf8(bytes).ok()?
        },
        Err(_) => return None,
    };
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
#[path = "clipboard_transfer_tests.rs"]
mod tests;
