// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
