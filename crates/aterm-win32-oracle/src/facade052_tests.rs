// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::VecDeque;
#[cfg(unix)]
fn status(code: i32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code << 8)
}
#[cfg(windows)]
fn status(code: i32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code as u32)
}
fn answer(code: i32, stdout: &str) -> Result<Output> {
    Ok(Output {
        status: status(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: b"controlled tool refusal".to_vec(),
    })
}
struct Fake(VecDeque<Result<Output>>);
impl Runner for Fake {
    fn output(&mut self, _: &Path, _: &[String], _: &Path, _: Option<&Path>) -> Result<Output> {
        self.0
            .pop_front()
            .expect("every injected command is accounted for")
    }
}
fn tools() -> Tools {
    Tools {
        targo: "/controlled/targo".into(),
        trustfmt: "/controlled/trustfmt".into(),
        stock: Some(("/controlled/cargo".into(), "/controlled/rustc".into())),
        nm: "/controlled/nm".into(),
    }
}
fn messages() -> String {
    [("conformance", "/fixture/conformance.rlib"), ("facade", "/fixture/facade.rlib")]
        .map(|(name, file)| {
            format!(
                r#"{{"reason":"compiler-artifact","target":{{"name":"{name}"}},"filenames":["{file}"]}}"#
            )
        })
        .join("\n")
}

#[test]
fn only_the_conformance_units_error_is_a_rejection() {
    let error = |name: &str| {
        format!(
            r#"{{"reason":"compiler-message","target":{{"name":"{name}"}},"message":{{"level":"error","message":"controlled mismatch"}}}}"#
        )
    };
    for (label, result, expected) in [
        (
            "the plant is rejected",
            answer(101, &error("conformance")),
            Some(false),
        ),
        ("the facade conforms", answer(0, ""), Some(true)),
        (
            "a dependency failed",
            answer(101, &error("windows_sys")),
            None,
        ),
        ("the compiler died", answer(101, ""), None),
        (
            "cargo could not start",
            Err("controlled start failure".into()),
            None,
        ),
    ] {
        let mut runner = Fake(VecDeque::from([answer(0, ""), result]));
        let got = conforms(
            &mut runner,
            &tools(),
            Path::new("/fixture"),
            Path::new("/target"),
            TRIPLES[0],
            Some("plant-type"),
        );
        assert_eq!(got.ok(), expected, "{label}");
        assert!(runner.0.is_empty(), "{label}: the actual command path ran");
    }
}

#[test]
fn unavailable_evidence_never_satisfies_an_import_plant() {
    let refs = " U __imp_IsProcessorFeaturePresent\n";
    let defs = |dll: &str| format!("{dll}:\n0000 T __imp_IsProcessorFeaturePresent\n");
    type Scenario = (&'static str, Vec<Result<Output>>, Option<&'static str>);
    let scenarios: Vec<Scenario> = vec![
        (
            "inspected",
            vec![
                answer(0, ""),
                answer(0, &messages()),
                answer(0, refs),
                answer(0, &defs("kernelbase.dll")),
            ],
            Some("kernelbase.dll"),
        ),
        ("clean failure", vec![answer(101, "")], None),
        ("build failure", vec![answer(0, ""), answer(101, "")], None),
        ("no probe rlib", vec![answer(0, ""), answer(0, "{}")], None),
        (
            "nm failed",
            vec![answer(0, ""), answer(0, &messages()), answer(1, "")],
            None,
        ),
        (
            "no reference",
            vec![
                answer(0, ""),
                answer(0, &messages()),
                answer(0, ""),
                answer(0, &defs("kernel32.dll")),
            ],
            None,
        ),
        (
            "undefined import",
            vec![
                answer(0, ""),
                answer(0, &messages()),
                answer(0, refs),
                answer(0, "kernel32.dll:\n"),
            ],
            None,
        ),
    ];
    for (label, steps, dll) in scenarios {
        let mut fake = Fake(steps.into());
        let got = imports(
            &mut fake,
            &tools(),
            Path::new("/fixture"),
            Path::new("/target"),
            TRIPLES[1],
            "probe-facade,plant-dll",
            Side::Planted,
        );
        let want = dll.map(|d| {
            Imports::from([(
                "__imp_IsProcessorFeaturePresent".to_string(),
                BTreeSet::from([d.to_string()]),
            )])
        });
        assert_eq!(got.ok(), want, "{label}");
        assert!(fake.0.is_empty(), "{label}: no hidden skipped command");
    }
}

#[test]
fn upstream_library_is_the_build_scripts_import_library() {
    let msg = |paths: &str| {
        format!(
            r#"{{"reason":"build-script-executed","package_id":"x","linked_libs":[],"linked_paths":[{paths}]}}"#
        )
    };
    let text = [
        msg(r#""native=/reg/other/lib""#),
        msg(r#""dependency=/d","native=/reg/windows_aarch64_msvc-0.52.6/lib""#),
    ]
    .join("\n");
    let only = |p: &Path| p == Path::new("/reg/windows_aarch64_msvc-0.52.6/lib/windows.0.52.0.lib");
    assert_eq!(
        upstream_library(text.as_bytes(), only).unwrap(),
        Path::new("/reg/windows_aarch64_msvc-0.52.6/lib/windows.0.52.0.lib")
    );
    assert!(upstream_library(text.as_bytes(), |_| false).is_err());
    assert!(upstream_library(b"", |_| true).is_err());
}

#[test]
fn every_defining_member_is_kept() {
    let text = "kernel32.dll:\n0 T __imp_A\n0 T A\n\napi-ms-win.dll:\n0 T __imp_A\n0 U B\n";
    let defs = defined_in(text);
    assert_eq!(
        defs["__imp_A"],
        BTreeSet::from(["api-ms-win.dll".to_string(), "kernel32.dll".to_string()])
    );
    assert!(!defs.contains_key("B"));
}
