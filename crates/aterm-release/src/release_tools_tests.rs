// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A /1 freezer cannot supply /2's built_from binding. Repair the old proof once;
/// a failed repair is not permission for another compile/exec cycle.
#[test]
fn a_v1_cutter_record_gets_one_v2_repair() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let release = temp.path().join("release");
    let source = tool_source(temp.path(), "skew-cutter", CUTTER_VERSION, 0);
    let cutter = stage_cutter(
        &release,
        COMMIT,
        &source,
        &release.with_extension("events"),
        None,
    )
    .unwrap();
    let mut legacy = aterm_toml::to_string(&cutter).unwrap();
    legacy = legacy
        .lines()
        .filter(|line| !line.starts_with("built_from ="))
        .map(|line| {
            if line == "schema = 2" {
                "schema = 1"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(cutter_record_path(&release), legacy).unwrap();
    assert!(recorded_cutter(&release, COMMIT).is_err());
    let old_launch = |name: &str| match name {
        RECORD_ENV => Some(cutter_record_path(&release).to_string_lossy().into_owned()),
        FREEZER_ENV => Some(COMMIT.into()),
        CONTRACT_ENV => Some("cutter.toml/1".into()),
        _ => None,
    };
    assert_eq!(
        Recovery::from_launch(COMMIT, false, old_launch),
        Recovery::Refreeze
    );

    // The new writer supplies the missing binding. This is usable evidence, rather
    // than a default value invented by the reader of an incompatible proof.
    publish_cutter(&release, &cutter).unwrap();
    assert_eq!(recorded_cutter(&release, COMMIT).unwrap(), cutter);
    let mut repaired = aterm_uds::spawnfd::command("/usr/bin/true");
    Recovery::Refreeze.hand(&mut repaired, COMMIT, COMMIT, None);
    let handed = repaired.get_envs().collect::<Vec<_>>();
    let read = |name: &str| {
        handed
            .iter()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| *value)
            .map(|value| value.to_string_lossy().into_owned())
            .or_else(|| old_launch(name))
    };
    assert_eq!(Recovery::from_launch(COMMIT, false, read), Recovery::Refuse);
    assert_eq!(Recovery::from_launch(COMMIT, true, read), Recovery::Initial);
}

#[test]
fn a_local_phase_is_recorded_when_its_operation_returns_an_error() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let log = temp.path().join("events");
    let operation = || -> Result<(), ()> {
        let _phase = crate::cut_timing::Timer::start(&log, "failed-operation");
        Err(())
    };
    assert!(operation().is_err());
    let text = std::fs::read_to_string(&log).unwrap();
    let line = text.lines().last().unwrap();
    let event: aterm_json::Value =
        aterm_json::from_str(line.trim().strip_prefix("release-event: ").unwrap()).unwrap();
    assert_eq!(event["event"].as_str(), Some("cut-gate-finished"));
    assert_eq!(event["gate"].as_str(), Some("failed-operation"));
    assert!(event["execution_s"].as_f64().is_some());
    assert!(event.get("outcome").is_none(), "telemetry is no verdict");
}

/// NEGATIVE CONTROLS: the old tool.toml path must not veto repair; own-writer failures
/// and a second failure must not repair; a marker for a prior candidate cannot veto R.
#[test]
fn launch_contract_bounds_recovery_without_authenticating_the_proof() {
    let old = "f".repeat(40);
    let decide = |fields: &[(&str, &str)]| {
        Recovery::from_launch(COMMIT, false, |name| {
            fields
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
    };
    assert_eq!(decide(&[]), Recovery::Initial);
    assert_eq!(
        decide(&[(RECORD_ENV, "/release/tool.toml")]),
        Recovery::Refreeze
    );
    let own = [
        (RECORD_ENV, "/release/cutter.toml"),
        (FREEZER_ENV, COMMIT),
        (CONTRACT_ENV, CONTRACT),
    ];
    assert_eq!(decide(&own), Recovery::Refuse);
    assert_eq!(
        decide(&[
            (RECORD_ENV, "/release/cutter.toml"),
            (FREEZER_ENV, &old),
            (CONTRACT_ENV, CONTRACT)
        ]),
        Recovery::Refreeze
    );
    assert_eq!(
        decide(&[
            (RECORD_ENV, "/release/cutter.toml"),
            (FREEZER_ENV, COMMIT),
            (CONTRACT_ENV, "older")
        ]),
        Recovery::Refreeze
    );
    assert_eq!(
        decide(&[(RECORD_ENV, "/release/tool.toml"), (REFREEZE_ENV, COMMIT)]),
        Recovery::Refuse
    );
    assert_eq!(decide(&[(REFREEZE_ENV, COMMIT)]), Recovery::Refuse);
    assert_eq!(
        decide(&[(RECORD_ENV, "/release/tool.toml"), (REFREEZE_ENV, &old)]),
        Recovery::Refreeze
    );
    assert_eq!(
        Recovery::from_launch(COMMIT, true, |name| match name {
            RECORD_ENV => Some("/release/cutter.toml".into()),
            FREEZER_ENV | REFREEZE_ENV => Some(COMMIT.into()),
            CONTRACT_ENV => Some(CONTRACT.into()),
            _ => None,
        }),
        Recovery::Initial,
        "a valid stamp dispatches normally even after a prior repair"
    );
    for (recovery, prior, expected) in [
        (Recovery::Initial, None, None),
        (Recovery::Refreeze, None, Some(COMMIT)),
        (Recovery::Initial, Some(COMMIT), Some(COMMIT)),
    ] {
        let mut command = aterm_uds::spawnfd::command("/usr/bin/true");
        recovery.hand(
            &mut command,
            COMMIT,
            COMMIT,
            prior.map(std::ffi::OsStr::new),
        );
        let env = command
            .get_envs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            env[std::ffi::OsStr::new(REFREEZE_ENV)],
            expected.map(std::ffi::OsStr::new)
        );
        assert_eq!(
            env[std::ffi::OsStr::new(FREEZER_ENV)],
            Some(std::ffi::OsStr::new(COMMIT))
        );
        assert_eq!(
            env[std::ffi::OsStr::new(CONTRACT_ENV)],
            Some(std::ffi::OsStr::new(CONTRACT))
        );
    }
}

/// NEGATIVE CONTROL: accepting the legacy proof bypasses the re-freeze; trusting the
/// repair hint instead of the new proof accepts the deliberately mutated image.
#[test]
fn legacy_tool_record_needs_a_new_content_proof() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let (release, _, _, old) = staged_fixture(temp.path());
    publish(&release, &old).unwrap();
    assert!(proved_cutter(&old.cutter.path, &record_path(&release)).is_err());
    let repaired = stage_cutter(
        &release,
        COMMIT,
        &old.cutter.path,
        &release.with_extension("events"),
        None,
    )
    .unwrap();
    publish_cutter(&release, &repaired).unwrap();
    assert_eq!(
        proved_cutter(&repaired.tool.path, &cutter_record_path(&release)).unwrap(),
        repaired
    );
    std::fs::write(&repaired.tool.path, b"invalid replacement").unwrap();
    assert!(proved_cutter(&repaired.tool.path, &cutter_record_path(&release)).is_err());
}

/// NEGATIVE CONTROLS: requiring a verifier prevents the first proof; accepting another
/// commit, path, schema or changed image makes one of the mutated bindings pass.
#[test]
fn a_cutter_can_be_frozen_before_the_verifier_but_every_binding_is_required() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let release = temp.path().join("release");
    let source = tool_source(temp.path(), "cutter-only", CUTTER_VERSION, 0);
    let cutter = stage_cutter(
        &release,
        COMMIT,
        &source,
        &release.with_extension("events"),
        None,
    )
    .unwrap();
    assert!(!record_path(&release).exists());
    publish_cutter(&release, &cutter).unwrap();
    assert_eq!(recorded_cutter(&release, COMMIT).unwrap(), cutter);
    assert!(recorded_cutter(&release, &"f".repeat(40)).is_err());
    for field in 0..3 {
        let mut mutant = cutter.clone();
        match field {
            0 => mutant.schema += 1,
            1 => mutant.tool.path = source.clone(),
            _ => mutant.tool.sha256 = "0".repeat(64),
        }
        assert!(publish_cutter(&release, &mutant).is_err());
    }
    std::fs::write(&cutter.tool.path, b"mutated after first exec completed").unwrap();
    assert!(recorded_cutter(&release, COMMIT).is_err());
    assert!(
        stage_cutter(
            &release,
            COMMIT,
            &source,
            &release.with_extension("events"),
            None
        )
        .is_err()
    );
}

/// Byte equality alone cannot authorize adoption: each digest component, Cargo's
/// freshness and the prior immutable copy must independently remain valid.
#[test]
fn adoption_binds_both_commits_and_rejects_changed_or_unproved_inputs() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let release = temp.path().join("release");
    let source = tool_source(temp.path(), "adopt-cutter", CUTTER_VERSION, 0);
    let inputs = crate::cutter_inputs::Inputs {
        digest: "1".repeat(64),
        sources: "2".repeat(64),
        seal: "3".repeat(64),
    };
    let original = stage_cutter(
        &release,
        COMMIT,
        &source,
        &release.with_extension("events"),
        Some(inputs.clone()),
    )
    .unwrap();
    publish_cutter(&release, &original).unwrap();
    let next = "b".repeat(40);
    let mut observation = crate::cutter_inputs::Observation {
        inputs,
        all_fresh: true,
    };
    let adopted = adopt_cutter(&release, &next, &source, &observation)
        .unwrap()
        .unwrap();
    assert_eq!(adopted.commit, next);
    assert_eq!(adopted.built_from, COMMIT);
    assert_eq!(adopted.tool, original.tool);
    publish_cutter(&release, &adopted).unwrap();
    assert_eq!(recorded_cutter(&release, &next).unwrap(), adopted);
    for field in 0..3 {
        let mut changed = observation.inputs.clone();
        match field {
            0 => changed.digest = "4".repeat(64),
            1 => changed.sources = "4".repeat(64),
            _ => changed.seal = "4".repeat(64),
        }
        let changed = crate::cutter_inputs::Observation {
            inputs: changed,
            all_fresh: true,
        };
        assert!(
            adopt_cutter(&release, &"c".repeat(40), &source, &changed)
                .unwrap()
                .is_none()
        );
    }
    observation.all_fresh = false;
    assert!(
        adopt_cutter(&release, &next, &source, &observation)
            .unwrap()
            .is_none()
    );
    observation.all_fresh = true;
    let different = tool_source(temp.path(), "different-image", CUTTER_VERSION, 1);
    assert!(
        adopt_cutter(&release, &next, &different, &observation)
            .unwrap()
            .is_none()
    );
    let mut unproved = original;
    unproved.inputs = None;
    publish_cutter(&release, &unproved).unwrap();
    observation.all_fresh = true;
    assert!(
        adopt_cutter(&release, &next, &source, &observation)
            .unwrap()
            .is_none()
    );
}

#[test]
fn only_a_parsed_release_preview_can_start_away_from_its_frozen_commit() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    crate::ledger::testing::init(
        &repo,
        &[
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ],
    );
    let git = GitCli::new(&repo);
    crate::ledger::git_ok(&git, &["commit", "--allow-empty", "-qm", "checkout"]).unwrap();
    let candidate = "1".repeat(40);
    let source = tool_source(temp.path(), "source", CUTTER_VERSION, 0);
    let release = temp.path().join("release");
    let cutter = stage_cutter(
        &release,
        &candidate,
        &source,
        &temp.path().join("log"),
        None,
    )
    .unwrap();
    publish_cutter(&release, &cutter).unwrap();
    let read = |args: &[&str]| {
        proved_running_at(
            &cutter.tool.path,
            &cutter_record_path(&release),
            &repo,
            Some(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>()),
        )
    };
    assert_eq!(
        read(&["release", "--dry-run", "--mac-only"]).unwrap(),
        candidate
    );
    // Negative controls: another dry verb, a real cut, an invalid preview and absent
    // arguments all retain startup's clean/exact-checkout requirement.
    for args in [
        &["release", "--mac-only"][..],
        &["cut", "--dry-run"],
        &["release", "--dry-run", "--unknown"],
        &["release", "--dry-run", "--forget"],
    ] {
        assert!(read(args).is_err(), "{args:?}");
    }
    assert!(
        proved_running_at(
            &cutter.tool.path,
            &cutter_record_path(&release),
            &repo,
            None
        )
        .is_err()
    );
}

fn tool_source(dir: &Path, name: &str, version: &str, exit: i32) -> PathBuf {
    assert!(!version.contains('\''), "the shell literal stays quoted");
    let path = dir.join(name);
    let body = format!(
        "#!/bin/sh\n\
         [ \"$#\" -eq 1 ] && [ \"$1\" = \"--version\" ] || exit 64\n\
         printf '%s\\n' '{version}'\n\
         exit {exit}\n"
    );
    aterm_tempfile::stand_in(&path, &body).expect("make tool stand-in");
    // On macOS stand-ins are symlinks into a shared executable cache. Production custody
    // correctly refuses symlink sources, so give it the regular file the stand-in names.
    std::fs::canonicalize(path).expect("resolve regular stand-in")
}

fn staged_fixture(base: &Path) -> (PathBuf, PathBuf, PathBuf, ToolSet) {
    let release = base.join("release");
    let cutter = tool_source(base, "source-cutter", CUTTER_VERSION, 0);
    let verify = tool_source(base, "source-verify", VERIFY_VERSION, 0);
    let tools = stage(&release, COMMIT, &cutter, &verify).expect("stage frozen tools");
    (release, cutter, verify, tools)
}

fn write_record(release: &Path, tools: &ToolSet) {
    let text = aterm_toml::to_string(tools).expect("serialize record");
    std::fs::write(record_path(release), text).expect("write record mutation");
}

fn relative_to_current(path: &Path) -> PathBuf {
    let from_path = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let to_path = std::fs::canonicalize(path).unwrap();
    let from: Vec<_> = from_path.components().collect();
    let to: Vec<_> = to_path.components().collect();
    let shared = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    assert!(shared > 0, "Unix absolute paths share their root");
    let mut relative = PathBuf::new();
    for _ in shared..from.len() {
        relative.push("..");
    }
    for part in &to[shared..] {
        relative.push(part.as_os_str());
    }
    assert!(!relative.is_absolute());
    relative
}

#[test]
fn frozen_tools_are_copied_hashed_proved_and_recorded() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = aterm_tempfile::tempdir().expect("tempdir");
    let (release, cutter, verify, tools) = staged_fixture(temp.path());

    assert!(!record_path(&release).exists());
    assert_eq!(tools.commit, COMMIT);
    assert_eq!(
        tools.set_sha256,
        set_digest(&tools.cutter.sha256, &tools.verify.sha256)
    );
    let frozen = release.join(".ubd/tools").join(&tools.set_sha256);
    assert_eq!(tools.cutter.path, frozen.join("aterm-release"));
    assert_eq!(tools.verify.path, frozen.join("aterm-verify"));
    for (source, tool) in [(&cutter, &tools.cutter), (&verify, &tools.verify)] {
        assert_eq!(
            std::fs::read(source).expect("read source"),
            std::fs::read(&tool.path).expect("read frozen copy")
        );
        assert_eq!(crate::dmg::sha256_file(source).unwrap(), tool.sha256);
        assert_eq!(crate::dmg::sha256_file(&tool.path).unwrap(), tool.sha256);
        assert_eq!(
            std::fs::metadata(&tool.path)
                .expect("frozen metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    let record = publish(&release, &tools).expect("publish record");
    assert_eq!(record, record_path(&release));
    assert_eq!(
        std::fs::metadata(&record)
            .expect("record metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(recorded(&release, COMMIT).unwrap(), tools);
}

#[test]
fn a_mutated_frozen_copy_is_refused_and_never_replaced() {
    use std::io::Write as _;

    let temp = aterm_tempfile::tempdir().expect("tempdir");
    for mutate_cutter in [true, false] {
        let case = temp
            .path()
            .join(if mutate_cutter { "cutter" } else { "verify" });
        std::fs::create_dir(&case).unwrap();
        let (release, cutter, verify, tools) = staged_fixture(&case);
        publish(&release, &tools).expect("publish valid record");
        assert_eq!(recorded(&release, COMMIT).unwrap(), tools);

        let changed = if mutate_cutter {
            &tools.cutter
        } else {
            &tools.verify
        };
        let unchanged = if mutate_cutter {
            &tools.verify
        } else {
            &tools.cutter
        };
        let mut copied = std::fs::OpenOptions::new()
            .append(true)
            .open(&changed.path)
            .expect("open frozen tool for mutation");
        copied.write_all(b"\n# mutation\n").unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the mutation fixture must finish changing frozen bytes before their content is proved"
        )]
        copied.sync_all().unwrap();
        drop(copied);
        let mutated = std::fs::read(&changed.path).unwrap();

        assert!(recorded(&release, COMMIT).is_err());
        assert!(publish(&release, &tools).is_err());
        assert!(stage(&release, COMMIT, &cutter, &verify).is_err());
        assert_eq!(std::fs::read(&changed.path).unwrap(), mutated);
        assert_eq!(
            crate::dmg::sha256_file(&unchanged.path).unwrap(),
            unchanged.sha256
        );
    }
}

#[test]
fn the_manifest_refuses_each_mutated_binding_and_unknown_fields() {
    let temp = aterm_tempfile::tempdir().expect("tempdir");
    let (release, _, _, tools) = staged_fixture(temp.path());
    publish(&release, &tools).expect("publish valid record");

    let mut mutant = tools.clone();
    mutant.schema += 1;
    write_record(&release, &mutant);
    assert!(recorded(&release, COMMIT).is_err(), "schema mutation");

    let mut mutant = tools.clone();
    mutant.commit = "1".repeat(40);
    write_record(&release, &mutant);
    assert!(recorded(&release, COMMIT).is_err(), "commit mutation");

    let mut mutant = tools.clone();
    mutant.set_sha256 = "0".repeat(64);
    write_record(&release, &mutant);
    assert!(recorded(&release, COMMIT).is_err(), "set digest mutation");

    let mut mutant = tools.clone();
    mutant.cutter.path = release
        .join(".ubd/tools")
        .join(&tools.set_sha256)
        .join("other");
    write_record(&release, &mutant);
    assert!(recorded(&release, COMMIT).is_err(), "cutter path mutation");

    let mut mutant = tools.clone();
    mutant.verify.path = release
        .join(".ubd/tools")
        .join(&tools.set_sha256)
        .join("other");
    write_record(&release, &mutant);
    assert!(recorded(&release, COMMIT).is_err(), "verify path mutation");

    let mut text = aterm_toml::to_string(&tools).expect("serialize valid record");
    text.push_str("\nunknown = true\n");
    std::fs::write(record_path(&release), text).unwrap();
    assert!(recorded(&release, COMMIT).is_err(), "unknown field");

    write_record(&release, &tools);
    assert_eq!(recorded(&release, COMMIT).unwrap(), tools);
}

#[test]
fn first_exec_requires_the_exact_line_and_a_successful_exit() {
    let temp = aterm_tempfile::tempdir().expect("tempdir");
    let exact = tool_source(temp.path(), "exact", CUTTER_VERSION, 0);
    assert!(prove_exec(&exact, CUTTER_VERSION).is_ok());

    let wrong = tool_source(temp.path(), "wrong", "not the cutter version", 0);
    let error = prove_exec(&wrong, CUTTER_VERSION).unwrap_err();
    assert!(error.contains("not the exact line"), "{error}");

    let failed = tool_source(temp.path(), "failed", CUTTER_VERSION, 23);
    let error = prove_exec(&failed, CUTTER_VERSION).unwrap_err();
    assert!(error.contains("ended"), "{error}");

    let extra = temp.path().join("extra");
    aterm_tempfile::stand_in(
        &extra,
        &format!("#!/bin/sh\nprintf '%s\\n' '{}' extra\n", CUTTER_VERSION),
    )
    .unwrap();
    let extra = std::fs::canonicalize(extra).unwrap();
    let error = prove_exec(&extra, CUTTER_VERSION).unwrap_err();
    assert!(error.contains("not the exact line"), "{error}");
}

/// Printing the proof line is insufficient until the process exits and stdout reaches EOF.
/// The output loop has no reader thread: this test stops it synchronously once the line is
/// observed, and return proves cleanup cannot leave such a reader across the later exec.
/// NEGATIVE CONTROL: returning at the line, or calling `wait` after a detached line reader,
/// either accepts this non-exit or never returns.
#[test]
fn a_version_line_without_exit_is_refused_with_no_reader_left() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let path = temp.path().join("line-then-hang");
    aterm_tempfile::stand_in(
        &path,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\nwhile :; do sleep 60; done\n",
            CUTTER_VERSION
        ),
    )
    .unwrap();
    let path = std::fs::canonicalize(path).unwrap();
    let expected = format!("{CUTTER_VERSION}\n");
    let mut observed = false;
    let error = prove_exec_until(
        &path,
        CUTTER_VERSION,
        std::time::Instant::now() + aterm_uds::deadline::FIRST_EXEC_HANG,
        &mut |bytes| {
            observed |= bytes == expected.as_bytes();
            observed
        },
    )
    .unwrap_err();
    assert!(
        observed,
        "the child wrote the exact line before it was stopped"
    );
    assert!(error.contains("stopped after observing output"), "{error}");
}

#[test]
fn staging_requires_first_exec_proof_from_both_copies() {
    let temp = aterm_tempfile::tempdir().expect("tempdir");
    for wrong_cutter in [true, false] {
        let case = temp
            .path()
            .join(if wrong_cutter { "cutter" } else { "verify" });
        std::fs::create_dir(&case).unwrap();
        let release = case.join("release");
        let cutter = tool_source(
            &case,
            "source-cutter",
            if wrong_cutter {
                "wrong cutter"
            } else {
                CUTTER_VERSION
            },
            0,
        );
        let verify = tool_source(
            &case,
            "source-verify",
            if wrong_cutter {
                VERIFY_VERSION
            } else {
                "wrong verify"
            },
            0,
        );

        assert!(stage(&release, COMMIT, &cutter, &verify).is_err());
        assert!(!record_path(&release).exists());
    }
}

#[test]
fn staging_refuses_a_relative_release_root_before_writing_it() {
    let temp = aterm_tempfile::tempdir().expect("tempdir");
    let release = relative_to_current(temp.path()).join("release");
    let cutter = tool_source(temp.path(), "source-cutter", CUTTER_VERSION, 0);
    let verify = tool_source(temp.path(), "source-verify", VERIFY_VERSION, 0);

    assert!(stage(&release, COMMIT, &cutter, &verify).is_err());
    assert!(!release.exists());
}

#[test]
fn copy_refuses_a_symlink_source_without_creating_a_destination() {
    use std::os::unix::fs::symlink;

    let temp = aterm_tempfile::tempdir().expect("tempdir");
    let source = tool_source(temp.path(), "source", CUTTER_VERSION, 0);
    let linked = temp.path().join("linked-source");
    let destination = temp.path().join("destination");
    symlink(source, &linked).expect("make source symlink");

    assert!(copy_executable(&linked, &destination).is_err());
    assert!(!destination.exists());
}

/// Content-addressed leaf names do not make a symlinked storage root or set immutable. Both
/// are refused before a tool can be written through them. Replacing `symlink_metadata` with
/// `exists` follows either link and makes these negative controls fail.
#[test]
fn staging_refuses_symlinked_storage_directories() {
    use std::os::unix::fs::symlink;

    for link_root in [true, false] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let release = temp.path().join("release");
        let outside = temp.path().join("compiler-owned");
        std::fs::create_dir_all(&outside).unwrap();
        let cutter = tool_source(temp.path(), "source-cutter", CUTTER_VERSION, 0);
        let verify = tool_source(temp.path(), "source-verify", VERIFY_VERSION, 0);
        if link_root {
            std::fs::create_dir_all(release.join(".ubd")).unwrap();
            symlink(&outside, release.join(".ubd/tools")).unwrap();
        } else {
            let prototype = ToolSet::new(
                COMMIT,
                Tool {
                    path: PathBuf::new(),
                    sha256: crate::dmg::sha256_file(&cutter).unwrap(),
                },
                Tool {
                    path: PathBuf::new(),
                    sha256: crate::dmg::sha256_file(&verify).unwrap(),
                },
            );
            std::fs::create_dir_all(release.join(".ubd/tools")).unwrap();
            symlink(
                &outside,
                release.join(".ubd/tools").join(prototype.set_sha256),
            )
            .unwrap();
        }

        let error = stage(&release, COMMIT, &cutter, &verify).unwrap_err();
        assert!(error.contains("not a real directory"), "{error}");
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    }
}
