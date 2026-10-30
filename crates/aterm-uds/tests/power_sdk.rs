// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `src/power/iokit.rs` held to the macOS SDK: its declarations by the shared declaration
//! probe (`crates/aterm-objc/tests/support/sdk_decls.rs`), its message constants by a probe of
//! `IOKit/IOMessage.h`, which the shared one does not include.

#![cfg(target_os = "macos")]

#[path = "../../aterm-objc/tests/support/sdk_consts.rs"]
mod sdk_consts;
#[path = "../../aterm-objc/tests/support/sdk_decls.rs"]
mod sdk_decls;

use sdk_decls::Floor;

const SUBJECT: &[&str] = &["power/iokit.rs"];

#[test]
fn the_parser_saw_every_declaration() {
    sdk_decls::the_parser_saw_every_declaration(
        SUBJECT,
        &Floor {
            fns: 10,
            structs: 0,
            opaque: 4,
        },
    );
}

#[test]
fn every_power_declaration_matches_the_sdk_on_both_arches() {
    sdk_decls::every_declaration_matches_the_sdk_on_both_arches(SUBJECT);
}

#[test]
fn a_wrong_prototype_fails_the_probe_on_both_arches() {
    sdk_decls::a_wrong_prototype_fails_the_probe_on_both_arches(SUBJECT, "IOAllowPowerChange");
}

/// The message constants `src/power/iokit.rs` declares: `(name, value)`.
fn messages() -> Vec<(String, String)> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/power/iokit.rs");
    let src = std::fs::read_to_string(path).expect("the module is readable");
    src.lines()
        .filter_map(sdk_consts::parse_const_decl)
        .filter(|(name, _)| name.starts_with("kIOMessage"))
        .collect()
}

/// The probe of [`messages`], with the row named `off` (if any) asserted one higher.
fn message_probe(off: Option<&str>) -> String {
    let mut s = String::from("#include <IOKit/IOMessage.h>\n");
    for (name, value) in messages() {
        let bump = if off == Some(name.as_str()) {
            " + 1"
        } else {
            ""
        };
        let lit = sdk_consts::as_c_literal(&value);
        s.push_str(&format!(
            "_Static_assert({name} == ({lit}){bump}, \"{name}\");\n"
        ));
    }
    s.push_str("int main(void) { return 0; }\n");
    s
}

fn compile(arch: &str, body: &str, tag: &str) -> Result<(), String> {
    sdk_consts::compile(
        concat!(env!("CARGO_PKG_NAME"), "-power-messages"),
        arch,
        body,
        tag,
    )
}

#[test]
fn every_power_message_matches_the_sdk_on_both_arches() {
    assert_eq!(
        messages().len(),
        4,
        "the four messages the listener answers"
    );
    let body = message_probe(None);
    for arch in ["arm64", "x86_64"] {
        sdk_consts::green_verdict(
            arch,
            "a power message disagrees with IOKit/IOMessage.h",
            compile(arch, &body, "green"),
        );
    }
}

#[test]
fn a_wrong_message_fails_the_probe_on_both_arches() {
    let row = "kIOMessageSystemWillSleep";
    assert!(
        messages().iter().any(|(name, _)| name == row),
        "{row} is not declared any more; pick another row"
    );
    let body = message_probe(Some(row));
    for arch in ["arm64", "x86_64"] {
        sdk_consts::inverted_verdict(
            arch,
            row,
            compile(arch, &body, "inverted"),
            &format!("{arch}: the probe accepted `{row}` one off, so the green run checks nothing"),
        );
    }
}
