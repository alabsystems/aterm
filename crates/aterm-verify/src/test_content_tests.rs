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

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;

        for invalid_name in [false, true] {
            let malformed = OsString::from_wide(&[0xd800]);
            let command = if invalid_name {
                bed.command(&binary).env(malformed, "value")
            } else {
                bed.command(&binary).env("EXPECTED", malformed)
            };
            let command = bed.finalize(command);
            let evidence = EvidenceBuilder::new(
                Some("11".repeat(32)),
                Some("22".repeat(32)),
                bed.root.join("scratch"),
            )
            .evidence(&binary, &command);
            assert!(!evidence.bounded(), "malformed UTF-16 must not be replaced");
            assert!(
                evidence
                    .reason
                    .as_deref()
                    .unwrap()
                    .contains("not valid Unicode")
            );
        }
        let command = bed.finalize(bed.command(&binary).env("\u{e9}", "\u{503c}"));
        let evidence = EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            bed.root.join("scratch"),
        )
        .evidence(&binary, &command);
        assert!(
            evidence.bounded(),
            "valid Unicode stays representable: {evidence:?}"
        );
    }
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

/// One verifier invocation as a release makes it: its own random scratch root and run record.
#[cfg(unix)]
struct Invocation {
    scratch: PathBuf,
    run: String,
}

#[cfg(unix)]
impl Invocation {
    fn new(bed: &Bed, run: &str) -> Self {
        Self {
            scratch: bed.root.join(format!("scratch-{run}")),
            run: run.into(),
        }
    }

    /// `binary`'s finalized command in this invocation. Cargo's recording pass runs under the
    /// private home, and on macOS Cargo ends the runner's `DYLD_FALLBACK_LIBRARY_PATH` with the
    /// loader's defaults, `$HOME/lib` first, so the replayed record holds the scratch root there
    /// as well as in the capabilities. `edit` changes the replayed command before it is
    /// finalized.
    fn command(&self, bed: &Bed, binary: &Binary, edit: impl FnOnce(Cmd) -> Cmd) -> Cmd {
        let home = self.scratch.join("release-test-home");
        let libraries = std::env::join_paths([
            bed.root.join("target/debug/deps"),
            bed.root.join("toolchain/lib"),
            home.join("lib"),
            PathBuf::from("/usr/local/lib"),
            PathBuf::from("/usr/lib"),
        ])
        .unwrap();
        let command = Cmd::replayed(
            &binary.record.argv,
            &binary.record.cwd,
            binary.record.env.clone(),
        )
        .env("DYLD_FALLBACK_LIBRARY_PATH", libraries)
        .env("CARGO_BUILD_JOBS", "4")
        .env("RUST_TEST_THREADS", "2")
        .env("HOME", home)
        .env("TMPDIR", self.scratch.join("tmp"))
        .env(
            crate::testrun::SUBPROCESS_SLOTS_ENV,
            self.scratch.join("test-subprocs.noindex"),
        )
        .env(crate::runs::VERIFY_RUN_ENV, &self.run)
        .env(aterm_capacity::COVER_ENV, format!("cover-{}", self.run));
        bed.finalize(edit(command))
    }

    fn builder(&self) -> EvidenceBuilder {
        EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("22".repeat(32)),
            self.scratch.clone(),
        )
    }

    fn evidence(&self, bed: &Bed, binary: &Binary) -> Evidence {
        self.builder()
            .evidence(binary, &self.command(bed, binary, |command| command))
    }
}

/// What a build does to an output whose inputs did not change: the same bytes and mode in a
/// new file, so the inode, link count and times all differ.
#[cfg(unix)]
fn relink(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    let fresh = path.with_extension("relinked");
    std::fs::write(&fresh, std::fs::read(path).unwrap()).unwrap();
    std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(mode)).unwrap();
    std::fs::rename(&fresh, path).unwrap();
}

/// The names whose exact values differ between two finalized commands.
#[cfg(unix)]
fn environment_differences(first: &Cmd, second: &Cmd) -> Vec<String> {
    let (first, second) = (effective_env(first), effective_env(second));
    first
        .iter()
        .filter(|(name, value)| second.get(*name) != Some(value))
        .map(|(name, _)| String::from_utf8_lossy(name).into_owned())
        .collect()
}

/// The release's adjacent-hop carry: a binary whose executable and declared inputs a hop left
/// byte-equal has the key its previous run recorded, although that run was another verifier
/// invocation (another random scratch root, run record and capacity cover) and the build wrote
/// the executable and its helper again.
/// NEGATIVE CONTROLS: the two finalized environments differ in exactly the per-invocation
/// variables, the loader path among them, so the equal key is the normalization's and not an
/// environment that never held the scratch root; a package which declares that it reads its
/// home gets distinct keys naming that variable alone; and before `content_environment`
/// replaced the scratch root in the loader path and `file_identity` dropped the inode, link
/// count and times, this test's two keys differed.
#[cfg(unix)]
#[test]
fn one_binary_has_one_key_under_two_runs_of_an_unrelated_hop() {
    let bed = Bed::new(BOUNDED);
    std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
    std::fs::write(bed.package.join("fixtures/value.txt"), "one").unwrap();
    let binary = bed.binary();
    let before = Invocation::new(&bed, "101-1001");
    let first = before.evidence(&bed, &binary);
    assert!(first.bounded(), "{first:?}");

    std::fs::create_dir_all(bed.root.join("crates/q/src")).unwrap();
    std::fs::write(bed.root.join("crates/q/src/lib.rs"), "// another package\n").unwrap();
    std::fs::write(
        bed.package.join("src/lib.rs"),
        "pub fn answer() -> u8 { 1 } // a comment the executable does not hold\n",
    )
    .unwrap();
    relink(&bed.exe);
    relink(&bed.helper);
    let after = Invocation::new(&bed, "202-2002");
    let second = after.evidence(&bed, &binary);

    let mut per_run = vec![
        aterm_capacity::COVER_ENV,
        crate::testrun::SUBPROCESS_SLOTS_ENV,
        crate::runs::VERIFY_RUN_ENV,
        "DYLD_FALLBACK_LIBRARY_PATH",
        "HOME",
        "TMPDIR",
    ];
    per_run.sort_unstable();
    assert_eq!(
        environment_differences(
            &before.command(&bed, &binary, |command| command),
            &after.command(&bed, &binary, |command| command),
        ),
        per_run,
        "the two invocations differ in every per-run value and in nothing else"
    );
    assert_eq!(
        moved_parts(&first.parts, &second.parts),
        Vec::<String>::new()
    );
    assert_eq!(first.content_key, second.content_key);

    let first = before.builder().evidence(
        &binary,
        &before.command(&bed, &binary, |command| {
            command.env("EXPECTED", before.scratch.join("value"))
        }),
    );
    let second = after.builder().evidence(
        &binary,
        &after.command(&bed, &binary, |command| {
            command.env("EXPECTED", after.scratch.join("value"))
        }),
    );
    assert!(
        first.bounded() && second.bounded(),
        "{first:?} / {second:?}"
    );
    assert_eq!(
        moved_parts(&first.parts, &second.parts),
        ["env:EXPECTED"],
        "a custom variable remains exact even when it names the scratch directory"
    );
    assert_ne!(first.content_key, second.content_key);

    for capability in ["HOME", "DYLD_FALLBACK_LIBRARY_PATH"] {
        let reader = Bed::new(&format!(
            "[package.metadata.aterm.test-inputs]\nbounded = true\ncontent-only = true\n\
             read-only = true\npaths = []\nvolatile-environment = [\"{capability}\"]"
        ));
        let binary = reader.binary();
        let first = Invocation::new(&reader, "101-1001").evidence(&reader, &binary);
        let second = Invocation::new(&reader, "202-2002").evidence(&reader, &binary);
        assert!(
            first.bounded() && second.bounded(),
            "{first:?} / {second:?}"
        );
        assert_eq!(
            moved_parts(&first.parts, &second.parts),
            [format!("env:{capability}")],
            "a target which reads a capability gets its exact value"
        );
        assert_ne!(first.content_key, second.content_key);
    }
}

/// Every part of the key still decides it: each change below gives another key, and the parts
/// a refused carry would name are exactly the ones that change touched.
/// NEGATIVE CONTROL: the parts these changes move are every part of the key outside the
/// environment, so a part added to the key without a change here fails the last assertion.
#[cfg(unix)]
#[test]
fn every_part_of_the_key_still_moves_it() {
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    let bed = Bed::new(BOUNDED);
    std::fs::create_dir_all(bed.package.join("fixtures")).unwrap();
    let fixture = bed.package.join("fixtures/value.txt");
    std::fs::write(&fixture, "one").unwrap();
    let binary = bed.binary();
    let run = Invocation::new(&bed, "101-1001");
    let base = run.evidence(&bed, &binary);
    assert!(base.bounded(), "{base:?}");
    let mut proven = std::collections::BTreeSet::new();
    let mut moves = |what: &str, changed: Evidence, parts: &[&str]| {
        assert!(changed.bounded(), "{what}: {changed:?}");
        assert_ne!(base.content_key, changed.content_key, "{what}");
        assert_eq!(moved_parts(&base.parts, &changed.parts), parts, "{what}");
        proven.extend(parts.iter().map(|part| (*part).to_owned()));
    };
    let edited = |edit: &dyn Fn(Cmd) -> Cmd| {
        run.builder()
            .evidence(&binary, &run.command(&bed, &binary, edit))
    };

    std::fs::write(&fixture, "two").unwrap();
    moves(
        "a declared fixture",
        run.evidence(&bed, &binary),
        &["package-inputs"],
    );
    std::fs::write(&fixture, "one").unwrap();
    std::fs::write(bed.root.join("trust.lock"), "build = 2\n").unwrap();
    moves(
        "the toolchain lock",
        run.evidence(&bed, &binary),
        &["package-inputs"],
    );
    std::fs::write(bed.root.join("trust.lock"), "build = 1\n").unwrap();

    std::fs::write(&bed.exe, b"another test executable").unwrap();
    moves(
        "executable bytes",
        run.evidence(&bed, &binary),
        &["executable"],
    );
    std::fs::write(&bed.exe, b"test executable").unwrap();
    std::fs::set_permissions(&bed.exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    moves(
        "executable mode",
        run.evidence(&bed, &binary),
        &["executable-access"],
    );
    std::fs::set_permissions(&bed.exe, std::fs::Permissions::from_mode(0o755)).unwrap();

    std::fs::write(&bed.helper, b"another helper executable").unwrap();
    moves(
        "helper bytes",
        run.evidence(&bed, &binary),
        &["helper:CARGO_BIN_EXE_helper"],
    );
    std::fs::write(&bed.helper, b"helper executable").unwrap();
    std::fs::set_permissions(&bed.helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    moves(
        "helper mode",
        run.evidence(&bed, &binary),
        &["helper:CARGO_BIN_EXE_helper-access"],
    );
    std::fs::set_permissions(&bed.helper, std::fs::Permissions::from_mode(0o755)).unwrap();

    let command = run.command(&bed, &binary, |command| command);
    moves(
        "the seal's carry context",
        EvidenceBuilder::new(
            Some("33".repeat(32)),
            Some("22".repeat(32)),
            run.scratch.clone(),
        )
        .evidence(&binary, &command),
        &["carry-context"],
    );
    moves(
        "the verifier",
        EvidenceBuilder::new(
            Some("11".repeat(32)),
            Some("44".repeat(32)),
            run.scratch.clone(),
        )
        .evidence(&binary, &command),
        &["verifier"],
    );

    moves(
        "an argument",
        edited(&|mut command| {
            command.args.push("--exact".into());
            command
        }),
        &["argv"],
    );
    moves(
        "an ordinary variable",
        edited(&|command| command.env("ORDINARY", "value")),
        &["env:ORDINARY"],
    );
    moves(
        "a UTF-8 environment name",
        edited(&|command| command.env("\u{e9}", "value")),
        &["env:\u{e9}"],
    );
    for names in [
        vec![vec![b'X', 0xff]],
        vec![vec![b'X', 0xfe]],
        vec![vec![b'X', 0xff], vec![b'X', 0xfe]],
    ] {
        let unbounded = edited(&|mut command| {
            for name in &names {
                command = command.env(OsString::from_vec(name.clone()), "value");
            }
            command
        });
        assert!(
            !unbounded.bounded(),
            "non-UTF-8 names cannot share a lossy key"
        );
        assert!(
            unbounded
                .reason
                .as_deref()
                .unwrap()
                .contains("name is not UTF-8")
        );
    }
    moves(
        "the test thread ceiling",
        edited(&|command| command.env("RUST_TEST_THREADS", "19")),
        &["env:RUST_TEST_THREADS"],
    );
    moves(
        "the compile width",
        edited(&|command| command.env("CARGO_BUILD_JOBS", "2")),
        &["env:CARGO_BUILD_JOBS"],
    );
    moves(
        "a home outside the verifier's scratch",
        edited(&|command| command.env("HOME", bed.root.join("home"))),
        &["env:HOME"],
    );
    moves(
        "a library directory outside the verifier's scratch",
        edited(&|command| {
            command.env(
                "DYLD_FALLBACK_LIBRARY_PATH",
                std::env::join_paths([
                    bed.root.join("another-toolchain/lib"),
                    run.scratch.join("release-test-home/lib"),
                ])
                .unwrap(),
            )
        }),
        &["env:DYLD_FALLBACK_LIBRARY_PATH"],
    );
    moves(
        "another directory below the verifier's scratch",
        edited(&|command| command.env("TMPDIR", run.scratch.join("another-tmp"))),
        &["env:TMPDIR"],
    );

    moves(
        "another target of the package",
        run.evidence(&bed, &bed.binary_for("-p p --test other")),
        &["package-inputs", "spec"],
    );
    let mut elsewhere = binary.clone();
    elsewhere.record.cwd.clone_from(&bed.root);
    moves(
        "the working directory",
        run.evidence(&bed, &elsewhere),
        &["working-directory", "working-directory-resolved"],
    );
    let copy = bed.root.join("target/debug/deps/p-1111111111111111");
    std::fs::copy(&bed.exe, &copy).unwrap();
    let mut copied = binary.clone();
    copied.record.argv[0] = copy.into_os_string();
    moves(
        "the executable's path",
        run.evidence(&bed, &copied),
        &["argv", "executable-path"],
    );
    let helper = bed.root.join("target/debug/helper-copy");
    std::fs::copy(&bed.helper, &helper).unwrap();
    moves(
        "the helper's path",
        edited(&|command| command.env("CARGO_BIN_EXE_helper", &helper)),
        &[
            "env:CARGO_BIN_EXE_helper",
            "helper:CARGO_BIN_EXE_helper-path",
        ],
    );

    let outside_environment: std::collections::BTreeSet<String> = base
        .parts
        .keys()
        .filter(|part| !part.starts_with("env:"))
        .cloned()
        .collect();
    proven.retain(|part| !part.starts_with("env:"));
    assert_eq!(proven, outside_environment);
}

/// Only an element below the scratch root loses the root's name.
/// NEGATIVE CONTROLS: a sibling whose name merely starts with the root's, a relative element
/// and an element outside the root stay exact, and what follows the root stays in the value.
#[cfg(unix)]
#[test]
fn only_elements_below_the_scratch_root_are_normalized() {
    let normalized = |value: &str, scratch: &str| {
        String::from_utf8(normalized_scratch_list(
            value.as_bytes(),
            Path::new(scratch),
        ))
        .unwrap()
    };
    assert_eq!(
        normalized("/t/deps:/tmp/atv1/home/lib:/usr/lib", "/tmp/atv1"),
        "/t/deps:$VERIFIER_SCRATCH/home/lib:/usr/lib"
    );
    assert_eq!(
        normalized("/t/deps:/tmp/atv1/home/lib:/usr/lib", "/tmp/atv1"),
        normalized("/t/deps:/tmp/atv2/home/lib:/usr/lib", "/tmp/atv2"),
    );
    assert_ne!(
        normalized("/tmp/atv1/home/lib", "/tmp/atv1"),
        normalized("/tmp/atv1/home/bin", "/tmp/atv1"),
    );
    assert_eq!(normalized("/tmp/atv1", "/tmp/atv1"), "$VERIFIER_SCRATCH");
    for exact in [
        "/tmp/atv12/home/lib",
        "tmp/atv1/home",
        "/usr/lib:/opt/lib",
        "",
    ] {
        assert_eq!(normalized(exact, "/tmp/atv1"), exact);
    }
}
