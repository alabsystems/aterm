// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::testrun::{Binary, Record};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Bed {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
    package: PathBuf,
    exe: PathBuf,
    helper: PathBuf,
}

impl Bed {
    fn new(metadata: &str) -> Self {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join(format!(
            "workspace-{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let package = root.join("crates/p");
        std::fs::create_dir_all(package.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target/debug/deps")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/p\"]\n",
        )
        .unwrap();
        std::fs::write(root.join("trust.lock"), "build = 1\n").unwrap();
        std::fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname = \"p\"\nversion = \"0.0.0\"\n{metadata}\n"),
        )
        .unwrap();
        std::fs::write(package.join("src/lib.rs"), "pub fn answer() -> u8 { 1 }\n").unwrap();
        let exe = root.join("target/debug/deps/p-0000000000000000");
        let helper = root.join("target/debug/helper");
        std::fs::write(&exe, b"test executable").unwrap();
        std::fs::write(&helper, b"helper executable").unwrap();
        #[cfg(unix)]
        for executable in [&exe, &helper] {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            _dir: dir,
            root,
            package,
            exe,
            helper,
        }
    }

    fn binary(&self) -> Binary {
        self.binary_for("-p p --lib")
    }

    fn binary_for(&self, spec: &str) -> Binary {
        Binary {
            header: String::new(),
            spec: spec.into(),
            harness: true,
            record: Record {
                cwd: self.package.clone(),
                argv: vec![self.exe.clone().into_os_string(), "--skip".into()],
                env: vec![
                    ("CARGO_PKG_NAME".into(), "p".into()),
                    (
                        "CARGO_MANIFEST_DIR".into(),
                        self.package.clone().into_os_string(),
                    ),
                    (
                        "CARGO_BIN_EXE_helper".into(),
                        self.helper.clone().into_os_string(),
                    ),
                    ("PATH".into(), OsString::new()),
                    ("DUP".into(), "old".into()),
                    ("DUP".into(), "effective".into()),
                ],
            },
        }
    }

    fn command(&self, binary: &Binary) -> Cmd {
        self.finalize(Cmd::replayed(
            &binary.record.argv,
            &binary.record.cwd,
            binary.record.env.clone(),
        ))
    }

    fn finalize(&self, command: Cmd) -> Cmd {
        crate::exec::finalize_exact_command(
            command.counting_waits(),
            crate::exec::ExecEnv {
                events: None,
                cwd: &self.package,
                path: OsStr::new(""),
                scratch: &self.root,
                child_ceiling: None,
                remove_env: &[],
                add_env: &[],
                stage_groups: None,
                host: &crate::exec::LIVE_HOST,
                hang_samples: None,
            },
        )
        .unwrap()
    }

    fn evidence(&self, binary: &Binary) -> Evidence {
        EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            self.root.join("scratch"),
        )
        .evidence(binary, &self.command(binary))
    }
}

const BOUNDED: &str = "[package.metadata.aterm.test-inputs]\n\
    bounded = true\ncontent-only = true\nread-only = true\npaths = [\"fixtures\"]\nvolatile-environment = []";

#[test]
fn an_exact_replay_without_a_finalization_receipt_is_unbounded() {
    let bed = Bed::new(BOUNDED);
    std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
    let binary = bed.binary();
    let replayed = Cmd::replayed(
        &binary.record.argv,
        &binary.record.cwd,
        binary.record.env.clone(),
    );
    let evidence = EvidenceBuilder::new(
        Some("11".repeat(32)),
        Some("22".repeat(32)),
        bed.root.join("scratch"),
    )
    .evidence(&binary, &replayed);
    assert!(!evidence.bounded());
    assert!(
        evidence
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("finalized complete environment"))
    );
    assert!(bed.evidence(&binary).bounded());
}

#[test]
fn fixture_context_environment_and_helper_bytes_are_in_the_key() {
    let bed = Bed::new(BOUNDED);
    std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
    let fixture = bed.package.join("fixtures/value.txt");
    std::fs::write(&fixture, "one").unwrap();
    let binary = bed.binary();
    let first = bed.evidence(&binary);
    assert!(first.bounded(), "{first:?}");

    std::fs::write(
        bed.package.join("src/lib.rs"),
        "pub fn answer() -> u8 { 2 } // another target stayed byte-equal\n",
    )
    .unwrap();
    assert_eq!(
        first.content_key,
        bed.evidence(&binary).content_key,
        "compiled source is represented by executable bytes, not double-counted as runtime input"
    );

    std::fs::write(&fixture, "two").unwrap();
    let fixture_changed = bed.evidence(&binary);
    assert_ne!(first.content_key, fixture_changed.content_key);
    std::fs::write(&fixture, "one").unwrap();

    let context_changed = EvidenceBuilder::new(
        Some("33".repeat(32)),
        Some("22".repeat(32)),
        bed.root.join("scratch"),
    )
    .evidence(&binary, &bed.command(&binary));
    assert_ne!(first.content_key, context_changed.content_key);
    let verifier_changed = EvidenceBuilder::new(
        Some("11".repeat(32)),
        Some("44".repeat(32)),
        bed.root.join("scratch"),
    )
    .evidence(&binary, &bed.command(&binary));
    assert_ne!(first.content_key, verifier_changed.content_key);

    std::fs::write(bed.root.join("trust.lock"), "build = 2\n").unwrap();
    assert_ne!(first.content_key, bed.evidence(&binary).content_key);
    std::fs::write(bed.root.join("trust.lock"), "build = 1\n").unwrap();

    std::fs::write(&bed.exe, b"changed harness and test executable").unwrap();
    let harness_changed = bed.evidence(&binary);
    assert_ne!(first.exe_sha256, harness_changed.exe_sha256);
    assert_ne!(first.content_key, harness_changed.content_key);
    std::fs::write(&bed.exe, b"test executable").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let bytes_equal = bed.evidence(&binary);
        std::fs::set_permissions(&bed.exe, std::fs::Permissions::from_mode(0o644)).unwrap();
        let not_executable = bed.evidence(&binary);
        assert_eq!(bytes_equal.exe_sha256, not_executable.exe_sha256);
        assert_ne!(bytes_equal.content_key, not_executable.content_key);
        std::fs::set_permissions(&bed.exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut env_changed = binary.clone();
    env_changed.record.env.push(("DUP".into(), "new".into()));
    assert_ne!(first.content_key, bed.evidence(&env_changed).content_key);

    let mut scheduling = bed.command(&binary);
    scheduling = bed.finalize(scheduling.env("RUST_TEST_THREADS", "2").env(
        crate::testrun::SUBPROCESS_SLOTS_ENV,
        bed.root.join("scratch/slots.first"),
    ));
    let scheduled_first = EvidenceBuilder::new(
        Some("11".repeat(32)),
        Some("22".repeat(32)),
        bed.root.join("scratch"),
    )
    .evidence(&binary, &scheduling);
    scheduling = bed.finalize(scheduling.env("RUST_TEST_THREADS", "19").env(
        crate::testrun::SUBPROCESS_SLOTS_ENV,
        bed.root.join("scratch/slots.other"),
    ));
    assert_ne!(
        scheduled_first.content_key,
        EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            bed.root.join("scratch"),
        )
        .evidence(&binary, &scheduling)
        .content_key,
        "the exact scheduling ceiling is a run-time input"
    );

    let first_scratch = bed.root.join("scratch.first");
    let second_scratch = bed.root.join("scratch.second");
    let capability_command = |scratch: &Path, run: &str| {
        bed.finalize(
            bed.command(&binary)
                .env("RUST_TEST_THREADS", "2")
                .env("HOME", scratch.join("release-test-home"))
                .env(
                    crate::testrun::SUBPROCESS_SLOTS_ENV,
                    scratch.join("test-subprocs.random.noindex"),
                )
                .env(crate::runs::VERIFY_RUN_ENV, run)
                .env(aterm_capacity::COVER_ENV, format!("cover-{run}")),
        )
    };
    let first_invocation = EvidenceBuilder::new(
        Some("11".repeat(32)),
        Some("22".repeat(32)),
        first_scratch.clone(),
    )
    .evidence(&binary, &capability_command(&first_scratch, "101-1001"));
    let second_invocation = EvidenceBuilder::new(
        Some("11".repeat(32)),
        Some("22".repeat(32)),
        second_scratch.clone(),
    )
    .evidence(&binary, &capability_command(&second_scratch, "202-2002"));
    assert_eq!(
        first_invocation.content_key, second_invocation.content_key,
        "two verifier invocations carry when only capabilities the target declares unread change"
    );

    std::fs::write(&bed.helper, b"changed helper executable").unwrap();
    assert_ne!(first.content_key, bed.evidence(&binary).content_key);
}

/// Every path which selects execution or an input root is absolute, and the command hashes the
/// file it actually executes rather than trusting a parallel record field. A symlink alias to
/// that physical executable remains eligible, but its invocation spelling gets a distinct key.
/// NEGATIVE CONTROLS: a relative program/cwd/manifest/helper or a substituted program is
/// unbounded; removing any corresponding production check makes that arm bounded.
#[test]
fn ambiguous_execution_and_input_paths_are_unbounded() {
    let bed = Bed::new(BOUNDED);
    std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
    std::fs::write(bed.package.join("fixtures/value.txt"), "value").unwrap();
    let binary = bed.binary();
    let builder = EvidenceBuilder::new(
        Some("11".repeat(32)),
        Some("22".repeat(32)),
        bed.root.join("scratch"),
    );
    let reason = |command: Cmd| {
        let evidence = builder.evidence(&binary, &command);
        assert!(!evidence.bounded(), "{evidence:?}");
        evidence.reason.unwrap_or_default()
    };

    let mut relative_program = bed.command(&binary);
    relative_program.program = "target/debug/deps/p-0000000000000000".into();
    assert!(
        reason(relative_program).contains("paths must be absolute"),
        "a relative executable must fail closed"
    );

    let mut substituted_program = bed.command(&binary);
    substituted_program.program.clone_from(&bed.helper);
    assert!(
        reason(substituted_program).contains("not its recorded executable"),
        "bytes from a different command program must not be attributed to the record"
    );

    let mut no_cwd = bed.command(&binary);
    no_cwd.cwd = None;
    assert!(reason(no_cwd).contains("no exact working directory"));
    let mut relative_cwd = bed.command(&binary);
    relative_cwd.cwd = Some("crates/p".into());
    assert!(reason(relative_cwd).contains("working directory is not absolute"));
    let mut missing_cwd = bed.command(&binary);
    missing_cwd.cwd = Some(bed.root.join("missing-cwd"));
    assert!(reason(missing_cwd).contains("cannot be resolved"));
    let mut file_cwd = bed.command(&binary);
    file_cwd.cwd = Some(bed.helper.clone());
    assert!(reason(file_cwd).contains("is not a directory"));
    let direct = builder.evidence(&binary, &bed.command(&binary));
    let mut other_cwd = bed.command(&binary);
    other_cwd.cwd = Some(bed.root.clone());
    let other_cwd = bed.finalize(other_cwd);
    let other_cwd = builder.evidence(&binary, &other_cwd);
    assert!(other_cwd.bounded(), "{other_cwd:?}");
    assert_ne!(
        direct.content_key, other_cwd.content_key,
        "Cargo's non-package cwd is valid, but remains an execution input"
    );

    let relative_manifest =
        bed.finalize(bed.command(&binary).env("CARGO_MANIFEST_DIR", "crates/p"));
    assert!(reason(relative_manifest).contains("CARGO_MANIFEST_DIR is not absolute"));
    let relative_helper = bed.finalize(
        bed.command(&binary)
            .env("CARGO_BIN_EXE_helper", "target/debug/helper"),
    );
    assert!(reason(relative_helper).contains("names a non-absolute helper"));

    #[cfg(unix)]
    {
        let alias = bed.root.join("target/debug/deps/p-alias");
        std::os::unix::fs::symlink(&bed.exe, &alias).unwrap();
        let mut through_alias = bed.command(&binary);
        through_alias.program = alias;
        let through_alias = bed.finalize(through_alias);
        let through_alias = builder.evidence(&binary, &through_alias);
        assert!(direct.bounded() && through_alias.bounded());
        assert_eq!(direct.exe_sha256, through_alias.exe_sha256);
        assert_ne!(
            direct.content_key, through_alias.content_key,
            "the actual symlink invocation path remains in the command identity"
        );

        let cwd_alias = bed.root.join("cwd-alias");
        std::os::unix::fs::symlink(&bed.package, &cwd_alias).unwrap();
        let mut through_cwd_alias = bed.command(&binary);
        through_cwd_alias.cwd = Some(cwd_alias.clone());
        let through_cwd_alias = bed.finalize(through_cwd_alias);
        let first_cwd_target = builder.evidence(&binary, &through_cwd_alias);
        assert!(first_cwd_target.bounded(), "{first_cwd_target:?}");
        std::fs::remove_file(&cwd_alias).unwrap();
        std::os::unix::fs::symlink(&bed.root, &cwd_alias).unwrap();
        let second_cwd_target = builder.evidence(&binary, &through_cwd_alias);
        assert!(second_cwd_target.bounded(), "{second_cwd_target:?}");
        assert_ne!(
            first_cwd_target.content_key, second_cwd_target.content_key,
            "retargeting an unchanged cwd symlink must change the content key"
        );
    }
}

#[test]
fn undeclared_and_escaping_readers_are_unbounded() {
    let undeclared = Bed::new("");
    let evidence = undeclared.evidence(&undeclared.binary());
    assert!(!evidence.bounded());
    assert!(
        evidence
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("does not declare")
    );

    let undeclared_environment = Bed::new(
        "[package.metadata.aterm.test-inputs]\nbounded = true\ncontent-only = true\nread-only = true\npaths = []",
    );
    let evidence = undeclared_environment.evidence(&undeclared_environment.binary());
    assert!(!evidence.bounded());
    assert!(
        evidence
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("volatile environment read set")
    );

    let metadata_observer = Bed::new(
        "[package.metadata.aterm.test-inputs]\n\
         bounded = true\nread-only = true\npaths = []\nvolatile-environment = []",
    );
    let evidence = metadata_observer.evidence(&metadata_observer.binary());
    assert!(!evidence.bounded());
    assert!(evidence.reason.unwrap().contains("content-only = true"));

    let external_tool = Bed::new(
        "[package.metadata.aterm.test-inputs]\n\
         bounded = true\ncontent-only = true\nread-only = true\npaths = []\n\
         volatile-environment = []\ntools = [\"git\"]",
    );
    let evidence = external_tool.evidence(&external_tool.binary());
    assert!(!evidence.bounded());
    assert!(evidence.reason.unwrap().contains("unknown field \"tools\""));

    let writable = Bed::new(
        "[package.metadata.aterm.test-inputs]\n\
         bounded = true\ncontent-only = true\npaths = []\nvolatile-environment = []",
    );
    let evidence = writable.evidence(&writable.binary());
    assert!(!evidence.bounded());
    assert!(evidence.reason.unwrap().contains("read-only = true"));

    let misspelled_capability = Bed::new(
        "[package.metadata.aterm.test-inputs]\n\
         bounded = true\ncontent-only = true\nread-only = true\npaths = []\n\
         volatile-environment = [\"ATERM_VERFIY_RUN\"]",
    );
    let evidence = misspelled_capability.evidence(&misspelled_capability.binary());
    assert!(!evidence.bounded());
    assert!(
        evidence
            .reason
            .unwrap()
            .contains("unknown verifier capability")
    );

    let escaping = Bed::new(
        "[package.metadata.aterm.test-inputs]\nbounded = true\ncontent-only = true\nread-only = true\nvolatile-environment = []\nworkspace-paths = [\"../outside\"]",
    );
    let evidence = escaping.evidence(&escaping.binary());
    assert!(!evidence.bounded());
    assert!(evidence.reason.unwrap().contains("escapes"));

    #[cfg(unix)]
    {
        std::fs::create_dir_all(escaping.package.join("fixtures")).unwrap();
        std::fs::write(escaping.package.join("fixtures/value.txt"), "value").unwrap();
        std::os::unix::fs::symlink(
            escaping.package.join("fixtures"),
            escaping.package.join("linked"),
        )
        .unwrap();
        std::fs::write(
            escaping.package.join("Cargo.toml"),
            "[package]\nname = \"p\"\nversion = \"0.0.0\"\n\
             [package.metadata.aterm.test-inputs]\nbounded = true\ncontent-only = true\nread-only = true\nvolatile-environment = []\npaths = [\"linked/value.txt\"]\n",
        )
        .unwrap();
        let evidence = escaping.evidence(&escaping.binary());
        assert!(!evidence.bounded());
        assert!(evidence.reason.unwrap().contains("symlink"));
    }
}

#[test]
fn a_declared_verifier_capability_is_hashed_exactly() {
    let bed = Bed::new(
        "[package.metadata.aterm.test-inputs]\n\
         bounded = true\ncontent-only = true\nread-only = true\npaths = []\nvolatile-environment = [\"ATERM_VERIFY_RUN\"]",
    );
    let binary = bed.binary();
    let command =
        |run: &str| bed.finalize(bed.command(&binary).env(crate::runs::VERIFY_RUN_ENV, run));
    let builder = EvidenceBuilder::new(
        Some("11".repeat(32)),
        Some("22".repeat(32)),
        bed.root.join("scratch"),
    );
    assert_ne!(
        builder.evidence(&binary, &command("101-1001")).content_key,
        builder.evidence(&binary, &command("202-2002")).content_key,
        "a target which declares that it reads a capability gets its exact value"
    );
}

#[test]
fn a_target_fixture_invalidates_only_its_declared_reader() {
    let bed = Bed::new(
        "[package.metadata.aterm.test-inputs]\n\
         bounded = true\n\
         content-only = true\n\
         read-only = true\n\
         paths = []\n\
         volatile-environment = []\n\
         [package.metadata.aterm.test-inputs.targets.\"test:reader\"]\n\
         paths = [\"fixtures\"]\n",
    );
    std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
    let fixture = bed.package.join("fixtures/value.txt");
    std::fs::write(&fixture, "before").unwrap();
    let reader = bed.binary_for("-p p --test reader");
    let sibling = bed.binary_for("-p p --test sibling");
    let reader_before = bed.evidence(&reader);
    let sibling_before = bed.evidence(&sibling);
    assert!(reader_before.bounded() && sibling_before.bounded());

    std::fs::write(&fixture, "after").unwrap();
    assert_ne!(reader_before.content_key, bed.evidence(&reader).content_key);
    assert_eq!(
        sibling_before.content_key,
        bed.evidence(&sibling).content_key,
        "a sibling target did not declare or read this fixture"
    );
}

#[test]
fn proof_parser_refuses_fail_and_the_obsolete_sha_only_format() {
    let bed = Bed::new(
        "[package.metadata.aterm.test-inputs]\nbounded = true\ncontent-only = true\nread-only = true\npaths = []\nvolatile-environment = []",
    );
    let evidence = bed.evidence(&bed.binary());
    let mut proof = CarryProof::passed("-p p --lib", &evidence, 1.25);
    let mut passed = Passed::new();
    passed.insert(proof.spec.clone(), proof.clone());
    let encoded = encode_passed(&passed).unwrap();
    assert_eq!(read_passed(&encoded).unwrap(), passed);

    let legacy_without_json = encoded
        .lines()
        .map(|line| {
            let mut value: aterm_json::Value = aterm_json::from_str(line).unwrap();
            value.as_object_mut().unwrap().remove("json");
            aterm_json::to_string(&value).unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        read_passed(&legacy_without_json).is_err(),
        "a proof which cannot reconstruct the prior invocation fails closed"
    );

    proof.outcome = "FAIL".into();
    let fail = format!("{}\n", aterm_json::to_string(&proof).unwrap());
    assert!(read_passed(&fail).unwrap_err().contains("only a prior ok"));
    assert!(read_passed(&format!("{} -p p --lib\n", "aa".repeat(32))).is_err());
}
