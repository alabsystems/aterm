// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual late-bound table's prototypes, compared to dns_sd.h rather than copied assertions.

#![cfg(target_os = "macos")]

#[path = "../../aterm-objc/tests/support/sdk_consts.rs"]
#[expect(dead_code, reason = "shared SDK declaration probe support")]
mod sdk_consts;
#[path = "../../aterm-objc/tests/support/sdk_decls.rs"]
#[expect(
    dead_code,
    reason = "the table is checked against the SDK, with a negative control"
)]
mod sdk_decls;

const SUBJECT: &[&str] = &["bonjour/api.rs"];

#[test]
fn the_actual_dns_sd_table_matches_the_sdk_on_both_arches() {
    sdk_decls::every_declaration_matches_the_sdk_on_both_arches(SUBJECT);
}

#[test]
fn a_wrong_dns_sd_table_prototype_fails_on_both_arches() {
    sdk_decls::a_wrong_prototype_fails_the_probe_on_both_arches(SUBJECT, "DNSServiceBrowse");
}
