// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn source(head: &str, body: &str) -> String {
    format!("{head}\n{BODY_MARKER} hand-written\n{body}\n")
}

fn all(darwin_x86: &str, linux_arm: &str) -> BTreeMap<&'static str, String> {
    [
        (
            "darwin_aarch64.rs",
            source("// arm", "fn a() {}\nfn b() {}"),
        ),
        ("darwin_x86_64.rs", darwin_x86.to_string()),
        ("linux_gnu_x86_64.rs", source("// x86", "fn l() {}")),
        ("linux_gnu_aarch64.rs", linux_arm.to_string()),
    ]
    .into()
}

#[test]
fn identical_sections_pass_whatever_precedes_them() {
    let sources = all(
        &source("// different generated head", "fn a() {}\nfn b() {}"),
        &source("// another head", "fn l() {}"),
    );
    assert_eq!(compare(&sources), []);
}

#[test]
fn an_architecture_only_body_edit_is_named_with_its_line() {
    let sources = all(
        &source("// x86", "fn a() {}\nfn b() { 1 }"),
        &source("// arm", "fn l() {}"),
    );
    assert_eq!(
        compare(&sources),
        [SiblingFinding::Differs {
            os: Os::Darwin,
            left: "darwin_aarch64.rs".into(),
            right: "darwin_x86_64.rs".into(),
            line: 3,
        }]
    );
}

#[test]
fn a_missing_or_doubled_marker_is_its_own_finding() {
    let doubled = format!("{BODY_MARKER}\n{BODY_MARKER}\n");
    let sources = all(&doubled, "no marker at all");
    let findings = compare(&sources);
    assert_eq!(
        findings,
        [
            SiblingFinding::Marker {
                file: "darwin_x86_64.rs".into(),
                count: 2
            },
            SiblingFinding::Marker {
                file: "linux_gnu_aarch64.rs".into(),
                count: 0
            },
        ]
    );
    let families: Vec<_> = findings.iter().map(SiblingFinding::os).collect();
    assert_eq!(families, [Some(Os::Darwin), Some(Os::GnuLinux)]);
}
