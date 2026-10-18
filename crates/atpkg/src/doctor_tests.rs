// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::activate::{activate_build, install_shims};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-doctor-{label}-"))
}

fn tool(name: &str) -> crate::store::ToolName {
    crate::store::ToolName::new(name).unwrap()
}

fn synthetic_home(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-dhome-{label}-"))
}

/// The build tree alone — no shims, no channel. Split out so a test can construct the
/// half-wired states the witness checks are about.
fn install_build_tree(layout: &Layout, program: &str, build: u64) -> PathBuf {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    // The executable the shim forwards to (`<program>.exe` on Windows) must exist for the
    // broken-shim scan (check 4) to see a healthy layout.
    std::fs::write(
        dir.join("bin").join(tool(program).exe_file()),
        b"#!/bin/true\n",
    )
    .unwrap();
    dir
}

fn install(layout: &Layout, program: &str, build: u64) {
    let dir = install_build_tree(layout, program, build);
    install_shims(
        layout,
        &dir,
        &[program.to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    activate_build(layout, &dir).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
}

fn names(list: &[&str]) -> std::collections::BTreeSet<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

fn builds(list: &[&str]) -> std::collections::BTreeMap<String, u64> {
    list.iter().map(|s| ((*s).to_string(), 1u64)).collect()
}

/// `PATH` holding only the managed `bin/`.
fn on_bin(l: &Layout) -> std::ffi::OsString {
    std::env::join_paths([l.bin_dir()]).unwrap()
}

/// The whole report at clock 0 over `probes`: (healthy, stdout, stderr).
fn report_of(
    l: &Layout,
    home: &Path,
    path: Option<&std::ffi::OsStr>,
    probes: &Probes,
) -> (bool, String, String) {
    report_at(l, home, path, 0, probes)
}

/// [`report_of`] at clock `now`.
fn report_at(
    l: &Layout,
    home: &Path,
    path: Option<&std::ffi::OsStr>,
    now: i64,
    probes: &Probes,
) -> (bool, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ok = run_with(
        l,
        Some(home),
        path,
        now,
        None,
        "doctor",
        probes,
        &mut out,
        &mut err,
    );
    (
        ok,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

/// A dev-linked program is present, not missing, and the link outranks a stale removed-ledger
/// line.
#[test]
fn a_dev_linked_program_is_neither_missing_nor_removed_even_when_the_ledger_names_it() {
    let (unexplained, on_purpose, dev_linked) = split_missing(
        &names(&["ay", "trust"]),
        &builds(&[]),
        &names(&["trust"]),
        &names(&["trust"]),
    );
    assert_eq!(
        dev_linked,
        vec!["trust".to_string()],
        "the link is the newer decision"
    );
    assert!(
        on_purpose.is_empty(),
        "a linked program is not 'removed on purpose': {on_purpose:?}"
    );
    assert_eq!(
        unexplained,
        vec!["ay".to_string()],
        "only the genuinely absent one is missing"
    );
}

/// A removed-ledger `trust` keeps its whole coherence group from activating: the ledger
/// explains `trust` only, so the siblings it took down are named as missing.
#[test]
fn the_suppressed_compiler_tuple_is_reported() {
    let (unexplained, on_purpose, _dev_linked) = split_missing(
        &names(&[
            "ay", "clean", "nn", "ny", "trust", "trust-cg", "trust-ir", "trust-mc", "trust-vc",
            "ty",
        ]),
        &builds(&["ay", "clean", "nn", "ny", "trust-mc", "ty"]),
        &names(&["trust"]),
        &names(&[]),
    );
    assert_eq!(
        unexplained,
        vec![
            "trust-cg".to_string(),
            "trust-ir".to_string(),
            "trust-vc".to_string()
        ],
        "the siblings the suppressed compiler took with it must be named"
    );
    assert_eq!(
        on_purpose,
        vec!["trust".to_string()],
        "the ledger accounts for the compiler itself, and that must be said out loud"
    );
}

/// Fail-quiet: with no cached index nothing is reported missing.
#[test]
fn no_cached_index_invents_no_missing_programs() {
    let l = layout("no-index");
    let (unexplained, on_purpose, dev_linked) =
        missing_against_index(&l, &builds(&["ay"]), &names(&[]));
    assert!(unexplained.is_empty() && on_purpose.is_empty() && dev_linked.is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

// (9) A rustup `trust` entry that resolves but is not the managed seam warns with the one
// re-point and moves nothing; a link into the store is a note; a real directory gets the
// detach fix; the seam itself and an absent entry say nothing here.
#[cfg(unix)]
#[test]
fn a_foreign_rustup_trust_entry_is_named_with_its_repoint_and_never_repointed() {
    let l = layout("foreign-seam");
    let build = l.build_dir("trust", 6808);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin").join("trustc"), b"trustc").unwrap();
    crate::activate::atomic_symlink(&build, &crate::seam::store_current(&l)).unwrap();
    let home = synthetic_home("foreign-seam");
    let rustup = home.join(".rustup");
    std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
    let dev = home.join("trust/build/host/stage2");
    std::fs::create_dir_all(dev.join("bin")).unwrap();
    let entry = rustup.join("toolchains/trust");
    let said = || seam_line(&crate::seam::status(&l, &rustup, "trust"), &l);
    std::os::unix::fs::symlink(&dev, &entry).unwrap();
    let line = said().expect("a foreign link is said");
    assert!(line.starts_with("warn — "), "{line}");
    assert!(
        line.contains(&format!(
            "ln -sfn '{}' '{}'",
            crate::seam::seam_target(&l, "trust").display(),
            entry.display()
        )),
        "the exact re-point command, quoted — the real prefix has a space in \
         `Application Support`: {line}"
    );
    assert_eq!(std::fs::read_link(&entry).unwrap(), dev, "nothing moved");
    // A numbered build inside the store, and the pre-view link at `current`: notes.
    for inside in [build.clone(), crate::seam::store_current(&l)] {
        std::fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink(&inside, &entry).unwrap();
        let line = said().unwrap();
        assert!(line.starts_with("note — "), "{line}");
    }
    // The seam itself, and no entry at all: nothing to say from this line.
    std::fs::remove_file(&entry).unwrap();
    crate::seam::refresh_view(&l, "trust").unwrap();
    std::os::unix::fs::symlink(crate::seam::seam_target(&l, "trust"), &entry).unwrap();
    assert_eq!(said(), None);
    std::fs::remove_file(&entry).unwrap();
    assert_eq!(said(), None);
    // A real directory gets the detach fix, since nothing can be linked over it.
    std::fs::create_dir_all(entry.join("bin")).unwrap();
    let line = said().unwrap();
    assert!(line.contains(crate::seam::DETACH_FIX), "{line}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A held toolchain update and a lease are said (what the wait waits on; the lease's holder),
/// and a healthy store stays healthy. A lease that cannot be read is treated as held; a free
/// one, and no record, say nothing.
#[test]
fn a_held_toolchain_update_and_a_lease_are_said_and_healthy() {
    let l = layout("held-flip");
    install(&l, "ay", 18);
    // The build the held flip moves off is in the store: the record describes a wait.
    install(&l, "trust", 9192);
    let home = synthetic_home("held-flip");
    let t0 = 1_790_000_000;
    assert!(crate::quiet::note_deferred(
        &l,
        "rustc",
        t0,
        t0,
        &crate::quiet::Busy::Running {
            exe: PathBuf::from("/w/target/debug/build/x/build-script-build"),
        },
        &crate::quiet::Moves {
            from: std::collections::BTreeMap::from([("trust".to_string(), 9192u64)]),
            to: std::collections::BTreeMap::from([("trust".to_string(), 9200u64)]),
            assets: std::collections::BTreeMap::new(),
        },
    ));
    let subject = crate::lease::Subject::build("trust", 9192).unwrap();
    let lease = crate::lease::take(
        &l.prefix,
        &subject,
        "aterm-release (pid 7) \u{2014} a release cut in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    let path = on_bin(&l);
    let (ok, report, _) = report_at(&l, &home, Some(&path), t0 + 600, &Probes::default());
    assert!(
        ok,
        "a held update and a lease are states, not faults:\n{report}"
    );
    assert!(
        report.contains("/w/target/debug/build/x/build-script-build")
            && report.contains("aterm-release (pid 7)"),
        "the wait names what it waits on, and the lease its holder:\n{report}"
    );
    drop(lease);
    assert!(
        lease_line(
            &subject,
            &crate::lease::Holders::Unknown("permission denied (os error 13)".into())
        )
        .is_some()
    );
    assert_eq!(lease_line(&subject, &crate::lease::Holders::Free), None);
    crate::quiet::clear(&l);
    assert!(hold_lines(&l, t0).is_empty(), "nothing held, nothing said");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A held-update record that outlived its builds is left by the report (a first cut cleared
/// it here, making the one no-mutation verb a store writer): a wait while its build stands,
/// `Outlived` once the store is emptied by hand, and the whole report, run twice, writes
/// nothing; the doors that write (`repair`, the next pass) end it.
#[cfg(unix)]
#[test]
fn a_held_update_record_that_outlived_its_builds_is_left_by_the_report() {
    let l = layout("held-flip-outlived");
    let home = synthetic_home("held-flip-outlived");
    let t0 = 1_790_000_000;
    install(&l, "trust", 9192);
    assert!(crate::quiet::note_deferred(
        &l,
        "rustc",
        t0,
        t0,
        &crate::quiet::Busy::Running {
            exe: PathBuf::from("/w/target/debug/deps/x"),
        },
        &crate::quiet::Moves {
            from: std::collections::BTreeMap::from([("trust".to_string(), 9192u64)]),
            to: std::collections::BTreeMap::from([("trust".to_string(), 9200u64)]),
            assets: std::collections::BTreeMap::new(),
        },
    ));
    assert!(matches!(
        crate::quiet::held(&l),
        crate::quiet::Held::Waits(_)
    ));
    std::fs::remove_dir_all(l.prefix.join("store")).unwrap();
    let path = on_bin(&l);
    for _ in 0..2 {
        let _ = read_only_report(&l, &home, &path);
    }
    assert!(matches!(
        crate::quiet::held(&l),
        crate::quiet::Held::Outlived(_)
    ));
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (11) The C toolchain: an answer (none, or one that refuses) fails the report, and its next
/// act is the platform's installer, never `update` (on macOS a refusing compiler names none:
/// the install act would only say the tools are installed); a compiler that builds, and one
/// doctor could not prove either way, leave the report healthy.
#[test]
fn doctor_fails_when_the_c_toolchain_cannot_build() {
    use crate::prereq::CcVerdict;
    let l = layout("cc-prereq");
    install(&l, "ay", 18);
    let home = synthetic_home("cc-prereq");
    let path = on_bin(&l);
    let cc = PathBuf::from("/opt/cc/bin/cc");
    for (verdict, healthy, names_act) in [
        (CcVerdict::NoDriver, false, true),
        (
            CcVerdict::Broken {
                driver: cc.clone(),
                why: "stdio.h: No such file or directory".to_string(),
            },
            false,
            !cfg!(target_os = "macos"),
        ),
        (CcVerdict::Ready { driver: cc.clone() }, true, false),
        (
            CcVerdict::Unproven {
                driver: cc.clone(),
                why: "no private directory under /ro: read-only".to_string(),
            },
            true,
            false,
        ),
        (CcVerdict::Unanswered { driver: cc.clone() }, true, false),
    ] {
        let label = format!("{verdict:?}");
        let probes = Probes {
            cc: Some(verdict),
            ..Probes::default()
        };
        let (ok, out, err) = report_of(&l, &home, Some(&path), &probes);
        assert_eq!(ok, healthy, "{label}: {out}{err}");
        assert_eq!(
            out.contains(&format!("next — {}", crate::prereq::act())),
            names_act,
            "{label}: {out}"
        );
        assert!(!out.contains("next — aterm pkg update"), "{label}: {out}");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn dev_link_only_toolset_is_healthy_but_deleted_tool_is_not() {
    let l = layout("dev-only");
    let home = synthetic_home("dev-only");
    let checkout = home.join("local-build");
    std::fs::create_dir_all(checkout.join("bin")).unwrap();
    let binary = checkout.join("bin/ay");
    std::fs::write(&binary, b"#!/bin/true\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::linkmode::link(&l, "ay", &checkout, &[PathBuf::from("bin/ay")]).unwrap();
    let path = on_bin(&l);
    let (ok, out, _) = report_of(&l, &home, Some(&path), &Probes::default());
    assert!(ok, "a dev-linked toolset is not an empty store:\n{out}");
    assert!(
        crate::ops::active_builds(&l).is_empty(),
        "no fabricated signed build"
    );

    std::fs::remove_file(binary).unwrap();
    assert!(live_dev_links(&l).0.is_empty());
    assert!(!report_of(&l, &home, Some(&path), &Probes::default()).0);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn dev_link_health_rejects_non_executable_and_foreign_destinations() {
    let l = layout("dev-link-targets");
    let home = synthetic_home("dev-link-targets");
    let checkout = home.join("checkout");
    std::fs::create_dir_all(checkout.join("bin")).unwrap();
    let binary = checkout.join("bin/ay");
    std::fs::write(&binary, b"#!/bin/true\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).unwrap();
    crate::linkmode::link(&l, "ay", &checkout, &[PathBuf::from("bin/ay")]).unwrap();
    assert!(
        live_dev_links(&l).0.is_empty(),
        "a non-executable file cannot run"
    );
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(live_dev_links(&l).0, names(&["ay"]));
    let foreign = home.join("foreign-ay");
    std::fs::copy(&binary, &foreign).unwrap();
    crate::platform::install_shim_to(&l.shim(&tool("ay")), &foreign).unwrap();
    let (live, problems) = live_dev_links(&l);
    assert!(
        live.is_empty(),
        "a marker cannot bless a foreign shim destination"
    );
    assert_eq!(problems.len(), 1);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (3b) Reroute order: upstream `~/.cargo/bin` ahead of the reroute dir. The walk counts only
/// an executable `cargo` in an absolute directory outside the prefix (one under the prefix
/// would be the stub counting itself), and a mis-ordered PATH is advisory: the toolset stays
/// healthy.
#[cfg(unix)]
#[test]
fn upstream_cargo_is_found_outside_the_prefix_and_a_misordered_path_is_advisory() {
    let l = layout("reroute");
    install(&l, "ay", 18);
    let cargo_in = |dir: &Path, mode: u32| {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("cargo"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(dir.join("cargo"), std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let upstream = crate::testdir::dir("atpkg-doctor-reroute-upstream-");
    cargo_in(&upstream, 0o755);
    let inside = l.prefix.join("inside");
    cargo_in(&inside, 0o755);
    let unrunnable = crate::testdir::dir("atpkg-doctor-reroute-noexec-");
    cargo_in(&unrunnable, 0o644);
    let entries = [
        inside.clone(),
        unrunnable.to_path_buf(),
        PathBuf::from("relative"),
        upstream.to_path_buf(),
    ];
    assert_eq!(
        upstream_index_on_path(&entries, &l.prefix, "cargo"),
        Some(3)
    );
    assert_eq!(
        upstream_index_on_path(&entries[..3], &l.prefix, "cargo"),
        None
    );

    crate::reroute::lay(&l).unwrap();
    let home = synthetic_home("reroute");
    let path =
        std::env::join_paths([upstream.to_path_buf(), l.reroute_dir(), l.bin_dir()]).unwrap();
    let (ok, out, _) = report_of(&l, &home, Some(&path), &Probes::default());
    assert!(
        ok && out.contains("is ahead of the reroute dir"),
        "a mis-ordered PATH is said, and advisory:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A store with nothing installed, a `*toolset*` row reading "no build for this
/// architecture", per-program `unavailable on <target>` rows, and optionally one served
/// member held after a failed stage.
fn unserved_platform_store(label: &str, with_held_claude: bool) -> crate::testdir::ScratchLayout {
    let l = layout(label);
    let mut programs = std::collections::BTreeMap::new();
    let row = |state: &str| crate::ProgramStatus {
        installed_build: None,
        state: state.to_string(),
        tree_root: String::new(),
    };
    programs.insert(
        "*toolset*".to_string(),
        row("unavailable: no build for this architecture"),
    );
    for p in ["ay", "trust", "ty"] {
        programs.insert(
            p.to_string(),
            row(&crate::state::unavailable("x86_64-pc-windows-msvc", "")),
        );
    }
    if with_held_claude {
        programs.insert(
            "claude".to_string(),
            row(&crate::state::held_failed(
                "tree_root mismatch: expected 15c4, got 3a44",
                "now: aterm pkg install claude",
            )),
        );
    }
    crate::status::write(
        &l,
        &crate::Status {
            schema: 1,
            programs,
            ..Default::default()
        },
    )
    .unwrap();
    l
}

/// The whole report over an [`unserved_platform_store`]: `(healthy, stdout)`.
fn report_over(l: &Layout, label: &str) -> (bool, String) {
    let home = synthetic_home(label);
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let healthy = run_with(
        l,
        Some(&home),
        Some(&path),
        0,
        None,
        "doctor",
        &Probes::default(),
        &mut out,
        &mut err,
    );
    let _ = std::fs::remove_dir_all(&home);
    (healthy, String::from_utf8(out).unwrap())
}

/// Off macOS (Windows, Linux) an unserved platform is a note, not a problem, unless a
/// served member's row records a failure; on macOS it stays a problem. Pins both sides of
/// [`toolset_unserved_here`].
#[test]
fn an_unserved_platform_is_a_note_not_a_problem_off_macos_and_unchanged_on_macos() {
    // Nothing served is missing: healthy off macOS, a problem on macOS.
    let l = unserved_platform_store("unserved-quiet", false);
    let (healthy, out) = report_over(&l, "unserved-quiet");
    if !cfg!(target_os = "macos") {
        assert!(healthy, "an unserved platform is not a fault: {out}");
        assert!(
            out.contains(
                "note — the ALab toolset is unavailable on this platform: no build is \
                 published for this target (ay, trust, ty)"
            ),
            "{out}"
        );
        assert!(
            out.contains("0 program(s) active — none is published for this platform"),
            "{out}"
        );
        assert!(!out.contains("PROBLEM"), "{out}");
        assert!(
            !out.contains("next — "),
            "nothing to do, nothing recommended: {out}"
        );
        assert!(!out.contains("--default-set"), "nothing impossible: {out}");
        assert!(out.contains("doctor: healthy"), "{out}");
    } else {
        assert!(!healthy, "{out}");
        assert!(
            out.contains(
                "PROBLEM — no ALab programs are installed (*toolset*: unavailable: no \
                 build for this architecture)"
            ),
            "{out}"
        );
        assert!(
            out.contains("next — aterm pkg install --default-set"),
            "{out}"
        );
        assert!(!out.contains("unavailable on this platform"), "{out}");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);

    // A served member held after a failed stage is the problem and its own door the act,
    // never the whole-set install (which honours the hold and repeats it), on any platform.
    let l = unserved_platform_store("unserved-held", true);
    let (healthy, out) = report_over(&l, "unserved-held");
    assert!(!healthy, "a held member is a problem everywhere: {out}");
    assert!(
        out.contains("claude: held: last attempt failed with tree_root mismatch"),
        "the held row is listed with its retry: {out}"
    );
    assert!(
        !out.contains("ok — claude: held:"),
        "a digest hold is never the benign group hold: {out}"
    );
    assert!(out.contains("next — aterm pkg install claude"), "{out}");
    assert!(
        !out.contains("next — aterm pkg install --default-set"),
        "a pass that repeats the hold is not the act: {out}"
    );
    if !cfg!(target_os = "macos") {
        assert!(
            out.contains(
                "PROBLEM — no ALab programs are installed (claude: held: last attempt \
                 failed with"
            ),
            "{out}"
        );
        assert!(!out.contains("--default-set"), "nothing impossible: {out}");
    } else {
        // On macOS the architecture row is still the first problem named; the held
        // member, listed below it, is what decides the act.
        assert!(
            out.contains(
                "PROBLEM — no ALab programs are installed (*toolset*: unavailable: no \
                 build for this architecture)"
            ),
            "{out}"
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);

    // After the served members install: the `*toolset*` row is gone, the unserved members'
    // rows remain, and one program is active. Off macOS the note stands (it keys on the
    // rows that last) and the report is healthy; on macOS nothing is said of the platform.
    let l = unserved_platform_store("unserved-after", false);
    clear_row(&l, "*toolset*");
    install(&l, "claude", 2026092201);
    let (healthy, out) = report_over(&l, "unserved-after");
    assert!(healthy, "{out}");
    assert!(out.contains("1 program(s) active"), "{out}");
    assert!(!out.contains("PROBLEM"), "{out}");
    assert_eq!(
        out.contains("unavailable on this platform"),
        !cfg!(target_os = "macos"),
        "the note is never said on macOS: {out}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);

    // With codex held beside the installed claude, the held row is the problem and the act
    // is the pass that forgets the hold (a plain `update` repeats it), on every platform.
    let l = unserved_platform_store("unserved-codex-held", false);
    clear_row(&l, "*toolset*");
    install(&l, "claude", 2026092201);
    let mut s = crate::status::read(&l).unwrap_or_default();
    s.programs.insert(
        "codex".to_string(),
        crate::ProgramStatus {
            installed_build: None,
            state: crate::state::held_failed(
                "tree_root mismatch: expected e498, got 6576",
                "now: aterm pkg install codex",
            ),
            tree_root: String::new(),
        },
    );
    crate::status::write(&l, &s).unwrap();
    let (healthy, out) = report_over(&l, "unserved-codex-held");
    assert!(!healthy, "{out}");
    assert!(
        out.contains("PROBLEM — the toolset is incomplete; 1 program(s) active"),
        "{out}"
    );
    assert!(
        out.contains("  codex: held: last attempt failed with tree_root mismatch"),
        "the held row is listed with its retry: {out}"
    );
    assert!(out.contains("next — aterm pkg install codex"), "{out}");
    assert!(!out.contains("next — aterm pkg update"), "{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The note masks one verdict, not every `*toolset*` row: an unreachable index on the same
/// empty store stays a problem with the ordinary act, as on every platform.
#[test]
fn an_unreachable_index_is_a_problem_on_an_unserved_platform_too() {
    let l = unserved_platform_store("unserved-offline", false);
    let mut s = crate::status::read(&l).unwrap_or_default();
    s.programs.insert(
        "*toolset*".to_string(),
        crate::ProgramStatus {
            installed_build: None,
            state: crate::state::TOOLSET_INDEX_UNREACHABLE.to_string(),
            tree_root: String::new(),
        },
    );
    crate::status::write(&l, &s).unwrap();
    let (healthy, out) = report_over(&l, "unserved-offline");
    assert!(
        !healthy,
        "an unreachable index is a fault everywhere: {out}"
    );
    assert!(
        out.contains(
            "PROBLEM — no ALab programs are installed (*toolset*: unavailable: index \
             unreachable)"
        ),
        "{out}"
    );
    assert!(!out.contains("none is missing"), "{out}");
    assert!(
        out.contains("next — aterm pkg install --default-set"),
        "the ordinary act, on every platform: {out}"
    );
    // The per-member rows still say what they say: off macOS, the note.
    assert_eq!(
        out.contains("unavailable on this platform"),
        !cfg!(target_os = "macos"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The empty-store verdict on an unserved platform is quiet only when nothing is recorded
/// against a served member and the index lists none missing; otherwise the act is that
/// member's door, and a toolset-wide fault gets the ordinary act.
#[test]
fn an_unserved_store_is_quiet_only_when_no_served_member_is_missing() {
    let v = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert!(unserved_store_is_quiet(true, &[], &[]));
    assert!(
        !unserved_store_is_quiet(true, &[], &v(&["claude"])),
        "a served member missing with no row is not 'none is missing'"
    );
    assert!(!unserved_store_is_quiet(
        true,
        &v(&["codex: error: x"]),
        &[]
    ));
    assert!(
        !unserved_store_is_quiet(false, &[], &[]),
        "never off an unserved platform"
    );

    assert_eq!(
        unserved_platform_next(&[], &v(&["claude", "codex"])).as_deref(),
        Some("aterm pkg install claude")
    );
    assert_eq!(
        unserved_platform_next(&v(&["codex: held: last attempt failed with x; y"]), &[]).as_deref(),
        Some("aterm pkg install codex")
    );
    assert_eq!(
        unserved_platform_next(&v(&["*toolset*: unavailable: index unreachable"]), &[]),
        None,
        "a toolset-wide fault is not this platform's: the ordinary act answers it"
    );
    assert_eq!(unserved_platform_next(&[], &[]), None);

    // The filter's predicate: exactly the two unserved spellings, nothing else.
    assert!(is_unserved_toolset_problem(
        "*toolset*: unavailable: no build for this architecture"
    ));
    assert!(is_unserved_toolset_problem(
        "*toolset*: blocked: no build for this architecture"
    ));
    for other in [
        "*toolset*: unavailable: index unreachable",
        "*toolset*: unavailable: every published build was refused",
        "*toolset*: unavailable: [packages].exclude leaves nothing to install",
        "*toolset*: unavailable: running under Rosetta translation",
        "ay: unavailable: no build for this architecture",
    ] {
        assert!(!is_unserved_toolset_problem(other), "{other}");
    }
    assert!(is_held_problem(
        "codex: held: last attempt failed with x; y"
    ));
    assert!(!is_held_problem(
        "trust: held: pinned build 7 is not published"
    ));
    assert!(!is_held_problem(
        "codex: error: held: last attempt failed with"
    ));
    // The held member is found wherever it sorts — behind a `*toolset*` row too.
    assert_eq!(
        first_held_member(&v(&[
            "*toolset*: unavailable: no build for this architecture",
            "ay: error: stage failed",
            "codex: held: last attempt failed with x; y",
        ])),
        Some("codex")
    );
    assert_eq!(
        first_held_member(&v(&["*toolset*: unavailable: index unreachable"])),
        None
    );
}

/// A held member's door at both places the next act names it (the empty store with a
/// toolset problem, and the held row as the one recorded problem): its own install, or for a
/// coherence-group member the pass that moves the tuple whole.
#[test]
fn a_held_member_s_next_act_is_its_tuple_s_door_when_grouped() {
    let held = [format!(
        "trust: {}",
        crate::state::held_failed("x", "now: aterm pkg update --retry")
    )];
    let in_rustc = |program: &str| program == "trust";
    let alone = |_: &str| false;
    let empty = NextFacts {
        toolset_problem: true,
        empty_store: true,
        recorded_problems: &held,
        ..NextFacts::default()
    };
    let only = NextFacts {
        recorded_problems: &held,
        ..NextFacts::default()
    };
    for facts in [&empty, &only] {
        assert_eq!(
            next_act(facts, in_rustc).as_deref(),
            Some("aterm pkg update --retry"),
            "{facts:?}"
        );
        assert_eq!(
            next_act(facts, alone).as_deref(),
            Some("aterm pkg install trust"),
            "{facts:?}"
        );
    }
    // With no held row, the same two states take their ordinary acts.
    let none: [String; 0] = [];
    assert_eq!(
        next_act(
            &NextFacts {
                recorded_problems: &none,
                ..empty
            },
            in_rustc
        )
        .as_deref(),
        Some("aterm pkg install --default-set")
    );
    assert_eq!(
        next_act(
            &NextFacts {
                recorded_problems: &none,
                ..only
            },
            in_rustc
        ),
        None
    );
}

/// Remove one row from the record (the toolset-wide verdict a clean pass clears).
fn clear_row(l: &Layout, program: &str) {
    let mut s = crate::status::read(l).unwrap_or_default();
    s.programs.remove(program);
    crate::status::write(l, &s).unwrap();
}

/// The unexplained-missing list loses exactly the unserved members, and only when
/// there are any to lose (`None` — every platform but Windows — is the identity).
#[test]
fn drop_unserved_removes_only_the_unserved_members() {
    let missing = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert_eq!(
        drop_unserved(missing(&["ay", "claude", "trust"]), None),
        missing(&["ay", "claude", "trust"])
    );
    assert_eq!(
        drop_unserved(
            missing(&["ay", "claude", "trust"]),
            Some(&missing(&["ay", "trust"]))
        ),
        missing(&["claude"])
    );
    assert_eq!(
        drop_unserved(missing(&["ay"]), Some(&missing(&[]))),
        missing(&["ay"])
    );
}

/// A stray `-`-prefixed row (`[programs.--help]`) is not a missing program; the control, a
/// plausible program name in the same error state, still fails, so the scan is narrowed to
/// stray flags, not disabled.
#[test]
fn a_stray_flag_row_is_not_a_missing_program() {
    for (name, healthy) in [("--help", true), ("trust-vc", false)] {
        let l = layout(&format!("stray-{}", name.trim_start_matches('-')));
        install(&l, "ay", 18);
        let mut status = crate::status::read(&l).unwrap_or_default();
        status.programs.insert(
            name.to_string(),
            crate::ProgramStatus {
                installed_build: None,
                state: format!("error: {name} is not named in the signed index"),
                tree_root: String::new(),
            },
        );
        crate::status::write(&l, &status).unwrap();
        let home = synthetic_home(&format!("stray-{}", name.trim_start_matches('-')));
        let path = on_bin(&l);
        let (ok, out, err) = report_of(&l, &home, Some(&path), &Probes::default());
        assert_eq!(ok, healthy, "{name}: {out}{err}");
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// `aborted: <phase>` (a coherence-group transaction killed mid-flight) and a yanked pin's
/// tombstone (stub shims every other check skips) are faults.
#[test]
fn an_aborted_transaction_is_a_problem_doctor_can_see() {
    for fault in [
        "aborted: activate",
        "error: x",
        "unavailable: y",
        "blocked: z",
        "tombstoned: pin yanked/below floor",
    ] {
        assert!(is_problem_state(fault), "{fault} is a fault");
    }
    // Allow-by-prefix: a state the scan does not recognize reads as benign.
    for benign in ["active", "dev-linked (skipped)", "staged 4821"] {
        assert!(!is_problem_state(benign), "{benign} is not a fault");
    }
}

/// Exhaustive over (declined) x (store empty) x (0 / 1 / many problems): a declined store
/// lists nothing, an empty one resumes after the reason its verdict named inline, a populated
/// one lists every problem.
#[test]
fn the_problem_listing_follows_the_verdict_that_named_it() {
    for empty in [true, false] {
        for n in [0, 1, 5] {
            assert_eq!(
                problem_listing_start(true, empty, n),
                None,
                "a declined store lists nothing (empty={empty}, n={n}): the decline is \
                 the verdict, and orphan lines would describe one nobody gave"
            );
        }
    }
    // Empty store: the verdict names reason #1 inline, so the listing resumes after it
    // and each problem is printed exactly once.
    assert_eq!(problem_listing_start(false, true, 0), None);
    assert_eq!(problem_listing_start(false, true, 1), Some(1));
    assert_eq!(problem_listing_start(false, true, 5), Some(1));
    // Populated store: the verdict names only a count, so all of them list.
    assert_eq!(problem_listing_start(false, false, 0), None);
    assert_eq!(problem_listing_start(false, false, 1), Some(0));
    assert_eq!(problem_listing_start(false, false, 5), Some(0));
}

#[test]
fn every_recorded_problem_is_reported_not_just_the_first() {
    let mut status = crate::Status::default();
    for (name, state) in [
        ("ay", "active"),
        ("clean", "error: no build for this Mac"),
        ("trust", "blocked: disk full"),
        ("ty", "aborted: activate"),
    ] {
        status.programs.insert(
            name.to_string(),
            crate::ProgramStatus {
                installed_build: None,
                state: state.into(),
                tree_root: String::new(),
            },
        );
    }
    assert_eq!(
        recorded_problems(Some(&status)),
        vec![
            "clean: error: no build for this Mac".to_string(),
            "trust: blocked: disk full".to_string(),
            "ty: aborted: activate".to_string(),
        ],
        "all three faults are reported — one error, one blocked, one aborted"
    );
}

/// A toolset the user removed is not broken.
#[test]
fn a_declined_store_is_healthy_while_empty() {
    let l = layout("declined-store");
    let home = synthetic_home("declined-store");
    let path = on_bin(&l);
    std::fs::create_dir_all(&l.prefix).unwrap();
    std::fs::write(l.declined(), b"# removed on purpose\n").unwrap();
    let (ok, out, err) = report_of(&l, &home, Some(&path), &Probes::default());
    assert!(
        ok,
        "a deliberate removal is a healthy state, not a fault:\n{out}{err}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// What fails the exit code and what does not, one fixture each: a dangling bin shim,
/// an active build whose completeness marker is gone, a fish-breaking stray `.sh` in
/// `shell.d`, and a shim naming a build the channel does not select are STRUCTURAL; a
/// program with no channel witness (gc only abstains) and a managed `bin/` missing from
/// PATH are advisory — said, and exit 0.
#[test]
fn structural_faults_fail_the_report_and_advisories_exit_zero() {
    type Setup = fn(&Layout, &Path);
    let cases: [(&str, Setup, Option<&str>, bool); 6] = [
        (
            "dangling-shim",
            |l, _| {
                install(l, "ay", 18);
                let ghost = tool("ghost");
                crate::platform::install_shim(
                    &l.build_dir("ay", 99).join("bin"),
                    &ghost,
                    &l.shim(&ghost),
                )
                .unwrap();
            },
            None,
            false,
        ),
        (
            "incomplete-active",
            |l, _| {
                install(l, "ay", 18);
                // The shim resolves (check 4 quiet), but the marker is gone (check 5).
                crate::store::discard_build(&l.build_dir("ay", 18));
                install_build_tree(l, "ay", 18);
                assert!(!crate::store::build_is_complete(&l.build_dir("ay", 18)));
            },
            None,
            false,
        ),
        (
            "stray-sh",
            |l, home| {
                install(l, "ay", 18);
                let shell_d = home.join(".aterm/shell.d");
                std::fs::create_dir_all(&shell_d).unwrap();
                std::fs::write(shell_d.join("00-atpkg.sh"), b"echo stray\n").unwrap();
            },
            None,
            false,
        ),
        (
            "witness-mismatch",
            |l, _| {
                install(l, "ay", 19);
                // 18 COMPLETE on disk (check 5 quiet), and ONLY the shim re-pointed at it.
                let older = install_build_tree(l, "ay", 18);
                crate::store::mark_build_ready(&older).unwrap();
                let ay = tool("ay");
                crate::platform::install_shim(&older.join("bin"), &ay, &l.shim(&ay)).unwrap();
            },
            None,
            false,
        ),
        (
            "witness-absent",
            |l, _| {
                let dir = install_build_tree(l, "ay", 18);
                install_shims(l, &dir, &["ay".to_string()], crate::activate::Aliases::Off).unwrap();
                crate::store::mark_build_ready(&dir).unwrap();
            },
            None,
            true,
        ),
        (
            "not-on-path",
            |l, _| install(l, "ay", 18),
            Some("/usr/bin:/bin"),
            true,
        ),
    ];
    for (label, setup, path, healthy) in cases {
        let l = layout(label);
        let home = synthetic_home(label);
        setup(&l, &home);
        let path = path.map(std::ffi::OsString::from);
        let (ok, out, err) = report_of(&l, &home, path.as_deref(), &Probes::default());
        assert_eq!(ok, healthy, "{label}:\n{out}{err}");
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// A shim whose target is there but unreadable (`Path::exists()` says `false`) is not a
/// broken shim: a warn naming the shim, the target and the errno.
#[cfg(unix)]
#[test]
fn a_shim_whose_target_cannot_be_stat_ed_is_not_reported_broken() {
    if crate::platform::our_uid() == 0 {
        return; // root reads through mode 0; the case does not exist.
    }
    let l = layout("unstattable-shim");
    install(&l, "ay", 18);
    let home = synthetic_home("unstattable-shim");
    let holder = l.build_dir("ay", 18).join("bin");
    let saved = std::fs::metadata(&holder).unwrap().permissions();
    std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
    let target = crate::platform::resolve_shim(&l.shim(&tool("ay"))).unwrap();
    let armed = std::fs::metadata(&target).is_err();
    let (_, out, err) = report_of(&l, &home, None, &Probes::default());
    std::fs::set_permissions(&holder, saved).unwrap();
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        armed,
        "the fixture must make the stat fail, or it proves nothing"
    );
    assert!(
        !err.contains("broken bin shim"),
        "a shim we could not look at was condemned as broken: {err}"
    );
    assert!(
        out.contains("could not be checked"),
        "a shim the scan could not look at must be reported, not skipped: {out}"
    );
}

/// The rc states doctor reports, read the way the pass reads them: no rc, an rc never
/// wired, wired by a real pass, opted out (wired once, block deleted), sourced through a
/// line that is not atpkg's (`tools/install.sh`'s block — also in the login profiles
/// macOS bash falls through to, which atpkg has no row for), and consent-fenced. A
/// profile that does not source `shell.d` is not listed.
#[cfg(unix)]
#[test]
fn rc_wiring_reads_each_state() {
    use crate::hooks::RcState;
    let l = layout("rc-wiring");
    let home = synthetic_home("rc-wiring");
    let shell_d = home.join(".aterm/shell.d");
    std::fs::create_dir_all(&shell_d).unwrap();
    let zshrc = home.join(".zshrc");
    let install_sh_block = |hook: &str| {
        format!(
            "export FOO=1\n# >>> aterm ALab toolset (managed by install.sh) >>>\n\
             if [ -f \"{h}\" ]; then . \"{h}\"; fi\n# <<< aterm ALab toolset <<<\n",
            h = shell_d.join(hook).display()
        )
    };
    assert!(
        crate::hooks::rc_wiring(&home).is_empty(),
        "no rc, nothing to report"
    );
    std::fs::write(&zshrc, "export FOO=1\n").unwrap();
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".zshrc", RcState::Unwired)]
    );
    // Wired by a real pass, so the ledger entry the opt-out state needs is the one the
    // pass writes, not a shape this test invented.
    let pass = crate::hooks::pass_at(&l, &home, crate::hooks::RcWiring::HonorOptOut);
    assert_eq!(pass.rc(), [(".zshrc", crate::hooks::RcOutcome::Appended)]);
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".zshrc", RcState::Wired)]
    );
    std::fs::write(&zshrc, "export FOO=1\n").unwrap();
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".zshrc", RcState::OptedOut)]
    );
    // install.sh's block on top of the opt-out ledger entry: the toolchain reaches this
    // shell, so the rc is neither opted out nor unwired.
    std::fs::write(&zshrc, install_sh_block("00-atpkg.zsh")).unwrap();
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".zshrc", RcState::SourcedElsewhere)]
    );
    // Consent-fenced: the rc resolves under a folder macOS guards, so no pass opens it.
    let docs = home.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("zshrc"), "export FOO=1\n").unwrap();
    std::fs::remove_file(&zshrc).unwrap();
    std::os::unix::fs::symlink(docs.join("zshrc"), &zshrc).unwrap();
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".zshrc", RcState::ConsentFenced)]
    );
    std::fs::remove_file(&zshrc).unwrap();
    // The login profiles: content is read before the ledger, in an atpkg row and in the
    // fall-through profile atpkg has no row for alike.
    std::fs::write(
        home.join(".bash_profile"),
        install_sh_block("00-atpkg.bash"),
    )
    .unwrap();
    std::fs::write(home.join(".profile"), "export BAR=1\n").unwrap();
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".bash_profile", RcState::SourcedElsewhere)]
    );
    std::fs::remove_file(home.join(".bash_profile")).unwrap();
    std::fs::write(home.join(".bash_login"), install_sh_block("00-atpkg.bash")).unwrap();
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".bash_login", RcState::SourcedElsewhere)]
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A `satisfied by system` row, and a member held on its current build (its coherence group's
/// new pin is not published for this target), on the owner's row and on a sibling's, are said
/// as the pass wrote them and are never a fault, recorded or in the exit code; a system copy
/// that is gone is still not one.
#[test]
fn a_system_satisfied_or_held_member_is_never_a_fault() {
    let l = layout("not-a-fault");
    install(&l, "ay", 19);
    install(&l, "trust", 6808);
    install(&l, "trust-cg", 3095);
    let exe = l.prefix.join("fake-system-gh");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    let triple = "x86_64-apple-darwin";
    let rows = [
        ("gh", None, crate::state::system(&exe, Some("2026-08-27"))),
        (
            "trust",
            Some(6808),
            crate::state::held_unpublished(None, 8595, triple, 6808),
        ),
        (
            "trust-cg",
            Some(3095),
            crate::state::held_unpublished(Some("trust"), 8595, triple, 3095),
        ),
    ];
    let mut status = crate::Status {
        schema: 1,
        updated_at: "2026-09-15T00:00:00Z".into(),
        enabled: true,
        index_source: "x/y".into(),
        outcome: "up to date".into(),
        last_success_at: "2026-09-15T00:00:00Z".into(),
        ..crate::Status::default()
    };
    for (p, build, state) in &rows {
        status.programs.insert(
            (*p).to_string(),
            crate::ProgramStatus {
                installed_build: *build,
                state: state.clone(),
                tree_root: String::new(),
            },
        );
    }
    crate::status::write(&l, &status).unwrap();
    assert!(recorded_problems(Some(&status)).is_empty());
    let home = synthetic_home("not-a-fault");
    let path = on_bin(&l);
    let (ok, out, err) = report_of(&l, &home, Some(&path), &Probes::default());
    assert!(ok, "{out}{err}");
    for (p, _, state) in &rows {
        assert!(
            out.contains(&format!("ok — {p}: {state}")),
            "{p}'s row is said as the pass wrote it:\n{out}"
        );
    }
    std::fs::remove_file(&exe).unwrap();
    let (ok, out, err) = report_of(&l, &home, Some(&path), &Probes::default());
    assert!(
        ok,
        "a gone system copy is a warning, not a fault:\n{out}{err}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A file at a command-link path is a warning, never a fault, and doctor leaves it
/// byte-identical; a link there says nothing.
#[cfg(unix)]
#[test]
fn a_file_at_a_command_link_path_is_a_warning_with_its_remedy() {
    let l = layout("copied-cmd");
    install(&l, "ay", 18);
    let home = synthetic_home("copied-cmd");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("aterm"), b"old copy").unwrap();
    std::os::unix::fs::symlink(
        "/Applications/aterm.app/Contents/MacOS/atpkg",
        bin.join("atpkg"),
    )
    .unwrap();
    let (ok, out, _) = report_of(&l, &home, None, &Probes::default());
    assert!(ok, "a copied command is a warning, not a fault:\n{out}");
    assert!(
        out.contains(&format!("warn — {} is a file", bin.join("aterm").display())),
        "{out}"
    );
    assert!(
        !out.contains(&format!("{} is a file", bin.join("atpkg").display())),
        "a link there says nothing: {out}"
    );
    assert_eq!(
        std::fs::read(bin.join("aterm")).unwrap(),
        b"old copy",
        "doctor never fixes"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A foreign copy of a managed tool ahead of the managed bin/ on PATH is a warn in the
/// canonical words, never a fault and never touched; a copy behind it is not mentioned.
#[cfg(unix)]
#[test]
fn a_shadowed_managed_member_is_a_warning_never_a_fault() {
    let l = layout("shadowed");
    install(&l, "ay", 19);
    let foreign = crate::testdir::dir("atpkg-doctor-shadow-");
    let exe = foreign.join("ay");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = synthetic_home("shadowed");
    let ahead = std::env::join_paths([foreign.to_path_buf(), l.bin_dir()]).unwrap();
    let (ok, out, _) = report_of(&l, &home, Some(&ahead), &Probes::default());
    assert!(ok, "a shadow is a warning, not a structural fault:\n{out}");
    assert!(
        out.contains(&format!("warn — ay: {}", crate::state::shadowed(19, &exe))),
        "{out}"
    );
    assert!(
        exe.exists() && crate::which(&l, "ay").is_some(),
        "never fixed"
    );
    let behind = std::env::join_paths([l.bin_dir(), foreign.to_path_buf()]).unwrap();
    let (ok, out, _) = report_of(&l, &home, Some(&behind), &Probes::default());
    assert!(ok && !out.contains("SHADOWED"), "{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (10h) Claude Code's rating surveys are off only by the two switches it reads; the env
/// switch is read as Claude Code 2.1.284 reads it (trimmed, lower-cased, one of
/// `1 true yes on`).
#[test]
fn the_claude_session_survey_is_off_only_by_the_switches_claude_reads() {
    assert!(claude_survey_off(r#"{"feedbackSurveyRate": 0}"#));
    assert!(claude_survey_off(
        r#"{"feedbackSurveyRate": 0.0, "model": "x"}"#
    ));
    for on in [
        r#""1""#,
        r#""true""#,
        r#""TRUE""#,
        r#"" yes ""#,
        r#""On""#,
        "true",
        "1",
    ] {
        assert!(
            claude_survey_off(&format!(
                r#"{{"env": {{"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": {on}}}}}"#
            )),
            "{on}"
        );
    }
    // The vendor's default shows it: no key, a non-zero rate, a falsy or unread env
    // value, unreadable JSON.
    assert!(!claude_survey_off("{}"));
    assert!(!claude_survey_off(r#"{"feedbackSurveyRate": 0.05}"#));
    for off in [
        r#""0""#,
        r#""false""#,
        r#""no""#,
        r#""off""#,
        r#""2""#,
        r#""y""#,
        r#""disabled""#,
        r#""""#,
        "false",
        "0",
        "null",
        r#"{"x": 1}"#,
    ] {
        assert!(
            !claude_survey_off(&format!(
                r#"{{"env": {{"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": {off}}}}}"#
            )),
            "{off}"
        );
    }
    assert!(!claude_survey_off("not json"));
}

/// Claude Code's top-effort key, as every (10h) fixture below names it: its one
/// spelling, [`CLAUDE_TOP_EFFORT_KEY`] (the publication baseline bans the joined
/// word in exported source).
const TOP: &str = CLAUDE_TOP_EFFORT_KEY;

/// The settings text `json` with each `<top>` replaced by [`TOP`]: the fixtures
/// stay readable JSON, and no source line spells the key.
fn top(json: &str) -> String {
    json.replace("<top>", TOP)
}

/// (10h) Each default as a settings file leaves it, by the window's own rule
/// (`aterm_primer::add_claude_defaults`): set to the default (for the survey switch,
/// whatever spelling Claude Code reads as true; for the rate, any spelling of zero, or
/// the switch covering it), set by hand and kept, unset — and held back: the effort
/// defaults while an effort other than `xhigh` is set by hand (per model, as
/// `/effort medium` saves it, or top-level), the switch while a survey probability other
/// than zero is. THE DAMAGE AN EARLIER BUILD LEFT — the top-effort key written over a
/// per-model `medium` — reads as the override it is, never the default. The per-model
/// autocompact window `/autocompact` saves leaves the top-level one unset, to add. A file
/// the window refuses to add to is one error naming why.
#[test]
fn each_claude_default_reads_as_the_window_s_rule_leaves_it() {
    use DefaultState::{Default as D, HeldForEffort, HeldForRate, Manual, OverridesEffort, Unset};
    let row = |top_s, effort, rate, switch, drafts, compact| {
        Ok(vec![
            (TOP, top_s),
            ("effortLevel", effort),
            ("feedbackSurveyRate", rate),
            (CLAUDE_SURVEY_SWITCH, switch),
            (CLAUDE_DRAFTS_KEY, drafts),
            (CLAUDE_COMPACT_WINDOW_KEY, compact),
        ])
    };
    let medium = "modelSettings.claude-opus-5-5.effortLevel = \"medium\"";
    let per_model = r#"{"modelSettings": {"claude-opus-5-5": {"autoCompactWindow": 400000}}}"#;
    type Want<'a> = Result<Vec<(&'a str, DefaultState)>, String>;
    let cases: Vec<(&str, String, Want)> = vec![
        (
            "every default set",
            top(
                r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0,
                    "feedbackDrafts": "quiet", "autoCompactWindow": 400000,
                    "env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}}"#,
            ),
            row(D, D, D, D, D, D),
        ),
        (
            "set by hand",
            top(r#"{"<top>": false, "effortLevel": "medium"}"#),
            row(
                Manual("false".into()),
                Manual("\"medium\"".into()),
                Unset,
                Unset,
                Unset,
                Unset,
            ),
        ),
        (
            "the switch covers the rate, in any spelling Claude reads as true",
            r#"{"env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "yes"}}"#.into(),
            row(Unset, Unset, D, D, Unset, Unset),
        ),
        (
            "a switch Claude reads as false is the person's own",
            r#"{"env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "off"}}"#.into(),
            row(Unset, Unset, Unset, Manual("\"off\"".into()), Unset, Unset),
        ),
        (
            "a rate set by hand is kept, whatever the switch",
            r#"{"env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}, "feedbackSurveyRate": 5}"#
                .into(),
            row(Unset, Unset, Manual("5".into()), D, Unset, Unset),
        ),
        (
            "any zero is the default, and leaves the switch to add",
            r#"{"feedbackSurveyRate": 0.0}"#.into(),
            row(Unset, Unset, D, Unset, Unset, Unset),
        ),
        (
            "a probability set by hand holds the switch back",
            r#"{"feedbackSurveyRate": 0.3}"#.into(),
            row(
                Unset,
                Unset,
                Manual("0.3".into()),
                HeldForRate("0.3".into()),
                Unset,
                Unset,
            ),
        ),
        (
            "a per-model effort holds both effort defaults back",
            r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "medium"}}}"#.into(),
            row(
                HeldForEffort(medium.into()),
                HeldForEffort(medium.into()),
                Unset,
                Unset,
                Unset,
                Unset,
            ),
        ),
        (
            "a hand-set xhigh is the default's own effort",
            r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "xhigh"}}}"#.into(),
            row(Unset, Unset, Unset, Unset, Unset, Unset),
        ),
        (
            "a per-model autocompact window leaves the top-level one to add",
            per_model.into(),
            row(Unset, Unset, Unset, Unset, Unset, Unset),
        ),
        (
            "what that build's window wrote over a per-model medium",
            top(
                "{\n  \"modelSettings\": {\n    \"claude-opus-5-5\": {\n      \
                 \"effortLevel\": \"medium\"\n    }\n  },\n  \"<top>\": true,\n  \
                 \"effortLevel\": \"xhigh\",\n  \"feedbackSurveyRate\": 0\n}\n",
            ),
            row(OverridesEffort(medium.into()), D, D, Unset, Unset, Unset),
        ),
        (
            "a top-level effort set by hand is overridden the same way",
            top(r#"{"<top>": true, "effortLevel": "low"}"#),
            row(
                OverridesEffort("effortLevel = \"low\"".into()),
                Manual("\"low\"".into()),
                Unset,
                Unset,
                Unset,
                Unset,
            ),
        ),
        (
            "an env that is no object",
            r#"{"env": "FOO=1"}"#.into(),
            Err("its \"env\" is not a JSON object".to_string()),
        ),
        (
            "a top level that is no object",
            top(r#"["<top>"]"#),
            Err("the top level is not a JSON object".to_string()),
        ),
    ];
    for (label, text, want) in cases {
        assert_eq!(claude_defaults_state(&text), want, "{label}: {text}");
    }
    for text in ["not json", "", "  \n"] {
        assert!(claude_defaults_state(text).is_err(), "{text:?}");
    }
    // The window adds to a file with any default unset — but not one whose only unset
    // key a person's own probability holds back.
    let rest = format!(
        r#""{TOP}": true, "effortLevel": "xhigh", "feedbackDrafts": "quiet",
            "autoCompactWindow": 400000"#
    );
    assert!(window_adds(
        &format!(r#"{{{rest}, "feedbackSurveyRate": 0}}"#),
        false
    ));
    assert!(!window_adds(
        &format!(r#"{{{rest}, "feedbackSurveyRate": 0.3}}"#),
        false
    ));
    assert!(window_adds(per_model, false));
}

/// (10h) A "Keep" answer to Claude Code's own autocompact offer, which Claude Code records
/// only in `~/.claude.json`, holds the autocompact window back as the window holds it
/// (`aterm_primer::claude_compact_offers_kept_in`): the row is an `ok` naming the kept model,
/// never the warn promising `400000`, and the window has nothing left to add. A
/// `~/.claude.json` that cannot be read holds it back too, and says so; an accepted offer
/// (saved in the settings) holds nothing back.
#[test]
fn a_kept_autocompact_offer_holds_the_window_back_in_the_report() {
    use DefaultState::{HeldForOffer, Unset};
    let home = synthetic_home("claude-offer");
    let claude = home.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    // Every other default in place.
    let settings = top(
        r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0,
            "feedbackDrafts": "quiet", "env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}}"#,
    );
    std::fs::write(claude.join("settings.json"), &settings).unwrap();
    let state = home.join(".claude.json");
    let offer = |choice: &str| {
        format!(
            r#"{{"autoCompactOffer": {{"acct": {{"claude-opus-5-5": {{"eligibleLoggedAt": 1,
                "resolved": {{"choice": "{choice}", "tokens": 1000000}}}}}}}}}}"#
        )
    };
    let window = || probe_claude_settings(&home).unwrap().unwrap()[5].clone();

    assert_eq!(
        window(),
        (CLAUDE_COMPACT_WINDOW_KEY, Unset),
        "no ~/.claude.json"
    );
    std::fs::write(&state, offer("accept")).unwrap();
    assert_eq!(window(), (CLAUDE_COMPACT_WINDOW_KEY, Unset), "accepted");
    std::fs::write(&state, offer("keep")).unwrap();
    let kept = window();
    assert_eq!(kept.0, CLAUDE_COMPACT_WINDOW_KEY);
    assert!(matches!(kept.1, HeldForOffer(why) if why.contains("claude-opus-5-5")));
    assert!(!window_adds(&settings, true));
    assert!(window_adds(&settings, false));
    std::fs::write(&state, "{").unwrap();
    let unreadable = window();
    assert!(
        matches!(&unreadable.1, HeldForOffer(why) if !why.is_empty()),
        "{unreadable:?}"
    );

    // As the report says it: an ok naming why, and no warn.
    std::fs::write(&state, offer("keep")).unwrap();
    let l = layout("claude-offer");
    let v283 = crate::vendor_direct::Version::parse("2.1.283")
        .unwrap()
        .build_id();
    install(&l, "claude", v283);
    let now = crate::flow::rfc3339_to_unix("2026-09-28T00:00:00Z").unwrap();
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let _ = run_with(
        &l,
        Some(&home),
        Some(&path),
        now,
        None,
        "doctor",
        &Probes {
            claude_defaults: probe_claude_settings(&home),
            ..Probes::default()
        },
        &mut out,
        &mut std::io::sink(),
    );
    let report = String::from_utf8_lossy(&out);
    let row = report
        .lines()
        .find(|line| line.contains(CLAUDE_COMPACT_WINDOW_KEY))
        .unwrap();
    assert!(row.starts_with("doctor: ok — claude:"), "{row}");
    assert!(
        row.contains("unset") && row.contains("claude-opus-5-5"),
        "{row}"
    );
    assert!(!report.contains("warn — claude:"), "{report}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (10h) The refusals the settings text cannot show, on real links in a scratch home: a
/// symlink to a missing file, a link to itself, and a file whose directory this user cannot
/// write (a read-only store link, or the plain file) while a key is left to add. Each is a
/// refusal. Controls: no file is no row, the same link into a writable directory is judged by
/// its keys, and a read-only file that needs nothing added keeps its rows.
#[cfg(unix)]
#[test]
fn a_dangling_or_unwritable_settings_file_is_the_one_refusal_warn() {
    use DefaultState::{Default as D, Unset};
    if crate::platform::our_uid() == 0 {
        return; // root writes through mode 0; the unwritable case does not exist.
    }
    let home = synthetic_home("claude-links");
    let claude = home.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    assert_eq!(probe_claude_settings(&home), None, "no file, no row");

    std::os::unix::fs::symlink(home.join("dotfiles/settings.json"), &settings).unwrap();
    let dangling = probe_claude_settings(&home);
    std::fs::remove_file(&settings).unwrap();

    let store = home.join("store");
    std::fs::create_dir_all(&store).unwrap();
    let bare = store.join("bare.json");
    std::fs::write(&bare, "{}").unwrap();
    let complete = store.join("complete.json");
    std::fs::write(
        &complete,
        top(
            r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0,
                "feedbackDrafts": "quiet", "autoCompactWindow": 400000,
                "env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}}"#,
        ),
    )
    .unwrap();
    std::os::unix::fs::symlink(&bare, &settings).unwrap();
    let writable = probe_claude_settings(&home);
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
    let linked_read_only = probe_claude_settings(&home);
    std::fs::remove_file(&settings).unwrap();
    std::os::unix::fs::symlink(&complete, &settings).unwrap();
    let complete_read_only = probe_claude_settings(&home);
    std::fs::remove_file(&settings).unwrap();
    std::fs::write(&settings, "{}").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o555)).unwrap();
    let plain_read_only = probe_claude_settings(&home);
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_file(&settings).unwrap();
    std::os::unix::fs::symlink(&settings, &settings).unwrap();
    let looped = probe_claude_settings(&home);

    for (label, refused) in [
        ("dangling", &dangling),
        ("linked into a read-only store", &linked_read_only),
        ("a self-link", &looped),
        ("plain file in a read-only directory", &plain_read_only),
    ] {
        assert!(
            matches!(refused, Some(Err(_))),
            "{label}: the refusal, not silence: {refused:?}"
        );
    }
    assert_eq!(
        writable,
        Some(Ok(vec![
            (TOP, Unset),
            ("effortLevel", Unset),
            ("feedbackSurveyRate", Unset),
            (CLAUDE_SURVEY_SWITCH, Unset),
            (CLAUDE_DRAFTS_KEY, Unset),
            (CLAUDE_COMPACT_WINDOW_KEY, Unset),
        ])),
        "the same link into a writable directory is judged by its keys"
    );
    assert_eq!(
        complete_read_only,
        Some(Ok(vec![
            (TOP, D),
            ("effortLevel", D),
            ("feedbackSurveyRate", D),
            (CLAUDE_SURVEY_SWITCH, D),
            (CLAUDE_DRAFTS_KEY, D),
            (CLAUDE_COMPACT_WINDOW_KEY, D),
        ])),
        "nothing to add, nothing written: the rows still hold"
    );
}

/// THE AGENT'S OWN COPY ON PATH, decided by where the shell is: outside aterm the
/// user's own copy is the design — a note naming no remedy, whether the reroute dir is
/// absent, behind the copy, or the stub not laid at all; inside aterm with the stub
/// answering first it is routed at exec time — an ok; inside aterm with no stub it is
/// shadowed in this shell — a warn. Never a fault.
#[cfg(unix)]
#[test]
fn outside_aterm_with_the_stub_laid_the_users_own_copy_is_a_note_never_shadowed() {
    let l = layout("agent-outside");
    install(&l, "ay", 19);
    let v280 = crate::vendor_direct::Version::parse("2.1.280")
        .unwrap()
        .build_id();
    install(&l, "claude", v280);
    crate::reroute::lay(&l).unwrap();
    let stub = crate::reroute::stub_path(&l, "claude");
    assert!(
        crate::reroute::is_reroute_stub(&stub),
        "the twin earns a stub"
    );
    let foreign = crate::testdir::dir("atpkg-doctor-agent-outside-foreign-");
    let exe = foreign.join("claude");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = synthetic_home("agent-outside");
    let outside = crate::reroute::StubEnv::default();
    let inside = crate::reroute::StubEnv { in_aterm: true };
    let iterm = std::env::join_paths([foreign.to_path_buf(), l.bin_dir()]).unwrap();
    let behind =
        std::env::join_paths([foreign.to_path_buf(), crate::reroute::dir(&l), l.bin_dir()])
            .unwrap();
    let stub_first =
        std::env::join_paths([crate::reroute::dir(&l), foreign.to_path_buf(), l.bin_dir()])
            .unwrap();
    let own_copy = crate::state::agent_own_copy_outside_aterm(v280, &exe);
    let check = |label: &str, path: &std::ffi::OsStr, stub_env, want: &str| {
        let (ok, out, err) = report_of(
            &l,
            &home,
            Some(path),
            &Probes {
                stub_env,
                ..Probes::default()
            },
        );
        assert!(ok, "{label}: never a fault:\n{out}{err}");
        let rows: Vec<&str> = out.lines().filter(|r| r.contains("— claude: ")).collect();
        assert_eq!(rows.len(), 1, "{label}: {out}");
        assert!(
            rows[0].starts_with(&format!("doctor: {want} — claude: ")),
            "{label}: {}",
            rows[0]
        );
        rows[0].to_string()
    };
    for (label, path) in [("no reroute dir", &iterm), ("reroute behind", &behind)] {
        let row = check(label, path, outside, "note");
        assert!(row.ends_with(&own_copy), "{label}: {row}");
    }
    let row = check("routed", &stub_first, inside, "ok");
    assert!(
        row.ends_with(&crate::state::agent_routed_in_shell(v280, &stub, &exe)),
        "{row}"
    );
    // The stub absent: outside aterm STILL the note — the rc hook's false arm demotes
    // `agents/` there, so a remedy would do nothing; inside aterm, shadowed.
    std::fs::remove_file(&stub).unwrap();
    let row = check("no stub, outside", &iterm, outside, "note");
    assert!(row.ends_with(&own_copy), "{row}");
    check("no stub, inside", &iterm, inside, "warn");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A shim that exports less than its build declares (a plain shim over a build whose sidecar
/// declares `DISABLE_AUTOUPDATER=1`, what an older `repair` left) is named, a warn and not a
/// fault, so the vendor's own updater running here is never passed over; once the shim
/// exports what the build declares, nothing is said.
#[cfg(unix)]
#[test]
fn a_shim_that_exports_less_than_its_build_declares_is_named() {
    let l = layout("shim-env");
    let claude = crate::vendor_direct::Version::parse("2.1.280")
        .unwrap()
        .build_id();
    let dir = install_build_tree(&l, "claude", claude);
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    crate::activate::install_tools(&l, &dir, &[tool("claude")], crate::activate::Aliases::Off)
        .unwrap();
    activate_build(&l, &dir).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    crate::shim_env::write_sidecar(&dir, &env).unwrap();
    let home = synthetic_home("shim-env");
    let path = on_bin(&l);
    let drift = |out: &str| {
        out.lines().any(|line| {
            line.starts_with("doctor: warn — claude: ") && line.contains("exports nothing")
        })
    };
    let (ok, out, err) = report_of(&l, &home, Some(&path), &Probes::default());
    assert!(ok && drift(&out), "named, and not a fault:\n{out}{err}");
    crate::activate::install_tools_env(
        &l,
        &dir,
        &[tool("claude")],
        crate::activate::Aliases::Off,
        &env,
    )
    .unwrap();
    let (ok, out, err) = report_of(&l, &home, Some(&path), &Probes::default());
    assert!(ok && !drift(&out), "{out}{err}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// The speaker is the verb typed, so a script keying on the prefix keys on the verb it ran.
#[test]
fn status_never_speaks_as_doctor() {
    let l = layout("speaker");
    install(&l, "ay", 18);
    let home = synthetic_home("speaker");
    let path = on_bin(&l);
    for (invoked, other) in [("status", "doctor:"), ("doctor", "status:")] {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            invoked,
            &Probes::default(),
            &mut out,
            &mut err,
        );
        for stream in [&out, &err] {
            let text = String::from_utf8_lossy(stream);
            assert!(
                !text.lines().any(|line| line.starts_with(other)),
                "invoked as {invoked}, no line may speak as {other}:\n{text}"
            );
            assert!(
                text.lines()
                    .all(|line| line.is_empty() || line.starts_with(invoked)),
                "every line is signed by the verb the user typed ({invoked}):\n{text}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// An empty store passes every structural check vacuously and is still not health: the
/// report fails, keeps the byte-stable "found N problem(s)" line, and names exactly one next
/// act, the whole-set install.
#[test]
fn an_unhealthy_report_names_one_next_act() {
    let l = layout("next-act");
    let home = synthetic_home("next-act");
    let path = on_bin(&l);
    let (ok, text, _) = report_of(&l, &home, Some(&path), &Probes::default());
    assert!(!ok, "{text}");
    assert!(
        text.contains("doctor: found 1 problem(s)"),
        "the verdict line keeps its bytes:\n{text}"
    );
    let next: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("doctor: next — "))
        .collect();
    assert_eq!(
        next,
        vec!["doctor: next — aterm pkg install --default-set"],
        "exactly one next act, and it is the whole-set install:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A bundle placed by hand is newer than the updater's receipt: "on disk" is the newer of
/// receipt and sealed bundle (no phantom downgrade), and the note fires only when that is
/// strictly newer than what runs.
#[test]
fn on_disk_build_is_the_newer_of_receipt_and_bundle_and_the_note_needs_installed_newer_than_running()
 {
    let base = layout("posture-bundle");
    let updates = base.prefix.join("Updates");
    std::fs::create_dir_all(&updates).unwrap();
    let report = |bundle: Option<u64>| -> String {
        let mut out = Vec::new();
        report_aterm_posture_at(&updates, bundle, &[], true, "atpkg", &mut out);
        String::from_utf8(out).unwrap()
    };
    std::fs::write(
        updates.join("installed.toml"),
        "build_number = 1786405661\n",
    )
    .unwrap();
    std::fs::write(
        updates.join("status.toml"),
        "current_build = 1789000876\nstaged_build = 1789000876\n",
    )
    .unwrap();
    assert_eq!(
        report(Some(1_789_000_876)),
        "atpkg: ok — aterm build 1789000876 installed\n",
        "on disk is derived from the bundle; no downgrade note"
    );
    assert_eq!(
        report(None),
        "atpkg: ok — aterm build 1786405661 installed\n",
        "a receipt OLDER than what runs is no note: that note is for NEWER"
    );
    assert!(
        report(Some(1_789_999_999))
            .contains("aterm build 1789999999 is on disk (running build 1789000876)"),
        "a bundle strictly newer than the running process"
    );
    // A receipt newer than both (the updater installed, the process predates it).
    std::fs::write(
        updates.join("installed.toml"),
        "build_number = 1790000000\n",
    )
    .unwrap();
    assert!(report(Some(1_789_000_876)).contains("aterm build 1790000000 is on disk"));
    let _ = std::fs::remove_dir_all(&base.prefix);
}

/// The Info.plist scans bind the value to the key right before it: the build only as an
/// integer, the short version only when it is shaped as one.
#[test]
fn plist_bundle_version_reads_only_the_string_bound_to_the_key() {
    assert_eq!(
        plist_bundle_version(
            "<plist><dict><key>CFBundleShortVersionString</key><string>0.79.0</string>\
             <key>CFBundleVersion</key>\n  <string>1789000876</string></dict></plist>"
        ),
        Some(1_789_000_876)
    );
    assert_eq!(
        plist_bundle_version("<key>CFBundleVersion</key><key>Other</key><string>54</string>"),
        None,
        "an intervening key cannot lend its string"
    );
    assert_eq!(
        plist_bundle_version("<key>CFBundleVersion</key><string>0.79.0</string>"),
        None,
        "not an integer build"
    );
    assert_eq!(plist_bundle_version("<plist/>"), None);
    let plist = "<key>CFBundleVersion</key>\n<string>1789000876</string>\n\
                 <key>CFBundleShortVersionString</key>\n<string>0.93.0</string>\n";
    assert_eq!(plist_short_version(plist).as_deref(), Some("0.93.0"));
    assert_eq!(
        plist_short_version("<key>CFBundleShortVersionString</key>\n<string>x</string>"),
        None,
        "a token that is not version-shaped is not printed"
    );
    assert_eq!(
        plist_short_version("<key>Other</key><string>0.1.0</string>"),
        None
    );
}

/// The `devtools` row: off the list is a warning, never a structural problem (the exit stays
/// healthy); a report that does not read the machine says nothing.
#[test]
fn the_devtools_row_is_a_warning_never_a_fault() {
    use aterm_containment::consent::{
        DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel,
    };
    let l = layout("devtools-row");
    install(&l, "ay", 19);
    let home = synthetic_home("devtools-row");
    let path = on_bin(&l);
    let absent = Probes {
        devtools: Some(crate::devtools::Facts {
            subject: Some("com.aterm.aterm".to_string()),
            list: DevToolList {
                listing: DevToolListing::Absent,
                label: DevToolListLabel::Read,
                modified_at_s: None,
                rows: Some(1),
            },
            log: DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest),
            other_responsible: None,
        }),
        ..Probes::default()
    };
    let (ok, out, _) = report_of(&l, &home, Some(&path), &absent);
    assert!(
        ok && out.contains("doctor: warn — devtools: "),
        "not being on the list is advisory, never structural:\n{out}"
    );
    let (_, out, _) = report_of(&l, &home, Some(&path), &Probes::default());
    assert!(!out.contains("devtools:"), "no reading, no row:\n{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A workspace `[trust]` policy the installed targo refuses fails the report, naming the
/// refused key, and its next act is publishing the newer Trust group.
#[test]
fn a_workspace_policy_the_installed_targo_refuses_is_a_problem_that_names_the_cure() {
    let l = layout("wspolicy");
    install(&l, "trust", 6808);
    let home = synthetic_home("wspolicy");
    let path = on_bin(&l);
    let probes = Probes {
        workspace: Some(WorkspacePolicyProbe {
            workspace: PathBuf::from("/work/ty"),
            policy: WorkspacePolicy::UnknownField {
                field: "compiler_timeout_secs".into(),
            },
        }),
        ..Probes::default()
    };
    let (ok, out, err) = report_of(&l, &home, Some(&path), &probes);
    assert!(!ok, "{out}{err}");
    assert!(
        err.contains("`compiler_timeout_secs`") && err.contains(PUBLISH_RUSTC_GROUP),
        "names the refused key and the cure: {err}"
    );
    assert!(
        out.contains("next — publish the newer Trust coherence group"),
        "next act: {out}"
    );
}

/// An accepted policy beside a rustup `trust` naming a LOCAL toolchain is healthy — the
/// local seal only warns — and with the entry really laid at that toolchain, the one
/// entry gets one warning, not a second re-point line from (9).
#[cfg(unix)]
#[test]
fn an_accepted_workspace_policy_is_ok_and_a_local_seal_only_warns() {
    let l = layout("wsok");
    install(&l, "trust", 6808);
    let home = synthetic_home("wsok");
    let path = on_bin(&l);
    let local = home.join("trust-d3866677");
    std::fs::create_dir_all(&local).unwrap();
    let probes = Probes {
        workspace: Some(WorkspacePolicyProbe {
            workspace: PathBuf::from("/work/aterm"),
            policy: WorkspacePolicy::Accepted,
        }),
        local_seal: Some(LocalSealProbe {
            link: PathBuf::from("/Users//me/.rustup/toolchains/trust"),
            link_target: local.clone(),
            trustc: "d3866677".into(),
            stale: None,
        }),
        ..Probes::default()
    };
    let (ok, out, err) = report_of(&l, &home, Some(&path), &probes);
    assert!(ok, "{out}{err}");
    assert_eq!(out.matches("warn — rustup `trust`").count(), 1, "{out}");
    let toolchains = home.join(".rustup").join("toolchains");
    std::fs::create_dir_all(&toolchains).unwrap();
    std::os::unix::fs::symlink(&local, toolchains.join("trust")).unwrap();
    let (_, out, _) = report_of(&l, &home, Some(&path), &probes);
    assert_eq!(out.matches("warn — rustup `trust`").count(), 1, "{out}");
    assert!(!out.contains("is NOT the managed store"), "{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// Where no Trust is published for this target (trust's row reads `unavailable on <target>`)
/// and the store holds none, a local rustup `trust` is the only Trust the machine can have —
/// docs/LINUX-LOCAL-TOOLCHAIN.md's route — so off macOS it is a note, not a thing to look at:
/// a Linux machine with that link read "2 thing(s) to look at" for ever. A link to a tree that
/// is gone still warns, and so does any link once a managed build is installed, and on macOS,
/// where the seal serves the machine.
#[test]
fn a_local_trust_where_none_is_published_is_a_note_off_macos() {
    let l = unserved_platform_store("unserved-local-trust", false);
    let home = synthetic_home("unserved-local-trust");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let local = home.join("trust-41d79979");
    std::fs::create_dir_all(&local).unwrap();
    let mut probes = Probes {
        local_seal: Some(LocalSealProbe {
            link: PathBuf::from("/home/me/.rustup/toolchains/trust"),
            link_target: local.clone(),
            trustc: "41d799797b".into(),
            stale: None,
        }),
        ..Probes::default()
    };
    // The full report, and the default one: the verdict and only the rows it counts.
    let report = |probes: &Probes| {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            probes,
            &mut out,
            &mut err,
        );
        let out = String::from_utf8(out).unwrap();
        let err = String::from_utf8(err).unwrap();
        let shown = present(&out, &err, "doctor", Detail::Problems).joined();
        (out, shown)
    };
    let warned = "warn — rustup `trust` names a local toolchain, not the managed one";
    let (out, shown) = report(&probes);
    if cfg!(target_os = "macos") {
        assert!(out.contains(warned), "{out}");
        assert!(shown.contains("rustup `trust`"), "counted: {shown}");
    } else {
        let note = out
            .lines()
            .find(|line| line.contains("note — rustup `trust`"))
            .unwrap_or_else(|| panic!("a note on the local trust: {out}"));
        assert!(
            note.contains(&local.display().to_string()),
            "it names the toolchain: {note}"
        );
        assert!(!out.contains("warn — rustup `trust`"), "{out}");
        assert!(!shown.contains("rustup `trust`"), "not counted: {shown}");
    }
    // A target that is gone breaks every `cargo +trust`: a warning everywhere.
    if let Some(seal) = probes.local_seal.as_mut() {
        seal.link_target = home.join("gone");
    }
    let (out, shown) = report(&probes);
    assert!(
        out.contains("warn — rustup `trust` names ") && out.contains("which no longer exists"),
        "{out}"
    );
    assert!(shown.contains("rustup `trust`"), "counted: {shown}");
    // With a managed build installed the local link is a choice against it: a warning with
    // the re-point back.
    if let Some(seal) = probes.local_seal.as_mut() {
        seal.link_target = local.clone();
    }
    install(&l, "trust", 6808);
    let (out, shown) = report(&probes);
    assert!(out.contains(warned), "{out}");
    assert!(out.contains("to use the managed one: "), "{out}");
    assert!(shown.contains("rustup `trust`"), "counted: {shown}");
}

/// A local toolchain older than the store's, or gone, is not a publisher's unpublished seal:
/// its fix is `repair`, said once, not again by (9) as a foreign link to re-point by hand nor
/// as a channel rustup lacks.
#[cfg(unix)]
#[test]
fn an_older_local_toolchain_is_named_stale_with_repair_as_its_fix() {
    let l = layout("stale-seal");
    install(&l, "trust", 9192);
    let home = synthetic_home("stale-seal");
    let path = on_bin(&l);
    let seal = |stale: crate::seam::Stale| Probes {
        local_seal: Some(LocalSealProbe {
            link: PathBuf::from("/Users//me/.rustup/toolchains/trust"),
            link_target: PathBuf::from("/Users//me/trust/build/host/stage2"),
            trustc: "1a2b3c4d5e".into(),
            stale: Some(stale),
        }),
        ..Probes::default()
    };
    let repair = "fix: `aterm pkg repair` re-points it";
    let older = crate::seam::Stale::Older {
        its: "2026-07-17".into(),
        store: "2026-09-17".into(),
    };
    let (_, out, _) = report_of(&l, &home, Some(&path), &seal(older));
    assert_eq!(out.matches(repair).count(), 1, "{out}");
    assert!(!out.contains("features only it has"), "{out}");
    // A link to a tree that is gone: every `cargo +trust` fails and repair relinks.
    let toolchains = home.join(".rustup").join("toolchains");
    std::fs::create_dir_all(&toolchains).unwrap();
    std::os::unix::fs::symlink(
        "/Users//me/trust/build/host/stage2",
        toolchains.join("trust"),
    )
    .unwrap();
    let (_, out, _) = report_of(&l, &home, Some(&path), &seal(crate::seam::Stale::Dangling));
    assert_eq!(out.matches(repair).count(), 1, "{out}");
    assert!(!out.contains("is NOT the managed store"), "{out}");
    assert!(
        !out.contains("rustup has no working `trust` toolchain"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn targo_refusal_text_yields_the_field_name() {
    let stderr = "error: unknown field `compiler_timeout_secs`, expected one of `enabled`, \
                  `level`\n    --> Cargo.toml:3151:1\n";
    assert_eq!(
        unknown_field_in(stderr).as_deref(),
        Some("compiler_timeout_secs")
    );
    assert_eq!(
        unknown_field_in("warning: unused manifest key: trust\n"),
        None
    );
    assert_eq!(unknown_field_in(""), None);
}

/// A store with trust 8589 active, a synthetic home, and PATH on the store's bin —
/// what every (5f) test runs the whole report over. Returns the build's `bin/`.
#[cfg(unix)]
fn tippy_sibling_store(
    label: &str,
) -> (
    crate::testdir::ScratchLayout,
    crate::testdir::Dir,
    std::ffi::OsString,
    PathBuf,
) {
    let l = layout(label);
    install(&l, "trust", 8589);
    let home = synthetic_home(label);
    let path = on_bin(&l);
    let bin = l.build_dir("trust", 8589).join("bin");
    (l, home, path, bin)
}

/// The whole report with default probes: (exit ok, stdout, stderr).
#[cfg(unix)]
fn whole_report(l: &Layout, home: &Path, path: &OsStr) -> (bool, String, String) {
    report_of(l, home, Some(path), &Probes::default())
}

#[cfg(unix)]
fn healthy(out: &str) -> bool {
    out.lines().any(|line| line == "doctor: healthy")
}

/// The report withholds "healthy" for one tool that cannot run, and exits 0.
#[cfg(unix)]
fn assert_not_healthy_exit_0(ok: bool, out: &str, err: &str) {
    assert_withheld_exit_0(ok, out, err, "1 managed tool(s) cannot run");
}

/// The report withholds "healthy" for one tool doctor cannot show to run — its warn
/// says "may not run" or "cannot tell" — and says exactly that, never "cannot run".
#[cfg(unix)]
fn assert_unproven_exit_0(ok: bool, out: &str, err: &str) {
    assert_withheld_exit_0(
        ok,
        out,
        err,
        "1 managed tool(s) may not run or cannot be checked",
    );
}

#[cfg(unix)]
fn assert_withheld_exit_0(ok: bool, out: &str, err: &str, counted: &str) {
    assert!(ok, "nothing structural: exit 0\n{out}{err}");
    assert!(
        !healthy(out),
        "a tool that cannot run is not health:\n{out}"
    );
    assert!(
        out.contains(&format!("doctor: not healthy — {counted} (see below)")),
        "{out}"
    );
    assert!(!err.contains("FAIL"), "{err}");
}

/// (5f) The view: no seam, nothing to say; an attached seam's stock names are clones and the
/// report is healthy. A hard-linked `rustc`, or one with `trustc`'s attributes and other
/// bytes, withholds "healthy" at exit 0; the re-assertion `repair` runs heals it.
#[cfg(unix)]
#[test]
fn a_rustup_view_whose_stock_names_are_not_the_stores_tools_is_not_healthy() {
    let (l, home, path, bin) = tippy_sibling_store("view");
    for tool in ["trustc", "targo", "trustdoc", "tippy", "tippy-driver"] {
        std::fs::write(bin.join(tool), tool).unwrap();
    }
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");

    let rustup = home.join(".rustup");
    std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
    crate::seam::attach(&l, &rustup, "trust").unwrap();
    let store_trustc = bin.join("trustc");
    let view_rustc = crate::seam::view_dir(&l, "trust").join("bin").join("rustc");
    let check = |want_healthy: bool| {
        let (ok, out, err) = whole_report(&l, &home, &path);
        if want_healthy {
            assert!(ok && healthy(&out), "{out}{err}");
        } else {
            assert_not_healthy_exit_0(ok, &out, &err);
            assert!(out.contains("is not a clone of the store's"), "{out}");
        }
    };
    check(true);

    // A hard link in place of the view's clone.
    std::fs::remove_file(&view_rustc).unwrap();
    std::fs::hard_link(&store_trustc, &view_rustc).unwrap();
    check(false);
    let _ = crate::seam::reassert(&l, &rustup);
    check(true);

    // The bundle's shape: the Trust tool's length, mode and time, other bytes.
    let modified = std::fs::metadata(&store_trustc)
        .unwrap()
        .modified()
        .unwrap();
    std::fs::remove_file(&view_rustc).unwrap();
    std::fs::write(&view_rustc, b"trustC").unwrap();
    std::fs::set_permissions(
        &view_rustc,
        std::fs::metadata(&store_trustc).unwrap().permissions(),
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&view_rustc)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert!(
        crate::clone::is_clone_of(&store_trustc, &view_rustc),
        "the attributes cannot tell it apart"
    );
    check(false);
    let _ = crate::seam::reassert(&l, &rustup);
    assert!(presents(&store_trustc, &view_rustc));
    check(true);
}

/// (5f) The view follows a dev-link: with trust dev-linked to a sysroot checkout, a view left
/// presenting the store is the wrong compiler (each stock name the checkout ships cannot
/// run); once re-asserted it is compared with the checkout's tools (healthy); unlinked, the
/// store's view is right again.
#[cfg(unix)]
#[test]
fn a_dev_linked_trusts_view_is_healthy_when_it_presents_the_checkout() {
    let (l, home, path, bin) = tippy_sibling_store("view-devlink");
    for tool in ["trustc", "targo", "trustdoc", "tippy", "tippy-driver"] {
        std::fs::write(bin.join(tool), tool).unwrap();
    }
    let rustup = home.join(".rustup");
    std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
    crate::seam::attach(&l, &rustup, "trust").unwrap();

    let checkout = home.join("stage2");
    std::fs::create_dir_all(checkout.join("bin")).unwrap();
    std::fs::create_dir_all(checkout.join("lib")).unwrap();
    for tool in ["trustc", "targo", "tippy"] {
        let executable = checkout.join("bin").join(tool);
        std::fs::write(&executable, format!("dev {tool}")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::linkmode::link(&l, "trust", &checkout, &[PathBuf::from("bin/trustc")]).unwrap();
    let (ok, out, err) = whole_report(&l, &home, &path);
    // `rustc` and `cargo` differ; the checkout ships no trustdoc, so `rustdoc` is not
    // compared.
    assert_withheld_exit_0(ok, &out, &err, "2 managed tool(s) cannot run");
    assert!(out.contains("is not the dev-linked checkout's"), "{out}");

    let _ = crate::seam::reassert(&l, &rustup);
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(
        ok && healthy(&out) && !out.contains("is not the"),
        "{out}{err}"
    );

    crate::linkmode::unlink(&l, "trust").unwrap();
    let _ = crate::seam::reassert(&l, &rustup);
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
}

/// (5f) A store-less dev-link has a seam too, checked against the checkout, the only
/// thing there is: a stub replaced by a copy is named. And once the checkout stops being
/// a sysroot the seam refuses, and with no store build behind the view doctor names the
/// one fix — silence there left `rustc +trust` running a compiler with no sysroot.
#[cfg(unix)]
#[test]
fn a_store_less_dev_linked_seam_is_checked_against_the_checkout() {
    let l = layout("view-devlink-nostore");
    let home = synthetic_home("view-devlink-nostore");
    let path = on_bin(&l);
    let checkout = home.join("stage2");
    std::fs::create_dir_all(checkout.join("bin")).unwrap();
    std::fs::create_dir_all(checkout.join("lib")).unwrap();
    for tool in ["trustc", "targo"] {
        let executable = checkout.join("bin").join(tool);
        std::fs::write(&executable, format!("dev {tool}")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::linkmode::link(&l, "trust", &checkout, &[PathBuf::from("bin/trustc")]).unwrap();
    let rustup = home.join(".rustup");
    std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
    let _ = crate::seam::reassert(&l, &rustup);
    let (_, out, _) = whole_report(&l, &home, &path);
    assert!(!out.contains("is not the"), "{out}");

    let view_rustc = crate::seam::view_dir(&l, "trust").join("bin").join("rustc");
    std::fs::remove_file(&view_rustc).unwrap();
    std::fs::write(&view_rustc, b"a copy").unwrap();
    let (_, out, _) = whole_report(&l, &home, &path);
    assert!(out.contains("is not the dev-linked checkout's"), "{out}");

    std::fs::remove_dir_all(checkout.join("lib")).unwrap();
    let _ = crate::seam::reassert(&l, &rustup);
    let (_, out, _) = whole_report(&l, &home, &path);
    assert!(
        out.contains("which atpkg cannot present") && out.contains("fix: `aterm pkg unlink trust`"),
        "{out}"
    );
}

/// (5f) tippy refuses a symlinked `trustc`: the report names a tippy that cannot run, once,
/// with the one fix. A plain `trustc`, or a bundle with no tippy, says nothing.
#[cfg(unix)]
#[test]
fn a_symlinked_trust_compiler_is_a_tippy_that_cannot_run() {
    let (l, home, path, bin) = tippy_sibling_store("symlinked-trustc");
    let trustc = bin.join("trustc");
    std::fs::write(bin.join("targo"), b"targo").unwrap();
    std::fs::write(bin.join("real"), b"trustc").unwrap();
    std::os::unix::fs::symlink("real", &trustc).unwrap();
    std::fs::write(bin.join("tippy"), b"tippy").unwrap();
    std::fs::write(bin.join("tippy-driver"), b"tippy-driver").unwrap();
    let warns = |out: &str| -> Vec<String> {
        out.lines()
            .filter(|line| line.contains("tippy cannot run"))
            .map(str::to_owned)
            .collect()
    };
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert_not_healthy_exit_0(ok, &out, &err);
    let said = warns(&out);
    assert!(
        said.len() == 1 && said[0].ends_with(&format!(" (the bundle's fix: {TRUST_COMPILER_FIX})")),
        "{out}"
    );

    std::fs::remove_file(&trustc).unwrap();
    std::fs::write(&trustc, b"trustc").unwrap();
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out) && warns(&out).is_empty(), "{out}{err}");

    std::fs::remove_file(&trustc).unwrap();
    std::os::unix::fs::symlink("real", &trustc).unwrap();
    std::fs::remove_file(bin.join("tippy")).unwrap();
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out) && warns(&out).is_empty(), "{out}{err}");
}

/// `targo trust`'s chain rule, component by component: root or us, and no group/other
/// write bit — the sticky bit exempts nothing (`/tmp`), and a foreign owner is named first.
/// A writable directory says whose it is: only the user's own is theirs to `chmod`.
#[test]
fn a_chain_component_is_refused_by_targo_s_rule() {
    let writable = |mode, yours| Some(ChainWhy::Writable { mode, yours });
    let foreign = Some(ChainWhy::Foreign {
        owner: 502,
        ours: 501,
    });
    for (owner, mode, want) in [
        (501, 0o040_755, None),
        (501, 0o040_700, None),
        (0, 0o040_755, None),
        (501, 0o040_775, writable(0o775, true)),
        (501, 0o040_757, writable(0o757, true)),
        (0, 0o040_775, writable(0o775, false)),
        (0, 0o041_777, writable(0o1777, false)),
        (502, 0o040_755, foreign),
        (502, 0o040_775, foreign),
    ] {
        assert_eq!(
            chain_component_refusal(501, owner, mode),
            want,
            "owner {owner} mode {mode:o}"
        );
    }
}

/// The walk mirrors what `targo trust` checks for a toolchain: the physical chain of
/// `bin/` (the sysroot is its parent), then `lib/` and each `lib/rustlib/<T>/lib`. Below
/// the sysroot a link is refused outright and nothing behind it is judged, a `<T>` with no
/// `lib/` is not on the loader path, and a directory several runtime paths share
/// (`lib/rustlib`) is named once. Modes are set explicitly: the runner's umask decides
/// nothing here.
#[cfg(unix)]
#[test]
fn the_trust_chain_walk_names_each_refused_directory_once_top_down() {
    use std::os::unix::fs::symlink;
    let root = crate::testdir::dir("atpkg-doctor-chain-");
    let root = std::fs::canonicalize(&*root).unwrap();
    let store = root.join("share/aterm/pkg/store/trust");
    let (plain, linked) = (store.join("7"), store.join("8"));
    let rustlib = plain.join("lib/rustlib");
    for dir in [
        plain.join("bin"),
        rustlib.join("aarch64-x/lib"),
        rustlib.join("wasm32-y/lib"),
        rustlib.join("src/rust"),
        linked.join("bin"),
        root.join("elsewhere/lib"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(rustlib.join("components"), b"rustc\n").unwrap();
    symlink(root.join("elsewhere"), rustlib.join("linked-x")).unwrap();
    symlink(root.join("elsewhere/lib"), linked.join("lib")).unwrap();
    let set = |dir: &Path, mode: u32| {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry.path());
                dirs.push(entry.path());
            }
        }
    }
    for dir in &dirs {
        set(dir, 0o755);
    }
    set(&root, 0o700);
    // On both chains' common stem; on every runtime path below `lib/`; on one target's
    // path only; on a target with no `lib/` (not judged); behind each link (not judged).
    set(&root.join("share/aterm"), 0o775);
    set(&rustlib, 0o775);
    set(&rustlib.join("wasm32-y"), 0o757);
    set(&rustlib.join("src"), 0o777);
    set(&root.join("elsewhere"), 0o757);
    set(&root.join("elsewhere/lib"), 0o777);
    // Above the scratch root is the machine's own chain (`/tmp` is `1777`): not this test's.
    let ours = crate::platform::our_uid();
    let refused = |build: &Path| -> Vec<ChainRefusal> {
        trust_chain_refusals(build, ours)
            .into_iter()
            .filter(|r| r.dir.starts_with(&root))
            .collect()
    };
    let writable = |dir: PathBuf, mode| ChainRefusal {
        dir,
        why: ChainWhy::Writable { mode, yours: true },
    };
    let redirected = |dir: PathBuf| ChainRefusal {
        dir,
        why: ChainWhy::Redirected,
    };
    assert_eq!(
        refused(&plain),
        vec![
            writable(root.join("share/aterm"), 0o775),
            writable(rustlib.clone(), 0o775),
            redirected(rustlib.join("linked-x")),
            writable(rustlib.join("wasm32-y"), 0o757),
        ]
    );
    assert_eq!(
        refused(&linked),
        vec![
            writable(root.join("share/aterm"), 0o775),
            redirected(linked.join("lib")),
        ]
    );
    for dir in &dirs {
        set(dir, 0o755);
    }
    std::fs::remove_file(rustlib.join("linked-x")).unwrap();
    assert_eq!(refused(&plain), Vec::new());
    assert_eq!(refused(&linked), vec![redirected(linked.join("lib"))]);
}

/// (5f)(g) A directory `targo trust` refuses on the active Trust build's chain withholds
/// "healthy" as one tool that cannot run, with a warn naming the build, the directory and
/// its next step, which the short report keeps: `chmod go-w` for a writable directory of
/// the user's own, never for root's (`/tmp`'s `1777` must stay as it is), and the bundle's
/// fix for a link. A walk of another build, or one that refused nothing, says nothing.
#[cfg(unix)]
#[test]
fn a_refused_directory_on_the_trust_chain_names_its_next_step() {
    let (l, home, path, _) = tippy_sibling_store("trust-chain");
    let probes = |build: u64, refused: Vec<ChainRefusal>| Probes {
        trust_chain: Some(TrustChainProbe { build, refused }),
        ..Probes::default()
    };
    let refusal = |dir: &str, why| {
        vec![ChainRefusal {
            dir: PathBuf::from(dir),
            why,
        }]
    };
    let shared = refusal(
        "/home/u/.local/share/aterm",
        ChainWhy::Writable {
            mode: 0o775,
            yours: true,
        },
    );
    let cases = [
        (
            shared.clone(),
            "/home/u/.local/share/aterm",
            Some("775"),
            "fix: run `chmod go-w '/home/u/.local/share/aterm'`".to_string(),
        ),
        (
            refusal(
                "/tmp",
                ChainWhy::Writable {
                    mode: 0o1777,
                    yours: false,
                },
            ),
            "/tmp",
            Some("1777"),
            "fix: set [packages].prefix, or HOME, under directories only you can write".to_string(),
        ),
        (
            refusal("/s/trust/8589/lib", ChainWhy::Redirected),
            "/s/trust/8589/lib",
            None,
            format!("fix: {TRUST_LIB_FIX}"),
        ),
    ];
    for (refused, dir, mode, fix) in cases {
        let (ok, out, err) = report_of(&l, &home, Some(&path), &probes(8589, refused));
        assert_not_healthy_exit_0(ok, &out, &err);
        let names = |line: &str| {
            line.starts_with("doctor: warn — ")
                && line.contains("trust build 8589")
                && line.contains("`targo trust`")
                && line.ends_with(&fix)
        };
        assert!(
            out.lines().any(|line| names(line)
                && line.contains(dir)
                && mode.is_none_or(|mode| line.contains(&format!("mode {mode}")))),
            "{out}"
        );
        assert!(
            fix.contains("chmod")
                || !out
                    .lines()
                    .any(|line| line.contains("`targo trust`") && line.contains("chmod")),
            "only a directory of the user's own is given a chmod\n{out}"
        );
        let shown = present(&out, &err, "doctor", Detail::Problems);
        assert!(shown.rest.lines().any(names), "{}", shown.rest);
    }
    for quiet in [probes(8588, shared), probes(8589, Vec::new())] {
        let (ok, out, err) = report_of(&l, &home, Some(&path), &quiet);
        assert!(
            ok && healthy(&out) && !out.contains("`targo trust` refuses"),
            "{out}{err}"
        );
    }
}

/// Trust build 8595 as the older bundles ship it: `rustc` and `cargo` separate files that
/// differ from `trustc` and `targo`, beside `tippy`, with a `lib/` (a driver dylib, a nested
/// rlib, a symlink) and a `share/`, active through plain shims for `trust`, `trustc`,
/// `targo`, `tippy` and `alab-tippy`. Returns the layout, the synthetic home, `PATH` and the
/// build's `bin/`.
#[cfg(unix)]
fn affected_trust_store(
    label: &str,
) -> (
    crate::testdir::ScratchLayout,
    crate::testdir::Dir,
    std::ffi::OsString,
    PathBuf,
) {
    let l = layout(label);
    let dir = install_build_tree(&l, "trust", 8595);
    let bin = dir.join("bin");
    for (file, body) in [
        ("trustc", "frontend 8595 / code signature: trustc"),
        ("rustc", "frontend 8595 / code signature: rustc_"),
        ("targo", "targo 8595"),
        ("cargo", "cargo copy 8595"),
        ("tippy", "tippy 8595"),
        ("tippy-driver", "tippy-driver 8595"),
        ("targo-tippy", "targo-tippy 8595"),
    ] {
        std::fs::write(bin.join(file), body).unwrap();
    }
    let rustlib = dir.join("lib/rustlib/aarch64-apple-darwin/lib");
    std::fs::create_dir_all(&rustlib).unwrap();
    std::fs::write(dir.join("lib/librustc_driver-5cfd.dylib"), b"driver").unwrap();
    std::fs::write(rustlib.join("libstd.rlib"), b"std").unwrap();
    std::os::unix::fs::symlink("../../share", dir.join("lib/rustlib/share-link")).unwrap();
    std::fs::create_dir_all(dir.join("share")).unwrap();
    std::fs::write(dir.join("share/README"), b"readme").unwrap();
    std::fs::create_dir_all(l.bin_dir()).unwrap();
    for (name, file) in [
        ("trust", "trust"),
        ("trustc", "trustc"),
        ("targo", "targo"),
        ("tippy", "tippy"),
        ("alab-tippy", "tippy"),
    ] {
        write_plain_shim(&l, name, &bin.join(file));
    }
    activate_build(&l, &dir).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    let home = synthetic_home(label);
    let path = on_bin(&l);
    (l, home, path, bin)
}

/// A plain shim at `bin/<name>`, written directly so no route is decided.
#[cfg(unix)]
fn write_plain_shim(l: &Layout, name: &str, target: &Path) {
    let shim = l.shim(&tool(name));
    std::fs::write(
        &shim,
        crate::platform::sh_shim_content_env(target, &crate::shim_env::ShimEnv::NONE),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Everything `lstat` says about `dirs` and every entry under them — path, inode, link
/// count, mode, size, mtime and ctime to the nanosecond, symlink target — sorted: what
/// any write, link, unlink, chmod or create would move.
#[cfg(unix)]
fn tree_snapshot(dirs: &[&Path]) -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
    while let Some(path) = stack.pop() {
        let m = std::fs::symlink_metadata(&path).unwrap();
        out.push(format!(
            "{} ino={} nlink={} mode={:o} size={} mtime={}.{} ctime={}.{} link={:?}",
            path.display(),
            m.ino(),
            m.nlink(),
            m.mode(),
            m.size(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
            std::fs::read_link(&path).ok()
        ));
        if m.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                stack.push(entry.unwrap().path());
            }
        }
    }
    out.sort();
    out
}

/// The whole report, with proof that it wrote nothing: the prefix and the home are
/// `lstat`-identical before and after.
#[cfg(unix)]
fn read_only_report(l: &Layout, home: &Path, path: &OsStr) -> (bool, String, String) {
    let before = tree_snapshot(&[&l.prefix, home]);
    let report = whole_report(l, home, path);
    assert_eq!(
        before,
        tree_snapshot(&[&l.prefix, home]),
        "doctor wrote to the fixture:\n{}",
        report.1
    );
    report
}

/// `out` has exactly one line about trust build 8595's exec root, at `level`, naming
/// each of `facts`.
#[cfg(unix)]
fn assert_one_root_line(out: &str, level: &str, facts: &[&str]) {
    let lines: Vec<&str> = out
        .lines()
        .filter(|line| line.contains(" — trust build 8595: "))
        .collect();
    assert!(
        lines.len() == 1
            && lines[0].starts_with(&format!("doctor: {level} — trust build 8595: "))
            && facts.iter().all(|fact| lines[0].contains(fact)),
        "one {level} line naming {facts:?}:\n{out}"
    );
}

/// (5f)(c)-(f) The exec root over the whole report, every state read-only (doctor lays and
/// sweeps nothing): no root, the root laid, an alias unrouted, a planted file, a symlinked
/// `compat/trust`, an unreadable `rustc`, stray roots, and a build that stops needing one.
#[cfg(unix)]
#[test]
fn an_affected_trust_build_is_healthy_only_while_its_exec_root_routes_every_shim() {
    let (l, home, path, bin) = affected_trust_store("exec-root");
    let root = crate::compat::root_dir(&l, 8595);
    let repair = "`aterm pkg repair` lays the exec root";

    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert_not_healthy_exit_0(ok, &out, &err);
    assert_one_root_line(&out, "warn", &[&root.display().to_string(), repair]);

    let _ = crate::compat::reconcile(&l, crate::seam::Depth::Shallow);
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
    assert_one_root_line(&out, "note", &[]);

    write_plain_shim(&l, "alab-tippy", &bin.join("tippy"));
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert_not_healthy_exit_0(ok, &out, &err);
    assert_one_root_line(&out, "warn", &["1 of 5", "alab-tippy", repair]);
    let _ = crate::compat::reconcile(&l, crate::seam::Depth::Shallow);

    let planted = root.join("lib").join("planted.dylib");
    std::fs::write(&planted, b"not the build's").unwrap();
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert_unproven_exit_0(ok, &out, &err);
    assert_one_root_line(&out, "warn", &[&planted.display().to_string(), repair]);
    let _ = crate::compat::reconcile(&l, crate::seam::Depth::Deep);
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out) && err.is_empty(), "{out}{err}");

    let roots = crate::compat::roots_dir(&l);
    let aside = l.prefix.with_file_name(format!(
        "{}-compat-trust-aside",
        l.prefix.file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(&roots, &aside).unwrap();
    std::os::unix::fs::symlink(&aside, &roots).unwrap();
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert_unproven_exit_0(ok, &out, &err);
    assert_one_root_line(
        &out,
        "warn",
        &[&roots.display().to_string(), "fix: remove that link"],
    );
    std::fs::remove_file(&roots).unwrap();
    std::fs::rename(&aside, &roots).unwrap();

    if crate::platform::our_uid() != 0 {
        let rustc = bin.join("rustc");
        let mode = std::fs::metadata(&rustc).unwrap().permissions();
        std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (ok, out, err) = read_only_report(&l, &home, &path);
        std::fs::set_permissions(&rustc, mode).unwrap();
        assert_unproven_exit_0(ok, &out, &err);
        assert_one_root_line(&out, "warn", &["cannot tell"]);
    }

    let gone = crate::compat::root_dir(&l, 9999);
    std::fs::create_dir_all(gone.join("bin")).unwrap();
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert!(ok && healthy(&out), "advisory, never health:\n{out}{err}");
    assert!(
        out.lines().any(|line| line.starts_with("doctor: warn — ")
            && line.contains(&gone.display().to_string())
            && line.contains("aterm pkg gc")),
        "{out}"
    );
    std::fs::remove_dir_all(&gone).unwrap();

    std::fs::remove_file(bin.join("rustc")).unwrap();
    std::fs::hard_link(bin.join("trustc"), bin.join("rustc")).unwrap();
    let _ = crate::compat::sweep(&l);
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
    assert!(!out.contains(" — trust build 8595: "), "{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// What an exec-root warn claims decides how the closing line counts it: no root, or a
/// shim left unrouted, is a tool that CANNOT run; a root that differs from its build, or a
/// `compat` blocked while a shim routed earlier might still run, MAY NOT; blocked with
/// nothing routed cannot; every shim routed through a matching root runs. A blocked
/// `compat` names where it is blocked.
#[test]
fn the_exec_root_claim_counts_what_cannot_run_and_what_may_not() {
    use crate::compat::{Blocked, Inspection, RootState};
    let names = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let shims = names(&["alab-tippy", "targo", "tippy", "trustc"]);
    let blocked = |at: &str, symlink: bool| {
        RootState::Blocked(Blocked {
            at: PathBuf::from(at),
            symlink,
            root: PathBuf::from("/p/compat/trust/8595"),
        })
    };
    for (label, state, unrouted, want, names_at) in [
        (
            "no root",
            RootState::Absent,
            names(&[]),
            ToolClaim::CannotRun,
            None,
        ),
        (
            "unrouted",
            RootState::Matches,
            names(&["alab-tippy", "targo", "tippy"]),
            ToolClaim::CannotRun,
            None,
        ),
        (
            "differs",
            RootState::Differs(PathBuf::from("/p/compat/trust/8595/bin/tippy")),
            names(&["targo"]),
            ToolClaim::Unproven,
            None,
        ),
        (
            "blocked, one routed earlier",
            blocked("/p/compat/trust", true),
            names(&["targo"]),
            ToolClaim::Unproven,
            Some("/p/compat/trust"),
        ),
        (
            "blocked, none routed",
            blocked("/p/compat", false),
            shims.clone(),
            ToolClaim::CannotRun,
            Some("/p/compat "),
        ),
        (
            "routed",
            RootState::Matches,
            names(&[]),
            ToolClaim::Runs,
            None,
        ),
    ] {
        let i = Inspection {
            differing: 2428,
            rustc_ino: 11,
            trustc_ino: 12,
            build_dir: PathBuf::from("/p/store/trust/8595"),
            root: PathBuf::from("/p/compat/trust/8595"),
            state,
            shims: shims.clone(),
            unrouted,
        };
        let (line, claim) = exec_root_line("doctor", 8595, &i);
        assert_eq!(claim, want, "{label}: {line}");
        if let Some(at) = names_at {
            assert!(line.contains(at), "{label}: {line}");
        }
    }
}

#[test]
fn the_trust_table_is_found_on_the_nearest_ancestor_manifest() {
    let root = synthetic_home("wstable");
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\n\n[trust]\ncompiler_timeout_secs = 1\n",
    )
    .unwrap();
    let nested = root.join("crates").join("x");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    assert_eq!(
        workspace_with_trust_table(&nested),
        Some(root.to_path_buf())
    );
    let plain = synthetic_home("wsplain");
    std::fs::write(plain.join("Cargo.toml"), "[workspace]\n").unwrap();
    assert_eq!(workspace_with_trust_table(&plain), None);
}

#[cfg(target_os = "macos")]
/// An index unreached for days (the last pass failed and the last success is that old) and
/// an index build unchanged for a month while reachable (a frozen publisher) are named
/// separately; a record whose last pass reached the index names neither, and a failure older
/// than the last success is not "unreached". The cause the last pass wrote is read back as
/// the reason.
#[test]
fn doctor_names_an_unreached_index_and_a_frozen_index_separately() {
    let l = layout("freshness");
    install(&l, "ay", 8256);
    let home = synthetic_home("freshness");
    let now = crate::flow::rfc3339_to_unix("2026-10-20T00:00:00Z").unwrap();
    let path = on_bin(&l);
    let run = |status: crate::Status| -> String {
        crate::status::write(&l, &status).unwrap();
        report_at(&l, &home, Some(&path), now, &Probes::default()).1
    };
    let base = crate::Status {
        schema: 1,
        updated_at: "2026-10-19T00:00:00Z".into(),
        enabled: true,
        index_source: "alabsystems/aterm".into(),
        outcome: "up to date".into(),
        last_success_at: "2026-10-19T00:00:00Z".into(),
        last_index_build: 32,
        index_build_changed_at: "2026-10-01T00:00:00Z".into(),
        last_pass: "ok".into(),
        last_pass_at: "2026-10-19T00:00:00Z".into(),
        ..crate::Status::default()
    };
    let (unreached, frozen) = ("has not been reached", "publishing looks frozen");
    let out = run(base.clone());
    assert!(!out.contains(unreached) && !out.contains(frozen), "{out}");
    let out = run(crate::Status {
        last_success_at: "2026-10-10T00:00:00Z".into(),
        last_pass: "failed".into(),
        ..base.clone()
    });
    assert!(
        out.contains("doctor: warn — the signed index has not been reached for 10 day(s)"),
        "{out}"
    );
    assert!(!out.contains(frozen), "unreachable is not frozen:\n{out}");
    for (last_pass, last_pass_at) in [
        ("failed", "2026-10-09T00:00:00Z"),
        ("ok", "2026-10-19T00:00:00Z"),
    ] {
        let out = run(crate::Status {
            last_success_at: "2026-10-10T00:00:00Z".into(),
            last_pass: last_pass.into(),
            last_pass_at: last_pass_at.into(),
            ..base.clone()
        });
        assert!(
            !out.contains(unreached),
            "{last_pass} at {last_pass_at}:\n{out}"
        );
    }
    let out = run(crate::Status {
        index_build_changed_at: "2026-09-10T00:00:00Z".into(),
        ..base
    });
    assert!(
        out.contains("index build 32 has not changed in 40 day(s)")
            && out.contains(frozen)
            && !out.contains(unreached),
        "a frozen publisher is named:\n{out}"
    );
    assert_eq!(
        unreached_cause(
            "up to date (index build 32) (index from cache \u{2014} dns lookup failed: \
             github.com (os error 8))"
        ),
        Some("dns lookup failed: github.com (os error 8)")
    );
    assert_eq!(
        unreached_cause("update failed: the download host refused (HTTP 403)"),
        Some("the download host refused (HTTP 403)")
    );
    assert_eq!(unreached_cause("up to date"), None);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// Warnings without a fault read "working", never "healthy"; every printed row fits
/// [`SHORT_ROW`] with its own next step kept at its end; ok rows and notes wait for
/// `--verbose`, which prints them all under the same verdict. A clean report is "healthy"; a
/// faulty one keeps its verdict and next act, and its structural rows (stderr) follow it, cut
/// the same way and whole under `--verbose`.
#[test]
fn the_report_leads_with_its_verdict_and_says_only_the_problems() {
    let unreached = format!(
        "doctor: warn — the signed index has not been reached for 10 day(s): the last pass \
         failed: {} — now: aterm pkg update",
        "x".repeat(200)
    );
    let report = format!(
        "doctor: ok — prefix /p\n\
         doctor: note — Trust builds use the ay pinned inside the trust bundle\n\
         {unreached}\n\
         doctor: 12 program(s) active\n\
         doctor: healthy\n"
    );
    let out = present(&report, "", "doctor", Detail::Problems).joined();
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].starts_with("doctor: working — 12 program(s)"),
        "{out}"
    );
    assert!(
        lines[1].starts_with("doctor: warn — the signed index")
            && lines[1].ends_with("aterm pkg update"),
        "the fact and its next step survive the cut: {out}"
    );
    assert_eq!(lines.len(), 3, "{out}");
    assert!(
        lines.iter().all(|l| l.chars().count() <= SHORT_ROW),
        "{out}"
    );
    assert!(
        !out.contains("healthy") && !out.contains("prefix /p"),
        "{out}"
    );
    let all = present(&report, "", "doctor", Detail::Everything).joined();
    assert!(
        all.starts_with("doctor: working — 12 program(s)")
            && all.contains("doctor: ok — prefix /p")
            && all.contains(&unreached)
            && !all.contains("doctor: healthy"),
        "{all}"
    );

    let clean = "status: ok — prefix /p\nstatus: 3 program(s) active\nstatus: healthy\n";
    assert!(
        present(clean, "", "status", Detail::Problems)
            .joined()
            .starts_with("status: healthy"),
    );

    let faults = format!(
        "doctor: FAIL \u{2014} trust: its bin/ shims are split across builds {} \
         \u{2014} fix: aterm pkg repair, then aterm pkg update trust\n",
        (9100..9130)
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let report = "doctor: ok \u{2014} prefix /p\n\
                  doctor: found 1 problem(s)\n\
                  doctor: next \u{2014} aterm pkg update\n";
    let shown = present(report, &faults, "doctor", Detail::Problems);
    assert_eq!(shown.verdict, "doctor: found 1 problem(s)\n");
    assert!(
        shown.faults.chars().count() <= SHORT_ROW + 1
            && shown
                .faults
                .trim_end()
                .ends_with("fix: aterm pkg repair, then aterm pkg update trust"),
        "the fact yields room to the fix: {}",
        shown.faults
    );
    assert!(
        shown
            .rest
            .contains("doctor: next \u{2014} aterm pkg update"),
        "{}",
        shown.rest
    );
    assert_eq!(
        present(report, &faults, "doctor", Detail::Everything).faults,
        faults
    );
}

/// A channel head past the local index names `aterm pkg update` only where it can land it:
/// while no signed pass has looked since the probe saw a newer index; not when the last pass
/// reached the channel and found no complete newer release (update would loop), nor when it
/// downloaded a newer index the floor never took (refused, whatever the probe says). A failed
/// check is unknown, never "newest", and a stale answer is not said as fresh.
#[test]
fn the_channel_head_line_names_update_only_where_it_can_land_the_newer_index() {
    use crate::index_probe::{CachedProbe, RangeAnswer};
    use std::time::{Duration, UNIX_EPOCH};
    let now: i64 = 1_790_000_000;
    let at = |ago: u64| UNIX_EPOCH + Duration::from_secs(now.unsigned_abs() - ago);
    let line = |floor: u64, near, reached_index_at: Option<i64>, held: Option<Vec<u64>>| {
        let mut out = Vec::new();
        index_head_line(
            &CachedProbe { floor, near },
            &LastPass {
                reached_index_at,
                held,
            },
            now,
            "doctor",
            &mut out,
        );
        String::from_utf8(out).unwrap()
    };
    let published = Some((RangeAnswer::Published, at(120)));
    let not_newest = "doctor: warn — index 42 is not the newest";
    let said = line(42, published, None, None);
    assert!(
        said.starts_with(not_newest) && said.contains("run: aterm pkg update"),
        "{said}"
    );
    let said = line(42, published, Some(now - 60), Some(vec![42, 41]));
    assert!(
        said.starts_with(not_newest) && said.contains("no complete signed release newer than 42"),
        "{said}"
    );
    for near in [published, Some((RangeAnswer::Missing, at(30))), None] {
        let said = line(42, near, Some(now - 60), Some(vec![43, 42]));
        assert!(
            said.starts_with(not_newest)
                && said.contains("index 43 was downloaded but not accepted")
                && !said.contains("run: aterm pkg update"),
            "{said}"
        );
    }
    // Landed: a cache holding 43 is not "newer than 43".
    assert!(!line(43, None, Some(now - 60), Some(vec![43, 42])).contains("not the newest"));
    let said = line(42, Some((RangeAnswer::Failed, at(60))), None, None);
    assert!(
        said.starts_with("doctor: note — ") && said.contains("unknown"),
        "{said}"
    );
    assert!(
        line(42, Some((RangeAnswer::Missing, at(30))), None, None).starts_with("doctor: ok — ")
    );
    assert!(
        line(42, Some((RangeAnswer::Missing, at(3 * 3600))), None, None)
            .starts_with("doctor: note — ")
    );
}

/// The report reads the channel head off what the last signed pass left — the index
/// builds it downloaded, written by the cache's REAL writer under the source key the real
/// fetcher uses — so a newer index downloaded and refused is said with no probe answer
/// at all, and a source the probe does not watch reports nothing.
#[test]
fn doctor_reads_a_downloaded_newer_index_off_the_last_pass() {
    let l = layout("channel-head");
    install(&l, "ay", 8256);
    let home = synthetic_home("channel-head");
    crate::status::write(
        &l,
        &crate::Status {
            schema: 1,
            updated_at: "2026-09-22T17:58:00Z".into(),
            enabled: true,
            index_source: "alabsystems/aterm".into(),
            outcome: "up to date".into(),
            last_success_at: "2026-09-22T17:58:00Z".into(),
            last_index_build: 42,
            ..crate::Status::default()
        },
    )
    .unwrap();
    std::fs::write(l.floor(), "42").unwrap();
    let candidates: Vec<crate::select::Candidate> = [43u64, 42]
        .iter()
        .map(|b| crate::select::Candidate {
            label: format!("atpkg-index-{b}"),
            index_bytes: b"i".to_vec(),
            sig: b"s".to_vec(),
            roster_bytes: b"r".to_vec(),
            roster_sig: b"rs".to_vec(),
        })
        .collect();
    crate::cache::IndexCache::for_layout(&l)
        .store(&crate::net::github_source_id("alabsystems"), &candidates);
    let now = crate::flow::rfc3339_to_unix("2026-09-22T18:00:00Z").unwrap();
    let path = on_bin(&l);
    let run = |index_head: bool| {
        report_at(
            &l,
            &home,
            Some(&path),
            now,
            &Probes {
                index_head,
                ..Probes::default()
            },
        )
    };
    let (ok, out, _) = run(true);
    assert!(
        ok && out.contains("index 43 was downloaded but not accepted"),
        "advisory, and said:\n{out}"
    );
    let (_, out, _) = run(false);
    assert!(!out.contains("not the newest"), "{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}
