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
mod tests {
    use super::*;

    fn bundle(bodies: &[&str]) -> String {
        bodies
            .iter()
            .map(|b| format!("-----BEGIN CERTIFICATE-----\n{b}\n-----END CERTIFICATE-----\n"))
            .collect()
    }

    #[test]
    fn the_lossy_parser_skips_an_aux_info_block() {
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
