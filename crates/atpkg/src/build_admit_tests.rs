// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A plain build's claim on a scratch ledger and an injected machine ([`Fixed`]): no test reads
//! this user's ledger, records or machine.

use super::*;
use aterm_capacity::reading::{Proc, Procs};
use aterm_capacity::{Fixed, Reading};

#[cfg(unix)]
#[path = "build_admit_native_tests.rs"]
mod native;

#[cfg(target_os = "macos")]
#[path = "build_admit_cohort_tests.rs"]
mod cohort;

#[cfg(target_os = "macos")]
#[path = "build_admit_headless_tests.rs"]
mod headless;

#[cfg(target_os = "macos")]
#[path = "build_admit_host_tests.rs"]
mod host_table;

#[cfg(target_os = "macos")]
#[path = "build_admit_service_tests.rs"]
mod host_service;

#[cfg(target_os = "macos")]
#[path = "build_admit_terminal_tests.rs"]
mod terminal;

const GIB: u64 = 1 << 30;

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

fn stage(args: &[&str], tool: Tool) -> Option<String> {
    classify(&words(args), tool).map(|b| b.stage)
}

/// Each compiling verb claims, in the lane it runs, by every spelling; the lint claims in any
/// lane and as its own binary; the Trust lane claims over a compiling verb. NEGATIVE CONTROL: a
/// run, an alias, metadata, clean, help, a version, a formatter, a bare `targo build` (which
/// targo refuses) and `trust` over no compiling verb never claim.
#[test]
fn the_classifier_claims_for_compiling_verbs_and_nothing_else() {
    for (args, tool, want) in [
        (&["--unverified", "build"][..], Tool::Targo, "targo build"),
        (&["--unverified", "b"], Tool::Targo, "targo build"),
        (
            &["--unverified", "check", "-p", "a"],
            Tool::Targo,
            "targo check",
        ),
        (&["--unverified", "c"], Tool::Targo, "targo check"),
        (&["--unverified", "test"], Tool::Targo, "targo test"),
        (
            &["--unverified", "t", "--no-run"],
            Tool::Targo,
            "targo test",
        ),
        (
            &["--unverified", "bench", "--no-run"],
            Tool::Targo,
            "targo bench",
        ),
        (&["--unverified", "doc"], Tool::Targo, "targo doc"),
        (&["--unverified", "d"], Tool::Targo, "targo doc"),
        (
            &["--unverified", "rustc", "--", "-Zx"],
            Tool::Targo,
            "targo rustc",
        ),
        (&["--unverified", "rustdoc"], Tool::Targo, "targo rustdoc"),
        (&["--unverified", "fix"], Tool::Targo, "targo fix"),
        (&["tippy", "-p", "a"], Tool::Targo, "targo tippy"),
        (&["clippy"], Tool::Targo, "targo tippy"),
        (
            &["-p", "a", "--", "-D", "warnings"],
            Tool::Tippy,
            "targo tippy",
        ),
        (
            &["trust", "check", "-p", "a"],
            Tool::Targo,
            "targo trust check",
        ),
        (
            &["--config", "x=1", "--unverified", "test"],
            Tool::Targo,
            "targo test",
        ),
        (
            &["-Z", "unstable-options", "--unverified", "build"],
            Tool::Targo,
            "targo build",
        ),
        (&["+stable", "build"], Tool::Cargo, "cargo build"),
        (&["test", "--workspace"], Tool::Cargo, "cargo test"),
    ] {
        assert_eq!(stage(args, tool).as_deref(), Some(want), "{args:?}");
    }
    for (args, tool) in [
        (&["--unverified", "run", "-p", "a"][..], Tool::Targo),
        (&["--unverified", "r"], Tool::Targo),
        (&["--unverified", "verify", "-p", "a"], Tool::Targo),
        (&["--unverified", "lab", "instance"], Tool::Targo),
        (&["--unverified", "ship", "dev-app"], Tool::Targo),
        (&["--unverified", "forge"], Tool::Targo),
        (&["metadata"], Tool::Targo),
        (&["--unverified", "clean"], Tool::Targo),
        (&["--unverified", "test", "--help"], Tool::Targo),
        (&["--version"], Tool::Targo),
        (&["-V"], Tool::Cargo),
        (&["fmt"], Tool::Targo),
        (&["build"], Tool::Targo),
        (&["trust", "run"], Tool::Targo),
        (&["run"], Tool::Cargo),
        (&[], Tool::Targo),
    ] {
        assert_eq!(stage(args, tool), None, "{args:?}");
    }
}

#[test]
fn package_verification_claims_but_archive_only_and_list_do_not() {
    let build = classify(
        &words(&["--unverified", "package", "-p", "leaf"]),
        Tool::Targo,
    )
    .unwrap();
    assert_eq!(build.stage, "targo package");
    assert_eq!(
        stage(&["package"], Tool::Cargo).as_deref(),
        Some("cargo package")
    );
    for args in [
        vec!["--unverified", "package", "--no-verify", "-p", "leaf"],
        vec!["--unverified", "package", "--list"],
        vec!["--unverified", "package", "--help"],
    ] {
        assert!(classify(&words(&args), Tool::Targo).is_none());
    }
}

/// A CALL THAT ONLY RESOLVES WHAT IT WOULD BUILD NEVER CLAIMS: verify's unit-graph probe, the test
/// compile's own command with `--unit-graph -Z unstable-options` (`aterm_verify::stages::UNIT_GRAPH`),
/// and a `--build-plan`, build nothing. NEGATIVE CONTROL: the same command without the flag claims
/// as `targo test`, and so it did with the flag read as any unknown one, by hand.
#[test]
fn a_call_that_only_resolves_what_it_would_build_never_claims() {
    let compile = [
        "--config",
        "build.build-dir=/b/.cargo-build",
        "--unverified",
        "test",
        "-p",
        "aterm-agent",
        "-p",
        "aterm-gui",
        "--no-fail-fast",
        "--tests",
        "--no-run",
        "--message-format=json-render-diagnostics",
    ];
    let mut probe = compile.to_vec();
    probe.extend(["--unit-graph", "-Z", "unstable-options"]);
    assert_eq!(stage(&probe, Tool::Targo), None);
    let mut plan = compile.to_vec();
    plan.extend(["-Z", "unstable-options", "--build-plan"]);
    assert_eq!(stage(&plan, Tool::Targo), None);
    assert_eq!(stage(&compile, Tool::Targo).as_deref(), Some("targo test"));
    assert_eq!(
        stage(
            &["--unverified", "test", "-p", "a", "--", "--unit-graph"],
            Tool::Targo
        )
        .as_deref(),
        Some("targo test"),
        "a word after `--` is the test binaries'"
    );
}

/// A build's selection is the packages it names, sorted and each once, or the workspace, or what
/// its manifest selects; its variant is what else it shapes, sorted, and what changes neither
/// (jobs, the target directory, a test filter, `--no-run`) is not read. NEGATIVE CONTROL: a build
/// that shapes nothing has no variant.
#[test]
fn a_builds_selection_and_variant_are_read_and_the_rest_is_not() {
    let b = classify(
        &words(&[
            "--unverified",
            "test",
            "-p",
            "b",
            "-pa",
            "--package=b",
            "--release",
            "--features",
            "x",
            "-j",
            "4",
            "--target-dir",
            "/t",
            "--no-run",
            "filter",
            "--",
            "-p",
            "z",
        ]),
        Tool::Targo,
    )
    .expect("claims");
    assert_eq!(
        b.selection,
        Selection::Named(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(b.variant.as_deref(), Some("features x, release"));
    let w = classify(
        &words(&["--unverified", "build", "--workspace"]),
        Tool::Targo,
    )
    .expect("claims");
    assert_eq!(w.selection, Selection::Workspace);
    assert_eq!(w.variant, None);
    let here = classify(&words(&["tippy"]), Tool::Targo).expect("claims");
    assert_eq!(here.selection, Selection::Here);
    let flipped = classify(
        &words(&[
            "--unverified",
            "test",
            "--features=x",
            "-r",
            "-pb",
            "-p",
            "a",
        ]),
        Tool::Targo,
    )
    .expect("claims");
    assert_eq!(flipped.variant, b.variant, "the order shapes nothing");
}

/// A build's analogue is verify's stage that does its work or more: verify's own test compile and
/// lint commands, and a test build or lint whose targets those stages build, read verify's
/// `TestCompile` and `Tippy`. NEGATIVE CONTROL: a profile, a target, features, or targets beyond
/// the stage's (examples, benches or doctests for the test compile), a build verify has no stage
/// of, and the Trust lane read none.
#[test]
fn a_builds_analogue_is_the_verify_stage_whose_work_holds_its_own() {
    let analogue =
        |args: &[&str], tool: Tool| classify(&words(args), tool).expect("claims").analogue;
    let compile = verify_test_compile();
    for (args, tool, want) in [
        (&compile[..], Tool::Targo, Some("TestCompile")),
        (
            &["--unverified", "test", "-p", "a"],
            Tool::Targo,
            Some("TestCompile"),
        ),
        (
            &["--unverified", "test", "--lib"],
            Tool::Targo,
            Some("TestCompile"),
        ),
        (
            &["--unverified", "test", "--bins", "--test", "t"],
            Tool::Targo,
            Some("TestCompile"),
        ),
        (
            &["--unverified", "test", "--bin", "b", "--tests"],
            Tool::Targo,
            Some("TestCompile"),
        ),
        (
            &[
                "-p",
                "a",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings",
            ],
            Tool::Tippy,
            Some("Tippy"),
        ),
        (
            &["tippy", "--tests", "--examples"],
            Tool::Targo,
            Some("Tippy"),
        ),
        (&["tippy"], Tool::Targo, Some("Tippy")),
        (
            &["--unverified", "test", "--release", "--tests"],
            Tool::Targo,
            None,
        ),
        (
            &["--unverified", "test", "--features", "x"],
            Tool::Targo,
            None,
        ),
        (
            &["--unverified", "test", "--target", "x86_64-apple-darwin"],
            Tool::Targo,
            None,
        ),
        (
            &["--unverified", "test", "--no-default-features"],
            Tool::Targo,
            None,
        ),
        (&["--unverified", "test", "--examples"], Tool::Targo, None),
        (&["--unverified", "test", "--benches"], Tool::Targo, None),
        (
            &["--unverified", "test", "--all-targets"],
            Tool::Targo,
            None,
        ),
        (&["--unverified", "test", "--doc"], Tool::Targo, None),
        (
            &["--unverified", "test", "--example", "e"],
            Tool::Targo,
            None,
        ),
        (&["tippy", "--profile", "release"], Tool::Targo, None),
        (&["--unverified", "build", "-p", "a"], Tool::Targo, None),
        (&["trust", "test", "-p", "a"], Tool::Targo, None),
        (&["test", "-p", "a"], Tool::Cargo, None),
    ] {
        assert_eq!(analogue(args, tool), want, "{args:?}");
    }
}

/// Verify's test compile of `-p aterm-gui`, as its stage runs it (`stages::test_compile_args`).
fn verify_test_compile() -> Vec<&'static str> {
    vec![
        "--unverified",
        "test",
        "-p",
        "aterm-gui",
        "--no-fail-fast",
        "--tests",
        "--no-run",
        "--message-format=json-render-diagnostics",
    ]
}

/// A scratch checkout: `.git`, a virtual workspace manifest at its root, and a member `m` that
/// declares package `member`.
fn checkout(dir: &Path) -> PathBuf {
    let root = dir.join("ck");
    std::fs::create_dir_all(root.join(".git")).expect("a checkout");
    std::fs::create_dir_all(root.join("crates/m/src")).expect("a member");
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/m\"]\n",
    )
    .expect("its manifest");
    std::fs::write(
        root.join("crates/m/Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\n\n[dependencies]\nname = \"not this\"\n",
    )
    .expect("the member's manifest");
    root
}

/// A build's key names its checkout (the directory holding `.git` above where it reads its
/// manifest), and, where it names no package, the package its nearest manifest declares; a
/// workspace build's manifest declares none. `-C` and `--manifest-path` move where it reads.
/// NEGATIVE CONTROL: a build at a virtual manifest selects no package of its own.
#[test]
fn a_builds_key_names_its_checkout_and_the_package_it_builds() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = checkout(dir.path());
    let ck = root.to_string_lossy().into_owned();
    let read = |args: &[&str], cwd: &Path| {
        let b = classify(&words(args), Tool::Targo).expect("claims");
        let k = key(&b, cwd);
        (k.checkout, k.crates)
    };
    let member = Some(vec!["member".to_owned()]);
    assert_eq!(
        read(&["--unverified", "test"], &root.join("crates/m/src")),
        (ck.clone(), member.clone())
    );
    assert_eq!(read(&["--unverified", "test"], &root), (ck.clone(), None));
    assert_eq!(
        read(&["-C", "crates/m", "--unverified", "test"], &root),
        (ck.clone(), member.clone())
    );
    assert_eq!(
        read(
            &[
                "--unverified",
                "test",
                "--manifest-path",
                "crates/m/Cargo.toml"
            ],
            &root
        ),
        (ck.clone(), member)
    );
    assert_eq!(
        read(&["--unverified", "test", "-p", "x"], &root.join("crates/m")),
        (ck.clone(), Some(vec!["x".to_owned()]))
    );
    std::fs::write(
        root.join("crates/m/Cargo.toml"),
        "[package] # inline comment\nname = \"commented\" # trailing comment\nversion = \"0.1.0\"\n",
    )
    .expect("commented manifest");
    assert_eq!(
        read(&["--unverified", "test"], &root.join("crates/m")),
        (ck.clone(), Some(vec!["commented".to_owned()]))
    );
    #[cfg(unix)]
    {
        let link = root.join("link-manifest.toml");
        std::os::unix::fs::symlink(root.join("crates/m/Cargo.toml"), &link).expect("symlink");
        assert_eq!(
            read(
                &[
                    "--unverified",
                    "test",
                    "--manifest-path",
                    "link-manifest.toml",
                ],
                &root
            ),
            (ck, None)
        );
    }
}

/// A test build with nothing of its own kept claims what verify's test compile of the same
/// selection in the same checkout took, a fifth more, and so does verify's own test compile run
/// by hand (`--tests --no-run`); a release build does not read verify's, which builds no release,
/// and claims the cold figure. NEGATIVE CONTROL: a build verify has no stage of claims the cold
/// figure too.
#[test]
fn a_test_build_reads_verifys_test_compile_and_a_release_build_does_not() {
    let k = |stage: &str, variant: Option<&str>| Key {
        stage: stage.to_owned(),
        checkout: "/ck".to_owned(),
        crates: Some(vec!["aterm-gui".to_owned()]),
        variant: variant.map(str::to_owned),
    };
    let mut verify = Records::default();
    verify.push(k("TestCompile", None), 15 * GIB, 1);
    let own = Records::default();
    let claims = |args: &[&str]| {
        let b = classify(&words(args), Tool::Targo).expect("claims");
        let at = Key {
            checkout: "/ck".to_owned(),
            ..key(&b, Path::new("/ck"))
        };
        need(&b, &at, &own, &verify, Some(48 * GIB)).bytes
    };
    assert_eq!(
        claims(&["--unverified", "test", "-p", "aterm-gui"]),
        18 * GIB
    );
    assert_eq!(
        claims(&verify_test_compile()),
        18 * GIB,
        "verify's own command"
    );
    assert_eq!(
        claims(&["--unverified", "test", "-p", "aterm-gui", "--lib"]),
        18 * GIB
    );
    let mut release = verify_test_compile();
    release.push("--release");
    assert_eq!(claims(&release), 16 * GIB, "the cold figure");
    let build = classify(
        &words(&["--unverified", "build", "-p", "aterm-gui"]),
        Tool::Targo,
    )
    .expect("claims");
    let none = need(
        &build,
        &k("targo build", None),
        &own,
        &verify,
        Some(48 * GIB),
    );
    assert_eq!(none.bytes, 16 * GIB);
}

/// The acceptance machine for the seats here: 24 GiB, 20 of it available.
fn machine() -> Fixed {
    Fixed::new(Reading::of_gib(24, 20, 14))
}

/// A seat on `dir`'s scratch ledger and records, on `machine`, asked by `pid`.
fn seat<'m>(dir: &Path, machine: &'m Fixed, pid: u32) -> Seat<'m> {
    Seat {
        ledger: Some(Ledger::at(dir.join("ledger"))),
        machine,
        own: Some(dir.join("targo-memory.json")),
        verify: Some(dir.join("stage-memory.json")),
        mode: Admit::Wait,
        pace: Pace::default(),
        pid,
        marker: None,
    }
}

fn proc(pid: u32, ppid: u32) -> Proc {
    Proc {
        pid,
        ppid,
        footprint: 0,
        written: 0,
        cpu_ns: 0,
    }
}

/// Hold `need` in `dir`'s scratch ledger, as this process.
fn hold(dir: &Path, m: &Fixed, need: Need) -> Claim {
    aterm_capacity::admit_with(
        &Ledger::at(dir.join("ledger")),
        m,
        need,
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("held")
}

fn test_build() -> Build {
    classify(&words(&["--unverified", "test", "-p", "a"]), Tool::Targo).expect("claims")
}

#[cfg(unix)]
fn cohort_seat<'m>(dir: &Path, machine: &'m Fixed) -> Seat<'m> {
    use std::os::unix::fs::DirBuilderExt as _;
    let home = dir.join("private-home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let namespace =
        aterm_capacity::cohort_namespace::Namespace::create_private(dir, &home).unwrap();
    let mut selected = seat(dir, machine, std::process::id());
    selected.ledger = Some(Ledger::at(namespace.fence()));
    selected
}

#[cfg(unix)]
#[test]
fn cohort_ledgers_refuse_holder_only_build_admission() {
    for mode in [Admit::Wait, Admit::Now] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let m = machine();
        let mut selected = cohort_seat(dir.path(), &m);
        selected.mode = mode;
        let mut said = Vec::new();
        let got = admit(&test_build(), dir.path(), &selected, &mut |line| {
            said.push(line.to_owned())
        });
        assert!(matches!(got, Admission::Refused(_)), "{got:?}");
        assert_eq!(said.len(), 1);
        assert!(said[0].contains("cohort"), "{said:?}");
        assert!(said[0].contains("no build started"), "{said:?}");
        assert!(selected.ledger.as_ref().unwrap().list().unwrap().is_empty());
    }
    // The ordinary holder route remains available with the same machine and build.
    let dir = aterm_tempfile::tempdir().unwrap();
    let m = machine();
    let ordinary = seat(dir.path(), &m, std::process::id());
    assert!(matches!(
        admit(&test_build(), dir.path(), &ordinary, &mut |_| {}),
        Admission::Claimed(_)
    ));
}

#[cfg(unix)]
#[test]
fn cohort_ledgers_refuse_before_covering_or_own_claim_fallback() {
    for covers in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let m = machine();
        let me = std::process::id();
        m.set_procs(Some(Procs::of(vec![proc(me, 1)])));
        let mut selected = cohort_seat(dir.path(), &m);
        let mut need = Need::new("existing cohort controller").mem(4 * GIB);
        if covers {
            need = need.covers();
        }
        let stage = aterm_capacity::admit_with(
            selected.ledger.as_ref().unwrap(),
            &m,
            need,
            Admit::Now,
            Pace::default(),
            &mut |_| {},
        )
        .unwrap();
        selected.marker = Some(stage.record().cover_marker());
        let got = admit(&test_build(), dir.path(), &selected, &mut |_| {});
        assert!(
            matches!(got, Admission::Refused(_)),
            "covers={covers}: {got:?}"
        );
        assert_eq!(selected.ledger.as_ref().unwrap().list().unwrap().len(), 1);
    }
}

#[cfg(unix)]
#[test]
fn cohort_ledgers_refuse_the_launch_port_without_unclaimed_fallback() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let m = machine();
    let selected = cohort_seat(dir.path(), &m);
    assert_eq!(
        run_if_claimed_at(
            &dir.path().join("must-not-be-launched"),
            &words(&["--unverified", "build"]),
            Tool::Targo,
            &[],
            dir.path(),
            &selected
        ),
        Some(std::process::ExitCode::from(75))
    );
    assert!(selected.ledger.as_ref().unwrap().list().unwrap().is_empty());
}

/// A build an ancestor's covering claim covers claims nothing, and no second record appears.
/// NEGATIVE CONTROL: under an ancestor's claim that does not cover (a VM's), the build claims for
/// itself, and the ledger holds both.
#[test]
fn a_build_under_a_covering_ancestor_claims_nothing() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let me = std::process::id();
    let build_pid = me.wrapping_add(11);
    m.set_procs(Some(Procs::of(vec![proc(build_pid, me), proc(me, 1)])));
    let stage = hold(
        dir.path(),
        &m,
        Need::new("verify test compile").mem(4 * GIB).covers(),
    );
    let s = seat(dir.path(), &m, build_pid);
    let ledger = Ledger::at(dir.path().join("ledger"));
    match admit_bounded(
        &test_build(),
        dir.path(),
        &s,
        &|| panic!("verified coverage must precede any ancestor file probe"),
        &mut |_| {},
    ) {
        Admission::Covered(r) => assert_eq!(r.pid, me),
        other => panic!("expected cover, got {other:?}"),
    }
    assert_eq!(ledger.list().expect("listed").len(), 1);
    drop(stage);
    let vm = hold(dir.path(), &m, Need::new("lab vm").mem(4 * GIB));
    let held = match admit(&test_build(), dir.path(), &s, &mut |_| {}) {
        Admission::Claimed(h) => h,
        other => panic!("expected a claim, got {other:?}"),
    };
    let listed = ledger.list().expect("listed");
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(
        held.claim().record().need.covers,
        "a build's claim covers its tree"
    );
    assert_eq!(held.claim().record().need.what, "targo test -p a");
    drop((held, vm));
}

/// A process that holds a claim of its own that does not cover builds unclaimed, saying so: a
/// second claim would wait on its own. NEGATIVE CONTROL: once it lets go, it claims.
#[test]
fn a_process_holding_its_own_claim_builds_unclaimed() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let me = std::process::id();
    let vm = hold(dir.path(), &m, Need::new("lab vm boot").mem(4 * GIB));
    let s = seat(dir.path(), &m, me);
    let mut said = Vec::new();
    let got = admit(&test_build(), dir.path(), &s, &mut |l| {
        said.push(l.to_owned())
    });
    assert!(matches!(got, Admission::Own(_)), "{got:?}");
    assert!(said.iter().any(|l| l.contains("lab vm boot")), "{said:?}");
    drop(vm);
    let got = admit(&test_build(), dir.path(), &s, &mut |_| {});
    assert!(matches!(got, Admission::Claimed(_)), "{got:?}");
}

/// An unreadable ledger refuses admission and the in-process launch port returns a failure,
/// rather than delegating an unclaimed execution to its caller. A repaired ledger can claim.
#[test]
fn an_unreadable_ledger_refuses_the_build_and_recovers_after_repair() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    std::fs::write(dir.path().join("ledger"), b"not a directory").expect("a file");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(3));
    let mut said = Vec::new();
    let got = admit(&test_build(), dir.path(), &s, &mut |l| {
        said.push(l.to_owned())
    });
    assert!(matches!(got, Admission::Refused(_)), "{got:?}");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("no build started"), "{said:?}");
    #[cfg(unix)]
    assert_eq!(
        run_if_claimed_at(
            Path::new("/bin/echo"),
            &words(&["--unverified", "build"]),
            Tool::Targo,
            &[],
            dir.path(),
            &s,
        ),
        Some(std::process::ExitCode::from(75)),
        "the caller must receive a refusal, never an unclaimed-launch fallback"
    );
    let mut missing = seat(dir.path(), &m, std::process::id().wrapping_add(3));
    missing.ledger = None;
    assert!(matches!(
        admit(&test_build(), dir.path(), &missing, &mut |_| {}),
        Admission::Refused(_)
    ));
    std::fs::remove_file(dir.path().join("ledger")).expect("remove the fixture's obstruction");
    // The refused ledger object retains its original namespace judgment. A new admission
    // observes the repaired path afresh rather than downgrading that old refusal in place.
    let repaired = seat(dir.path(), &m, std::process::id().wrapping_add(3));
    let got = admit(&test_build(), dir.path(), &repaired, &mut |_| {});
    assert!(matches!(got, Admission::Claimed(_)), "{got:?}");
    let listed = Ledger::at(dir.path().join("ledger"))
        .list()
        .expect("listed");
    assert_eq!(listed.len(), 1);
}

/// A build whose kept peak needs more than can ever be available here asks to run alone, and
/// is admitted alone. NEGATIVE CONTROL: one whose peak fits asks beside the others.
#[test]
fn a_build_beyond_what_can_be_available_asks_to_run_alone() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let pid = std::process::id().wrapping_add(5);
    let b = test_build();
    let k = key(&b, dir.path());
    Records::keep(
        &dir.path().join("targo-memory.json"),
        k.clone(),
        25 * GIB,
        1,
    )
    .expect("kept");
    let s = seat(dir.path(), &m, pid);
    let held = match admit(&b, dir.path(), &s, &mut |_| {}) {
        Admission::Claimed(h) => h,
        other => panic!("expected a claim, got {other:?}"),
    };
    assert_eq!(held.claim().record().need.mem, 30 * GIB);
    assert!(held.claim().alone().is_some(), "admitted alone");
    drop(held);
    let other = aterm_tempfile::tempdir().expect("scratch");
    let k2 = key(&b, other.path());
    Records::keep(&other.path().join("targo-memory.json"), k2, 2 * GIB, 1).expect("kept");
    let s = seat(other.path(), &m, pid);
    let held = match admit(&b, other.path(), &s, &mut |_| {}) {
        Admission::Claimed(h) => h,
        other => panic!("expected a claim, got {other:?}"),
    };
    assert_eq!(held.claim().record().need.mem, 3 * GIB);
    assert!(held.claim().alone().is_none());
}

/// A build that succeeded keeps its peak under its key, which the next build of it claims from.
/// NEGATIVE CONTROL: one that failed keeps nothing, and the next claims the cold figure.
#[test]
fn a_succeeded_build_keeps_its_peak_and_a_failed_one_keeps_none() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(9));
    let b = test_build();
    let claimed = |s: &Seat<'_>| match admit(&b, dir.path(), s, &mut |_| {}) {
        Admission::Claimed(h) => h,
        other => panic!("expected a claim, got {other:?}"),
    };
    let failed = claimed(&s);
    assert_eq!(failed.claim().record().need.mem, 12 * GIB, "cold");
    assert_eq!(failed.end(false, 5 * GIB, &mut |_| {}), None);
    let ok = claimed(&s);
    assert_eq!(ok.claim().record().need.mem, 12 * GIB, "still cold");
    assert_eq!(ok.end(true, 5 * GIB, &mut |_| {}), Some(5 * GIB));
    let next = claimed(&s);
    assert_eq!(next.claim().record().need.mem, 6 * GIB, "5 GiB and a fifth");
}

/// A failed build keeps its peak when it would claim more than its claim did, since its peak is
/// the least its work takes: under a 3 GiB claim, a 5 GiB failure raises the next to 6 GiB, and
/// under the cold 12 GiB a 14 GiB one raises it to 16.8. NEGATIVE CONTROL: a failure below what
/// was claimed leaves the record as it was.
#[test]
fn a_failed_build_keeps_a_peak_that_raises_its_claim() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(37));
    let b = test_build();
    let claimed = || match admit(&b, dir.path(), &s, &mut |_| {}) {
        Admission::Claimed(h) => h,
        other => panic!("expected a claim, got {other:?}"),
    };
    let cold = claimed();
    assert_eq!(cold.claim().record().need.mem, 12 * GIB, "cold");
    assert_eq!(cold.end(false, 14 * GIB, &mut |_| {}), Some(14 * GIB));
    let raised = claimed();
    assert_eq!(raised.claim().record().need.mem, 14 * GIB + 14 * GIB / 5);
    drop(raised);
    let other = aterm_tempfile::tempdir().expect("scratch");
    let s = seat(other.path(), &m, std::process::id().wrapping_add(37));
    let claimed = || match admit(&b, other.path(), &s, &mut |_| {}) {
        Admission::Claimed(h) => h,
        other => panic!("expected a claim, got {other:?}"),
    };
    let records = || Records::load(&other.path().join("targo-memory.json"));
    assert_eq!(claimed().end(true, 2 * GIB, &mut |_| {}), Some(2 * GIB));
    let red = claimed();
    assert_eq!(red.claim().record().need.mem, 3 * GIB);
    assert_eq!(red.end(false, 5 * GIB, &mut |_| {}), Some(5 * GIB));
    let next = claimed();
    assert_eq!(next.claim().record().need.mem, 6 * GIB);
    let before = records();
    assert_eq!(
        next.end(false, 4 * GIB, &mut |_| {}),
        None,
        "below its claim"
    );
    assert_eq!(records(), before);
}

/// AN ENDED BUILD'S CLAIM GOES BEFORE ITS PEAK IS KEPT: a book that cannot be written (its folder
/// is a file) says so while the claim is already gone, and `let_go` leaves the ledger empty before
/// its `keep` runs. NEGATIVE CONTROL (by hand: the claim let go after the book's write, as
/// before): the claim is still in the ledger when the failed write is said.
#[test]
fn an_ended_builds_claim_goes_before_its_peak_is_kept() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(41));
    let b = test_build();
    let claimed = || match admit(&b, dir.path(), &s, &mut |_| {}) {
        Admission::Claimed(h) => h,
        other => panic!("expected a claim, got {other:?}"),
    };
    let ledger = Ledger::at(dir.path().join("ledger"));
    let file = dir.path().join("a-file");
    std::fs::write(&file, "").expect("a file");
    let Held { claim, key, .. } = *claimed();
    let held = Held {
        claim,
        key,
        own: Some(file.join("targo-memory.json")),
        #[cfg(unix)]
        cohort: false,
    };
    let mut said = Vec::new();
    assert_eq!(
        held.end(true, 5 * GIB, &mut |line| {
            said.push((line.to_owned(), ledger.list().expect("listed").len()));
        }),
        None
    );
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].0.contains("was not kept"), "{said:?}");
    assert_eq!(said[0].1, 0, "the claim went before the write was tried");

    let ended = claimed().let_go();
    assert!(ledger.list().expect("listed").is_empty(), "let go");
    assert_eq!(ended.keep(true, 5 * GIB, &mut |_| {}), Some(5 * GIB));
}

/// A fresh build that compiled nothing keeps no peak while its key's heavy one is young, so
/// eight of them after a 10 GiB compile leave its next claim at 12 GiB, not a fresh run's 1 GiB
/// and a little. NEGATIVE CONTROL: the 3 GiB run, a quarter of the heavy one or more, is kept.
#[test]
fn a_fresh_builds_light_peak_never_lowers_its_claim() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(31));
    let b = test_build();
    let claimed = || match admit(&b, dir.path(), &s, &mut |_| {}) {
        Admission::Claimed(h) => h,
        other => panic!("expected a claim, got {other:?}"),
    };
    assert_eq!(claimed().end(true, 10 * GIB, &mut |_| {}), Some(10 * GIB));
    for _ in 0..8 {
        assert_eq!(claimed().end(true, 50 << 20, &mut |_| {}), None, "light");
    }
    assert_eq!(claimed().claim().record().need.mem, 12 * GIB);
    assert_eq!(claimed().end(true, 3 * GIB, &mut |_| {}), Some(3 * GIB));
}

/// A supervised build's status is its own and its peak (here the floor it is handed) is kept
/// only when it succeeded; its claim is gone once it ends. NEGATIVE CONTROL: a build that exits
/// 1 keeps nothing.
#[cfg(unix)]
#[test]
fn a_supervised_build_keeps_its_peak_only_when_it_succeeds() {
    use std::os::unix::process::ExitStatusExt as _;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(13));
    let b = test_build();
    let run = |script: &str| {
        let Admission::Claimed(held) = admit(&b, dir.path(), &s, &mut |_| {}) else {
            panic!("expected a claim");
        };
        let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh").into_command();
        cmd.args(["-c", script]);
        supervise(Some(*held), &mut cmd, &|| 3 * GIB, &mut |_| {}).expect("ran")
    };
    let records = || Records::load(&dir.path().join("targo-memory.json"));
    let failed = run("exit 1");
    assert_eq!(failed.code(), Some(1));
    assert!(records().rows().is_empty(), "a failed build keeps nothing");
    let ok = run("exit 0");
    assert!(ok.success());
    let rows = records();
    assert_eq!(rows.rows().len(), 1);
    assert_eq!(rows.rows()[0].peaks[0].bytes, 3 * GIB);
    assert!(
        Ledger::at(dir.path().join("ledger"))
            .list()
            .expect("listed")
            .is_empty(),
        "no claim outlives its build"
    );
    let killed = run("kill -TERM $$");
    assert_eq!(killed.signal(), Some(libc::SIGTERM));
}

/// The descriptor that holds `path` open in this process, found by what it opens (`fstat`'s
/// device and inode).
#[cfg(unix)]
fn descriptor_of(path: &Path) -> i32 {
    use std::os::unix::fs::MetadataExt as _;
    let want = std::fs::metadata(path).expect("the claim file");
    (0..4096)
        .find(|&fd| {
            let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: `fstat` writes one `stat` into the zeroed buffer, live across the call; an
            // unopened number answers EBADF and writes nothing.
            if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
                return false;
            }
            // SAFETY: zero-initialised and written whole by the call that succeeded.
            let st = unsafe { st.assume_init() };
            u64::try_from(st.st_dev).ok() == Some(want.dev()) && st.st_ino == want.ino()
        })
        .expect("this process holds the claim file")
}

/// The claim's descriptor stays home: a supervised build's child does not hold it, so no
/// descendant keeps the claim's lock past this process. NEGATIVE CONTROL: with its
/// close-on-exec flag cleared, the same child holds it.
#[cfg(unix)]
#[test]
fn a_supervised_builds_child_does_not_hold_the_claim() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(17));
    let b = test_build();
    let probe = |cleared: bool| {
        let Admission::Claimed(held) = admit(&b, dir.path(), &s, &mut |_| {}) else {
            panic!("expected a claim");
        };
        let fd = descriptor_of(held.claim().path());
        if cleared {
            // SAFETY: `fcntl` on a descriptor this process holds open, with plain integers.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
        }
        let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh").into_command();
        cmd.args(["-c", &aterm_uds::host::held_probe(fd)]);
        let status = supervise(Some(*held), &mut cmd, &|| 0, &mut |_| {}).expect("ran");
        status.code()
    };
    assert_eq!(probe(false), Some(0), "the child holds no claim");
    assert_eq!(probe(true), Some(3), "a cleared flag hands it on");
}

#[cfg(unix)]
const FORWARD_STANDIN: &str = "ATPKG_BUILD_ADMIT_FORWARD_STANDIN";

/// The build [`standin_front_supervises_a_cat`] runs, where it is not the cat itself.
#[cfg(unix)]
const FORWARD_BUILD: &str = "ATPKG_BUILD_ADMIT_FORWARD_BUILD";

/// A build that says its own pid and becomes `cat` on its stdin.
#[cfg(unix)]
const A_CAT: &str = "echo \"ATPKG_CHILD $$\"; exec cat";

/// The stand-in front of [`a_front_passes_every_signal_on_and_ends_by_it`]: claim in the scratch
/// ledger `FORWARD_STANDIN` names, and supervise a shell that says its pid and becomes `cat` on
/// this process's stdin ([`A_CAT`]), or the one `FORWARD_BUILD` names; end as it ended
/// ([`end_like`]). Run alone, it does nothing.
#[cfg(unix)]
#[test]
fn standin_front_supervises_a_cat() {
    let Some(dir) = std::env::var_os(FORWARD_STANDIN) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let m = machine();
    let s = seat(&dir, &m, std::process::id());
    let Admission::Claimed(held) = admit(&test_build(), &dir, &s, &mut |_| {}) else {
        panic!("expected a claim");
    };
    let build = std::env::var(FORWARD_BUILD).unwrap_or_else(|_| A_CAT.to_owned());
    let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh").into_command();
    cmd.args(["-c", &build]);
    let status = supervise(Some(*held), &mut cmd, &|| 0, &mut |_| {}).expect("ran");
    let _ = end_like(status);
    std::process::exit(status.code().unwrap_or(1));
}

/// A front sent any signal that ends a process with no core passes it on to its build and ends
/// by it, as the build did: a supervisor's SIGTERM, a `timeout`'s SIGALRM, a SIGUSR1, and a
/// SIGINT sent to a front with no terminal (an agent's shell, a supervisor's pipe), whose build
/// no terminal key reaches. NEGATIVE CONTROL (by hand: SIGUSR1 left out of `front::PASSED_ON`):
/// the front dies of it alone and its build lives on.
#[cfg(unix)]
#[test]
fn a_front_passes_every_signal_on_and_ends_by_it() {
    for sig in [
        libc::SIGTERM,
        libc::SIGHUP,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGALRM,
        libc::SIGPROF,
        libc::SIGVTALRM,
        libc::SIGINT,
    ] {
        a_front_passes_on(sig, A_CAT);
    }
}

/// A build a signal ended through its front leaves nothing running under the claim's end: a
/// shell killed by the SIGTERM a front passed on leaves the `cat` it started, reparented away,
/// and the front sends that the SIGTERM too and waits for it before the claim ends and the front
/// ends by it. NEGATIVE CONTROL (by hand: `end_tree` returning at once): the cat outlives the
/// front and its claim.
#[cfg(target_os = "macos")]
#[test]
fn a_front_ends_what_its_signalled_build_left_running() {
    a_front_passes_on(
        libc::SIGTERM,
        "exec 3<&0; cat <&3 & echo \"ATPKG_CHILD $!\"; wait",
    );
}

/// A build a signal ended that left a process deaf to that signal keeps its claim until the
/// tree is actually drained: with the escalation grace far away the front, its claim still
/// in the ledger, waits while the deaf `cat` lives and ends once the cat is killed; with a
/// short grace the front kills the cat itself before the claim ends, though the cat's stdin
/// (this test's pipe) is still open, so nothing else could have ended it. NEGATIVE CONTROL
/// (the source this test was written against first, where the claim ended once
/// [`super::TREE_ENDS_WITHIN`] passed, whatever still ran): the short-grace front never
/// killed the cat, and this test failed with the front not yet ended at its hang detector.
#[cfg(target_os = "macos")]
#[test]
fn a_front_holds_its_claim_until_a_deaf_leftover_is_drained() {
    use std::os::unix::process::ExitStatusExt as _;
    const DEAF: &str = "exec 3<&0; (trap '' TERM; exec cat <&3) & \
                        echo \"ATPKG_CHILD $!\"; echo \"ATPKG_ROOT $$\"; wait";
    let alive = |pid: i32| {
        // SAFETY: `kill` with signal 0 sends nothing: it asks whether `pid` still exists.
        let sent = unsafe { libc::kill(pid, 0) };
        sent == 0
    };
    let spawn_front = |grace_ms: &str| {
        use std::io::BufRead as _;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let turn = crate::platform::FORWARDING_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut front = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "build_admit::tests::standin_front_supervises_a_cat",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(FORWARD_STANDIN, dir.path())
            .env(FORWARD_BUILD, DEAF)
            .env(TREE_END_GRACE_ENV, grace_ms)
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the stand-in front");
        drop(turn);
        let out = front.stdout.take().expect("its stdout");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut lines = std::io::BufReader::new(out).lines();
            let (mut child, mut root) = (None, None);
            for line in lines.by_ref() {
                let Ok(line) = line else { break };
                for (mark, slot) in [("ATPKG_CHILD ", &mut child), ("ATPKG_ROOT ", &mut root)] {
                    if let Some(at) = line.find(mark) {
                        *slot = line[at + mark.len()..].trim().parse::<i32>().ok();
                    }
                }
                if let (Some(child), Some(root)) = (child, root) {
                    let _ = tx.send(Some((child, root)));
                    lines.for_each(drop);
                    return;
                }
            }
            let _ = tx.send(None);
        });
        let (child, root) = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("hang detector: the build never started")
            .expect("the build said its pids");
        (dir, front, child, root)
    };
    let claims = |dir: &Path| {
        Ledger::at(dir.join("ledger"))
            .list()
            .expect("the ledger listed")
            .len()
    };
    // Grace far away: custody is held on the live leftover, and release follows its drain.
    {
        let (dir, mut front, child, root) = spawn_front("600000");
        let front_pid = i32::try_from(front.id()).expect("a pid");
        // SAFETY: `kill` with two integers; the pid is this test's own unreaped child.
        assert_eq!(unsafe { libc::kill(front_pid, libc::SIGTERM) }, 0);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the build root reaped",
            || (!alive(root)).then_some(()),
        );
        assert!(alive(child), "the deaf cat outlived its root");
        assert!(
            matches!(front.try_wait(), Ok(None)),
            "the front ended while its tree still ran"
        );
        assert_eq!(claims(dir.path()), 1, "the claim is still held");
        // SAFETY: `kill` with two integers; `child` is this fixture's own process.
        assert_eq!(unsafe { libc::kill(child, libc::SIGKILL) }, 0);
        let status = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the front's end",
            || front.try_wait().ok().flatten(),
        );
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
        assert_eq!(claims(dir.path()), 0, "the claim went with the drain");
        drop(front.stdin.take());
    }
    // Short grace: the front itself ends the deaf leftover before its claim ends.
    {
        let (dir, mut front, child, _root) = spawn_front("200");
        let front_pid = i32::try_from(front.id()).expect("a pid");
        // SAFETY: `kill` with two integers; the pid is this test's own unreaped child.
        assert_eq!(unsafe { libc::kill(front_pid, libc::SIGTERM) }, 0);
        let status = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the front's end",
            || front.try_wait().ok().flatten(),
        );
        let cat_alive = alive(child);
        if cat_alive {
            // SAFETY: `kill` with two integers; `child` is this fixture's own process.
            unsafe { libc::kill(child, libc::SIGKILL) };
        }
        drop(front.stdin.take());
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
        assert!(!cat_alive, "the deaf cat outlived the front and its claim");
        assert_eq!(claims(dir.path()), 0, "the claim went with the drain");
    }
}

/// [`a_front_passes_every_signal_on_and_ends_by_it`] for `sig`, sent to a stand-in front in a
/// session of its own (no controlling terminal) running `build`, which says the pid of a `cat`
/// on the front's stdin: the front died of `sig` and the cat is gone.
#[cfg(unix)]
fn a_front_passes_on(sig: libc::c_int, build: &str) {
    use std::io::BufRead as _;
    use std::os::unix::process::ExitStatusExt as _;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    // Spawned in this process's forwarding turn: a lease test of this binary that stands aside
    // ignores SIGINT meanwhile, and a front that inherits an ignored signal keeps it ignored.
    let turn = crate::platform::FORWARDING_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut front = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "build_admit::tests::standin_front_supervises_a_cat",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(FORWARD_STANDIN, dir.path())
        .env(FORWARD_BUILD, build)
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in front");
    drop(turn);
    let out = front.stdout.take().expect("its stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = std::io::BufReader::new(out).lines();
        let pid = lines.find_map(|l| {
            let l = l.ok()?;
            let at = l.find("ATPKG_CHILD ")?;
            l[at + "ATPKG_CHILD ".len()..].trim().parse::<i32>().ok()
        });
        let _ = tx.send(pid);
        lines.for_each(drop);
    });
    let child = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("hang detector: the build never started")
        .expect("the build said its pid");
    let front_pid = i32::try_from(front.id()).expect("a pid");
    // SAFETY: `kill` with two integers; the pid is this test's own unreaped child.
    assert_eq!(unsafe { libc::kill(front_pid, sig) }, 0);
    let status =
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the front's end", || {
            front.try_wait().ok().flatten()
        });
    // SAFETY: `kill` with signal 0 sends nothing: it asks whether `child` still exists.
    let alive = unsafe { libc::kill(child, 0) } == 0;
    drop(front.stdin.take());
    assert_eq!(status.signal(), Some(sig), "{sig}: {status:?}");
    assert!(!alive, "the cat the build named outlived signal {sig}");
}

#[cfg(unix)]
const KEY_STANDIN: &str = "ATPKG_BUILD_ADMIT_KEY_STANDIN";

/// The stand-in of [`a_key_passes_on_only_where_the_terminals_key_misses_the_build`]: in a
/// session of its own, take its stdin (a terminal) as its controlling terminal, whose foreground
/// job it then leads, and say whether a key's signal passes on there, then with the terminal's
/// foreground handed to another group. Run alone, it does nothing.
#[cfg(unix)]
#[test]
fn standin_reads_key_passes_on_its_terminal() {
    if std::env::var_os(KEY_STANDIN).is_none() {
        return;
    }
    // SAFETY: `ioctl` with `TIOCSCTTY` and an integer argument on this process's stdin; it
    // leads its own session (the test spawned it with `setsid`), so stdin becomes its terminal.
    assert_eq!(
        unsafe { libc::ioctl(0, libc::TIOCSCTTY.into(), 0) },
        0,
        "a terminal"
    );
    let leading = front::key_passes(0);
    let mut other = aterm_uds::spawnfd::command("/bin/cat")
        .process_group(0)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("a group of its own");
    let other_group = libc::pid_t::try_from(other.id()).expect("a pid");
    // SAFETY: `signal` ignoring SIGTTOU, so this process may hand on its terminal's foreground;
    // `tcsetpgrp` with two integers, to a group of this process's own session.
    let handed = unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::tcsetpgrp(0, other_group)
    };
    assert_eq!(handed, 0, "the foreground handed on");
    let elsewhere = front::key_passes(0);
    drop(other.stdin.take());
    other.wait().expect("the other group's end");
    println!("KEY_PASSES leading={leading} elsewhere={elsewhere}");
}

/// A key's signal passes on only where the terminal's key would not reach the build itself:
/// with no terminal, a descriptor that is none (a pipe), or a terminal whose foreground job is
/// another group's; a front leading its terminal's foreground job does not pass it on, since
/// the key reached its whole job, the build with it. NEGATIVE CONTROL (by hand: `key_passes`
/// answering `true` for any terminal): the leading front reads `leading=true`.
#[cfg(unix)]
#[test]
fn a_key_passes_on_only_where_the_terminals_key_misses_the_build() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    assert!(front::key_passes(-1));
    let (read, _write) = std::io::pipe().expect("a pipe");
    assert!(front::key_passes(read.as_raw_fd()));
    let (master, slave) = aterm_pty::open_pty_pair().expect("a terminal");
    // SAFETY: both descriptors were just opened and are owned by nothing else.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "build_admit::tests::standin_reads_key_passes_on_its_terminal",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(KEY_STANDIN, "1")
        .setsid(true)
        .stdin(slave)
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("the stand-in");
    drop(master);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("KEY_PASSES leading=false elsewhere=true"),
        "{said}"
    );
}

/// A supervised build is handed its claim's cover marker, so a build it starts is covered by
/// that claim alone (`aterm_capacity::covering`), whatever marker its front inherited. NEGATIVE
/// CONTROL (by hand: `supervise` handing on no marker): the build reads what the test set.
#[cfg(unix)]
#[test]
fn a_supervised_build_is_handed_its_claims_cover_marker() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(37));
    let Admission::Claimed(held) = admit(&test_build(), dir.path(), &s, &mut |_| {}) else {
        panic!("expected a claim");
    };
    let marker = held.claim().record().cover_marker();
    let out = dir.path().join("cover");
    let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh").into_command();
    cmd.args([
        "-c",
        &format!("printf %s \"$ATERM_CAPACITY_COVER\" > '{}'", out.display()),
    ])
    .env(aterm_capacity::COVER_ENV, aterm_capacity::NO_COVER);
    let status = supervise(Some(*held), &mut cmd, &|| 0, &mut |_| {}).expect("ran");
    assert!(status.success());
    assert_eq!(std::fs::read_to_string(&out).expect("it said"), marker);
}

#[cfg(unix)]
const TOUCH_STANDIN: &str = "ATPKG_BUILD_ADMIT_TOUCH_STANDIN";

/// The stand-ins of [`a_fronts_floor_is_its_reaped_childs_resident_peak`]: with `TOUCH_STANDIN`
/// set to `touch`, touch 256 MiB and end; set to `front`, run one of those as this process's one
/// child, reap it, and say [`children_maxrss`]. Run alone, it does nothing.
#[cfg(unix)]
#[test]
fn standin_touches_or_reads_its_child() {
    match std::env::var(TOUCH_STANDIN).as_deref() {
        Ok("touch") => {
            let held = vec![1u8; 256 << 20];
            assert_eq!(held.iter().map(|&b| u64::from(b)).sum::<u64>(), 256 << 20);
        }
        Ok("front") => {
            let status = touching_child("touch")
                .stdout(aterm_uds::spawnfd::Stdio::null())
                .status()
                .expect("the child");
            assert!(status.success());
            println!("CHILDREN_MAXRSS {}", children_maxrss());
        }
        _ => {}
    }
}

/// This test binary as [`standin_touches_or_reads_its_child`] in `role`.
#[cfg(unix)]
fn touching_child(role: &str) -> aterm_uds::spawnfd::Command {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args([
        "--exact",
        "build_admit::tests::standin_touches_or_reads_its_child",
        "--test-threads=1",
        "--nocapture",
    ])
    .env(TOUCH_STANDIN, role)
    .stderr(aterm_uds::spawnfd::Stdio::null());
    cmd
}

/// A front's floor is its reaped child's resident peak, bytes: a child that touched 256 MiB
/// reads at least that and less than 2 GiB, so a wrong unit (KiB read as bytes, bytes as KiB) or
/// a misread layout fails. NEGATIVE CONTROL (by hand: the KiB scaling applied on macOS): 256 GiB
/// and more.
#[cfg(unix)]
#[test]
fn a_fronts_floor_is_its_reaped_childs_resident_peak() {
    let out = touching_child("front")
        .output()
        .expect("the stand-in front");
    let said = String::from_utf8_lossy(&out.stdout);
    let rss: u64 = said
        .lines()
        .find_map(|l| {
            l[l.find("CHILDREN_MAXRSS ")? + "CHILDREN_MAXRSS ".len()..]
                .parse()
                .ok()
        })
        .expect("it said");
    assert!(rss >= 256 << 20, "{} MiB", rss >> 20);
    assert!(rss < 2 << 30, "{} MiB", rss >> 20);
}

const MODE_STANDIN: &str = "ATPKG_BUILD_ADMIT_MODE_STANDIN";

/// The stand-in of [`a_scratch_ledger_admits_at_once`]: say how [`Seat::live`] admits, in the
/// environment the test hands it. Run alone, it does nothing.
#[test]
fn standin_says_how_the_live_seat_admits() {
    if std::env::var_os(MODE_STANDIN).is_some() {
        println!("ADMITS {:?}", Seat::live().mode);
    }
}

/// How [`Seat::live`] admits in a stand-in run with `ledger` as [`LEDGER_ENV`] and `admit` as
/// [`ADMIT_ENV`] (each unset where `None`).
fn live_admission(ledger: Option<&Path>, admit: Option<&str>) -> String {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args([
        "--exact",
        "build_admit::tests::standin_says_how_the_live_seat_admits",
        "--test-threads=1",
        "--nocapture",
    ])
    .env(MODE_STANDIN, "1")
    .env_remove(LEDGER_ENV)
    .env_remove(ADMIT_ENV);
    if let Some(ledger) = ledger {
        cmd.env(LEDGER_ENV, ledger);
    }
    if let Some(admit) = admit {
        cmd.env(ADMIT_ENV, admit);
    }
    let out = cmd.output().expect("the stand-in");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| Some(l[l.find("ADMITS ")? + "ADMITS ".len()..].to_owned()))
        .expect("it said")
}

/// A debug front on a scratch ledger (a test's) admits at once, so a test's build never waits
/// on this machine's memory, as a critical pressure would hold it on an empty ledger; the
/// variable still rules where it is set. NEGATIVE CONTROL: with no scratch ledger, the live seat
/// waits.
#[test]
fn a_scratch_ledger_admits_at_once() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ledger = dir.path().join("ledger");
    assert_eq!(live_admission(Some(&ledger), None), "Now");
    assert_eq!(live_admission(Some(&ledger), Some("wait")), "Wait");
    assert_eq!(live_admission(None, None), "Wait");
    assert_eq!(live_admission(None, Some("now")), "Now");
}

#[cfg(unix)]
const END_STANDIN: &str = "ATPKG_BUILD_ADMIT_END_STANDIN";

/// The stand-in front of [`a_front_ends_by_the_signal_its_build_died_of`]: claim in the scratch
/// ledger `END_STANDIN` names (`<dir>:<signal>`), supervise a shell that sends itself that signal,
/// and end as the front does ([`raise_like`]), else by the exit code it would return. Run alone,
/// it does nothing.
#[cfg(unix)]
#[test]
fn standin_front_ends_as_its_build() {
    use std::os::unix::process::ExitStatusExt as _;
    let Some(spec) = std::env::var_os(END_STANDIN) else {
        return;
    };
    let spec = spec.to_string_lossy().into_owned();
    let (dir, sig) = spec.rsplit_once(':').expect("<dir>:<signal>");
    let dir = PathBuf::from(dir);
    let m = machine();
    let s = seat(&dir, &m, std::process::id());
    let Admission::Claimed(held) = admit(&test_build(), &dir, &s, &mut |_| {}) else {
        panic!("expected a claim");
    };
    let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh").into_command();
    cmd.args(["-c", &format!("kill -{sig} $$")]);
    let status = supervise(Some(*held), &mut cmd, &|| 0, &mut |_| {}).expect("ran");
    raise_like(&status);
    std::process::exit(
        status
            .code()
            .or_else(|| status.signal().map(|n| 128 + n))
            .unwrap_or(1),
    );
}

/// A front ends by the signal that ended its build, where that signal dumps no core: a SIGKILL
/// (an OOM kill, `kill -9`), a SIGUSR1 and a SIGALRM read as those deaths through the front, as
/// they did when the call was the build. NEGATIVE CONTROL (by hand: `raise_like` raising
/// nothing): the front exits 137, 158 and 142 instead.
#[cfg(unix)]
#[test]
fn a_front_ends_by_the_signal_its_build_died_of() {
    use std::os::unix::process::ExitStatusExt as _;
    for (name, sig) in [
        ("KILL", libc::SIGKILL),
        ("USR1", libc::SIGUSR1),
        ("ALRM", libc::SIGALRM),
    ] {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let mut front = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "build_admit::tests::standin_front_ends_as_its_build",
                "--test-threads=1",
            ])
            .env(END_STANDIN, format!("{}:{name}", dir.path().display()))
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the stand-in front");
        let status = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the front's end",
            || front.try_wait().ok().flatten(),
        );
        assert_eq!(status.signal(), Some(sig), "{name}: {status:?}");
    }
}

/// A core signal's death is not raised (macOS would record a crash of the front's executable):
/// the front ends `128 + n`, as a shell reports one. NEGATIVE CONTROL: a signal with no core is
/// no exit code at all, and ends the process ([`a_front_ends_by_the_signal_its_build_died_of`]).
#[cfg(unix)]
#[test]
fn a_core_signals_death_is_an_exit_code_through_the_front() {
    use std::os::unix::process::ExitStatusExt as _;
    for (sig, code) in [
        (libc::SIGSEGV, 139),
        (libc::SIGABRT, 134),
        (libc::SIGQUIT, 131),
    ] {
        assert_eq!(
            end_like(std::process::ExitStatus::from_raw(sig)),
            std::process::ExitCode::from(code)
        );
    }
    assert!(!ENDS_WITHOUT_CORE.contains(&libc::SIGSEGV));
    assert!(!ENDS_WITHOUT_CORE.contains(&libc::SIGQUIT));
    assert!(ENDS_WITHOUT_CORE.contains(&libc::SIGKILL));
}

#[cfg(unix)]
const NEVER_STANDIN: &str = "ATPKG_BUILD_ADMIT_NEVER_STANDIN";

/// An isolated front whose build never starts. No program is spawned by this front.
/// A caller sees the arming witness before a queued signal can end this process.
#[cfg(unix)]
#[test]
fn standin_front_has_no_build_for_its_queued_signal() {
    use std::io::Write as _;
    let Some(signal) = std::env::var_os(NEVER_STANDIN) else {
        return;
    };
    aterm_uds::secret::no_core_dumps();
    let mut limit = libc::rlimit {
        rlim_cur: 1,
        rlim_max: 1,
    };
    // SAFETY: the limit is one initialized, writable rlimit, live for this call.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_CORE, &raw mut limit) },
        0
    );
    assert_eq!((limit.rlim_cur, limit.rlim_max), (0, 0));
    let front = front::Front::arm().expect("an armed front");
    println!("NEVER_FRONT_ARMED core=0");
    std::io::stdout()
        .flush()
        .expect("the caller sees the witness");
    let signal = match signal.to_str().expect("a signal name") {
        "none" => None,
        "QUIT" => Some(libc::SIGQUIT),
        other => panic!("unknown stand-in signal {other}"),
    };
    if let Some(signal) = signal {
        front::note_for_test(signal);
    }
    front.never();
    front.disarm();
    println!("NEVER_FRONT_RETURNED");
}

/// A queued key signal still ends a front whose build never started. Core signals become
/// shell-style exit codes, never a core-dump death or an unrelated normal return. The ordinary
/// control precedes the QUIT assertion. Run only the safe315 baseline or newer:
///315 returns normally for QUIT; older raw-raise production is not this baseline.
#[cfg(unix)]
#[test]
fn a_never_started_front_ends_with_its_queued_signal_without_dumping_core() {
    use std::os::unix::process::ExitStatusExt as _;
    for (signal, code) in [("none", 0), ("QUIT", 131)] {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        cmd.args([
            "--exact",
            "build_admit::tests::standin_front_has_no_build_for_its_queued_signal",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(NEVER_STANDIN, signal);
        let out = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
            .expect("the isolated front's outcome within the hang detector");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stdout.contains("NEVER_FRONT_ARMED core=0"),
            "{signal}: missing actual arming/core-limit witness: {stdout} {stderr}"
        );
        assert!(!out.status.core_dumped(), "{signal}: {:?}", out.status);
        assert_eq!(out.status.signal(), None, "{signal}: {stdout} {stderr}");
        assert_eq!(out.status.code(), Some(code), "{signal}: {stdout} {stderr}");
        assert_eq!(
            stdout.contains("NEVER_FRONT_RETURNED"),
            signal == "none",
            "{signal}: {stdout} {stderr}"
        );
    }
}

/// A stand-in `cargo` in `dir` that writes what it was handed (`ATPKG_ROUTED` and its words)
/// to `dir/ran`, then exits `code`.
#[cfg(unix)]
fn stand_in_cargo(dir: &Path, code: u8) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("cargo");
    let ran = dir.join("ran");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s|%s' \"${{ATPKG_ROUTED-unset}}\" \"$*\" > '{}'\nexit {code}\n",
            ran.display()
        ),
    )
    .expect("the stand-in");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("its mode");
    path
}

/// A routed cargo build claims, runs under its claim with the variables its route sets, ends as
/// it ended, and keeps its peak under `cargo build`. NEGATIVE CONTROL: a routed `cargo run`
/// claims nothing and is left to the route's own exec (the stand-in never runs here).
#[cfg(unix)]
#[test]
fn a_routed_cargo_build_runs_under_its_claim_and_a_run_is_left_alone() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(23));
    let cargo = stand_in_cargo(dir.path(), 0);
    let env = [("ATPKG_ROUTED".into(), "1".into())];
    let run =
        |args: &[&str]| run_if_claimed_at(&cargo, &words(args), Tool::Cargo, &env, dir.path(), &s);
    assert_eq!(run(&["run", "-p", "a"]), None);
    assert!(
        !dir.path().join("ran").exists(),
        "a run is the route's to exec"
    );
    assert_eq!(
        run(&["build", "-p", "a"]),
        Some(std::process::ExitCode::from(0))
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ran")).expect("it ran"),
        "1|build -p a"
    );
    let kept = Records::load(&dir.path().join("targo-memory.json"));
    assert_eq!(kept.rows().len(), 1);
    assert_eq!(kept.rows()[0].key.stage, "cargo build");
    let failing = aterm_tempfile::tempdir().expect("scratch");
    let cargo = stand_in_cargo(failing.path(), 3);
    let s = seat(failing.path(), &m, std::process::id().wrapping_add(23));
    assert_eq!(
        run_if_claimed_at(
            &cargo,
            &words(&["build"]),
            Tool::Cargo,
            &[],
            failing.path(),
            &s
        ),
        Some(std::process::ExitCode::from(3))
    );
}

/// A stand-in `targo`'s script that, at its start, looks in `ledger` for a claim of its parent's
/// process (a file `<start>-<ppid>-<seq>.claim`) and adds a line `held` or `none` to `out`.
#[cfg(unix)]
fn parents_claim_probe(ledger: &Path, out: &Path) -> String {
    format!(
        "for f in '{}'/*-\"$PPID\"-*.claim; do [ -e \"$f\" ] && {{ echo held >> '{}'; exit 0; }}; \
         done; echo none >> '{}'",
        ledger.display(),
        out.display(),
        out.display()
    )
}

/// A stand-in `targo` in `dir` that runs [`parents_claim_probe`], whatever it is handed.
#[cfg(unix)]
fn looks_for_its_parents_claim(dir: &Path, ledger: &Path, out: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let targo = dir.join("targo");
    std::fs::write(
        &targo,
        format!("#!/bin/sh\n{}\n", parents_claim_probe(ledger, out)),
    )
    .expect("the stand-in");
    std::fs::set_permissions(&targo, std::fs::Permissions::from_mode(0o755)).expect("runnable");
    targo
}

/// THE LOCK ORDER, BOUND TO ITS MODEL (`atpkg_build_admit_model`): a build's claim is held
/// before its `targo` exists, so the build directory's lock, which only `targo` takes, is only
/// ever taken under a claim, or once a holder that kept it died (`LockUnderClaim`); a claimed
/// builder waiting for the lock can never be waited on by the lock's holder (`NoWaitCycle`); and
/// an unclaimed build's inner front never waits for a claim whose builder waits for that build's
/// lock (`NoNestedWaitCycle`); and a front no lock is held over (D: a test's, a `targo run`
/// program's) never holds its claim beside a claimed builder's (`NoUnrequestedOvercommit`). The
/// model holds the four laws and every run of it ends. Its mutants: the rejected wrapper's order
/// (claim from inside the build) breaks the first two and deadlocks with A holding its claim and
/// B the lock; an exec before the hand-off breaks the first; an inner front that waits breaks the
/// third and deadlocks with A holding its claim and waiting for C's lock; a holder that lets its
/// claim go while its build runs (a watch that failed, taken for the build's end) breaks the
/// first; D admitted at once by cargo's words breaks the fourth. The code, as a call runs it: a stand-in `targo` that `run_if_claimed_at`
/// ran (classify, admit, then supervise's spawn) finds its parent's claim held at its start,
/// once; the probe itself finds none for a stand-in run before any claim. The hop's order is
/// bound through the real binary (`tests/build_hop.rs`: the stand-in the front became finds a
/// claim held in its own pid while holding no descriptor of it, and its control, an exec before
/// the hand-off, finds none), and the inner front's and D's by
/// `an_unproven_nested_lock_never_forces_admission_or_waits_in_a_cycle`, the ordinary-file
/// regression, and the real Cargo covered-build control in the integration suite.
/// NEGATIVE CONTROL (by hand: `run_if_claimed_at` running the target before it admits, the
/// wrapper's order): the stand-in finds none first.
#[cfg(unix)]
#[test]
fn a_builds_claim_is_held_before_its_targo_starts_as_the_model_orders() {
    use aterm_models_atpkg::atpkg_build_admit_model;
    use aterm_spec::interp::{State, find_deadlock, violated_invariants, with_buggy};
    let model = atpkg_build_admit_model();
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    let ended = |s: &State| s["a"] == 4 && s["b"] == 4 && s["c"] == 4 && s["d"] == 3;
    assert!(violated_invariants(&model).is_empty());
    assert_eq!(find_deadlock(&model, ended), None, "every run ends");
    let wrapper = with_buggy(&model, 1);
    let broken = violated_invariants(&wrapper);
    assert!(
        broken.contains("LockUnderClaim") && broken.contains("NoWaitCycle"),
        "{broken:?}"
    );
    let stuck = find_deadlock(&wrapper, ended).expect("the wrapper's order deadlocks");
    assert_eq!(
        (stuck["a"], stuck["held"], stuck["b"], stuck["lock"]),
        (2, 1, 1, 2),
        "A holds its claim and waits for the lock B holds while B waits for A's claim"
    );
    let early = violated_invariants(&with_buggy(&model, 2));
    assert!(
        early.contains("LockUnderClaim") && !early.contains("NoNestedWaitCycle"),
        "an exec before the hand-off builds unclaimed: {early:?}"
    );
    let waits = with_buggy(&model, 3);
    assert!(
        violated_invariants(&waits).contains("NoNestedWaitCycle"),
        "an inner front that waits"
    );
    let stuck = find_deadlock(&waits, ended).expect("an inner front that waits deadlocks");
    assert_eq!(
        (stuck["a"], stuck["held"], stuck["c"], stuck["lock"]),
        (2, 1, 2, 3),
        "A holds its claim and waits for C's lock while C's inner front waits for A's claim"
    );
    let let_go = violated_invariants(&with_buggy(&model, 4));
    assert!(
        let_go.contains("LockUnderClaim") && !let_go.contains("NoUnrequestedOvercommit"),
        "a holder that lets its claim go while its build runs leaves the lock unclaimed: \
         {let_go:?}"
    );
    let by_words = violated_invariants(&with_buggy(&model, 5));
    assert!(
        by_words.contains("NoUnrequestedOvercommit") && !by_words.contains("NoNestedWaitCycle"),
        "a front admitted at once by cargo's words alone holds its claim beside another: \
         {by_words:?}"
    );

    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(29));
    let ledger = dir.path().join("ledger");
    let out = dir.path().join("seen");
    let targo = looks_for_its_parents_claim(dir.path(), &ledger, &out);
    assert_eq!(
        run_if_claimed_at(
            &targo,
            &words(&["--unverified", "test", "-p", "a"]),
            Tool::Targo,
            &[],
            dir.path(),
            &s
        ),
        Some(std::process::ExitCode::from(0))
    );
    assert_eq!(std::fs::read_to_string(&out).expect("it looked"), "held\n");

    let early = dir.path().join("early");
    let status = aterm_uds::spawnfd::transparent("/bin/sh")
        .args(["-c", &parents_claim_probe(&ledger, &early)])
        .status()
        .expect("ran");
    assert!(status.success());
    let Admission::Claimed(late) = admit(&test_build(), dir.path(), &s, &mut |_| {}) else {
        panic!("expected a claim");
    };
    assert_eq!(
        std::fs::read_to_string(&early).expect("it looked"),
        "none\n"
    );
    drop(late);
}

/// A Cargo-lock filename is only a possible-cycle hint when a kernel probe says a lock is
/// held. An unlocked or unknown probe, a non-lock filename and a process outside the lineage
/// imply nothing. A hint changes neither the configured mode nor the coverage rules.
#[test]
fn only_a_held_file_in_the_lineage_is_a_possible_build_lock() {
    let line = || vec![41_u32, 42, 43];
    let files = |pid| {
        if pid == 42 {
            vec![PathBuf::from("/foreign/.cargo-lock")]
        } else {
            Vec::new()
        }
    };
    assert_eq!(possible_build_lock(&line, &files, &|_| false), None);
    assert_eq!(
        possible_build_lock(&line, &files, &|_| true),
        Some("/foreign/.cargo-lock".into())
    );
    assert_eq!(possible_build_lock(&|| vec![41], &files, &|_| true), None);
    assert_eq!(
        possible_build_lock(&line, &|_| vec!["/w/.cargo-lock.bak".into()], &|_| true),
        None
    );
}

#[test]
fn an_unproven_nested_lock_never_forces_admission_or_waits_in_a_cycle() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let mut s = seat(dir.path(), &m, std::process::id().wrapping_add(41));
    let ledger = s.ledger.as_ref().unwrap().clone();
    let unrelated = aterm_capacity::admit_with(
        &ledger,
        &m,
        Need::new("unrelated alone").mem(64 * GIB).alone(),
        Admit::Now,
        Pace::default(),
        &mut |_| {},
    )
    .expect("unrelated ALONE claim");
    let b = test_build();
    let lock = Path::new("/foreign/.cargo-lock");
    let mut said = Vec::new();
    let refused = admit_bounded(
        &b,
        dir.path(),
        &s,
        &|| Some(lock.to_path_buf()),
        &mut |line| said.push(line.to_owned()),
    );
    assert!(matches!(refused, Admission::Refused(_)), "{refused:?}");
    assert_eq!(
        ledger.list().expect("listed").len(),
        1,
        "no forced or waiting claim left"
    );
    assert!(said.iter().any(|line| line.contains("ownership is unproven") && line.contains("no build started")));
    s.pid = std::process::id();
    assert!(
        matches!(
            admit_bounded(
                &b,
                dir.path(),
                &s,
                &|| panic!("an own claim needs no hint probe"),
                &mut |_| {}
            ),
            Admission::Own(_)
        ),
        "own claim remains direct"
    );
    s.pid = s.pid.wrapping_add(41);
    s.mode = Admit::Now;
    let explicit = admit_bounded(
        &b,
        dir.path(),
        &s,
        &|| panic!("explicit Now needs no hint probe"),
        &mut |_| {},
    );
    assert!(
        matches!(explicit, Admission::Claimed(_)),
        "explicit Now is preserved: {explicit:?}"
    );
    drop(explicit);
    drop(unrelated);
    s.mode = Admit::Wait;
    let room = admit_bounded(
        &b,
        dir.path(),
        &s,
        &|| Some(lock.to_path_buf()),
        &mut |_| {},
    );
    assert!(
        matches!(room, Admission::Claimed(_)),
        "configured Wait admits normally when it fits: {room:?}"
    );
}

/// A NESTED BUILD UNDER AN UNPROVEN LOCK TAKES ITS TURN AT THE LEDGER'S LOCK: its one look
/// decides, and a look that met another admission holding the ledger's lock (one deciding at that
/// moment) decided nothing and is no wait that could close a cycle, so it looks again, saying who
/// holds the lock, and claims once the lock is let go. Its line naming the lock's holder proves
/// it met the lock before the test let it go. NEGATIVE CONTROL: the single look this replaced
/// refuses the build as a wait that could cycle.
#[test]
fn an_unproven_nested_lock_takes_its_turn_at_the_ledgers_lock() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let turn = Ledger::at(dir.path().join("ledger"))
        .hold_turn()
        .expect("the ledger's lock")
        .expect("free");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let path = dir.path().to_owned();
    let nested = std::thread::spawn(move || {
        let m = machine();
        let mut s = seat(&path, &m, std::process::id().wrapping_add(41));
        s.pace = Pace {
            poll: std::time::Duration::from_millis(5),
            line_every: std::time::Duration::ZERO,
        };
        let admitted = admit_bounded(
            &test_build(),
            &path,
            &s,
            &|| Some(PathBuf::from("/foreign/.cargo-lock")),
            &mut |line| {
                let _ = tx.send(line.to_owned());
            },
        );
        (
            matches!(admitted, Admission::Claimed(_)),
            format!("{admitted:?}"),
        )
    });
    let lock = aterm_capacity::Short::Turn {
        holder: Some(std::process::id()),
    }
    .to_string();
    let met = loop {
        let line = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("a line naming the lock's holder");
        if line.contains(&lock) {
            break line;
        }
    };
    drop(turn);
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "its admission", || {
        nested.is_finished().then_some(())
    });
    let (claimed, admitted) = nested.join().expect("the nested admission");
    assert!(claimed, "{admitted} after {met:?}");
}

/// A WAITING FRONT'S LEDGER SAYS WHAT IT WAITS FOR ([`front_claim`]), read off a real ledger: its
/// oldest claim that waits, by the memory it claims and the memory claims ahead of it (each held
/// one; none that claims no memory; no later waiter of its own process); once admitted it holds;
/// with no claim of its pid, nothing. NEGATIVE CONTROL: the cores-only claim held and the later
/// waiter of the same process change no count, and the count falls as the claim ahead lets go.
/// Every claim here is this process's; [`claims_of_other_processes_ahead_of_a_front_are_counted`]
/// reads other processes' claims.
#[test]
fn a_waiting_fronts_ledger_says_what_it_claims_and_how_many_are_ahead() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = machine();
    let me = std::process::id();
    let ledger = Ledger::at(dir.path().join("ledger"));
    assert_eq!(
        front_claim(&ledger, me).expect("read"),
        None,
        "no ledger yet"
    );
    let ahead = hold(
        dir.path(),
        &m,
        Need::new("verify test compile").mem(8 * GIB),
    );
    let cores = hold(dir.path(), &m, Need::new("cores only").cores(2));
    assert_eq!(
        front_claim(&ledger, me).expect("read"),
        Some(FrontClaim::Held)
    );
    let front = aterm_capacity::Waiter::enter(&ledger, &m, Need::new("targo build").mem(12 * GIB))
        .expect("entered");
    let later = aterm_capacity::Waiter::enter(&ledger, &m, Need::new("targo check").mem(4 * GIB))
        .expect("entered");
    assert_eq!(
        front_claim(&ledger, me).expect("read"),
        Some(FrontClaim::Waiting {
            mem: 12 * GIB,
            ahead: vec![me]
        })
    );
    drop(front);
    drop(ahead);
    assert_eq!(
        front_claim(&ledger, me).expect("read"),
        Some(FrontClaim::Waiting {
            mem: 4 * GIB,
            ahead: Vec::new()
        })
    );
    let aterm_capacity::Polled::Admitted(held) = later.poll(&m) else {
        panic!("4 GiB fits beside a cores-only claim on 20 GiB available");
    };
    assert_eq!(
        front_claim(&ledger, me).expect("read"),
        Some(FrontClaim::Held)
    );
    drop((held, cores));
    assert_eq!(front_claim(&ledger, me).expect("read"), None);
    assert_eq!(
        front_claim(&ledger, me.wrapping_add(1)).expect("read"),
        None,
        "another pid's"
    );
}

/// A reader finds the front's ledger as the front does: the one its own `ATERM_BUILD_ADMIT_LEDGER`
/// names in a debug build, else this user's. NEGATIVE CONTROL: an empty value is unset.
#[test]
fn a_reader_finds_the_fronts_ledger_by_the_fronts_own_environment() {
    let scratch = |key: &str| (key == LEDGER_ENV).then(|| "/scratch/ledger".to_owned());
    assert_eq!(
        ledger_of_front(&scratch).map(|l| l.dir().to_path_buf()),
        Some(PathBuf::from("/scratch/ledger"))
    );
    let empty = |key: &str| (key == LEDGER_ENV).then(String::new);
    assert_eq!(ledger_of_front(&empty), Ledger::user());
    assert_eq!(ledger_of_front(&|_| None), Ledger::user());
}

/// A claim's record as a ledger lists it: `pid`'s, laid at `start_ms`, asking `mem`, held or
/// waiting.
fn record(pid: u32, start_ms: u64, mem: u64, held: bool) -> Record {
    Record {
        accounting: aterm_capacity::Accounting::Holder,
        timing: aterm_capacity::TimingEvidence::default(),
        pid,
        start_ms,
        seq: 0,
        checkout: "/ws".to_owned(),
        need: Need::new("a build").mem(mem),
        held: held.then_some(aterm_capacity::Held {
            admitted_ms: start_ms,
            cores: 1,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

/// CLAIMS OF OTHER PROCESSES AHEAD OF A FRONT ARE COUNTED ([`front_claim_in`]), oldest first by
/// pid: a claim another process holds, laid before or after the front's, and one that waits since
/// earlier; never one that waits since later, nor one of no memory. The front's own oldest waiting
/// claim is the one read, wherever the list puts it. NEGATIVE CONTROL: the same records with the
/// earlier waiter laid after the front's claim count it out.
#[test]
fn claims_of_other_processes_ahead_of_a_front_are_counted() {
    const FRONT: u32 = 4100;
    let records = [
        record(FRONT, 5_000, 4 * GIB, false),
        record(4200, 3_000, 8 * GIB, true),
        record(4300, 9_000, 6 * GIB, true),
        record(4400, 4_000, 12 * GIB, false),
        record(4500, 6_000, 2 * GIB, false),
        record(4600, 2_000, 0, true),
        record(FRONT, 1_000, 10 * GIB, false),
    ];
    assert_eq!(
        front_claim_in(&records, FRONT),
        Some(FrontClaim::Waiting {
            mem: 10 * GIB,
            ahead: vec![4200, 4300]
        }),
        "its oldest waiting claim, laid at 1 s: only the held claims are ahead of it"
    );
    let later = [
        record(FRONT, 5_000, 4 * GIB, false),
        record(4200, 3_000, 8 * GIB, true),
        record(4300, 9_000, 6 * GIB, true),
        record(4400, 4_000, 12 * GIB, false),
        record(4500, 6_000, 2 * GIB, false),
        record(4600, 2_000, 0, true),
    ];
    assert_eq!(
        front_claim_in(&later, FRONT),
        Some(FrontClaim::Waiting {
            mem: 4 * GIB,
            ahead: vec![4200, 4400, 4300]
        }),
        "laid at 5 s: the held claims and the one waiting since 4 s, never the one since 6 s"
    );
    let mut swapped = later.clone();
    swapped[3].start_ms = 7_000;
    assert_eq!(
        front_claim_in(&swapped, FRONT),
        Some(FrontClaim::Waiting {
            mem: 4 * GIB,
            ahead: vec![4200, 4300]
        })
    );
    assert_eq!(
        front_claim_in(&[record(FRONT, 1_000, GIB, true)], FRONT),
        Some(FrontClaim::Held)
    );
    assert_eq!(front_claim_in(&later, 4700), None);
}
