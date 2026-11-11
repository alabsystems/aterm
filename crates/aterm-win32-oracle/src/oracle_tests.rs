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
fn artifacts() -> String {
    [ ("conformance","/fixture/conformance.rlib"),("facade","/fixture/facade.rlib")].map(|(name,file)|format!(r#"{{"reason":"compiler-artifact","target":{{"name":"{name}"}},"filenames":["{file}"]}}"#)).join("\n")
}
#[test]
fn a_subject_rejection_is_distinct_from_a_failed_compiler_or_dependency() {
    let error = |name: &str| {
        format!(
            r#"{{"reason":"compiler-message","target":{{"name":"{name}"}},"message":{{"level":"error","message":"controlled type mismatch"}}}}"#
        )
    };
    for (label, result, expected) in [
        (
            "conformance rejects the changed type",
            answer(1, &error("conformance")),
            Some(false),
        ),
        ("unchanged subject compiles", answer(0, ""), Some(true)),
        ("dependency failed", answer(1, &error("windows_sys")), None),
        ("compiler died without a diagnostic", answer(1, ""), None),
        (
            "compiler could not start",
            Err("controlled start failure".into()),
            None,
        ),
    ] {
        let mut runner = Fake(VecDeque::from([answer(0, ""), result]));
        let got = checked(
            &mut runner,
            &tools(),
            Path::new("/fixture"),
            Path::new("/target"),
            TRIPLES[0],
            "aterm-win32-conformance",
        );
        assert_eq!(got.ok(), expected, "{label}");
        assert!(runner.0.is_empty(), "{label}: the actual command path ran");
    }
}
#[test]
fn unavailable_probes_never_satisfy_an_import_plant() {
    let want = BTreeMap::from([("__imp_PeekMessageW".into(), "user32.dll".into())]);
    let actual = |dll: &str| format!("{dll}:\n000000 T __imp_PeekMessageW\n");
    type Scenario = (&'static str, Vec<Result<Output>>, Option<bool>);
    let scenarios: Vec<Scenario> = vec![
        (
            "real DLL mismatch",
            vec![
                answer(0, ""),
                answer(0, &artifacts()),
                answer(0, " U __imp_PeekMessageW\n"),
                answer(0, &actual("kernel32.dll")),
            ],
            Some(true),
        ),
        (
            "unchanged plant",
            vec![
                answer(0, ""),
                answer(0, &artifacts()),
                answer(0, " U __imp_PeekMessageW\n"),
                answer(0, &actual("user32.dll")),
            ],
            Some(false),
        ),
        ("build failure", vec![answer(0, ""), answer(1, "")], None),
        (
            "nm nonzero",
            vec![answer(0, ""), answer(0, &artifacts()), answer(1, "")],
            None,
        ),
        (
            "nm start failure",
            vec![
                answer(0, ""),
                answer(0, &artifacts()),
                Err("controlled missing nm".into()),
            ],
            None,
        ),
        (
            "missing artifact",
            vec![answer(0, ""), answer(0, "{}")],
            None,
        ),
        (
            "empty inspection",
            vec![
                answer(0, ""),
                answer(0, &artifacts()),
                answer(0, ""),
                answer(0, ""),
            ],
            None,
        ),
        ("clean failure", vec![answer(1, "")], None),
    ];
    for (name, steps, expected) in scenarios {
        let mut fake = Fake(steps.into());
        let result = caught_import(
            &want,
            import_table(
                &mut fake,
                &tools(),
                Path::new("/fixture"),
                Path::new("/target"),
                TRIPLES[0],
                "facade",
            ),
        );
        assert_eq!(result.ok(), expected, "{name}");
        assert!(fake.0.is_empty(), "{name}: no hidden skipped command");
    }
}
#[test]
fn upstream_imports_come_from_windows_sys_and_windows() {
    let rlibs = |names: &[&str]| {
        names
            .iter()
            .map(|name| format!(r#"{{"reason":"compiler-artifact","target":{{"name":"{name}"}},"filenames":["/fixture/{name}.rlib"]}}"#))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let refs = " U __imp_PeekMessageW\n U __imp_CreateDXGIFactory1\n";
    let sys = "user32.dll:\n000000 T __imp_PeekMessageW\n";
    let windows = "dxgi.dll:\n000000 T __imp_CreateDXGIFactory1\n";
    let mut both = Fake(VecDeque::from([
        answer(0, ""),
        answer(0, &rlibs(&["conformance", "windows_sys", "windows"])),
        answer(0, refs),
        answer(0, sys),
        answer(0, windows),
    ]));
    let got = import_table(
        &mut both,
        &tools(),
        Path::new("/fixture"),
        Path::new("/target"),
        TRIPLES[0],
        "upstream",
    )
    .unwrap();
    assert_eq!(got["__imp_CreateDXGIFactory1"], "dxgi.dll");
    assert_eq!(got["__imp_PeekMessageW"], "user32.dll");
    assert!(both.0.is_empty());
    let mut missing = Fake(VecDeque::from([
        answer(0, ""),
        answer(0, &rlibs(&["conformance", "windows_sys"])),
        answer(0, refs),
        answer(0, sys),
    ]));
    assert!(
        import_table(
            &mut missing,
            &tools(),
            Path::new("/fixture"),
            Path::new("/target"),
            TRIPLES[0],
            "upstream",
        )
        .is_err()
    );
}
#[test]
fn hand_written_link_lines_are_counted() {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/com.rs"), "// link!( in prose\n").unwrap();
    std::fs::write(
        dir.path().join("src/dxgi.rs"),
        "link!(\"dxgi.dll\" \"system\" fn A());\n  link!(\"dxgi.dll\" \"system\" fn B());\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/d3d11.rs"),
        "link!(\"d3d11.dll\" \"system\" fn C());\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/dcomp.rs"), "").unwrap();
    std::fs::write(dir.path().join("src/uia.rs"), "").unwrap();
    assert_eq!(hand_written_imports(dir.path()).unwrap(), 3);
    std::fs::remove_file(dir.path().join("src/com.rs")).unwrap();
    assert!(hand_written_imports(dir.path()).is_err());
}
#[test]
fn plant_changes_exactly_one_real_source_occurrence() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let p = Plant {
        name: "dll",
        phase: "import",
        file: "library.rs",
        old: "user32.dll",
        new: "kernel32.dll",
    };
    std::fs::write(
        dir.path().join(p.file),
        "link!(\"user32.dll\" \"system\" fn Example());",
    )
    .unwrap();
    plant(dir.path(), &p).unwrap();
    assert!(
        read(&dir.path().join(p.file))
            .unwrap()
            .contains("kernel32.dll")
    );
    assert!(
        plant(dir.path(), &p).is_err(),
        "refuse vacuous or ambiguous plant"
    );
}
#[test]
fn every_plant_finds_its_one_spot_in_aterm_win32() {
    let krate = aterm_tempfile::manifest_dir().join("../aterm-win32");
    for p in PLANTS {
        let text = read(&krate.join(p.file)).unwrap();
        assert_eq!(
            text.matches(p.old).count(),
            1,
            "plant {} in {}",
            p.name,
            p.file
        );
    }
}
