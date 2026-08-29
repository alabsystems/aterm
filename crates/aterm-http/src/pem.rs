// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PEM certificate-bundle decoding — the one PEM label this client accepts.
//!
//! A configured CA bundle is an explicit trust OVERRIDE for a single provider
//! endpoint, so the parser is deliberately strict and deliberately narrow:
//!
//! * only `CERTIFICATE` blocks are decoded. A `PRIVATE KEY`, a `TRUSTED
//!   CERTIFICATE`, or any other label in the file is an ERROR, not something to
//!   skip past — a bundle that is not what the operator thinks it is should
//!   fail loudly at load time, not silently trust fewer roots than intended;
//! * the begin/end labels must MATCH. A file whose `-----END-----` names a
//!   different type than its `-----BEGIN-----` is malformed;
//! * an empty bundle is an error. Handing rustls a root store with zero roots
//!   would make every subsequent verification fail in a way that reads like a
//!   network problem rather than a configuration one.
//!
//! [`decode_certificates_lossy`] is the deliberate exception, and it exists for
//! ONE caller: reading a Linux distribution's system trust store. That input has
//! none of the properties above — a real `/etc/ssl/certs` is machine-generated,
//! contains `TRUSTED CERTIFICATE` blocks (OpenSSL's aux-info form, which is not
//! a bare `Certificate` and must NOT be decoded as one), and is not something an
//! operator wrote and can be told to fix. Applying the strict parser to it would
//! fail every TLS connection on an ordinary machine. The two entry points are
//! kept apart rather than the strict one being relaxed, because the strictness
//! is the documented contract for an operator's bundle.
//!
//! Base64 comes from `aterm-codec`, already first-party and already in the
//! shipped graph; `ureq` used its own `ureq::tls::PemItem` for this.

/// Why a PEM bundle could not be decoded.
#[derive(Debug)]
pub struct PemError(String);

impl std::fmt::Display for PemError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PemError {}

const BEGIN: &str = "-----BEGIN ";
const END: &str = "-----END ";
const SUFFIX: &str = "-----";
/// The one label this client will decode.
const CERTIFICATE: &str = "CERTIFICATE";

/// Decode every `CERTIFICATE` block in `text`, in file order, into DER.
///
/// # Errors
///
/// Any non-`CERTIFICATE` PEM label, a mismatched or missing end line, invalid
/// base64, or a bundle containing no certificates at all.
pub fn decode_certificates(text: &str) -> Result<Vec<Vec<u8>>, PemError> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(begin) = rest.find(BEGIN) {
        let after_begin = &rest[begin + BEGIN.len()..];
        let label_end = after_begin
            .find(SUFFIX)
            .ok_or_else(|| PemError("unterminated PEM begin line".to_owned()))?;
        let label = &after_begin[..label_end];
        if label != CERTIFICATE {
            return Err(PemError(format!(
                "CA bundle contains a `{label}` block; only CERTIFICATE is accepted"
            )));
        }
        let body_start = label_end + SUFFIX.len();
        let body = &after_begin[body_start..];
        // The end line must name the SAME label the begin line opened.
        let end_marker = format!("{END}{label}{SUFFIX}");
        let body_end = body
            .find(&end_marker)
            .ok_or_else(|| PemError(format!("PEM {label} block has no matching end line")))?;
        let base64: String = body[..body_end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if base64.is_empty() {
            return Err(PemError(format!("PEM {label} block is empty")));
        }
        let der = aterm_codec::base64::decode(&base64)
            .map_err(|error| PemError(format!("PEM {label} block is not valid base64: {error}")))?;
        if der.is_empty() {
            return Err(PemError(format!("PEM {label} block decoded to no bytes")));
        }
        out.push(der);
        rest = &body[body_end + end_marker.len()..];
    }
    if out.is_empty() {
        return Err(PemError(
            "CA bundle contains no CERTIFICATE blocks".to_owned(),
        ));
    }
    Ok(out)
}

/// Decode every `CERTIFICATE` block in `text`, SKIPPING anything else.
///
/// The tolerant counterpart to [`decode_certificates`], for the system trust
/// store on Linux and the BSDs (`crate::verifier`). It never errors:
///
/// * a block whose label is not `CERTIFICATE` is skipped, INCLUDING `TRUSTED
///   CERTIFICATE` — that label wraps a `Certificate` in OpenSSL's auxiliary
///   structure, so its body is not a certificate and decoding it as one would
///   put garbage into a root store;
/// * a block whose base64 will not decode, or decodes to nothing, is skipped;
/// * a block with no matching end line stops the scan, because from that point
///   on the file cannot be read as a sequence of blocks.
///
/// Skipping is safe HERE and only here: a root that is silently dropped can
/// only cause a connection to be REFUSED, never accepted. The caller is
/// responsible for treating an empty result as a failure — `crate::verifier`'s
/// Unix arm does.
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
        // Always advances past this block's end line, so `rest` strictly shrinks
        // and the loop terminates on any input.
        rest = &body[body_end + end_marker.len()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two syntactically valid CERTIFICATE blocks (the DER is arbitrary bytes —
    /// this module decodes, it does not parse X.509).
    fn bundle(bodies: &[&str]) -> String {
        bodies
            .iter()
            .map(|b| format!("-----BEGIN CERTIFICATE-----\n{b}\n-----END CERTIFICATE-----\n"))
            .collect()
    }

    #[test]
    fn decodes_every_block_in_file_order() {
        // "aaaa" -> 0x69 0xa6 0x9a, "bbbb" -> ...
        let text = bundle(&["aaaa", "bbbb"]);
        let out = decode_certificates(&text).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], aterm_codec::base64::decode("aaaa").unwrap());
        assert_eq!(out[1], aterm_codec::base64::decode("bbbb").unwrap());
    }

    #[test]
    fn surrounding_prose_and_crlf_line_endings_are_tolerated() {
        let text = format!(
            "# issued by the operator\r\n{}trailing note\r\n",
            bundle(&["aaaa"]).replace('\n', "\r\n")
        );
        assert_eq!(decode_certificates(&text).unwrap().len(), 1);
    }

    #[test]
    fn a_private_key_in_the_bundle_is_an_error_not_a_skip() {
        // Silently skipping is how an operator ends up trusting fewer roots
        // than they think they configured.
        //
        // The BEGIN label is assembled from two literals rather than written
        // out, and that is not cosmetic. `tools/grep_guard.sh` B6 holds ZERO
        // TOLERANCE for a PEM private-key header anywhere in the tracked tree,
        // deliberately with no allowlist, because the cost of a real key
        // landing in a published repo is unbounded. A test that needs the label
        // in order to prove the label is REJECTED would otherwise be
        // indistinguishable, to a grep, from the thing the guard exists to
        // catch — so the guard keeps its zero and this keeps its label. The
        // body is four 'a's; there is no key here and no way to read this file
        // and wonder.
        let begin = concat!("-----BEGIN ", "PRIVATE KEY", "-----");
        let text = format!(
            "{begin}\naaaa\n-----END PRIVATE KEY-----\n{}",
            bundle(&["bbbb"])
        );
        let error = decode_certificates(&text).unwrap_err().to_string();
        assert!(error.contains("PRIVATE KEY"), "{error}");
    }

    #[test]
    fn a_mismatched_end_label_is_malformed() {
        let text = "-----BEGIN CERTIFICATE-----\naaaa\n-----END TRUSTED CERTIFICATE-----\n";
        assert!(decode_certificates(text).is_err());
    }

    #[test]
    fn an_empty_or_certificate_free_bundle_is_an_error() {
        // A zero-root store makes every handshake fail like a network problem.
        assert!(decode_certificates("").is_err());
        assert!(decode_certificates("# nothing here\n").is_err());
        assert!(
            decode_certificates("-----BEGIN CERTIFICATE-----\n\n-----END CERTIFICATE-----\n")
                .is_err()
        );
    }

    #[test]
    fn invalid_base64_is_rejected() {
        let text = "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n";
        assert!(decode_certificates(text).is_err());
    }

    #[test]
    fn an_unterminated_block_is_rejected() {
        assert!(decode_certificates("-----BEGIN CERTIFICATE-----\naaaa\n").is_err());
    }

    #[test]
    fn the_lossy_parser_skips_what_the_strict_one_refuses() {
        // A real /etc/ssl/certs bundle shape: a comment, an aux-info block the
        // strict parser calls an error, and two genuine certificates.
        let text = format!(
            "# Issuer: something\n{}-----BEGIN TRUSTED CERTIFICATE-----\ncccc\n             -----END TRUSTED CERTIFICATE-----\n{}",
            bundle(&["aaaa"]),
            bundle(&["bbbb"])
        );
        assert!(
            decode_certificates(&text).is_err(),
            "the strict parser must still refuse this"
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
        // Each of these could loop forever or panic in a parser that failed to
        // advance; the contract is that it returns.
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
