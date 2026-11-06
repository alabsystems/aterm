// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native project authority, signed bytes and lease custody; no network or installation.

use std::path::{Path, PathBuf};

use atpkg::trust_lock::{self, TrustLock};
use atpkg_publish::project_toolchain::{self, Authority, Inputs};

use super::fixture::{self, COMMIT, FakeTools, git, put, put_exe, seed_mint};

struct World {
    _tmp: aterm_tempfile::TempDir,
    root: PathBuf,
    inputs: Inputs,
    tools: FakeTools,
    lock: TrustLock,
}

fn clean_compiler_environment(child: &mut aterm_uds::spawnfd::Command) {
    // Verify sets RUSTDOC for its recording pass; these children model a clean hop.
    for name in [
        "RUSTC",
        "CARGO_BUILD_RUSTC",
        "RUSTDOC",
        "CARGO_BUILD_RUSTDOC",
        "RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "TRUST_STAGE2_BIN",
    ] {
        child.env_remove(name);
    }
}

impl World {
    fn new() -> Self {
        let tmp = fixture::scratch("project-selected");
        let root = tmp.path().canonicalize().expect("root");
        let prefix = root.join("managed prefix");
        let payload = trust_lock::build_dir(&prefix, 7);
        fixture::toolchain(&payload);
        seed_mint(&payload, COMMIT);
        let receipt = atpkg::sha256_file(&payload.join("SEED-MINT")).expect("receipt");
        atpkg_publish::artifact::lineage::stamp_with(&payload, &payload.join("bin"), &|_| {
            Ok(fixture::version_text(COMMIT, "x86_64-unknown-linux-gnu"))
        })
        .expect("stamp signed lineage");
        put(&trust_lock::ready_marker(&prefix, 7), b"ok\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink("7", payload.parent().expect("program").join("current"))
            .expect("current");
        let lock = TrustLock {
            build: 7,
            tree_root: atpkg::tree::tree_root(&payload).expect("signed tree"),
            compiler_commit: COMMIT.into(),
            receipt: format!("SEED-MINT sha256:{receipt}"),
            stock_toolchain: "1.95.0".into(),
        };
        let repo = root.join("project with spaces");
        std::fs::create_dir_all(&repo).expect("repo");
        put(
            &repo.join("trust.lock"),
            atpkg_publish::adopt::lock_text(&lock)
                .expect("sealed lock")
                .as_bytes(),
        );
        let inputs = Inputs {
            prefix,
            repo,
            path: "/usr/bin:/bin".into(),
            cargo_home: root.join("cargo home"),
            overrides: vec![],
        };
        Self {
            _tmp: tmp,
            root,
            inputs,
            tools: FakeTools::new("x86_64-unknown-linux-gnu"),
            lock,
        }
    }

    fn select(
        &self,
        authority: Authority,
        explicit: Option<&Path>,
    ) -> atpkg_publish::error::Result<project_toolchain::Selected> {
        project_toolchain::select(
            &self.inputs,
            authority,
            explicit,
            &|build| {
                assert_eq!(build, 7);
                Ok(self.lock.tree_root.clone())
            },
            &self.tools,
        )
    }

    fn working(&self) -> Authority {
        Authority::working(&self.inputs.repo).expect("working lock")
    }

    fn commit_lock(&self) -> String {
        git(&self.inputs.repo, &["init", "-q", "--initial-branch=main"]);
        git(&self.inputs.repo, &["add", "trust.lock"]);
        git(&self.inputs.repo, &["commit", "-qm", "pinned"]);
        git(&self.inputs.repo, &["rev-parse", "HEAD"])
    }
}

#[test]
fn project_requires_lock_and_never_uses_a_parent_lock() {
    let w = World::new();
    let nested = w.inputs.repo.join("unlocked");
    std::fs::create_dir(&nested).expect("nested");
    assert!(Authority::working(&nested).is_err());
    put(&w.inputs.repo.join("trust.lock"), b"version = 999\n");
    assert!(Authority::working(&w.inputs.repo).is_err());
}

#[test]
fn signed_payload_and_project_commit_and_receipt_all_bind_before_selection() {
    let w = World::new();
    let mut lock = w.lock.clone();
    lock.compiler_commit = "a".repeat(40);
    assert!(
        w.select(
            Authority::Working {
                path: w.inputs.repo.join("trust.lock"),
                lock
            },
            None
        )
        .is_err()
    );
    let mut lock = w.lock.clone();
    lock.receipt = format!("SEED-MINT sha256:{}", "b".repeat(64));
    assert!(
        w.select(
            Authority::Working {
                path: w.inputs.repo.join("trust.lock"),
                lock
            },
            None
        )
        .is_err()
    );
    let mut lock = w.lock.clone();
    lock.tree_root = "c".repeat(64);
    assert!(
        w.select(
            Authority::Working {
                path: w.inputs.repo.join("trust.lock"),
                lock
            },
            None
        )
        .is_err()
    );
    let probes = w.tools.runs().len();
    put(
        &trust_lock::build_dir(&w.inputs.prefix, 7).join("bin/targo"),
        b"modified compiler",
    );
    assert!(w.select(w.working(), None).is_err());
    assert_eq!(
        w.tools.runs().len(),
        probes,
        "no executable may run for rejected signed bytes"
    );
}

#[test]
fn no_lock_selects_only_the_authenticated_managed_default() {
    let w = World::new();
    let authority = || Authority::Committed {
        repo: w.inputs.repo.clone(),
        source: "1".repeat(40),
        lock: None,
    };
    let selected = w.select(authority(), None).expect("managed default");
    assert_eq!(selected.build(), 7);
    assert_eq!(
        selected.payload(),
        trust_lock::build_dir(&w.inputs.prefix, 7)
    );
    let other = w.root.join("other compiler/bin");
    std::fs::create_dir_all(&other).expect("other");
    assert!(
        w.select(authority(), Some(&other)).is_err(),
        "deleting a lock cannot admit an older explicit SDK"
    );
}

#[test]
fn native_lease_holds_the_selected_payload_until_the_selection_drops() {
    // Exact ownership matters: no forked parallel fixture may inherit the selected lease.
    let w = World::new();
    let selected = w.select(w.working(), None).expect("selected");
    let subject = atpkg::lease::Subject::build("trust", 7).expect("subject");
    assert!(matches!(
        atpkg::lease::reclaim(&w.inputs.prefix, &subject),
        atpkg::lease::Reclaim::Keep(_)
    ));
    selected.recheck().expect("unchanged");
    drop(selected);
    // Other test children can inherit a live fd during fork. The owning-test contract
    // above is what is required; final-close behavior is tested by atpkg's exact-child suite.
}

#[test]
fn signature_authority_refusal_is_not_a_default_or_explicit_fallback() {
    let w = World::new();
    let error = project_toolchain::select(
        &w.inputs,
        w.working(),
        None,
        &|_| Err("withdrawn by signed head".into()),
        &w.tools,
    )
    .err()
    .expect("refused");
    assert!(error.to_string().contains("withdrawn"));
}

#[test]
fn compiler_overrides_and_mutating_selected_payload_are_refused() {
    let mut w = World::new();
    w.inputs
        .overrides
        .push(("ALAB_CARGO".into(), "other".into()));
    assert!(w.select(w.working(), None).is_err());
    w.inputs.overrides.clear();
    let selected = w.select(w.working(), None).expect("selected");
    put(
        &selected.payload().join("added-after-selection"),
        b"mutation",
    );
    assert!(selected.recheck().is_err());
}

#[test]
fn committed_lock_is_read_at_the_full_source_not_checkout_head() {
    let w = World::new();
    let repo = &w.inputs.repo;
    git(repo, &["init", "-q", "--initial-branch=main"]);
    git(repo, &["add", "trust.lock"]);
    git(repo, &["commit", "-qm", "pinned"]);
    let pinned = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["rm", "trust.lock"]);
    git(repo, &["commit", "-qm", "later unpinned"]);
    let later = git(repo, &["rev-parse", "HEAD"]);
    let Authority::Committed {
        repo: bound_repo,
        lock: Some(lock),
        source,
    } = Authority::committed(repo, &pinned).expect("historical lock")
    else {
        panic!("historical lock missing")
    };
    assert_eq!(source, pinned);
    assert_eq!(bound_repo, repo.canonicalize().expect("canonical source"));
    assert_eq!(lock, w.lock);
    assert!(matches!(
        Authority::committed(repo, &later).expect("source without lock"),
        Authority::Committed { lock: None, .. }
    ));
    put(
        &repo.join("trust.lock"),
        b"this working lock must not be read",
    );
    assert!(matches!(
        Authority::committed(repo, &pinned).expect("committed source"),
        Authority::Committed { lock: Some(_), .. }
    ));
}

#[test]
fn committed_source_requires_commit_and_regular_lock_blob() {
    let w = World::new();
    let repo = &w.inputs.repo;
    git(repo, &["init", "-q", "--initial-branch=main"]);
    git(repo, &["add", "trust.lock"]);
    git(repo, &["commit", "-qm", "pinned"]);
    for alias in ["HEAD", "1234567", &"A".repeat(40)] {
        assert!(Authority::committed(repo, alias).is_err(), "{alias}");
    }
    let tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
    assert!(
        Authority::committed(repo, &tree).is_err(),
        "tree OID is not source commit authority"
    );
    assert!(
        Authority::committed(repo, &"f".repeat(40)).is_err(),
        "missing source objects refuse"
    );
    git(repo, &["rm", "trust.lock"]);
    #[cfg(unix)]
    std::os::unix::fs::symlink("elsewhere", repo.join("trust.lock")).expect("symlink lock");
    git(repo, &["add", "trust.lock"]);
    git(repo, &["commit", "-qm", "symbolic lock"]);
    let symbolic = git(repo, &["rev-parse", "HEAD"]);
    assert!(Authority::committed(repo, &symbolic).is_err());
    assert!(Authority::working(repo).is_err());
    git(repo, &["rm", "trust.lock"]);
    put(&repo.join("trust.lock/nested"), b"not a lock blob");
    git(repo, &["add", "trust.lock"]);
    git(repo, &["commit", "-qm", "directory lock"]);
    assert!(Authority::committed(repo, &git(repo, &["rev-parse", "HEAD"])).is_err());
}

#[test]
fn replacement_refs_cannot_change_source_authority_and_fsmonitor_never_runs() {
    let w = World::new();
    let repo = &w.inputs.repo;
    git(repo, &["init", "-q", "--initial-branch=main"]);
    git(repo, &["add", "trust.lock"]);
    git(repo, &["commit", "-qm", "pinned"]);
    let original = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["rm", "trust.lock"]);
    git(repo, &["commit", "-qm", "unpinned"]);
    let replacement = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["replace", &original, &replacement]);
    assert!(
        matches!(Authority::committed(repo, &original).expect("raw original source"), Authority::Committed { lock: Some(lock), .. } if lock == w.lock)
    );
    git(repo, &["replace", "-d", &original]);
    let marker = w.root.join("must-not-exist");
    git(
        repo,
        &[
            "config",
            "core.fsmonitor",
            &format!("!touch {}", marker.display()),
        ],
    );
    assert!(Authority::committed(repo, &original).is_err());
    assert!(
        !marker.exists(),
        "source hooks/fsmonitor are never executed"
    );
}

#[test]
fn controller_preserves_child_status_and_rechecks_even_after_child_failure() {
    let w = World::new();
    let selected = w.select(w.working(), None).expect("selected");
    let error = project_toolchain::run_controller(
        &selected,
        &w.inputs,
        Path::new("/unused/native-front"),
        "/bin/sh".as_ref(),
        &["-c".into(), "exit 75".into()],
    )
    .expect_err("child refused");
    assert_eq!(error.exit_code(), 75);
    let error = project_toolchain::run_controller(
        &selected,
        &w.inputs,
        Path::new("/unused/native-front"),
        "/bin/sh".as_ref(),
        &[
            "-c".into(),
            "printf mutation > \"$ATPKG_PROJECT_PAYLOAD/changed-by-failed-child\"; exit 75".into(),
        ],
    )
    .expect_err("mutation on failed child");
    assert!(error.to_string().contains("payload changed"));
}

#[test]
fn working_lock_changes_refuse_before_controller_birth() {
    let w = World::new();
    let selected = w.select(w.working(), None).expect("selected");
    let marker = w.root.join("child-ran");
    let args = [
        "-c".into(),
        "printf ran > \"$1\"".into(),
        "controller".into(),
        marker.to_string_lossy().into_owned(),
    ];
    project_toolchain::run_controller(
        &selected,
        &w.inputs,
        Path::new("/unused/native-front"),
        "/bin/sh".as_ref(),
        &args,
    )
    .expect("positive child control");
    assert_eq!(std::fs::read(&marker).expect("marker"), b"ran");
    std::fs::remove_file(&marker).expect("remove marker");
    put(&w.inputs.repo.join("trust.lock"), b"changed before birth");
    assert!(
        project_toolchain::run_controller(
            &selected,
            &w.inputs,
            Path::new("/unused/native-front"),
            "/bin/sh".as_ref(),
            &args,
        )
        .is_err()
    );
    assert!(!marker.exists(), "changed authority must prevent birth");
}

#[test]
fn committed_controller_keeps_the_historical_pin_despite_working_tree_changes() {
    let w = World::new();
    let source = w.commit_lock();
    git(&w.inputs.repo, &["rm", "trust.lock"]);
    git(&w.inputs.repo, &["commit", "-qm", "later unlocked tree"]);
    put(&w.inputs.repo.join("trust.lock"), b"unrelated working lock");
    let authority = Authority::committed(&w.inputs.repo, &source).expect("historical source");
    let selected = w.select(authority, None).expect("selected historical pin");
    let marker = w.root.join("selected-targo");
    project_toolchain::run_controller(
        &selected,
        &w.inputs,
        Path::new("/unused/native-front"),
        "/bin/sh".as_ref(),
        &[
            "-c".into(),
            "test \"$ATPKG_PROJECT_TARGO\" = \"$1\" || exit 91; printf '%s' \"$ATPKG_PROJECT_TARGO\" > \"$2\"".into(),
            "controller".into(),
            selected.targo().to_string_lossy().into_owned(),
            marker.to_string_lossy().into_owned(),
        ],
    )
    .expect("committed authority ignores the later working lock");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("selected tool"),
        selected.targo().to_string_lossy()
    );
}

#[test]
fn committed_controller_cannot_move_its_authority_to_another_repository() {
    let mut w = World::new();
    let source = w.commit_lock();
    let selected = w
        .select(
            Authority::committed(&w.inputs.repo, &source).expect("source"),
            None,
        )
        .expect("selected");
    let other = w.root.join("another repository");
    git(
        &w.root,
        &[
            "clone",
            "-q",
            w.inputs.repo.to_str().expect("source path"),
            other.to_str().expect("clone path"),
        ],
    );
    assert_eq!(git(&other, &["rev-parse", "HEAD"]), source);
    w.inputs.repo = other;
    let marker = w.root.join("wrong-repository-child");
    let error = project_toolchain::run_controller(
        &selected,
        &w.inputs,
        Path::new("/unused/native-front"),
        "/bin/sh".as_ref(),
        &[
            "-c".into(),
            "touch \"$1\"".into(),
            "controller".into(),
            marker.to_string_lossy().into_owned(),
        ],
    )
    .expect_err("same object in another repository is not this authority");
    assert!(error.to_string().contains("repository differs"));
    assert!(!marker.exists());
}

#[test]
fn committed_authority_is_reread_after_failed_child_and_before_any_later_birth() {
    let w = World::new();
    let source = w.commit_lock();
    let selected = w
        .select(
            Authority::committed(&w.inputs.repo, &source).expect("source"),
            None,
        )
        .expect("selected");
    let object = w
        .inputs
        .repo
        .join(".git/objects")
        .join(&source[..2])
        .join(&source[2..]);
    assert!(
        object.is_file(),
        "the negative control removes a real source object"
    );
    let error = project_toolchain::run_controller(
        &selected,
        &w.inputs,
        Path::new("/unused/native-front"),
        "/bin/sh".as_ref(),
        &[
            "-c".into(),
            "rm \"$1\" || exit 92; exit 75".into(),
            "controller".into(),
            object.to_string_lossy().into_owned(),
        ],
    )
    .expect_err("failed child cannot hide a changed source authority");
    assert!(
        !object.exists(),
        "the child actually removed its source object"
    );
    assert_ne!(error.exit_code(), 75, "post-child source refusal wins");
    assert!(error.to_string().contains("source objects"), "{error}");
    let marker = w.root.join("missing-source-child");
    assert!(
        project_toolchain::run_controller(
            &selected,
            &w.inputs,
            Path::new("/unused/native-front"),
            "/bin/sh".as_ref(),
            &[
                "-c".into(),
                "touch \"$1\"".into(),
                "controller".into(),
                marker.to_string_lossy().into_owned(),
            ],
        )
        .is_err()
    );
    assert!(!marker.exists(), "missing source prevents another birth");
}

#[test]
fn actual_native_explicit_hop_never_reselects_by_source_or_pristine_cwd() {
    let w = World::new();
    let pristine = w.root.join("pristine with no lock");
    std::fs::create_dir(&pristine).expect("pristine");
    let exe = aterm_tempfile::bin_exe("atpkg-publish");
    let mut capability = aterm_uds::spawnfd::command(&exe);
    capability.args(["__build_admit", "--capabilities"]);
    let answer = aterm_uds::deadline::output_until_complete_stdout(
        &mut capability,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .expect("capability");
    assert!(answer.status.success());
    assert_eq!(answer.stdout, b"aterm-build-admit-hop-v2\n");
    for cwd in [&w.inputs.repo, &pristine] {
        // A shim hop would try to install the source's unavailable build7. This explicit
        // target executes pwd directly in both worlds, ignoring that cwd's lock discovery.
        let mut child = aterm_uds::spawnfd::command(&exe);
        child
            .args(["__build_admit", "/bin/pwd"])
            .current_dir(cwd)
            .env("CARGO_HOME", &w.inputs.cargo_home);
        clean_compiler_environment(&mut child);
        let out = aterm_uds::deadline::output_until_complete_stdout(
            &mut child,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("owned explicit hop");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8(out.stdout).expect("path"),
            format!("{}\n", cwd.display())
        );
    }
}

#[test]
#[cfg(unix)]
fn native_hop_checks_actual_config_and_arguments_before_compiler_birth() {
    let tmp = fixture::scratch("native-compiler-context");
    let root = tmp.path().canonicalize().expect("root");
    let cwd = root.join("source/nested");
    let home = root.join("private-cargo-home");
    std::fs::create_dir_all(&cwd).expect("nested source");
    std::fs::create_dir_all(&home).expect("cargo home");
    let target = root.join("bin/targo");
    let marker = root.join("compiler-was-born");
    put_exe(&target, b"#!/bin/sh\nprintf born > \"$HOP_MARKER\"\n");
    let exe = aterm_tempfile::bin_exe("atpkg-publish");
    let run = |args: &[&str], env: &[(&str, &str)], accepted: bool| {
        let mut child = aterm_uds::spawnfd::command(&exe);
        child
            .arg("__build_admit")
            .arg(&target)
            .args(args)
            .current_dir(&cwd)
            .env("CARGO_HOME", &home)
            .env("HOP_MARKER", &marker)
            .env("ALAB_CARGO", "/selected/internal-wrapper");
        clean_compiler_environment(&mut child);
        child.envs(env.iter().copied());
        let out = aterm_uds::deadline::output_until_complete_stdout(
            &mut child,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("bounded native hop");
        assert_eq!(
            out.status.success(),
            accepted,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            marker.exists(),
            accepted,
            "refusal must precede actual compiler birth"
        );
        if accepted {
            assert_eq!(std::fs::read(&marker).expect("real marker"), b"born");
            std::fs::remove_file(&marker).expect("clear marker");
        }
    };
    let config = cwd.join(".cargo/config.toml");
    put(&config, b"[build]\njobs = 2\n");
    run(&["--version"], &[], true);
    for key in [
        "rustc",
        "rustdoc",
        "rustc-wrapper",
        "rustc-workspace-wrapper",
    ] {
        put(
            &config,
            format!("build = {{ '{key}' = '/other/compiler' }}\n").as_bytes(),
        );
        run(&["--version"], &[], false);
    }
    std::fs::remove_file(&config).expect("remove nested override");
    let private_config = home.join("config.toml");
    put(&private_config, b"[build]\nrustdoc = '/other/rustdoc'\n");
    run(&["--version"], &[], false);
    std::fs::remove_file(&private_config).expect("remove home override");
    for name in [
        "RUSTC",
        "CARGO_BUILD_RUSTC",
        "RUSTDOC",
        "CARGO_BUILD_RUSTDOC",
        "RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    ] {
        run(&["--version"], &[(name, "/other/compiler")], false);
    }
    run(
        &["--config", "build.rustc='/other/compiler'", "--version"],
        &[],
        false,
    );
    run(
        &["--config=build.rustdoc='/other/rustdoc'", "--version"],
        &[],
        false,
    );
    let argument_config = cwd.join("argument.toml");
    put(
        &argument_config,
        b"[build]\nrustc-wrapper = '/other/wrapper'\n",
    );
    run(&["--config", "argument.toml", "--version"], &[], false);
    run(&["--config=build.jobs=2", "--version"], &[], true);
    for args in [
        &["-C", "another-directory", "--version"][..],
        &["-Canother-directory", "--version"][..],
        &["--directory=another-directory", "--version"][..],
    ] {
        run(args, &[], false);
    }
    run(
        &[
            "--version",
            "--",
            "--config",
            "build.rustc='/program/argument'",
        ],
        &[],
        true,
    );
    put(&config, b"build = [malformed\n");
    run(&["--version"], &[], false);
    put(&config, &vec![b' '; (1 << 20) + 1]);
    run(&["--version"], &[], false);
    put(&config, b"include = ['unread.toml']\n");
    run(&["--version"], &[], false);
    put(&cwd.join(".cargo/config"), b"[build]\njobs=1\n");
    run(&["--version"], &[], true);
    let mut capability = aterm_uds::spawnfd::command(&exe);
    capability
        .args(["__build_admit", "--capabilities"])
        .current_dir(&cwd)
        .env("RUSTC", "/unusable/compiler")
        .env("CARGO_HOME", &home);
    let out = aterm_uds::deadline::output_until_complete_stdout(
        &mut capability,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .expect("capability without compiler admission");
    assert!(out.status.success());
    assert_eq!(
        out.stdout,
        format!("{}\n", atpkg::build_admit::HOP_CAPABILITY).as_bytes()
    );
    assert!(!marker.exists());
}

#[test]
#[ignore = "exact child for isolated inherited Git context"]
fn source_read_child() {
    if std::env::var_os("ATPKG_PROJECT_TEST_SCOPE").is_some() {
        controller_scope_child();
        return;
    }
    let repo = std::env::var_os("ATPKG_PROJECT_TEST_SOURCE").expect("source");
    let source = std::env::var("ATPKG_PROJECT_TEST_COMMIT").expect("commit");
    assert!(matches!(
        Authority::committed(Path::new(&repo), &source).expect("admitted source"),
        Authority::Committed {
            lock: Some(TrustLock { build: 7, .. }),
            ..
        }
    ));
}

fn controller_scope_child() {
    let repo = PathBuf::from(std::env::var_os("ATPKG_PROJECT_TEST_SOURCE").expect("source"));
    let prefix = PathBuf::from(std::env::var_os("ATPKG_PROJECT_TEST_PREFIX").expect("prefix"));
    let scope = PathBuf::from(std::env::var_os("ATPKG_PROJECT_TEST_SCOPE").expect("scope"));
    let before = std::env::var("ATPKG_PROJECT_TEST_BEFORE").expect("phase") == "yes";
    let signal = std::env::var("ATPKG_PROJECT_TEST_SIGNAL")
        .expect("signal")
        .parse::<i32>()
        .expect("number");
    let inputs = Inputs {
        prefix,
        repo,
        path: "/usr/bin:/bin".into(),
        cargo_home: scope.join("cargo"),
        overrides: vec![],
    };
    let authority = Authority::working(&inputs.repo).expect("working authority");
    let lock = trust_lock::read(&inputs.repo.join("trust.lock")).expect("lock");
    let selected = project_toolchain::select(
        &inputs,
        authority,
        None,
        &|_| Ok(lock.tree_root.clone()),
        &FakeTools::new("x86_64-unknown-linux-gnu"),
    )
    .expect("leased fixture");
    let script = r#"
trap 'exit 130' INT
trap 'exit 131' QUIT
trap 'exit 143' TERM
(
  printf ready > "$scope/worker-ready"
  while [ ! -f "$scope/release-worker" ]; do /bin/sleep 0.02; done
  printf done > "$scope/worker-done"
) &
printf '%s\n' "$$" > "$scope/leader"
printf ready > "$scope/root-ready"
if [ "$before" = yes ]; then
  while [ ! -f "$scope/release-root" ]; do /bin/sleep 0.02; done
fi
exit 0
"#;
    let mut command = aterm_uds::spawnfd::command("/bin/bash");
    command
        .args(["-c", script])
        .env("scope", &scope)
        .env("before", if before { "yes" } else { "no" });
    let outcome = atpkg::build_admit::supervise_controller(&mut command, &mut |line| {
        if line.contains("root ended") {
            put(&scope.join("settling"), b"yes");
        }
        if line.contains("cancellation") {
            put(&scope.join("signal-observed"), b"yes");
        }
    })
    .expect("native group drained");
    assert!(scope.join("worker-done").is_file());
    assert_eq!(
        outcome.signal,
        if signal == 0 { None } else { Some(signal) }
    );
    assert_eq!(
        outcome.root.code(),
        Some(if before { 128 + signal } else { 0 })
    );
    selected
        .recheck()
        .expect("lease and bytes kept through descendant settlement");
}

#[test]
fn native_controller_keeps_sdk_lease_through_background_work_and_late_signals() {
    use std::time::{Duration, Instant};
    fn until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "native fixture reaches its required state"
            );
            // A bounded fixture wait for native lifecycle state, not a latency budget.
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            put(&self.0.join("release-worker"), b"yes");
            put(&self.0.join("release-root"), b"yes");
        }
    }
    let w = World::new();
    for (before, signal) in [
        (false, 0),
        (true, libc::SIGINT),
        (true, libc::SIGQUIT),
        (true, libc::SIGTERM),
        (false, libc::SIGINT),
        (false, libc::SIGQUIT),
        (false, libc::SIGTERM),
    ] {
        let scope = w.root.join(format!("scope-{before}-{signal}"));
        std::fs::create_dir(&scope).expect("scope");
        let stdout = scope.join("stdout");
        let stderr = scope.join("stderr");
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().expect("self"));
        command
            .args([
                "--exact",
                "artifact::project_toolchain::source_read_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ATPKG_PROJECT_TEST_SOURCE", &w.inputs.repo)
            .env("ATPKG_PROJECT_TEST_PREFIX", &w.inputs.prefix)
            .env("ATPKG_PROJECT_TEST_SCOPE", &scope)
            .env(
                "ATPKG_PROJECT_TEST_BEFORE",
                if before { "yes" } else { "no" },
            )
            .env("ATPKG_PROJECT_TEST_SIGNAL", signal.to_string())
            .stdout(aterm_uds::spawnfd::Stdio::from(
                std::fs::File::create(&stdout).expect("stdout"),
            ))
            .stderr(aterm_uds::spawnfd::Stdio::from(
                std::fs::File::create(&stderr).expect("stderr"),
            ));
        let mut child = command.spawn().expect("owned helper");
        let release = Release(scope.clone());
        until(|| scope.join("root-ready").is_file() && scope.join("worker-ready").is_file());
        if before {
            // SAFETY: this still-unreaped native handle pins this exact helper pid.
            assert!(child.owns_unreaped());
            assert_eq!(
                unsafe { libc::kill(i32::try_from(child.id()).expect("pid"), signal) },
                0
            );
        }
        until(|| scope.join("settling").is_file());
        assert!(
            child.try_wait().expect("owned status").is_none(),
            "root exit is not controller completion"
        );
        let subject = atpkg::lease::Subject::build("trust", 7).expect("subject");
        assert!(
            matches!(
                atpkg::lease::reclaim(&w.inputs.prefix, &subject),
                atpkg::lease::Reclaim::Keep(_)
            ),
            "SDK lease stays live after leader exit"
        );
        if !before && signal != 0 {
            // SAFETY: the outer helper is still owned and unreaped; the inner old root is never signalled.
            assert!(child.owns_unreaped());
            assert_eq!(
                unsafe { libc::kill(i32::try_from(child.id()).expect("pid"), signal) },
                0
            );
        }
        if signal != 0 {
            until(|| scope.join("signal-observed").is_file());
        }
        drop(release);
        let mut status = None;
        until(|| {
            status = child.try_wait().expect("helper wait");
            status.is_some()
        });
        assert!(
            status.expect("status").success(),
            "{}",
            std::fs::read_to_string(&stderr).expect("stderr")
        );
        assert!(
            std::fs::read_to_string(&stdout)
                .expect("stdout")
                .contains("1 passed; 0 failed")
        );
        assert!(scope.join("worker-done").is_file());
    }
}

#[test]
fn inherited_git_context_cannot_redirect_the_committed_lock_reader() {
    let w = World::new();
    let repo = &w.inputs.repo;
    git(repo, &["init", "-q", "--initial-branch=main"]);
    git(repo, &["add", "trust.lock"]);
    git(repo, &["commit", "-qm", "pinned"]);
    let source = git(repo, &["rev-parse", "HEAD"]);
    let other = w.root.join("unrelated repo");
    std::fs::create_dir(&other).expect("other");
    git(&other, &["init", "-q"]);
    put(&other.join("trust.lock"), b"not this source");
    git(&other, &["add", "trust.lock"]);
    git(&other, &["commit", "-qm", "other"]);
    let before = git(repo, &["status", "--porcelain"]);
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test executable"));
    child
        .args([
            "--exact",
            "artifact::project_toolchain::source_read_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("ATPKG_PROJECT_TEST_SOURCE", repo)
        .env("ATPKG_PROJECT_TEST_COMMIT", source)
        .env("GIT_DIR", other.join(".git"))
        .env("GIT_COMMON_DIR", other.join(".git"))
        .env("GIT_WORK_TREE", &other)
        .env("GIT_INDEX_FILE", other.join("wrong-index"))
        .env("GIT_OBJECT_DIRECTORY", other.join(".git/objects"))
        .env(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            other.join(".git/objects"),
        )
        .env("GIT_NAMESPACE", "wrong-world")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.worktree")
        .env("GIT_CONFIG_VALUE_0", &other);
    let out = aterm_uds::deadline::output_until_complete_stdout(
        &mut child,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .expect("isolated local reader");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed; 0 failed"),
        "one exact helper must execute"
    );
    assert_eq!(git(repo, &["status", "--porcelain"]), before);
    assert!(!other.join("wrong-index").exists());
}

#[test]
fn owned_aside_is_accepted_but_external_payload_indirection_is_refused() {
    let w = World::new();
    let logical = trust_lock::build_dir(&w.inputs.prefix, 7);
    let aside = logical.with_file_name("7.noindex");
    std::fs::rename(&logical, &aside).expect("owned aside");
    #[cfg(unix)]
    std::os::unix::fs::symlink("7.noindex", &logical).expect("owned aside alias");
    let selected = w.select(w.working(), None).expect("native aside contract");
    assert_eq!(selected.payload(), aside);
    drop(selected);
    std::fs::remove_file(&logical).expect("remove owned alias");
    let outside = w.root.join("outside owned store");
    std::fs::rename(&aside, &outside).expect("external tree");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &logical).expect("unowned link");
    assert!(w.select(w.working(), None).is_err());
}

#[test]
fn compat_execution_must_match_the_separately_authenticated_payload() {
    let mut w = World::new();
    let payload = trust_lock::build_dir(&w.inputs.prefix, 7);
    fixture::put_exe(&payload.join("bin/rustc"), b"other rustc bytes");
    fixture::put_exe(&payload.join("bin/targo-tippy"), b"fixture tippy");
    w.lock.tree_root = atpkg::tree::tree_root(&payload).expect("new signed fixture tree");
    put(
        &w.inputs.repo.join("trust.lock"),
        atpkg_publish::adopt::lock_text(&w.lock)
            .expect("lock")
            .as_bytes(),
    );
    assert!(
        w.select(w.working(), None).is_err(),
        "missing compat root is not repaired by selection"
    );
    let layout = atpkg::Layout {
        prefix: w.inputs.prefix.clone(),
    };
    atpkg::compat::ensure_root(&layout, &payload, atpkg::seam::Depth::Deep)
        .expect("fixture's owned exec root");
    let selected = w
        .select(w.working(), None)
        .expect("validated separate execution root");
    assert_ne!(selected.payload(), selected.execution());
    assert_eq!(selected.compiler().content.root, selected.execution());
    put(
        &selected.execution().join("bin/targo"),
        b"changed execution bytes",
    );
    assert!(selected.recheck().is_err());
    assert_eq!(
        atpkg::tree::tree_root(&payload).expect("payload unchanged"),
        w.lock.tree_root
    );
}
