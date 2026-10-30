// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A plain build's claim on a scratch ledger and an injected machine ([`Fixed`]): no test reads
//! this user's ledger, records or machine.

use super::*;
use aterm_capacity::reading::{Proc, Procs};
use aterm_capacity::{Fixed, Reading};

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
        (ck, Some(vec!["x".to_owned()]))
    );
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
    match admit(&test_build(), dir.path(), &s, &mut |_| {}) {
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

/// A ledger that cannot be read fails open: one line, and the build runs unclaimed. NEGATIVE
/// CONTROL: a usable ledger shows the claim.
#[test]
fn an_unreadable_ledger_fails_open_in_one_line() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    std::fs::write(dir.path().join("ledger"), b"not a directory").expect("a file");
    let m = machine();
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(3));
    let mut said = Vec::new();
    let got = admit(&test_build(), dir.path(), &s, &mut |l| {
        said.push(l.to_owned())
    });
    assert!(matches!(got, Admission::Open(_)), "{got:?}");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("runs unclaimed"), "{said:?}");
    let good = aterm_tempfile::tempdir().expect("scratch");
    let s = seat(good.path(), &m, std::process::id().wrapping_add(3));
    let got = admit(&test_build(), good.path(), &s, &mut |_| {});
    assert!(matches!(got, Admission::Claimed(_)), "{got:?}");
    let listed = Ledger::at(good.path().join("ledger"))
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
        let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh");
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
        let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh");
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
    let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh");
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
    let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh");
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
    let mut cmd = aterm_uds::spawnfd::transparent("/bin/sh");
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
    assert!(ENDS_WITHOUT_CORE.contains(&libc::SIGKILL));
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
/// ever taken under a claim (`LockUnderClaim`), and a claimed builder waiting for the lock can
/// never be waited on by the lock's holder (`NoWaitCycle`). The model holds both laws and every
/// run of it ends; the rejected wrapper's order (claim from inside the build) breaks both and
/// deadlocks with A holding its claim and B the lock. The code, as a call runs it: a stand-in
/// `targo` that `run_if_claimed_at` ran (classify, admit, then supervise's spawn) finds its
/// parent's claim held at its start, once; the probe itself finds none for a stand-in run before
/// any claim. NEGATIVE CONTROL (by hand: `run_if_claimed_at` running the target before it
/// admits, the wrapper's order): the stand-in finds none first.
#[cfg(unix)]
#[test]
fn a_builds_claim_is_held_before_its_targo_starts_as_the_model_orders() {
    use aterm_models_atpkg::atpkg_build_admit_model;
    use aterm_spec::interp::{State, find_deadlock, violated_invariants, with_buggy};
    let model = atpkg_build_admit_model();
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    let ended = |s: &State| s["a"] == 4 && s["b"] == 4;
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
