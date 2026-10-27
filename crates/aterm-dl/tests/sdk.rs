// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native loader declarations and flags must match both macOS SDK architectures.

#![cfg(target_os = "macos")]

#[path = "../../aterm-objc/tests/support/sdk_consts.rs"]
#[expect(dead_code, reason = "shared SDK declaration probe support")]
mod sdk_consts;
#[path = "../../aterm-objc/tests/support/sdk_decls.rs"]
mod sdk_decls;

const SUBJECT: &[&str] = &["darwin.rs"];

#[test]
fn the_parser_saw_every_declaration() {
    sdk_decls::the_parser_saw_every_declaration(
        SUBJECT,
        &sdk_decls::Floor {
            fns: 3,
            structs: 0,
            opaque: 0,
        },
    );
}

#[test]
fn every_loader_declaration_matches_the_sdk_on_both_arches() {
    sdk_decls::every_declaration_matches_the_sdk_on_both_arches(SUBJECT);
}

#[test]
fn a_wrong_loader_prototype_fails_the_probe_on_both_arches() {
    sdk_decls::a_wrong_prototype_fails_the_probe_on_both_arches(SUBJECT, "dlopen");
}
