// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn compilation_only_cannot_replace_test_execution_or_release_qualification() {
    assert!(
        super::parse(["test", "--all", "--compile-only"])
            .unwrap()
            .compile_only
    );
    for checks in [vec!["checkers"], vec!["test", "checkers"]] {
        let mut args = checks;
        args.extend(["--all", "--compile-only"]);
        assert!(super::parse(args).unwrap().compile_only);
    }
    for args in [
        vec!["release", "--app", "/test.app", "--compile-only"],
        vec!["test", "lint", "--compile-only"],
        vec!["--compile-only"],
    ] {
        assert!(super::parse(args).is_err());
    }
    for args in [
        vec!["test", "--all", "--compile-only", "--as-release"],
        vec!["test", "--all", "--as-release"],
        vec!["doc", "--all", "--as-release"],
    ] {
        assert!(super::parse(args).unwrap().as_release);
    }
    for args in [
        vec!["test", "--as-release"],
        vec!["--all", "--as-release"],
        vec!["lint", "--all", "--as-release"],
        vec!["test", "--compile-only", "--as-release"],
        vec![
            "test",
            "checkers",
            "--all",
            "--compile-only",
            "--as-release",
        ],
        vec!["release", "--app", "/test.app", "--as-release"],
    ] {
        assert!(super::parse(args).is_err());
    }
}

#[test]
fn focused_measurement_cannot_replace_release_qualification() {
    let good = super::parse(["measure", "--app", "/test.app", "--cursor-pacing"]).unwrap();
    assert!(good.cursor_pacing);
    for bad in [
        vec!["release", "--app", "/test.app", "--cursor-pacing"],
        vec!["test", "--app", "/test.app", "--cursor-pacing"],
        vec!["measure", "--cursor-pacing"],
    ] {
        assert!(super::parse(bad).is_err());
    }
}
