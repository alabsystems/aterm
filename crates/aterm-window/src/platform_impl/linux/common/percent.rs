// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Percent-decoding for `text/uri-list` drop payloads on Linux.

use std::str::Utf8Error;

/// Frames a `text/uri-list` into non-empty, non-comment lines before percent-decoding.
pub(crate) fn uri_list_entries(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.split(|&byte| byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|entry| !entry.is_empty() && !entry.starts_with(b"#"))
}

/// Percent-decodes `%XX` escapes (leaving incomplete or non-hex `%` verbatim) and validates UTF-8.
pub(crate) fn percent_decode_utf8(data: &[u8]) -> Result<String, Utf8Error> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'%' && i + 2 < data.len() {
            if let (Some(hi), Some(lo)) = (hex_val(data[i + 1]), hex_val(data[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(data[i]);
        i += 1;
    }
    String::from_utf8(out).map_err(|e| e.utf8_error())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "percent_tests.rs"]
mod tests;
