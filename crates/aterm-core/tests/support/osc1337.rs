// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 1337 `File=` fixtures shared by the inline-image integration tests.

/// A payload the OSC 1337 handler classifies as PNG. The engine stores an
/// iTerm2 image undecoded (the renderer decodes it), so the bytes after the
/// magic are inert.
pub const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// `ESC ] 1337 ; File=<args>:<base64 payload> ST`.
pub fn osc_1337_file(args: &str, payload: &[u8]) -> Vec<u8> {
    let b64 = aterm_codec::base64::encode(payload).expect("encode");
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]1337;File=");
    out.extend_from_slice(args.as_bytes());
    out.push(b':');
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}
