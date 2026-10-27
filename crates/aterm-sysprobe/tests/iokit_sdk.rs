// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IOKit and CoreFoundation declaration checks against the macOS SDK headers.

#![cfg(target_os = "macos")]

#[path = "../../aterm-objc/tests/support/sdk_consts.rs"]
#[expect(
    dead_code,
    reason = "mounted for sdk_decls' probe; the constant-line parser serves only the constant censuses"
)]
mod sdk_consts;
#[path = "../../aterm-objc/tests/support/sdk_decls.rs"]
mod sdk_decls;

use sdk_decls::Floor;

const SUBJECT: &[&str] = &["iokit.rs"];

#[test]
fn the_parser_saw_every_declaration() {
    sdk_decls::the_parser_saw_every_declaration(
        SUBJECT,
        &Floor {
            fns: 26,
            structs: 1,
            opaque: 6,
        },
    );
}

#[test]
fn every_iokit_declaration_matches_the_sdk_on_both_arches() {
    sdk_decls::every_declaration_matches_the_sdk_on_both_arches(SUBJECT);
}

#[test]
fn a_wrong_prototype_fails_the_probe_on_both_arches() {
    sdk_decls::a_wrong_prototype_fails_the_probe_on_both_arches(
        SUBJECT,
        "IORegistryEntryCreateCFProperty",
    );
}
