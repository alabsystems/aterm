// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_lab_app_requires_positive_debug_profile_evidence_and_no_test_harness() {
    let artifact = |profile: &str| {
        format!(
            "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"aterm\",\"kind\":[\"bin\"]}},\"executable\":\"/fixture/aterm\",\"profile\":{profile}}}"
        )
    };
    let valid = artifact(r#"{"debug_assertions":true,"test":false}"#);
    assert_eq!(
        built_debug_app(&valid, "aterm").unwrap(),
        PathBuf::from("/fixture/aterm")
    );
    for profile in [
        r#"{"debug_assertions":false,"test":false}"#,
        r#"{"debug_assertions":true,"test":true}"#,
        r#"{"debug_assertions":true}"#,
        r#"{"test":false}"#,
        r#"{"debug_assertions":"true","test":false}"#,
        r#"{"debug_assertions":false,"debug_assertions":true,"test":false}"#,
    ] {
        assert!(
            built_debug_app(&artifact(profile), "aterm").is_err(),
            "{profile}"
        );
    }
    let release = artifact(r#"{"debug_assertions":false,"test":false}"#);
    assert!(built_executable(&release, "aterm", "bin").is_ok());
}

#[test]
fn the_built_aterm_is_the_binary_cargo_reports() {
    let messages = [
        r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"aterm"},"executable":null,"fresh":true}"#,
        r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"aterm"},"filenames":["/w/target/opt-test/aterm"],"executable":"/w/target/opt-test/aterm","fresh":false}"#,
        r#"{"reason":"compiler-artifact","target":{"kind":["bench"],"name":"fabric_bench"},"executable":"/c/.cargo-build/opt-test/deps/fabric_bench-7b2c","fresh":false}"#,
        r#"{"reason":"build-finished","success":true}"#,
        "not json: a stray line",
    ]
    .join("\n");
    assert_eq!(
        built_binary(&messages, "aterm"),
        Ok(PathBuf::from("/w/target/opt-test/aterm"))
    );
    assert!(built_binary(&messages, "aterm-ctl").is_err());
    let nameless = r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"aterm"},"executable":null}"#;
    assert!(built_binary(nameless, "aterm").is_err());
    assert_eq!(
        built_executable(&messages, "fabric_bench", "bench"),
        Ok(PathBuf::from(
            "/c/.cargo-build/opt-test/deps/fabric_bench-7b2c"
        ))
    );
    assert!(built_executable(&messages, "fabric_bench", "example").is_err());
    let examples = r#"{"reason":"compiler-artifact","target":{"kind":["example"],"name":"probe"},"executable":"/cache/probe","fresh":true}"#;
    assert_eq!(
        built_executable(examples, "probe", "example"),
        Ok(PathBuf::from("/cache/probe"))
    );
    assert!(built_executable(examples, "probe", "lib").is_err());
    let library_tests=[
        r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"aterm_db"},"profile":{"test":false},"executable":null}"#,
        r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"aterm_db"},"profile":{"test":true},"executable":"/cache/deps/aterm_db-test"}"#,
    ].join("\n");
    assert_eq!(
        built_executable(&library_tests, "aterm_db", "lib"),
        Ok(PathBuf::from("/cache/deps/aterm_db-test"))
    );
}

#[test]
fn bench_output_environment_keeps_os_paths() {
    let keeps = |path: &Path| {
        let mut command = crate::exec::check_command("not-executed");
        set_bench_environment(&mut command, "KNOBS", "samples=3", ("OUTPUT", path));
        let vars: Vec<_> = command.get_envs().collect();
        assert!(vars.contains(&(
            std::ffi::OsStr::new("KNOBS"),
            Some(std::ffi::OsStr::new("samples=3"))
        )));
        assert!(vars.contains(&(std::ffi::OsStr::new("OUTPUT"), Some(path.as_os_str()))));
    };
    keeps(Path::new("/scratch/with spaces"));
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        keeps(&PathBuf::from(std::ffi::OsString::from_vec(
            b"/scratch/non-utf8-\xff".to_vec(),
        )));
    }
}

#[test]
fn conflicting_artifacts_do_not_select_a_measurement_executable() {
    let artifact = |name: &str, kind: &str, path: &str| {
        format!(
            r#"{{"reason":"compiler-artifact","target":{{"kind":["{kind}"],"name":"{name}"}},"executable":"{path}"}}"#
        )
    };
    let first = artifact("x", "bin", "/first");
    let second = artifact("x", "bin", "/second");
    for positive in [
        first.clone(),
        format!("{first}\n{first}"),
        format!(
            "{}\n{first}\n{}",
            artifact("other", "bin", "/other"),
            artifact("x", "example", "/example")
        ),
    ] {
        assert_eq!(
            built_executable(&positive, "x", "bin").unwrap(),
            PathBuf::from("/first")
        );
    }
    for conflicting in [format!("{first}\n{second}"), format!("{second}\n{first}")] {
        assert!(built_executable(&conflicting, "x", "bin").is_err());
    }
}

#[test]
fn mixed_type_target_kinds_do_not_select_measurement_executables() {
    let valid = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"x"},"profile":{"test":true},"executable":"/built/x"}"#;
    for kinds in [r#"["lib"]"#, r#"["rlib","lib"]"#] {
        let positive = valid.replace(r#"["lib"]"#, kinds);
        assert_eq!(
            built_executable(&positive, "x", "lib").unwrap(),
            PathBuf::from("/built/x")
        );
    }
    for kinds in [r#"["lib",7]"#, r#"[null,"lib"]"#, r#"["lib",{}]"#] {
        let malformed = valid.replace(r#"["lib"]"#, kinds);
        for wire in [
            malformed.clone(),
            format!("{valid}\n{malformed}"),
            format!("{malformed}\n{valid}"),
        ] {
            assert!(built_executable(&wire, "x", "lib").is_err(), "{kinds}");
        }
    }
}

#[test]
fn ambiguous_compiler_artifacts_do_not_select_measurement_executables() {
    let valid = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"x"},"profile":{"test":true},"executable":"/built/x"}"#;
    assert_eq!(
        built_executable(valid, "x", "lib").unwrap(),
        PathBuf::from("/built/x")
    );
    for message in include_str!("../testdata/ambiguous-compiler-artifacts.jsonl").lines() {
        for wire in [format!("{valid}\n{message}"), format!("{message}\n{valid}")] {
            assert!(built_executable(&wire, "x", "lib").is_err(), "{message}");
        }
    }
}

#[test]
fn another_build_detects_changed_bytes_with_identical_file_metadata() {
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("data-only-not-executed");
    std::fs::write(&path, b"original").unwrap();
    assert!(bench_profile::identity(&world.path().join("missing")).is_err());
    assert!(bench_profile::identity(world.path()).is_err());
    let before = std::fs::metadata(&path).unwrap();
    let identity = bench_profile::identity(&path).unwrap();
    assert_eq!(bench_profile::identity(&path).unwrap(), identity);
    std::fs::write(&path, b"replaced").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
        .unwrap();
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    let changed = bench_profile::identity(&path).unwrap();
    let report = aterm_json::json!({
        "aterm_binary": path.to_str().unwrap(),
        "aterm_binary_identity": { "start": changed, "end": changed },
    });
    assert!(another_build(&report, &path, &identity).is_some());
}

#[test]
fn a_report_of_another_path_or_another_file_there_is_another_build() {
    let built = Path::new("/w/target/opt-test/aterm");
    let id = "87127144 bytes, modified 1792221563.000000001";
    let report = |path: &str, start: &str, end: &str| {
        aterm_json::json!({
            "aterm_binary": path,
            "aterm_binary_identity": { "start": start, "end": end },
        })
    };
    let path = built.to_str().expect("utf-8");
    assert_eq!(another_build(&report(path, id, id), built, id), None);
    let why = another_build(&report("/w/target/debug/aterm", id, id), built, id).expect("refused");
    assert!(why.contains("/w/target/debug/aterm"), "{why}");
    let other = "84000000 bytes, modified 1792221999.000000001";
    for (start, end, moment) in [(other, id, "start"), (id, other, "end")] {
        let why = another_build(&report(path, start, end), built, id).expect("refused");
        assert!(why.contains(moment) && why.contains(other), "{why}");
    }
    let bare = aterm_json::json!({ "aterm_binary": path });
    assert!(another_build(&bare, built, id).is_some());
}

#[test]
fn a_second_run_on_one_day_gets_its_own_entry() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let (md, json) = ledger_paths(dir.path(), "2026-10-16", "fabric");
    assert!(md.ends_with("2026-10-16-fabric.md"), "{}", md.display());
    assert!(json.ends_with("2026-10-16-fabric.json"));
    std::fs::write(&md, "x").expect("write");
    let (md2, json2) = ledger_paths(dir.path(), "2026-10-16", "fabric");
    assert!(md2.ends_with("2026-10-16-fabric-2.md"), "{}", md2.display());
    assert!(json2.ends_with("2026-10-16-fabric-2.json"));
}

#[test]
fn the_entry_head_carries_every_provenance_field() {
    let p = Provenance {
        heading: "Fabric: post -> deliver, burst",
        protocol: "docs/measured/fabric/README.md",
        date: "2026-10-16",
        machine: "Apple M5 Max",
        macos: "27.0",
        aterm: "abc123",
        astream: "6aab5d26a405",
        spec: "samples=200",
        profile: "dev",
        elapsed_s: 81.0,
        command: "targo --unverified verify perf fabric",
        build: "`dev` — 96 units on the measured path: opt-level 0 ×87",
        thermal: "No thermal warning level has been recorded",
        cpus: "18 logical, 6 performance cores",
        load: "5.62 4.40 4.08 before the build, 6.00 5.00 4.00 after the run",
    };
    let head = p.render();
    for (field, value) in [
        ("heading", p.heading),
        ("protocol", p.protocol),
        ("date", p.date),
        ("machine", p.machine),
        ("macos", p.macos),
        ("aterm", p.aterm),
        ("astream", p.astream),
        ("spec", p.spec),
        ("profile", p.profile),
        ("elapsed", "81.0"),
        ("command", p.command),
        ("build", p.build),
        ("thermal", p.thermal),
        ("cpus", p.cpus),
        ("load", p.load),
    ] {
        assert!(head.contains(value), "the head lost the {field}:\n{head}");
    }
}
