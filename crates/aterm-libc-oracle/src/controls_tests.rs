// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::cargo::{Diagnostic, FailedTest};

fn error(code: &str, message: &str) -> Diagnostic {
    Diagnostic {
        code: Some(code.into()),
        message: message.into(),
        labels: Vec::new(),
        snippets: Vec::new(),
        spans: Vec::new(),
        rendered: format!("error[{code}]: {message}"),
    }
}

fn failed(outcome_errors: Vec<Diagnostic>, tests: &[&str]) -> Outcome {
    Outcome {
        success: false,
        code: Some(101),
        errors: outcome_errors,
        failed: tests
            .iter()
            .map(|t| FailedTest {
                name: format!("cell_mac::runtime::{t}"),
                stdout: String::new(),
            })
            .collect(),
        ..Outcome::default()
    }
}

#[test]
fn a_control_needs_its_own_evidence_not_just_a_red_build() {
    let expect = Expect::Error {
        code: "E0624",
        names: "si_status",
    };
    let right = failed(vec![error("E0624", "method `si_status` is private")], &[]);
    assert_eq!(expect.judge(&right), Ok(()));
    let unrelated = failed(vec![error("E0433", "failed to resolve: `libc_shim`")], &[]);
    assert!(matches!(
        expect.judge(&unrelated),
        Err(ControlFailure::Unattributed { .. })
    ));
    let green = Outcome {
        success: true,
        ..Outcome::default()
    };
    assert_eq!(expect.judge(&green), Err(ControlFailure::Escaped));
}

#[test]
fn a_test_control_names_its_test_and_a_green_control_must_pass() {
    let fd = Expect::TestFails("behaviour_fd_set");
    assert_eq!(
        fd.judge(&failed(vec![], &["behaviour_fd_isset", "behaviour_fd_set"])),
        Ok(())
    );
    assert!(fd.judge(&failed(vec![], &["behaviour_fd_isset"])).is_err());
    let green = Expect::Green;
    assert!(matches!(
        green.judge(&failed(vec![error("E0425", "c_int")], &[])),
        Err(ControlFailure::Spurious { .. })
    ));
}

#[test]
fn an_anchor_must_occur_exactly_as_often_as_the_control_says() {
    let text = "a\nlet fd = fd as usize;\nb\nlet fd = fd as usize;\n";
    let twice = Edit::Replace {
        anchor: "let fd = fd as usize;".into(),
        with: "x".into(),
        count: 2,
    };
    assert_eq!(apply("f.rs", text, &[twice]).as_deref(), Ok("a\nx\nb\nx\n"));
    assert_eq!(
        apply("f.rs", text, &[Edit::once("let fd = fd as usize;", "x")]),
        Err(ControlFailure::Anchor {
            file: "f.rs".into(),
            anchor: "let fd = fd as usize;".into(),
            found: 2,
            expected: 1,
        })
    );
    assert_eq!(
        apply("f.rs", "x", &[Edit::Append("\ny")]).as_deref(),
        Ok("x\ny")
    );
}

#[test]
fn an_si_pid_header_is_matched_by_its_safety_field() {
    let header = |side, unsafe_| ShapeDiff {
        side,
        record: ShapeRecord::InherentHeader {
            owner: "siginfo_t".into(),
            name: "si_pid".into(),
            header: format!(
                r#"{{"abi":"Rust","is_async":false,"is_const":false,"is_unsafe":{unsafe_}}}"#
            ),
        },
    };
    let diffs = [header(Side::Missing, true), header(Side::Unexpected, false)];
    assert!(si_pid_header(&diffs, Side::Missing, true));
    assert!(si_pid_header(&diffs, Side::Unexpected, false));
    assert!(!si_pid_header(&diffs, Side::Missing, false));
    assert_eq!(
        diff_verdict::<ShapeDiff>(Ok(&[]), "x", |_| false),
        Err(ControlFailure::Escaped)
    );
}

#[test]
fn a_symbol_control_needs_its_own_mismatch() {
    let planted = SymbolDiff::Mismatch {
        name: "getpid".into(),
        shim: PLANTED_SYMBOL.into(),
        reference: "getpid".into(),
    };
    let other = SymbolDiff::AbsentFromReference {
        name: "made_up".into(),
    };
    assert_eq!(
        diff_verdict(Ok(&[other.clone(), planted]), "x", planted_link_name_caught),
        Ok(())
    );
    assert!(matches!(
        diff_verdict(Ok(&[other]), "x", planted_link_name_caught),
        Err(ControlFailure::Unattributed { .. })
    ));
    let anchor = ControlFailure::Anchor {
        file: "darwin_aarch64.rs".into(),
        anchor: GETPID_DECL.into(),
        found: 0,
        expected: 1,
    };
    assert_eq!(
        diff_verdict::<SymbolDiff>(Err(&anchor), "x", planted_link_name_caught),
        Err(anchor)
    );
}

fn cell_source(body: &str) -> String {
    format!("// generated\n{}\n{body}\n", siblings::BODY_MARKER)
}

#[test]
fn a_sibling_control_proves_nothing_on_a_family_that_already_differs() {
    let body = format!("fn fd_set() {{ {FD_SET_GOOD} }}");
    let mut sources: BTreeMap<&'static str, String> = cells::CELLS
        .iter()
        .map(|cell| (cell.source, cell_source(&body)))
        .collect();
    let mut report = ControlsReport::default();
    sibling_controls(&sources, &mut report);
    assert_eq!(
        report.caught,
        [
            ControlId::SiblingBody { os: Os::Darwin },
            ControlId::SiblingBody { os: Os::GnuLinux }
        ]
    );

    sources.insert(
        "darwin_x86_64.rs",
        cell_source(&format!("{body}\nfn x86_only() {{}}")),
    );
    let mut report = ControlsReport::default();
    sibling_controls(&sources, &mut report);
    assert_eq!(report.caught, [ControlId::SiblingBody { os: Os::GnuLinux }]);
    assert!(matches!(
        report.failed.as_slice(),
        [(
            ControlId::SiblingBody { os: Os::Darwin },
            ControlFailure::Baseline { .. }
        )]
    ));
}
