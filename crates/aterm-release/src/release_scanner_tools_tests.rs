// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn report_uses_exact_publication_environment_and_stays_outside_candidate() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let candidate = dir.path().join("candidate");
    std::fs::create_dir(&candidate).unwrap();
    let mut publication = command("/engine/bin/pub");
    publication
        .current_dir(&candidate)
        .env("PATH", "/actual/export/path")
        .env("PUBLICATION_ENGINE", "/scratch/engine")
        .env("ATPKG_KEYS", "/selected/helper")
        .env(ENV, "/old/report.json")
        .env_remove("RUSTFLAGS")
        .arg("stage");
    let report = prepare(
        &publication,
        0,
        &dir.path().join("report.json"),
        &[&candidate],
        |cmd| {
            assert_eq!(cmd.get_program(), publication.get_program());
            assert_eq!(cmd.get_current_dir(), publication.get_current_dir());
            assert_eq!(
                cmd.get_args().collect::<Vec<_>>(),
                [std::ffi::OsStr::new("scanner-preflight")]
            );
            for (name, value) in publication.get_envs().filter(|(name, _)| *name != ENV) {
                assert!(cmd.get_envs().any(|pair| pair == (name, value)));
            }
            assert!(
                cmd.get_envs()
                    .any(|(name, value)| name == ENV && value.is_none())
            );
            Ok(test_output())
        },
    )
    .unwrap();
    let mut stage = publication;
    report.apply(&mut stage);
    assert!(
        stage
            .get_envs()
            .any(|(name, value)| name == ENV && value == Some(report.path.as_os_str()))
    );
    assert_eq!(std::fs::read_dir(&candidate).unwrap().count(), 0);
    // The probe preserves the interpreter prefix and replaces only the
    // engine operation, never asking Python to execute scanner-preflight.
    let mut interpreted = command("/held/python3.14");
    interpreted.args(["-I", "-u", "/engine/bin/pub", "stage", "aterm"]);
    interpreted
        .current_dir(&candidate)
        .env("PATH", "/held:/usr/bin");
    prepare(
        &interpreted,
        3,
        &dir.path().join("interpreted.json"),
        &[&candidate],
        |probe| {
            assert_eq!(probe.get_program(), interpreted.get_program());
            assert_eq!(
                probe.get_args().collect::<Vec<_>>(),
                ["-I", "-u", "/engine/bin/pub", "scanner-preflight"]
            );
            assert_eq!(probe.get_current_dir(), interpreted.get_current_dir());
            assert_eq!(
                probe.get_envs().find(|(name, _)| *name == "PATH"),
                interpreted.get_envs().find(|(name, _)| *name == "PATH")
            );
            Ok(test_output())
        },
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(report.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn unsafe_location_and_failed_or_malformed_probes_never_produce_handoff() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let candidate = dir.path().join("candidate");
    std::fs::create_dir(&candidate).unwrap();
    let cmd = command("/engine/bin/pub");
    assert!(
        prepare(
            &cmd,
            0,
            &candidate.join("report.json"),
            &[&candidate],
            |_| panic!("must refuse before launching")
        )
        .is_err()
    );
    for bytes in [
        b"old engine help".as_slice(),
        b"{}",
        b"{\"schema\":1,\"kind\":\"publication-scanner-tools\",\"tools\":{}}",
    ] {
        let result = prepare(
            &cmd,
            0,
            &dir.path().join("report.json"),
            &[&candidate],
            |_| {
                let mut out = test_output();
                out.stdout = bytes.to_vec();
                Ok(out)
            },
        );
        assert!(result.is_err());
        assert!(!dir.path().join("report.json").exists());
    }
    assert!(
        prepare(
            &cmd,
            0,
            &dir.path().join("report.json"),
            &[&candidate],
            |_| { Err("missing or broken engine".into()) }
        )
        .is_err()
    );
    assert!(
        prepare(
            &cmd,
            0,
            &dir.path().join("report.json"),
            &[&candidate],
            |_| {
                let mut out = test_output();
                out.stdout = vec![b' '; MAX_REPORT + 1];
                Ok(out)
            }
        )
        .is_err()
    );
    #[cfg(unix)]
    {
        let alias = dir.path().join("candidate-alias");
        std::os::unix::fs::symlink(&candidate, &alias).unwrap();
        assert!(
            prepare(
                &cmd,
                0,
                &alias.join("report.json"),
                &[&candidate],
                |_| panic!("alias must refuse before launching")
            )
            .is_err()
        );
    }
}

#[test]
fn malformed_tool_identity_is_not_a_preflight_pass() {
    let absolute = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for (path, hash) in [
        ("relative/tool".to_owned(), "a".repeat(64)),
        (absolute.clone(), "A".repeat(64)),
        (absolute, "a".repeat(63)),
    ] {
        let bytes = aterm_json::json!({"schema":1,"kind":"publication-scanner-tools",
            "tools":{"engine-owned-name":{"path":path,"sha256":hash}}})
        .to_string();
        assert!(validate(bytes.as_bytes()).is_err());
    }
}
