// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm_objc::cf` held to the SDK headers, on both arches, by the shared
//! declaration probe (`tests/support/sdk_decls.rs`; `aterm-cg` runs the same
//! probe over CoreGraphics).

#![cfg(target_os = "macos")]

use crate::sdk_decls::{self, Floor};

const SUBJECT: &[&str] = &["cf.rs"];

#[test]
fn the_parser_saw_every_declaration() {
    sdk_decls::the_parser_saw_every_declaration(
        SUBJECT,
        &Floor {
            fns: 17,
            structs: 4,
            opaque: 9,
        },
    );
}

#[test]
fn every_cf_declaration_matches_the_sdk_on_both_arches() {
    sdk_decls::every_declaration_matches_the_sdk_on_both_arches(SUBJECT);
}

#[test]
fn a_wrong_prototype_fails_the_probe_on_both_arches() {
    sdk_decls::a_wrong_prototype_fails_the_probe_on_both_arches(SUBJECT, "CFArrayGetCount");
}
