// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PEM certificate decoding for the system trust store on Linux and the BSDs
//! (`crate::verifier`'s Unix arm).
//!
//! That input is machine-generated: a real `/etc/ssl/certs` contains `TRUSTED
//! CERTIFICATE` blocks (OpenSSL's aux-info form, which is not a bare
//! `Certificate` and must NOT be decoded as one) and is not something an
//! operator wrote and can be told to fix. So [`decode_certificates_lossy`]
//! skips what it cannot take rather than failing the file; a root it drops can
//! only cause a connection to be REFUSED.

const BEGIN: &str = "-----BEGIN ";
const END: &str = "-----END ";
const SUFFIX: &str = "-----";
/// The one label this client will decode.
const CERTIFICATE: &str = "CERTIFICATE";

/// Decode every `CERTIFICATE` block in `text`, SKIPPING anything else.
///
/// For the system trust store on Linux and the BSDs (`crate::verifier`). It
/// never errors:
///
/// * any other label is skipped, including `TRUSTED CERTIFICATE` (OpenSSL's
///   aux-info wrapper, whose body is not a bare certificate);
/// * a block whose base64 will not decode, or decodes to nothing, is skipped;
/// * a block with no matching end line stops the scan.
///
/// Skipping is safe only here: a dropped root can only refuse a connection.
/// The caller treats an empty result as a failure.
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
        // Always advances past this block's end line, so the loop terminates.
        rest = &body[body_end + end_marker.len()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CERTIFICATE blocks around arbitrary bodies: this module decodes, it does
    /// not parse X.509.
    fn bundle(bodies: &[&str]) -> String {
        bodies
            .iter()
            .map(|b| format!("-----BEGIN CERTIFICATE-----\n{b}\n-----END CERTIFICATE-----\n"))
            .collect()
    }

    #[test]
    fn the_lossy_parser_skips_an_aux_info_block() {
        // A real /etc/ssl/certs bundle shape: a comment, an aux-info block, and
        // two genuine certificates, in file order.
        let text = format!(
            "# Issuer: something\n{}-----BEGIN TRUSTED CERTIFICATE-----\ncccc\n             -----END TRUSTED CERTIFICATE-----\n{}",
            bundle(&["aaaa"]),
            bundle(&["bbbb"])
        );
        let lossy = decode_certificates_lossy(&text);
        assert_eq!(lossy.len(), 2, "both CERTIFICATE blocks, and only those");
        assert_eq!(lossy[0], aterm_codec::base64::decode("aaaa").unwrap());
        assert_eq!(lossy[1], aterm_codec::base64::decode("bbbb").unwrap());
    }

    #[test]
    fn the_lossy_parser_drops_undecodable_blocks_and_keeps_going() {
        let text = format!(
            "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n{}",
            bundle(&["bbbb"])
        );
        assert_eq!(decode_certificates_lossy(&text).len(), 1);
    }

    #[test]
    fn the_lossy_parser_terminates_on_every_malformed_input() {
        assert!(decode_certificates_lossy("").is_empty());
        assert!(decode_certificates_lossy("-----BEGIN ").is_empty());
        assert!(decode_certificates_lossy("-----BEGIN CERTIFICATE-----\naaaa\n").is_empty());
        assert!(decode_certificates_lossy("-----BEGIN -----\n-----END -----\n").is_empty());
        assert!(
            decode_certificates_lossy("-----BEGIN CERTIFICATE-----\n\n-----END CERTIFICATE-----")
                .is_empty()
        );
    }
}
