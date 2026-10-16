// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Percent-decoding for the `text/uri-list` payloads both Linux backends receive on a drop.
//!
//! Added by the aterm project in 2026; see the repository NOTICE. It began as the Wayland
//! backend's inline helper (`wayland/seat/data_device.rs`) and moved here when the X11 backend
//! stopped using the `percent-encoding` crate for the same job (`x11/dnd.rs`), 2026-10-15.

use std::str::Utf8Error;

/// Frame a `text/uri-list` before decoding any URI. Raw line endings and
/// surrounding ASCII whitespace are list syntax; encoded whitespace and `#`
/// belong to the URI and must survive until its path is decoded.
pub(crate) fn uri_list_entries(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.split(|&byte| byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|entry| !entry.is_empty() && !entry.starts_with(b"#"))
}

/// Percent-decode bytes (`%XX` -> the byte) and interpret the result as UTF-8.
///
/// An incomplete or non-hex `%` escape is left verbatim, and bytes that do not decode to
/// UTF-8 are an error: the contract of `percent_encoding::percent_decode(..).decode_utf8()`,
/// which the X11 backend called here before.
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

/// One hex digit -> its 0..=15 value, or `None` if `b` is not `[0-9a-fA-F]`.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_basic_and_incomplete() {
        assert_eq!(percent_decode_utf8(b"a%20b").as_deref(), Ok("a b"));
        // A complete escape at the very end decodes.
        assert_eq!(percent_decode_utf8(b"x%2F").as_deref(), Ok("x/"));
        // Incomplete / non-hex escapes are left verbatim.
        assert_eq!(percent_decode_utf8(b"100%").as_deref(), Ok("100%"));
        assert_eq!(percent_decode_utf8(b"a%zzb").as_deref(), Ok("a%zzb"));
        // Bytes that decode to invalid UTF-8 are an error, escaped or not.
        assert!(percent_decode_utf8(b"%FF").is_err());
        assert!(percent_decode_utf8(b"\xFF").is_err());

        // Escaped syntax is filename content, not list framing or a comment.
        let entries: Vec<&[u8]> = uri_list_entries(
            b"#comment\r\nfile:///tmp/a%0D%0Afile:///tmp/b%20\r\nfile:///tmp/%23c\n",
        )
        .collect();
        assert_eq!(entries, [
            b"file:///tmp/a%0D%0Afile:///tmp/b%20".as_slice(),
            b"file:///tmp/%23c"
        ]);
        assert_eq!(
            percent_decode_utf8(entries[0]).as_deref(),
            Ok("file:///tmp/a\r\nfile:///tmp/b ")
        );
    }
}
