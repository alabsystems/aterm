// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lossy PEM certificate decoding for the system trust store on Linux and the BSDs.

const BEGIN: &str = "-----BEGIN ";
const END: &str = "-----END ";
const SUFFIX: &str = "-----";
const CERTIFICATE: &str = "CERTIFICATE";

/// Decode every `CERTIFICATE` block in `text`, skipping non-`CERTIFICATE` labels
/// (such as OpenSSL `TRUSTED CERTIFICATE` aux-info blocks) and malformed blocks.
#[must_use]
pub fn decode_certificates_lossy(text: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(begin) = rest.find(BEGIN) {
        let after_begin = &rest[begin + BEGIN.len()..];
        let Some(label_end) = after_begin.find(SUFFIX) else {
            break;
        };
        let label = &after_begin[..label_end];
        let body = &after_begin[label_end + SUFFIX.len()..];
        let end_marker = format!("{END}{label}{SUFFIX}");
        let Some(body_end) = body.find(&end_marker) else {
            break;
        };
        if label == CERTIFICATE {
            let base64: String = body[..body_end]
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if let Ok(der) = aterm_codec::base64::decode(&base64)
                && !der.is_empty()
            {
                out.push(der);
            }
        }
        rest = &body[body_end + end_marker.len()..];
    }
    out
}

#[cfg(test)]
#[path = "pem_tests.rs"]
mod tests;
