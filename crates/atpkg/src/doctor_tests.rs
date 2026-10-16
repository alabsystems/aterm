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

#[test]
fn index_age_math() {
    // 0 days.
    let then = crate::flow::rfc3339_to_unix("2026-07-01T00:00:00Z").unwrap();
    assert_eq!(index_age_days("2026-07-01T00:00:00Z", then), Some(0));
    // 31 days > 30.
    assert_eq!(
        index_age_days("2026-07-01T00:00:00Z", then + 31 * 86_400),
        Some(31)
    );
    // Garbage → None.
    assert_eq!(index_age_days("not-a-date", then), None);
}

// (9) A rustup `trust` entry that resolves but is not the managed seam warns with the one
// re-point and moves nothing; a link at a numbered store build is a note; the seam itself and
// an absent entry say nothing here.
#[cfg(unix)]
#[test]
fn a_foreign_rustup_trust_entry_is_named_with_its_repoint_and_never_repointed() {
    let l = layout("foreign-seam");
    // The managed store: trust/6808 with current -> 6808.
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
    std::os::unix::fs::symlink(&dev, &entry).unwrap();
    let st = crate::seam::status(&l, &rustup, "trust");
    let line = seam_line(&st, &l).expect("a foreign link is said");
    assert!(line.starts_with("warn — "), "{line}");
    assert!(line.contains("is NOT the managed store"), "{line}");
    assert!(
        line.contains(&format!(
            "ln -sfn '{}' '{}'",
            crate::seam::seam_target(&l, "trust").display(),
            entry.display()
        )),
        "the exact re-point command, quoted — the real prefix has a space in \
         `Application Support`: {line}"
    );
    assert!(!line.contains("records the seam"), "{line}");
    // Nothing moved: the entry still points at the dev build.
    assert_eq!(std::fs::read_link(&entry).unwrap(), dev);
    // A numbered build inside the store is a note, not a warn.
    std::fs::remove_file(&entry).unwrap();
    std::os::unix::fs::symlink(&build, &entry).unwrap();
    let line = seam_line(&crate::seam::status(&l, &rustup, "trust"), &l).unwrap();
    assert!(line.starts_with("note — "), "{line}");
    assert!(line.contains("inside the store, not the view"), "{line}");
    // So is a link at `current`.
    std::fs::remove_file(&entry).unwrap();
    std::os::unix::fs::symlink(crate::seam::store_current(&l), &entry).unwrap();
    let line = seam_line(&crate::seam::status(&l, &rustup, "trust"), &l).unwrap();
    assert!(line.starts_with("note — "), "{line}");
    // The seam itself, and no entry at all: nothing to say from this line.
    std::fs::remove_file(&entry).unwrap();
    crate::seam::refresh_view(&l, "trust").unwrap();
    std::os::unix::fs::symlink(crate::seam::seam_target(&l, "trust"), &entry).unwrap();
    assert_eq!(
        seam_line(&crate::seam::status(&l, &rustup, "trust"), &l),
        None
    );
    std::fs::remove_file(&entry).unwrap();
    assert_eq!(
        seam_line(&crate::seam::status(&l, &rustup, "trust"), &l),
        None
    );
    // A real directory gets the detach fix, since nothing can be linked over it.
    std::fs::create_dir_all(entry.join("bin")).unwrap();
    let line = seam_line(&crate::seam::status(&l, &rustup, "trust"), &l).unwrap();
    assert!(line.contains("a real directory"), "{line}");
    assert!(line.contains(crate::seam::DETACH_FIX), "{line}");
    // Through the whole report, with the synthetic home carrying the foreign link:
    // the line is printed and the report is still advisory about it.
    std::fs::remove_dir_all(&entry).unwrap();
    std::os::unix::fs::symlink(&dev, &entry).unwrap();
    if rustup_binary(Some(&home)).is_some_and(|bin| rustup_present(&bin))
        && std::env::var_os("RUSTUP_HOME").is_none()
    {
        let mut out = Vec::new();
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut std::io::sink(),
        );
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("is NOT the managed store"),
            "the report carries the seam line:\n{text}"
        );
        assert_eq!(
            std::fs::read_link(&entry).unwrap(),
            dev,
            "doctor moved nothing"
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

// (9) The heal, as doctor says it: the unattended re-assertion (`seam::reassert`) re-points
// rustup's `trust` from an older live build tree to the store's view, and doctor's note names
// the old target and the `ln -sfn` that puts it back. Run as printed, it puts the link back,
// the line says no pass will move it again, and the next pass does not.
#[cfg(unix)]
#[test]
fn a_stale_link_the_pass_replaced_is_said_with_the_way_back() {
    let l = layout("replaced-seam");
    let home = synthetic_home("replaced-seam");
    let rustup = home.join(".rustup");
    std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
    // One dated compiler script, hard-linked wherever a compiler goes and run once here:
    // macOS assesses a new executable on its first exec, which can outlast the probe bound.
    let script = home.join("dated-compiler");
    std::fs::write(
        &script,
        "#!/bin/sh\nd=$(cat \"$(dirname \"$0\")/../commit-date\")\n\
         echo \"rustc 1.99.0-dev (0000000 $d)\"\necho \"commit-date: $d\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::process::Command::new(&script).output();
    let dated = |sysroot: &Path, name: &str, date: &str| {
        std::fs::create_dir_all(sysroot.join("bin")).unwrap();
        std::fs::hard_link(&script, sysroot.join("bin").join(name)).unwrap();
        std::fs::write(sysroot.join("commit-date"), date).unwrap();
    };
    let build = l.build_dir("trust", 9192);
    std::fs::create_dir_all(build.join("lib").join("rustlib")).unwrap();
    dated(&build, "trustc", "2026-09-17");
    for tool in ["targo", "trustdoc", "tippy"] {
        std::fs::write(build.join("bin").join(tool), tool).unwrap();
    }
    crate::activate::atomic_symlink(&build, &crate::seam::store_current(&l)).unwrap();
    let stage2 = home.join("trust/build/host/stage2");
    dated(&stage2, "rustc", "2026-08-20");
    let entry = rustup.join("toolchains/trust");
    std::os::unix::fs::symlink(&stage2, &entry).unwrap();
    assert_eq!(
        replaced_seam_line(&l, &rustup),
        None,
        "nothing replaced yet"
    );

    let lines = crate::seam::reassert(&l, &rustup);
    assert!(
        lines.len() == 1 && lines[0].contains("to put it back"),
        "{lines:?}"
    );
    let line = replaced_seam_line(&l, &rustup).expect("the heal is said");
    let undo = format!("ln -sfn '{}' '{}'", stage2.display(), entry.display());
    assert!(
        line.starts_with("note — rustup `trust` was re-pointed at the store's view")
            && line.contains(&undo),
        "{line}"
    );
    // The whole report (`--verbose`) carries it and the default one does not list it: a
    // note. The report resolves the rustup home from RUSTUP_HOME first, so under `rustup run
    // trust …` (which exports it) it would read the machine's link, not this fixture's.
    if std::env::var_os("RUSTUP_HOME").is_none() {
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let (_, report, faults) = whole_report(&l, &home, &path);
        let said = format!("doctor: {line}");
        assert!(report.lines().any(|l| l == said), "{report}");
        let shown = present(&report, &faults, "doctor", Detail::Problems);
        assert!(!shown.joined().contains(&said), "{}", shown.joined());
    }

    let out = std::process::Command::new("ln")
        .arg("-sfn")
        .arg(&stage2)
        .arg(&entry)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let line = replaced_seam_line(&l, &rustup).expect("the undo is said");
    assert!(
        line.starts_with("note — ") && line.contains("no unattended pass re-points it"),
        "{line}"
    );
    let lines = crate::seam::reassert(&l, &rustup);
    assert!(lines[0].contains("put back since"), "{lines:?}");
    assert_eq!(std::fs::read_link(&entry).unwrap(), stage2);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A held toolchain update and a lease are said (what waits, on whom, until when, and the
/// command that ends it; the leased build and its holder), and a healthy store stays healthy.
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
    let lease = crate::lease::take(
        &l.prefix,
        &crate::lease::Subject::build("trust", 9192).unwrap(),
        "aterm-release (pid 7) \u{2014} a release cut in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let mut out = Vec::new();
    assert!(
        run_with(
            &l,
            Some(&home),
            Some(&path),
            t0 + 600,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut std::io::sink(),
        ),
        "a held update and a lease are states, not faults"
    );
    let report = String::from_utf8_lossy(&out);
    for part in [
        "doctor: note \u{2014} the Trust toolchain update (trust build 9192 \u{2192} 9200) is \
         staged and waiting since",
        "/w/target/debug/build/x/build-script-build is running from it",
        "`aterm pkg update` installs it now",
        "doctor: note \u{2014} trust build 9192 is in use by aterm-release (pid 7) \u{2014} \
         a release cut in /w; it stays as it is until that run ends",
    ] {
        assert!(report.contains(part), "{part}\n---\n{report}");
    }
    // A flip held by that lease names it in the lease row's words: one lease, one
    // vocabulary, in both rows.
    let subject = crate::lease::Subject::build("trust", 9192).unwrap();
    assert!(crate::quiet::note_deferred(
        &l,
        "rustc",
        t0,
        t0,
        &crate::quiet::Busy::Leased {
            what: subject.describe(),
            holders: crate::lease::holders(&l.prefix, &subject),
        },
        &crate::quiet::Moves {
            from: std::collections::BTreeMap::from([("trust".to_string(), 9192u64)]),
            to: std::collections::BTreeMap::from([("trust".to_string(), 9200u64)]),
            assets: std::collections::BTreeMap::new(),
        },
    ));
    let held = "trust build 9192 is in use by aterm-release (pid 7) \u{2014} a release \
                cut in /w";
    let lines = hold_lines(&l, t0);
    assert_eq!(
        lines.iter().filter(|line| line.contains(held)).count(),
        2,
        "{lines:?}"
    );
    drop(lease);
    // A lease on the rustup view is kept by the seam, not by gc, and says so.
    let view = crate::lease::take(
        &l.prefix,
        &crate::lease::Subject::view("trust").unwrap(),
        "targo (pid 8) \u{2014} held by `aterm pkg lease` in /w",
        crate::lease::DEFAULT_WAIT,
    )
    .unwrap();
    assert!(
        hold_lines(&l, t0).iter().any(|line| line
            == "note \u{2014} the rustup trust toolchain is in use by targo (pid 8) \u{2014} \
                held by `aterm pkg lease` in /w; \
                it stays as it is until that run ends"),
        "{:?}",
        hold_lines(&l, t0)
    );
    drop(view);
    // A lease that cannot be read names no run: it is kept until the lease reads.
    assert_eq!(
        lease_line(
            &subject,
            &crate::lease::Holders::Unknown("permission denied (os error 13)".into())
        )
        .as_deref(),
        Some(
            "note \u{2014} trust build 9192 is treated as in use (its lease is unreadable: \
             permission denied (os error 13)); it stays as it is until its lease can be read"
        )
    );
    assert_eq!(lease_line(&subject, &crate::lease::Holders::Free), None);
    crate::quiet::clear(&l);
    assert!(hold_lines(&l, t0).is_empty(), "nothing held, nothing said");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A held-update record that outlived its builds is said as that, not as a wait, and left as
/// it was; the same record with its build in the store is the negative control.
#[test]
fn a_held_update_record_that_outlived_its_builds_is_said_as_that_and_left() {
    let l = layout("held-flip-outlived");
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
    let lines = hold_lines(&l, t0 + 60);
    assert!(
        lines.len() == 1 && lines[0].contains("is staged and waiting since"),
        "the negative control: a record whose build stands is a wait: {lines:?}"
    );

    // The builds go by hand; the record stays behind.
    std::fs::remove_dir_all(l.prefix.join("store")).unwrap();
    let record = std::fs::read(crate::quiet::record_path(&l)).unwrap();
    let outlived = vec![
        "note \u{2014} the record of a held Trust toolchain update (trust build 9192 \
         \u{2192} 9200) outlived every build it moves off, so nothing waits for it; \
         `aterm pkg repair` ends it, as the next update pass does"
            .to_string(),
    ];
    for _ in 0..2 {
        assert_eq!(hold_lines(&l, t0 + 60), outlived);
        assert_eq!(
            std::fs::read(crate::quiet::record_path(&l)).unwrap(),
            record,
            "doctor changes nothing"
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// (11) The C toolchain: an answer (none, or one that refuses) is a FAIL whose next act is
/// the platform's installer, outranking `update`; an answering toolchain is `ok`, an unknown
/// warns, and no rustup is a note.
#[test]
fn doctor_fails_when_the_c_toolchain_cannot_build() {
    let l = layout("cc-prereq");
    install(&l, "ay", 18);
    let home = synthetic_home("cc-prereq");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let report = |cc: crate::prereq::CcVerdict| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let healthy = run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            &Probes {
                cc: Some(cc),
                ..Probes::default()
            },
            &mut out,
            &mut err,
        );
        (
            healthy,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    };
    let (healthy, out, err) = report(crate::prereq::CcVerdict::NoDriver);
    assert!(!healthy, "{out}{err}");
    assert!(err.contains("FAIL — no C compiler (`cc`) on PATH"), "{err}");
    assert!(out.contains("found 1 problem(s)"), "{out}");
    assert!(
        out.contains(&format!("next — {}", crate::prereq::act())),
        "the prerequisite's act, not `update`: {out}"
    );
    // A compiler that is there and refuses: a FAIL all the same, but on macOS the
    // install act would only say the tools are installed, so no `next` names it.
    let (healthy, out, err) = report(crate::prereq::CcVerdict::Broken {
        driver: PathBuf::from("/opt/cc/bin/cc"),
        why: "stdio.h: No such file or directory".to_string(),
    });
    assert!(!healthy, "{out}{err}");
    assert!(
        err.contains("FAIL — the C compiler at /opt/cc/bin/cc cannot build a program"),
        "{err}"
    );
    assert_eq!(
        out.contains(&format!("next — {}", crate::prereq::act())),
        !cfg!(target_os = "macos"),
        "{out}"
    );
    assert!(!out.contains("next — aterm pkg update"), "{out}");
    let (healthy, out, _) = report(crate::prereq::CcVerdict::Ready {
        driver: PathBuf::from("/opt/cc/bin/clang"),
    });
    assert!(healthy, "{out}");
    assert!(
        out.contains("ok — C toolchain: /opt/cc/bin/clang builds a C program"),
        "{out}"
    );
    // A probe that could not run (no private temp directory) is unknown: a warn,
    // never the FAIL a broken compiler is.
    let (healthy, out, _) = report(crate::prereq::CcVerdict::Unproven {
        driver: PathBuf::from("/opt/cc/bin/cc"),
        why: "no private directory under /ro: read-only".to_string(),
    });
    assert!(healthy, "an unproven compiler is not a problem: {out}");
    assert!(
        out.contains(
            "warn — the C compiler at /opt/cc/bin/cc answers, but the build probe could not run"
        ),
        "{out}"
    );
    let (healthy, out, _) = report(crate::prereq::CcVerdict::Unanswered {
        driver: PathBuf::from("/opt/cc/bin/cc"),
    });
    assert!(healthy, "an unknown is not a problem: {out}");
    assert!(
        out.contains("warn — the C compiler at /opt/cc/bin/cc did not answer"),
        "{out}"
    );
    if rustup_binary(Some(&home)).is_none() {
        assert!(
            out.contains("note — no rustup") && !out.contains("warn — rustup not found"),
            "{out}"
        );
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
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let mut out = Vec::new();
    assert!(run_with(
        &l,
        Some(&home),
        Some(&path),
        0,
        None,
        "doctor",
        &Probes::default(),
        &mut out,
        &mut std::io::sink(),
    ));
    let report = String::from_utf8_lossy(&out);
    assert!(report.contains("ay: dev-linked from"), "{report}");
    assert!(report.contains("1 program(s) active"), "{report}");
    assert!(
        !report.contains("no ALab programs are installed"),
        "{report}"
    );
    assert!(
        crate::ops::active_builds(&l).is_empty(),
        "no fabricated signed build"
    );

    std::fs::remove_file(binary).unwrap();
    assert!(live_dev_links(&l).0.is_empty());
    assert!(!run_with(
        &l,
        Some(&home),
        Some(&path),
        0,
        None,
        "doctor",
        &Probes::default(),
        &mut Vec::new(),
        &mut std::io::sink(),
    ));
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

/// (3b) Reroute: each stub's state and PATH order (upstream `~/.cargo/bin` ahead of the
/// reroute dir), all advisory: the warn leaves a healthy toolset healthy.
#[cfg(unix)]
#[test]
fn reroute_section_reports_stub_state_and_path_order() {
    let l = layout("reroute");
    install(&l, "ay", 18);
    let home = synthetic_home("reroute");
    // A fake upstream cargo outside the prefix, so the walk counts it (one under the prefix
    // would be the stub counting itself).
    let upstream = crate::testdir::dir("atpkg-doctor-reroute-upstream-");
    std::fs::write(upstream.join("cargo"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        upstream.join("cargo"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let run = |entries: &[PathBuf]| {
        let path = std::env::join_paths(entries).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let ok = run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut std::io::sink(),
        );
        (ok, String::from_utf8_lossy(&out).into_owned())
    };
    // Nothing laid and the reroute dir absent from PATH (a shell outside a session):
    // every row missing as a warning, the order line a note, the toolset still healthy.
    let (ok, out) = run(&[l.bin_dir(), upstream.clone()]);
    assert!(ok, "missing reroute stubs are advisory:\n{out}");
    for row in crate::reroute::TABLE {
        assert!(
            out.contains(&format!(
                "doctor: warn — reroute stub {} missing; the next aterm session lays it \
                 — now: aterm pkg repair",
                row.upstream
            )),
            "{out}"
        );
    }
    assert!(
        out.contains(
            "doctor: note — reroute dir not on this PATH (expected outside an aterm session)"
        ),
        "{out}"
    );
    crate::reroute::lay(&l).unwrap();
    let reroute = l.reroute_dir();
    // Upstream ahead of the reroute dir: a warning, exit 0.
    let (ok, out) = run(&[upstream.clone(), reroute.clone(), l.bin_dir()]);
    assert!(ok, "a mis-ordered PATH is advisory:\n{out}");
    assert!(
        out.contains("doctor: ok — reroute stub cargo laid"),
        "{out}"
    );
    assert!(
        out.contains(
            "doctor: warn — upstream cargo (PATH[0]) is ahead of the reroute dir \
             (PATH[1]) in this shell; a new aterm tab puts the reroute dir first"
        ),
        "{out}"
    );
    // The reroute dir first: the order a session guarantees.
    let (ok, out) = run(&[reroute.clone(), upstream.clone(), l.bin_dir()]);
    assert!(ok, "{out}");
    assert!(
        out.contains("doctor: ok — reroute dir precedes upstream cargo (PATH[0] < PATH[1])"),
        "{out}"
    );
    // On PATH with no upstream cargo anywhere: said, not warned.
    let (_, out) = run(&[reroute.clone(), l.bin_dir()]);
    assert!(
        out.contains("doctor: ok — reroute dir on PATH (PATH[0]); no upstream cargo on this PATH"),
        "{out}"
    );
    // A foreign occupant of a row's name is named by path and never claimed.
    std::fs::write(reroute.join("z3"), "#!/bin/sh\nexit 0\n").unwrap();
    let (ok, out) = run(&[reroute.clone(), upstream.clone(), l.bin_dir()]);
    assert!(ok, "{out}");
    assert!(
        out.contains(&format!(
            "doctor: warn — {} is not ours (foreign file; never touched)",
            reroute.join("z3").display()
        )),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&upstream);
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

/// On Windows an unserved platform is a note, not a problem, unless a served member's row
/// records a failure; elsewhere it stays a problem. Pins both sides of
/// [`toolset_unserved_here`].
#[test]
fn an_unserved_platform_is_a_note_not_a_problem_on_windows_and_unchanged_elsewhere() {
    // Nothing served is missing: healthy on Windows, a problem elsewhere.
    let l = unserved_platform_store("unserved-quiet", false);
    let (healthy, out) = report_over(&l, "unserved-quiet");
    if cfg!(windows) {
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
    if cfg!(windows) {
        assert!(
            out.contains(
                "PROBLEM — no ALab programs are installed (claude: held: last attempt \
                 failed with"
            ),
            "{out}"
        );
        assert!(!out.contains("--default-set"), "nothing impossible: {out}");
    } else {
        // Off Windows the architecture row is still the first problem named; the held
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
    // rows remain, and one program is active. On Windows the note stands (it keys on the
    // rows that last) and the report is healthy; elsewhere nothing is said of the platform.
    let l = unserved_platform_store("unserved-after", false);
    clear_row(&l, "*toolset*");
    install(&l, "claude", 2026092201);
    let (healthy, out) = report_over(&l, "unserved-after");
    assert!(healthy, "{out}");
    assert!(out.contains("1 program(s) active"), "{out}");
    assert!(!out.contains("PROBLEM"), "{out}");
    assert_eq!(
        out.contains("unavailable on this platform"),
        cfg!(windows),
        "the note is Windows-only: {out}"
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
    // The per-member rows still say what they say: on Windows, the note.
    assert_eq!(
        out.contains("unavailable on this platform"),
        cfg!(windows),
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

/// The rustup the report looks for is the platform's executable spelling: `rustup.exe`
/// on Windows, where a bare `rustup` is never a file.
#[test]
fn the_rustup_probe_uses_the_platform_executable_spelling() {
    assert_eq!(
        rustup_file_name(),
        format!("rustup{}", crate::platform::EXE_SUFFIX)
    );
    let home = synthetic_home("rustup-exe");
    let bin = home.join(".cargo").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join(rustup_file_name()), b"").unwrap();
    // `PATH` and `CARGO_HOME` are process-global and unsafe to set in edition 2024;
    // the `~/.cargo/bin` fallback is the arm the app-spawned pass takes.
    let found = rustup_binary(Some(&home));
    assert!(
        found
            .as_deref()
            .is_some_and(|p| p.ends_with(rustup_file_name()))
            || std::env::var_os("PATH").is_some_and(|_| found.is_some()),
        "the fallback must find the platform's rustup file: {found:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A stray `-`-prefixed row (`[programs.--help]`) is not a missing program; the second case
/// shows the scan is narrowed to stray flags, not disabled.
#[test]
fn a_stray_flag_row_is_not_a_missing_program() {
    let stray_row = |name: &str| {
        let l = layout(&format!("stray-{}", name.trim_start_matches('-')));
        install(&l, "ay", 18);
        let existing = crate::status::read(&l).unwrap_or_default();
        let mut programs = existing.programs;
        programs.insert(
            "ay".to_string(),
            crate::ProgramStatus {
                installed_build: Some(18),
                state: "active".into(),
                tree_root: String::new(),
            },
        );
        programs.insert(
            name.to_string(),
            crate::ProgramStatus {
                installed_build: None,
                state: format!("error: {name} is not named in the signed index"),
                tree_root: String::new(),
            },
        );
        crate::status::write(
            &l,
            &crate::Status {
                programs,
                ..existing
            },
        )
        .unwrap();
        l
    };

    let l = stray_row("--help");
    let home = synthetic_home("stray-help");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    assert!(
        run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut std::io::sink()
        ),
        "a mistyped flag left in the record is a stray row, not a missing program"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);

    // A plausible program name in the same error state still fails: narrowed, not disabled.
    let l = stray_row("trust-vc");
    let home = synthetic_home("stray-real");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    assert!(
        !run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut std::io::sink()
        ),
        "a real program name in an error state is still a problem doctor must report"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// `aborted: <phase>` (a coherence-group transaction killed mid-flight) and a yanked pin's
/// tombstone (stub shims every other check skips) are faults.
#[test]
fn an_aborted_transaction_is_a_problem_doctor_can_see() {
    assert!(
        is_problem_state("aborted: activate"),
        "a killed coherence-group transaction is a fault, not an informational state"
    );
    for fault in [
        "error: x",
        "unavailable: y",
        "blocked: z",
        "tombstoned: pin yanked/below floor",
    ] {
        assert!(is_problem_state(fault), "{fault} stays a fault");
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
    let l = layout("multi-problem");
    install(&l, "ay", 18);
    let existing = crate::status::read(&l).unwrap_or_default();
    let mut programs = existing.programs;
    for (name, state) in [
        ("ay", "active"),
        ("clean", "error: no build for this Mac"),
        ("trust", "blocked: disk full"),
        ("ty", "aborted: activate"),
    ] {
        programs.insert(
            name.to_string(),
            crate::ProgramStatus {
                installed_build: None,
                state: state.into(),
                tree_root: String::new(),
            },
        );
    }
    crate::status::write(
        &l,
        &crate::Status {
            programs,
            ..existing
        },
    )
    .unwrap();

    let home = synthetic_home("multi-problem");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    assert!(
        !run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut std::io::sink()
        ),
        "three recorded faults must fail the health verdict"
    );

    // Against the real collector: a `.find()` would return only `clean`.
    let status = crate::status::read(&l).expect("status present");
    assert_eq!(
        recorded_problems(Some(&status)),
        vec![
            "clean: error: no build for this Mac".to_string(),
            "trust: blocked: disk full".to_string(),
            "ty: aborted: activate".to_string(),
        ],
        "all three faults are reported — one error, one blocked, one aborted"
    );
    // The healthy member is not swept up in the reporting.
    assert!(
        !recorded_problems(Some(&status))
            .iter()
            .any(|p| p.starts_with("ay:")),
        "an active program is not a problem"
    );

    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A toolset the user removed is not broken.
#[test]
fn a_declined_store_is_healthy_while_empty() {
    let l = layout("declined-store");
    let home = synthetic_home("declined-store");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    std::fs::create_dir_all(&l.prefix).unwrap();
    std::fs::write(l.declined(), b"# removed on purpose\n").unwrap();
    let mut out = Vec::new();
    assert!(
        run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut std::io::sink()
        ),
        "a deliberate removal is a healthy state, not a fault"
    );
    // Nothing lays a reroute stub on a declined store, so a missing one is a note (out
    // of the default view and the problem count), never the warn that promises a lay.
    let out = String::from_utf8_lossy(&out);
    if !cfg!(windows) {
        assert!(
            out.contains(
                "doctor: note — reroute stub cargo not laid: the ALab toolset was removed \
                 on this machine"
            ),
            "{out}"
        );
    }
    assert!(!out.contains("reroute stub cargo missing"), "{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn broken_bin_symlink_fails() {
    let l = layout("broken");
    install(&l, "ay", 18);
    // Add a shim pointing at a nonexistent target (a dangling symlink on Unix, a `.cmd`
    // forwarding to a missing exe on Windows) via the same primitive a real install uses.
    let ghost = tool("ghost");
    crate::platform::install_shim(&l.build_dir("ay", 99).join("bin"), &ghost, &l.shim(&ghost))
        .unwrap();
    let home = synthetic_home("broken");
    assert!(
        !run_with(
            &l,
            Some(&home),
            None,
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut std::io::sink()
        ),
        "a dangling bin symlink is structural"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A shim whose target is there but unreadable (`Path::exists()` says `false`) is not a
/// broken shim: a warn naming the shim, the target and the errno.
#[cfg(unix)]
#[test]
fn a_shim_whose_target_cannot_be_stat_ed_is_not_reported_broken() {
    use std::os::unix::fs::PermissionsExt as _;
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
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let _ = run_with(
        &l,
        Some(&home),
        None,
        0,
        None,
        "doctor",
        &Probes::default(),
        &mut out,
        &mut err,
    );
    std::fs::set_permissions(&holder, saved).unwrap();
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        armed,
        "the fixture must make the stat fail, or it proves nothing"
    );
    let err = String::from_utf8_lossy(&err).into_owned();
    assert!(
        !err.contains("broken bin shim"),
        "a shim we could not look at was condemned as broken: {err}"
    );
    let out = String::from_utf8_lossy(&out).into_owned();
    assert!(
        out.contains("could not be checked"),
        "a shim the scan could not look at must be reported, not skipped: {out}"
    );
}

#[test]
fn active_build_with_missing_store_fails() {
    let l = layout("missing-store");
    install(&l, "ay", 18);
    // The shim resolves, but the completeness marker is gone (check 5 vs check 4).
    crate::store::discard_build(&l.build_dir("ay", 18));
    // Re-create just the bin so the shim isn't dangling (isolate check 5 from check 4).
    std::fs::create_dir_all(l.build_dir("ay", 18).join("bin")).unwrap();
    std::fs::write(
        l.build_dir("ay", 18)
            .join("bin")
            .join(tool("ay").exe_file()),
        b"#!/bin/true\n",
    )
    .unwrap();
    assert!(!crate::store::build_is_complete(&l.build_dir("ay", 18)));
    let home = synthetic_home("missing-store");
    assert!(
        !run_with(
            &l,
            Some(&home),
            None,
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut std::io::sink()
        ),
        "an incomplete active build is structural"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn stray_posix_sh_in_shell_d_fails() {
    let l = layout("stray-sh");
    install(&l, "ay", 18);
    let home = synthetic_home("stray-sh");
    let shell_d = home.join(".aterm/shell.d");
    std::fs::create_dir_all(&shell_d).unwrap();
    std::fs::write(shell_d.join("00-atpkg.sh"), b"echo stray\n").unwrap();
    assert!(
        !run_with(
            &l,
            Some(&home),
            None,
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut std::io::sink()
        ),
        "a fish-breaking stray .sh is structural"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// Each rc file in the pass's five states (wired, sourced by another line, opted out,
/// consent-fenced, not yet wired); when none reaches `bin/`, every file read is named.
#[cfg(unix)]
#[test]
fn rc_wiring_is_reported_in_all_five_states() {
    let l = layout("rc-wiring");
    install(&l, "ay", 18);
    let home = synthetic_home("rc-wiring");
    let shell_d = home.join(".aterm/shell.d");
    std::fs::create_dir_all(&shell_d).unwrap();
    let zshrc = home.join(".zshrc");
    let report = |home: &Path| {
        let mut out = Vec::new();
        let _ = run_with(
            &l,
            Some(home),
            None,
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut std::io::sink(),
        );
        String::from_utf8(out).unwrap()
    };

    // No rc at all: the report says the consequence for the files it read, named, never for
    // the machine as a whole.
    let none = report(&home);
    assert!(
        none.contains(
            "none of ~/.zshrc, ~/.bashrc, ~/.bash_profile, ~/.config/fish/config.fish, \
             ~/.bash_login, ~/.profile sources ~/.aterm/shell.d"
        ) && none.contains("`aterm <tool>`"),
        "no rc ⇒ the six files read are named, with the way in: {none}"
    );
    assert!(
        !none.contains("aterm-only"),
        "six files read are not the whole machine: {none}"
    );

    // An rc that exists but was never wired.
    std::fs::write(&zshrc, "export FOO=1\n").unwrap();
    let unwired = report(&home);
    assert!(
        unwired.contains("~/.zshrc does not source ~/.aterm/shell.d")
            && unwired.contains("`aterm pkg repair` appends atpkg's block"),
        "unwired names the pass that wires it: {unwired}"
    );

    // Wired by a real pass, so the ledger entry the opt-out state needs is the pass's own.
    let pass = crate::hooks::pass_at(&l, &home, crate::hooks::RcWiring::HonorOptOut);
    assert_eq!(
        pass.rc(),
        [(".zshrc", crate::hooks::RcOutcome::Appended)],
        "the pass wired ~/.zshrc"
    );
    let wired = report(&home);
    assert!(
        wired.contains("ok — ~/.zshrc sources ~/.aterm/shell.d")
            && wired.contains("delete it to opt out — `aterm pkg repair` puts it back"),
        "wired names the rc, the one-motion opt-out, and the one verb that undoes it: \
         {wired}"
    );
    assert!(
        !wired.contains("none of ~/"),
        "a wired rc reaches the toolchain: {wired}"
    );

    // Opted out: recorded as wired, block deleted.
    std::fs::write(&zshrc, "export FOO=1\n").unwrap();
    let opted = report(&home);
    assert!(
        opted.contains("~/.zshrc: atpkg's block was deleted — opted out")
            && opted.contains("aterm pkg repair"),
        "opted-out is honoured and names the one verb that re-wires: {opted}"
    );

    // Sourced by install.sh's `wire_shell_path` marker pair (the same hook), on top of the
    // opt-out ledger entry above: the toolchain reaches this shell, so the rc is not unwired.
    std::fs::write(
        &zshrc,
        format!(
            "export FOO=1\n# >>> aterm ALab toolset (managed by install.sh) >>>\n\
             if [ -f \"{h}\" ]; then . \"{h}\"; fi\n# <<< aterm ALab toolset <<<\n",
            h = shell_d.join("00-atpkg.zsh").display()
        ),
    )
    .unwrap();
    let foreign = report(&home);
    assert!(
        foreign.contains(
            "ok — ~/.zshrc sources ~/.aterm/shell.d via a line that is not atpkg's block"
        ),
        "install.sh's block is reported as what it is: {foreign}"
    );
    assert!(
        !foreign.contains("none of ~/")
            && !foreign.contains("does not source")
            && !foreign.contains("opted out"),
        "a shell that reaches the toolchain is neither unreached nor unwired: {foreign}"
    );

    // Consent-fenced: the rc resolves under a folder macOS guards, so no pass opens it and
    // neither does this report; the state tells the user why the rc stays untouched.
    let docs = home.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("zshrc"), "export FOO=1\n").unwrap();
    std::fs::remove_file(&zshrc).unwrap();
    std::os::unix::fs::symlink(docs.join("zshrc"), &zshrc).unwrap();
    let fenced = report(&home);
    assert!(
        fenced.contains(
            "~/.zshrc resolves under a folder macOS guards with a consent \
             dialog, so no atpkg pass opens it"
        ),
        "the fenced rc is named, with why: {fenced}"
    );
    assert!(
        !fenced.contains("does not source"),
        "doctor did not read it, so it claims nothing about its contents: {fenced}"
    );
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".zshrc", crate::hooks::RcState::ConsentFenced)]
    );

    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// install.sh's `path_block_rc_target` elects ~/.bash_profile, then ~/.bash_login, then
/// ~/.profile (Terminal.app starts a login bash); atpkg has a row for the first only, so the
/// other two are read for a shell.d line too. Content is read before the ledger, and a
/// profile that does not source shell.d is not listed.
#[cfg(unix)]
#[test]
fn an_install_sh_block_in_a_login_profile_is_reported_as_sourcing_shell_d() {
    let l = layout("rc-login-profile");
    install(&l, "ay", 18);
    let home = synthetic_home("rc-login-profile");
    let shell_d = home.join(".aterm/shell.d");
    std::fs::create_dir_all(&shell_d).unwrap();
    // install.sh's block, in the shape `wire_shell_path` writes it for bash.
    let block = |hook: &str| {
        format!(
            "export FOO=1\n\n# >>> aterm ALab toolset (managed by install.sh) >>>\n\
             if [ -f \"{h}\" ]; then . \"{h}\"; fi\n# <<< aterm ALab toolset <<<\n",
            h = shell_d.join(hook).display()
        )
    };
    let report = |home: &Path| {
        let mut out = Vec::new();
        let _ = run_with(
            &l,
            Some(home),
            None,
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut std::io::sink(),
        );
        String::from_utf8(out).unwrap()
    };

    // An atpkg row carrying install.sh's block, not atpkg's.
    std::fs::write(home.join(".bash_profile"), block("00-atpkg.bash")).unwrap();
    // A login profile that does not source shell.d is not reported.
    std::fs::write(home.join(".profile"), "export BAR=1\n").unwrap();
    let out = report(&home);
    assert!(
        out.contains(
            "ok — ~/.bash_profile sources ~/.aterm/shell.d via a line that is not \
             atpkg's block"
        ),
        "install.sh's block in an atpkg row is seen for what it is: {out}"
    );
    assert!(
        !out.contains("none of ~/") && !out.contains("aterm-only"),
        "a machine whose login bash reaches the toolchain is not reported unreached: \
         {out}"
    );
    assert!(
        !out.contains("~/.profile"),
        "a profile atpkg does not wire and that does not source shell.d is not listed: \
         {out}"
    );
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".bash_profile", crate::hooks::RcState::SourcedElsewhere)]
    );

    // The fall-through profile atpkg has no row for: the same block, the same answer.
    std::fs::remove_file(home.join(".bash_profile")).unwrap();
    std::fs::write(home.join(".bash_login"), block("00-atpkg.bash")).unwrap();
    let out = report(&home);
    assert!(
        out.contains(
            "ok — ~/.bash_login sources ~/.aterm/shell.d via a line that is not \
             atpkg's block"
        ),
        "an install.sh-only profile is read, and reported: {out}"
    );
    assert!(
        !out.contains("none of ~/"),
        "it reaches the toolchain: {out}"
    );
    assert_eq!(
        crate::hooks::rc_wiring(&home),
        vec![(".bash_login", crate::hooks::RcState::SourcedElsewhere)]
    );

    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A shim and the channel naming different builds is structural: something on PATH runs a
/// build the channel does not select, and gc stops reclaiming that program.
#[test]
fn a_shim_disagreeing_with_the_channel_is_structural() {
    let l = layout("witness-mismatch");
    install(&l, "ay", 19); // channel + shim both at 19
    // Stage 18 complete on disk (so check 5 stays quiet) and re-point only the shim.
    let older = install_build_tree(&l, "ay", 18);
    crate::store::mark_build_ready(&older).unwrap();
    let ay = tool("ay");
    crate::platform::install_shim(&older.join("bin"), &ay, &l.shim(&ay)).unwrap();
    let home = synthetic_home("witness-mismatch");
    assert!(
        !run_with(
            &l,
            Some(&home),
            None,
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut std::io::sink()
        ),
        "channel says 19, shims say 18 — structural"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A program with no channel witness is not breakage (gc just abstains), so it warns, exit 0.
#[test]
fn a_program_with_no_channel_witness_warns_but_exit_zero() {
    let l = layout("witness-absent");
    install_build_tree(&l, "ay", 18);
    let dir = l.build_dir("ay", 18);
    install_shims(&l, &dir, &["ay".to_string()], crate::activate::Aliases::Off).unwrap(); // shimmed, never activated
    crate::store::mark_build_ready(&dir).unwrap();
    let home = synthetic_home("witness-absent");
    assert!(
        run_with(
            &l,
            Some(&home),
            None,
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut std::io::sink()
        ),
        "an un-witnessed program is advisory, not structural"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn bin_not_on_path_warns_but_exit_zero() {
    let l = layout("notonpath");
    install(&l, "ay", 18);
    let home = synthetic_home("notonpath");
    // PATH without the managed bin/ → a warning, not a structural fail.
    let path = std::ffi::OsString::from("/usr/bin:/bin");
    assert!(
        run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            &Probes::default(),
            &mut std::io::sink(),
            &mut std::io::sink()
        ),
        "a PATH warning stays exit-0"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A `satisfied by system` row is an `ok` while the system binary is there, a warn (never a
/// problem) once it is gone, and never a recorded fault.
#[test]
fn a_system_satisfied_member_is_reported_and_never_a_fault() {
    let l = layout("system-satisfied");
    install(&l, "ay", 19);
    let exe = l.prefix.join("fake-system-gh");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    let existing = crate::status::read(&l).unwrap_or_default();
    let mut programs = existing.programs.clone();
    programs.insert(
        "gh".into(),
        crate::ProgramStatus {
            installed_build: None,
            state: crate::state::system(&exe, Some("2026-08-27")),
            tree_root: String::new(),
        },
    );
    crate::status::write(
        &l,
        &crate::Status {
            schema: 1,
            updated_at: "2026-08-27T00:00:00Z".into(),
            enabled: true,
            index_source: "alabsystems/aterm".into(),
            outcome: "up to date".into(),
            seams: Vec::new(),
            last_success_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            last_pass: String::new(),
            last_pass_at: String::new(),
            last_pass_attempted_index_build: 0,
            last_pass_attempted_at: String::new(),
            pass_seq: 0,
            stale_index_only_pass_seq: 0,
            programs,
            extra: Default::default(),
        },
    )
    .unwrap();
    let home = synthetic_home("system-satisfied");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
    let run = |l: &Layout| {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut err,
        );
        (ok, String::from_utf8_lossy(&out).into_owned())
    };
    let (ok, out) = run(&l);
    assert!(ok, "a satisfied member is not a problem:\n{out}");
    // The words the pass wrote: the canonical state, retirement note included.
    assert!(
        out.contains(&format!(
            "doctor: ok — gh: system: {} — not managed by aterm (managed copy retired \
             2026-08-27)",
            exe.display()
        )),
        "{out}"
    );
    assert!(recorded_problems(crate::status::read(&l).as_ref()).is_empty());
    // The system binary goes away: a warning naming the remedy, still not a problem.
    std::fs::remove_file(&exe).unwrap();
    let (ok, out) = run(&l);
    assert!(ok, "{out}");
    assert!(
        out.contains("doctor: warn — gh: recorded as `system: ")
            && out.contains("that copy is gone — the next `aterm pkg update` reinstalls"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A member held on its current build is an `ok` quoting its row, on the owner's row and on
/// a sibling's: never a fault, never silent.
#[test]
fn a_held_member_is_an_ok_line_quoting_its_row_never_a_fault() {
    let layout = layout("doctor-held");
    install(&layout, "trust", 6808);
    install(&layout, "trust-cg", 3095);
    let triple = "x86_64-apple-darwin";
    let mut programs = std::collections::BTreeMap::new();
    let own = crate::state::held_unpublished(None, 8595, triple, 6808);
    let sibling = crate::state::held_unpublished(Some("trust"), 8595, triple, 3095);
    for (p, build, state) in [("trust", 6808, &own), ("trust-cg", 3095, &sibling)] {
        programs.insert(
            p.to_string(),
            crate::ProgramStatus {
                installed_build: Some(build),
                state: state.clone(),
                tree_root: String::new(),
            },
        );
    }
    let status = crate::Status {
        schema: 1,
        updated_at: "2026-09-15T00:00:00Z".into(),
        enabled: true,
        index_source: "x/y".into(),
        outcome: "rustc NOT updated".into(),
        seams: Vec::new(),
        last_success_at: "2026-09-15T00:00:00Z".into(),
        programs,
        ..crate::Status::default()
    };
    crate::status::write(&layout, &status).unwrap();
    assert!(
        recorded_problems(Some(&status)).is_empty(),
        "a held row is deferred, not a fault"
    );
    let home = synthetic_home("doctor-held");
    let now = crate::flow::rfc3339_to_unix("2026-09-15T00:00:00Z").unwrap();
    let path = std::env::join_paths([layout.bin_dir()]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let ok = run_with(
        &layout,
        Some(&home),
        Some(&path),
        now,
        None,
        "doctor",
        &Probes::default(),
        &mut out,
        &mut err,
    );
    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(ok, "a held tuple is healthy:\n{text}");
    for (p, row) in [("trust", &own), ("trust-cg", &sibling)] {
        assert!(
            text.contains(&format!(
                "ok — {p}: {row} (nothing to do here; the next pass moves the group once \
                 the index publishes for this target)"
            )),
            "{p}'s held row is said, verbatim:\n{text}"
        );
    }
    assert!(
        !text.contains("PROBLEM"),
        "nothing about a hold is a problem:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A file at a command-link path is a warning naming what replaces it (on macOS the installed
/// app's `repair`, elsewhere moving it aside), never a fault, and left byte-identical; a link
/// there says nothing.
#[cfg(unix)]
#[test]
fn a_file_at_a_command_link_path_is_a_warning_with_its_remedy() {
    let l = layout("copied-cmd");
    install(&l, "ay", 18);
    let home = synthetic_home("copied-cmd");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("aterm"), b"old copy").unwrap();
    std::fs::write(bin.join("aterm-ctl"), b"a retired copy").unwrap();
    std::os::unix::fs::symlink(
        "/Applications/aterm.app/Contents/MacOS/atpkg",
        bin.join("atpkg"),
    )
    .unwrap();
    let mut out: Vec<u8> = Vec::new();
    let ok = run_with(
        &l,
        Some(&home),
        None,
        0,
        None,
        "doctor",
        &Probes::default(),
        &mut out,
        &mut std::io::sink(),
    );
    let out = String::from_utf8(out).unwrap();
    assert!(ok, "a copied command is a warning, not a fault:\n{out}");
    assert!(
        out.contains(&format!(
            "doctor: warn — {} is a file, not a link to the installed aterm",
            bin.join("aterm").display()
        )),
        "{out}"
    );
    // `#[cfg]`, not `cfg!()`: the macOS remedy names a macOS-only helper.
    #[cfg(target_os = "macos")]
    let remedy = format!(
        " — fix: run {} — it moves this file aside and links the app in its place",
        crate::hooks::repair_from_app_hint()
    );
    #[cfg(not(target_os = "macos"))]
    let remedy =
        " — fix: move it aside, and install aterm again if its link belongs here".to_string();
    assert!(out.contains(&remedy), "{out}");
    assert!(
        !out.contains(&format!("{} is a file", bin.join("atpkg").display())),
        "{out}"
    );
    // The default report keeps each row's step: the fix, never the `aterm ctl` a retired
    // name's remedy mentions.
    let short = present(&out, "", "doctor", Detail::Problems).joined();
    #[cfg(target_os = "macos")]
    let step = format!(
        "fix: run {}",
        crate::hooks::repair_from_app_hint()
            .split(" (")
            .next()
            .unwrap_or_default()
    );
    #[cfg(not(target_os = "macos"))]
    let step = "fix: move it aside".to_string();
    assert_eq!(short.matches(&step).count(), 2, "{short}");
    assert!(!short.contains("run `aterm ctl`"), "{short}");
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
    use std::os::unix::fs::PermissionsExt as _;
    let l = layout("shadowed");
    install(&l, "ay", 19);
    let foreign = l
        .prefix
        .parent()
        .unwrap()
        .join(format!("atpkg-doctor-shadow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&foreign);
    std::fs::create_dir_all(&foreign).unwrap();
    let exe = foreign.join("ay");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = synthetic_home("shadowed");
    let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
    let run = |path: &std::ffi::OsStr| {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            &l,
            Some(&home),
            Some(path),
            now,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut err,
        );
        (ok, String::from_utf8_lossy(&out).into_owned())
    };
    let ahead = std::env::join_paths([foreign.clone(), l.bin_dir()]).unwrap();
    let (ok, out) = run(&ahead);
    assert!(ok, "a shadow is a warning, not a structural fault:\n{out}");
    assert!(
        out.contains(&format!(
            "doctor: warn — ay: {}",
            crate::state::shadowed(19, &exe)
        )),
        "{out}"
    );
    assert!(
        exe.exists() && crate::which(&l, "ay").is_some(),
        "never fixed"
    );
    let behind = std::env::join_paths([l.bin_dir(), foreign.clone()]).unwrap();
    let (ok, out) = run(&behind);
    assert!(ok, "{out}");
    assert!(!out.contains("SHADOWED"), "{out}");
    // No alias laid (a vendor-shaped install): no fix-line either.
    let (_, out) = run(&ahead);
    assert!(!out.contains("for the managed one"), "{out}");
    let _ = std::fs::remove_dir_all(&foreign);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// In an aterm shell, a foreign `claude`/`codex` ahead of `agents/` is a shell that has not
/// run the hook: a warn whose remedy sources the hook in place, never `exec $SHELL` or
/// "remove or reorder that copy". The plain shadowed wording returns only when the twin is
/// gone; outside aterm the same PATH is a note (the `outside_aterm_…` test).
#[cfg(unix)]
#[test]
fn an_agent_program_shadowed_in_this_shell_names_the_hook_source_never_a_new_tab() {
    let l = layout("agent-shadow");
    // An ALab member too, so the report has a toolset to be healthy about.
    install(&l, "ay", 19);
    let v280 = crate::vendor_direct::Version::parse("2.1.280")
        .unwrap()
        .build_id();
    install(&l, "claude", v280);
    let twin = l.agent_shim(&tool("claude"));
    assert!(twin.exists(), "install lays the agents/ twin");
    let foreign = l
        .prefix
        .parent()
        .unwrap()
        .join(format!("atpkg-doctor-agent-foreign-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&foreign);
    std::fs::create_dir_all(&foreign).unwrap();
    let exe = foreign.join("claude");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = synthetic_home("agent-shadow");
    let now = crate::flow::rfc3339_to_unix("2026-09-16T00:00:00Z").unwrap();
    let run = |path: &std::ffi::OsStr| {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            &l,
            Some(&home),
            Some(path),
            now,
            None,
            "doctor",
            &Probes {
                stub_env: crate::reroute::StubEnv { in_aterm: true },
                ..Probes::default()
            },
            &mut out,
            &mut err,
        );
        (ok, String::from_utf8_lossy(&out).into_owned())
    };
    // A shell started before the install: no agents/ on its PATH.
    let no_agents = std::env::join_paths([foreign.clone(), l.bin_dir()]).unwrap();
    let (ok, out) = run(&no_agents);
    assert!(ok, "a warning, never a fault:\n{out}");
    assert!(
        out.contains(&format!(
            "doctor: warn — claude: {}",
            crate::state::agent_shadowed_in_shell(
                v280,
                &exe,
                &l.agents_dir(),
                false,
                &crate::cli::shell_remedy_command(&l),
            )
        )),
        "{out}"
    );
    assert!(
        !out.contains("new tab")
            && !out.contains("exec $SHELL")
            && !out.contains("remove or reorder")
            && !out.contains("not the pinned build"),
        "the in-place remedy only: {out}"
    );
    // agents/ present but behind the foreign copy: named as such.
    let behind = std::env::join_paths([foreign.clone(), l.bin_dir(), l.agents_dir()]).unwrap();
    let (ok, out) = run(&behind);
    assert!(ok, "{out}");
    assert!(
        out.contains(&crate::state::agent_shadowed_in_shell(
            v280,
            &exe,
            &l.agents_dir(),
            true,
            &crate::cli::shell_remedy_command(&l),
        )),
        "{out}"
    );
    // agents/ first (every aterm tab): nothing is shadowed.
    let first = std::env::join_paths([l.agents_dir(), foreign.clone(), l.bin_dir()]).unwrap();
    let (ok, out) = run(&first);
    assert!(ok, "{out}");
    assert!(!out.contains("SHADOWED"), "{out}");
    // The twin gone (a lane that could not lay it): the machine-wide wording again.
    std::fs::remove_file(&twin).unwrap();
    let (ok, out) = run(&no_agents);
    assert!(ok, "{out}");
    assert!(
        out.contains(&format!(
            "doctor: warn — claude: {} (not the copy aterm updates from Anthropic;",
            crate::state::shadowed(v280, &exe)
        )),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&foreign);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// With `reroute/claude` laid first on a session shell's PATH (no `agents/`), the stub runs
/// the managed copy: `ok`, naming the stub and the copy it out-ranks, not shadowed. Outside
/// aterm the stub passes through to the user's copy: a note. It still warns with no stub.
#[cfg(unix)]
#[test]
fn an_agent_program_routed_by_its_reroute_stub_is_ok_and_warns_only_where_it_is_not() {
    let l = layout("agent-routed");
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
    let foreign = l.prefix.parent().unwrap().join(format!(
        "atpkg-doctor-agent-routed-foreign-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&foreign);
    std::fs::create_dir_all(&foreign).unwrap();
    let exe = foreign.join("claude");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = synthetic_home("agent-routed");
    let now = crate::flow::rfc3339_to_unix("2026-09-23T00:00:00Z").unwrap();
    let run = |path: &std::ffi::OsStr, stub_env: crate::reroute::StubEnv| {
        let mut out: Vec<u8> = Vec::new();
        let ok = run_with(
            &l,
            Some(&home),
            Some(path),
            now,
            None,
            "doctor",
            &Probes {
                stub_env,
                ..Probes::default()
            },
            &mut out,
            &mut std::io::sink(),
        );
        (ok, String::from_utf8_lossy(&out).into_owned())
    };
    let inside = crate::reroute::StubEnv { in_aterm: true };
    let shadow_warn = format!(
        "doctor: warn — claude: {}",
        crate::state::agent_shadowed_in_shell(
            v280,
            &exe,
            &l.agents_dir(),
            false,
            &crate::cli::shell_remedy_command(&l),
        )
    );
    // The owner's shell: reroute first, the foreign copy next, no agents/.
    let stale =
        std::env::join_paths([crate::reroute::dir(&l), foreign.clone(), l.bin_dir()]).unwrap();
    let (ok, out) = run(&stale, inside);
    assert!(ok, "{out}");
    assert!(
        out.contains(&format!(
            "doctor: ok — claude: {}",
            crate::state::agent_routed_in_shell(v280, &stub, &exe)
        )),
        "{out}"
    );
    assert!(
        !out.contains("SHADOWED"),
        "the managed copy runs here: {out}"
    );
    assert!(
        out.contains("doctor: ok — reroute stub claude laid"),
        "{out}"
    );
    // Outside aterm the stub passes through to the user's own copy: a note naming no remedy
    // (sourcing the hook there demotes agents/, and the stub skips it anyway), never shadowed.
    let (ok, out) = run(&stale, crate::reroute::StubEnv::default());
    assert!(ok, "{out}");
    assert!(
        out.contains(&format!(
            "doctor: note — claude: {}",
            crate::state::agent_passed_through(v280, &stub, &exe)
        )),
        "{out}"
    );
    assert!(
        !out.contains("SHADOWED") && !out.contains("routed at exec time"),
        "{out}"
    );
    // agents/ first (every new session shell): nothing to route around, nothing said.
    let first = std::env::join_paths([
        crate::reroute::dir(&l),
        l.agents_dir(),
        foreign.clone(),
        l.bin_dir(),
    ])
    .unwrap();
    let (_, out) = run(&first, inside);
    assert!(
        !out.contains("SHADOWED") && !out.contains("routed at exec time"),
        "{out}"
    );
    // The stub absent: still the warning, inside aterm too.
    std::fs::remove_file(&stub).unwrap();
    let (ok, out) = run(&stale, inside);
    assert!(ok, "{out}");
    assert!(out.contains(&shadow_warn), "{out}");
    assert!(
        out.contains("doctor: warn — reroute stub claude missing"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&foreign);
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
    assert!(claude_survey_off(
        r#"{"env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}}"#
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

/// (10h) The switch's row: unset, the warn names the nested line the window adds; a value
/// Claude Code reads as true is the default; one it does not (`"off"`) is kept and the row
/// says the surveys stay on; an `env` that is no object is the one refusal.
#[test]
fn the_survey_switch_row_names_what_the_window_adds_and_what_a_hand_set_value_does() {
    use DefaultState::{Default as D, Manual, Unset};
    let switch = |text: &str| claude_defaults_state(text).unwrap()[3].clone();
    assert_eq!(switch("{}"), (CLAUDE_SURVEY_SWITCH, Unset));
    assert_eq!(
        switch(r#"{"env": {"OTHER": "x"}}"#),
        (CLAUDE_SURVEY_SWITCH, Unset)
    );
    for on in [r#""yes""#, r#""TRUE""#, "true"] {
        assert_eq!(
            switch(&format!(
                r#"{{"env": {{"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": {on}}}}}"#
            )),
            (CLAUDE_SURVEY_SWITCH, D),
            "{on}"
        );
    }
    assert_eq!(
        switch(r#"{"env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "off"}}"#),
        (CLAUDE_SURVEY_SWITCH, Manual("\"off\"".into()))
    );
    assert_eq!(
        claude_defaults_state(r#"{"env": "FOO=1"}"#),
        Err("its \"env\" is not a JSON object".to_string())
    );
    assert_eq!(
        default_line(CLAUDE_SURVEY_SWITCH, "\"1\""),
        r#""env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}"#
    );
    assert_eq!(
        default_line("feedbackSurveyRate", "0"),
        r#""feedbackSurveyRate": 0"#
    );
    // The window adds it to a file that has everything else.
    assert!(window_adds(
        &top(r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0}"#),
        false
    ));
    assert!(!window_adds(
        &top(
            r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0,
                "feedbackDrafts": "quiet", "autoCompactWindow": 400000,
                "env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "off"}}"#
        ),
        false
    ));

    let l = layout("agent-switch");
    let v283 = crate::vendor_direct::Version::parse("2.1.283")
        .unwrap()
        .build_id();
    install(&l, "claude", v283);
    let home = synthetic_home("agent-switch");
    let now = crate::flow::rfc3339_to_unix("2026-09-28T00:00:00Z").unwrap();
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let report = |text: &str| {
        let mut out: Vec<u8> = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            &Probes {
                claude_defaults: Some(claude_defaults_state(text)),
                ..Probes::default()
            },
            &mut out,
            &mut std::io::sink(),
        );
        String::from_utf8_lossy(&out).into_owned()
    };
    let unset = report(&top(
        r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0}"#,
    ));
    assert!(
        unset.contains(
            "doctor: warn — claude: env.CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY is unset in \
             ~/.claude/settings.json — an aterm window adds \"env\": \
             {\"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY\": \"1\"} at its next start"
        ),
        "{unset}"
    );
    let off = report(&top(
        r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0,
                "env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "off"}}"#,
    ));
    assert!(
        off.contains(
            "doctor: ok — claude: env.CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY is \"off\", set by \
             hand — kept (aterm adds its default only where a key is unset) — Claude Code \
             reads only 1, true, yes or on as off, so it will still show its rating \
             surveys (an aterm-supervised session dismisses each with a 0 unless [harness] \
             dismiss_surveys is false)"
        ),
        "{off}"
    );
    // A probability the person chose (`0.3`) is theirs: the switch would silence every
    // survey it asks for, so the window does not add it — and the row says why, as ok.
    let rated = report(&top(
        r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0.3,
                "feedbackDrafts": "quiet", "autoCompactWindow": 400000}"#,
    ));
    assert!(
        rated.contains(
            "doctor: ok — claude: env.CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY is unset and \
             stays so — feedbackSurveyRate is set by hand (0.3), and aterm adds the switch \
             that turns EVERY rating survey off only where that probability is unset or 0"
        ),
        "{rated}"
    );
    assert!(!rated.contains("warn — claude:"), "{rated}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A survey probability a person set by hand holds the switch back, as an effort does
/// the effort defaults: the switch silences every survey, whatever the probability asks
/// for. Zero asks for the same silence, so it holds nothing back; anything else (a
/// number, a string, `null`) is the person's own.
#[test]
fn a_hand_set_survey_probability_holds_the_switch_back() {
    use DefaultState::{Default as D, HeldForRate, Manual, Unset};
    for (text, kept) in [
        ("0.3", "0.3"),
        ("1", "1"),
        ("0.05", "0.05"),
        ("\"0.5\"", "\"0.5\""),
        ("null", "null"),
    ] {
        let json = format!(r#"{{"feedbackSurveyRate": {text}}}"#);
        assert_eq!(
            claude_survey_rate_set_by_hand(&json),
            Some(kept.to_string()),
            "{json}"
        );
        let states = claude_defaults_state(&json).unwrap();
        assert_eq!(
            states[3],
            (CLAUDE_SURVEY_SWITCH, HeldForRate(kept.into())),
            "{json}"
        );
        assert_eq!(
            states[2],
            ("feedbackSurveyRate", Manual(kept.into())),
            "{json}"
        );
    }
    for zero in ["0", "0.0", "-0", "0e0"] {
        let json = format!(r#"{{"feedbackSurveyRate": {zero}}}"#);
        assert_eq!(claude_survey_rate_set_by_hand(&json), None, "{json}");
        assert_eq!(
            claude_defaults_state(&json).unwrap()[3],
            (CLAUDE_SURVEY_SWITCH, Unset),
            "{json}"
        );
    }
    // Unset, or text that is not JSON: nothing is held.
    assert_eq!(claude_survey_rate_set_by_hand("{}"), None);
    assert_eq!(claude_survey_rate_set_by_hand("not json"), None);
    // A switch already there is the person's, and reads as the default when it is on.
    let both = claude_defaults_state(
        r#"{"feedbackSurveyRate": 0.3, "env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}}"#,
    )
    .unwrap();
    assert_eq!(both[3], (CLAUDE_SURVEY_SWITCH, D));
    // The window's own rule (`window_adds`): with every other default in place a held
    // switch leaves nothing to add; with the rate at zero it is added.
    let rest = format!(
        r#""{TOP}": true, "effortLevel": "xhigh", "feedbackDrafts": "quiet",
            "autoCompactWindow": 400000"#
    );
    assert!(!window_adds(
        &format!(r#"{{{rest}, "feedbackSurveyRate": 0.3}}"#),
        false
    ));
    assert!(window_adds(
        &format!(r#"{{{rest}, "feedbackSurveyRate": 0}}"#),
        false
    ));
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
    let kept =
        "Claude Code's autocompact offer was answered Keep (claude-opus-5-5) in ~/.claude.json";
    assert_eq!(
        window(),
        (CLAUDE_COMPACT_WINDOW_KEY, HeldForOffer(kept.into()))
    );
    assert!(!window_adds(&settings, true));
    assert!(window_adds(&settings, false));
    std::fs::write(&state, "{").unwrap();
    let unreadable = window();
    assert!(
        matches!(&unreadable.1, HeldForOffer(why) if why.starts_with(
            "~/.claude.json, where Claude Code records the answer to its autocompact offer, \
             cannot be read ("
        )),
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
    assert!(
        report.contains(&format!(
            "doctor: ok — claude: autoCompactWindow is unset and stays so — {kept}, and aterm \
             adds its 400k window only where no Keep answer stands (/autocompact in Claude \
             Code sets a window for a model)"
        )),
        "{report}"
    );
    assert!(!report.contains("warn — claude:"), "{report}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (10h) The rows: the defaults as one `ok`, a hand-set value as a kept `ok`, an unset key as
/// a warn naming what the next window start adds, and no row when the report did not read
/// the settings. The `env` survey switch sets no key.
#[test]
fn a_managed_claude_reports_each_default_as_set_by_hand_or_unset() {
    use DefaultState::{Default as D, Manual, Unset};
    assert_eq!(
        claude_defaults_state(&top(
            r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0,
                    "feedbackDrafts": "quiet", "autoCompactWindow": 400000,
                    "env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}}"#
        )),
        Ok(vec![
            (TOP, D),
            ("effortLevel", D),
            ("feedbackSurveyRate", D),
            (CLAUDE_SURVEY_SWITCH, D),
            (CLAUDE_DRAFTS_KEY, D),
            (CLAUDE_COMPACT_WINDOW_KEY, D),
        ])
    );
    assert_eq!(
        claude_defaults_state(&top(r#"{"<top>": false, "effortLevel": "medium"}"#)),
        Ok(vec![
            (TOP, Manual("false".into())),
            ("effortLevel", Manual("\"medium\"".into())),
            ("feedbackSurveyRate", Unset),
            (CLAUDE_SURVEY_SWITCH, Unset),
            (CLAUDE_DRAFTS_KEY, Unset),
            (CLAUDE_COMPACT_WINDOW_KEY, Unset),
        ])
    );
    // The `env` survey switch silences every survey, so it needs no rate beside it;
    // a rate a person set is kept, whatever the switch; any spelling of zero is the
    // default.
    let switched =
        claude_defaults_state(r#"{"env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}}"#).unwrap();
    assert_eq!(
        switched[2],
        ("feedbackSurveyRate", D),
        "the switch already covers the rate"
    );
    assert_eq!(
        claude_defaults_state(
            r#"{"env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}, "feedbackSurveyRate": 5}"#
        )
        .unwrap()[2],
        ("feedbackSurveyRate", Manual("5".into())),
        "a hand-set rate is kept, whatever the switch"
    );
    assert_eq!(
        claude_defaults_state(r#"{"feedbackSurveyRate": 0.0}"#).unwrap()[2],
        ("feedbackSurveyRate", D),
        "any spelling of zero is the default"
    );
    assert_eq!(switched[3], (CLAUDE_SURVEY_SWITCH, D));
    // The rate alone covers the session survey, not the switch: it stays unset, and the
    // window adds it.
    assert_eq!(
        claude_defaults_state(r#"{"feedbackSurveyRate": 0}"#).unwrap()[3],
        (CLAUDE_SURVEY_SWITCH, Unset)
    );
    // The per-model window `/autocompact` saves replaces the top-level one for its model
    // only: the top-level key stays unset, and the window adds it.
    let per_model = r#"{"modelSettings": {"claude-opus-5-5": {"autoCompactWindow": 400000}}}"#;
    assert_eq!(
        claude_defaults_state(per_model).unwrap()[5],
        (CLAUDE_COMPACT_WINDOW_KEY, Unset)
    );
    assert!(window_adds(per_model, false));

    let l = layout("agent-defaults");
    let v283 = crate::vendor_direct::Version::parse("2.1.283")
        .unwrap()
        .build_id();
    install(&l, "claude", v283);
    let home = synthetic_home("agent-defaults");
    let now = crate::flow::rfc3339_to_unix("2026-09-28T00:00:00Z").unwrap();
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let rows = |defaults: Option<Result<Vec<(&'static str, DefaultState)>, String>>| {
        let mut out: Vec<u8> = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            &Probes {
                claude_defaults: defaults,
                ..Probes::default()
            },
            &mut out,
            &mut std::io::sink(),
        );
        String::from_utf8_lossy(&out)
            .lines()
            .filter(|line| {
                line.contains("— claude: ") && line.contains("settings")
                    || line.contains("set by hand")
            })
            .map(str::to_owned)
            .collect::<Vec<String>>()
    };
    let all = rows(Some(claude_defaults_state(&top(
        r#"{"<top>": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0,
                "feedbackDrafts": "quiet", "autoCompactWindow": 400000,
                "env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}}"#,
    ))));
    assert_eq!(all.len(), 1, "{all:?}");
    assert!(all[0].contains(&format!(
        "doctor: ok — claude: {TOP}, effortLevel, feedbackSurveyRate, \
         env.CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY, feedbackDrafts, autoCompactWindow set to \
         aterm's defaults (the top-effort mode on, xhigh effort, the session-quality survey \
         off, every rating survey off, the drafted-feedback card quiet, autocompact at 400k \
         tokens) in ~/.claude/settings.json"
    )));
    // The parenthetical names only the defaults the row lists.
    let survey = rows(Some(claude_defaults_state(
        r#"{"effortLevel": "medium", "feedbackSurveyRate": 0}"#,
    )));
    assert!(
        survey.iter().any(|r| r
            == "doctor: ok — claude: feedbackSurveyRate set to aterm's default (the \
                session-quality survey off) in ~/.claude/settings.json"),
        "{survey:?}"
    );
    // A top-level effort set by hand: kept, and it holds the top-effort key back
    // (adding it `true` would override the `medium`).
    let mixed = rows(Some(claude_defaults_state(r#"{"effortLevel": "medium"}"#)));
    assert!(
        mixed
            .iter()
            .any(|r| r.contains("ok — claude: effortLevel is \"medium\", set by hand — kept")),
        "{mixed:?}"
    );
    assert!(
        mixed.iter().any(|r| r.contains(&format!(
            "ok — claude: {TOP} is unset and stays so — an effort is set by hand \
             (effortLevel = \"medium\")"
        ))),
        "{mixed:?}"
    );
    assert!(
        mixed
            .iter()
            .any(|r| r.contains("warn — claude: feedbackSurveyRate is unset")
                && r.contains("\"feedbackSurveyRate\": 0")),
        "the survey rate is no effort, and is still added: {mixed:?}"
    );
    assert!(
        !mixed
            .iter()
            .any(|r| r.contains(&format!("\"{TOP}\": true"))),
        "{mixed:?}"
    );
    assert!(rows(None).is_empty(), "no settings read, no row");
}

/// (10h) The window's effort rule: while an effort other than `xhigh` is set by hand
/// (per-model, as `/effort medium` saves it, or top-level), the top-effort key and
/// `effortLevel` are reported kept unset, naming that effort, never as a warn; the survey
/// rate is still added. A hand-set `xhigh` holds nothing back.
#[test]
fn an_effort_set_by_hand_holds_top_effort_and_xhigh_back_in_the_report() {
    use DefaultState::{HeldForEffort, Manual, Unset};
    let per_model = r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "medium"}}}"#;
    let named = "modelSettings.claude-opus-5-5.effortLevel = \"medium\"";
    assert_eq!(
        claude_defaults_state(per_model),
        Ok(vec![
            (TOP, HeldForEffort(named.into())),
            ("effortLevel", HeldForEffort(named.into())),
            ("feedbackSurveyRate", Unset),
            (CLAUDE_SURVEY_SWITCH, Unset),
            (CLAUDE_DRAFTS_KEY, Unset),
            (CLAUDE_COMPACT_WINDOW_KEY, Unset),
        ])
    );
    assert_eq!(
        claude_defaults_state(r#"{"effortLevel": "medium"}"#),
        Ok(vec![
            (TOP, HeldForEffort("effortLevel = \"medium\"".into())),
            ("effortLevel", Manual("\"medium\"".into())),
            ("feedbackSurveyRate", Unset),
            (CLAUDE_SURVEY_SWITCH, Unset),
            (CLAUDE_DRAFTS_KEY, Unset),
            (CLAUDE_COMPACT_WINDOW_KEY, Unset),
        ])
    );
    assert_eq!(
        claude_defaults_state(
            r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "xhigh"}}}"#
        ),
        Ok(vec![
            (TOP, Unset),
            ("effortLevel", Unset),
            ("feedbackSurveyRate", Unset),
            (CLAUDE_SURVEY_SWITCH, Unset),
            (CLAUDE_DRAFTS_KEY, Unset),
            (CLAUDE_COMPACT_WINDOW_KEY, Unset),
        ]),
        "a hand-set xhigh is the default's own effort"
    );

    let l = layout("agent-effort");
    let v283 = crate::vendor_direct::Version::parse("2.1.283")
        .unwrap()
        .build_id();
    install(&l, "claude", v283);
    let home = synthetic_home("agent-effort");
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
            claude_defaults: Some(claude_defaults_state(per_model)),
            ..Probes::default()
        },
        &mut out,
        &mut std::io::sink(),
    );
    let out = String::from_utf8_lossy(&out);
    for key in [TOP, "effortLevel"] {
        assert!(
            out.contains(&format!(
                "doctor: ok — claude: {key} is unset and stays so — an effort is set by hand \
                 ({named}), and aterm adds {TOP} and xhigh effort only where no other \
                 effort was chosen"
            )),
            "{out}"
        );
        assert!(
            !out.contains(&format!("warn — claude: {key} is unset")),
            "no promise to add {key}: {out}"
        );
    }
    assert!(
        out.contains("doctor: warn — claude: feedbackSurveyRate is unset"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (10h) A settings file the window refuses (unparsable, empty, or not a JSON object at the
/// top) is one warn naming why, with no per-key row promising a repair.
#[test]
fn a_settings_file_the_window_refuses_is_one_warn_naming_why() {
    for text in ["not json", "", "  \n"] {
        let refused = claude_defaults_state(text);
        assert!(refused.is_err(), "{text:?}: {refused:?}");
    }
    assert_eq!(
        claude_defaults_state(&top(r#"["<top>"]"#)),
        Err("the top level is not a JSON object".to_string())
    );

    let l = layout("agent-refused");
    let v283 = crate::vendor_direct::Version::parse("2.1.283")
        .unwrap()
        .build_id();
    install(&l, "claude", v283);
    let home = synthetic_home("agent-refused");
    let now = crate::flow::rfc3339_to_unix("2026-09-28T00:00:00Z").unwrap();
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    for text in ["{ not json".to_string(), top(r#"["<top>"]"#)] {
        let why = claude_defaults_state(&text).unwrap_err();
        let mut out: Vec<u8> = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            &Probes {
                claude_defaults: Some(Err(why.clone())),
                ..Probes::default()
            },
            &mut out,
            &mut std::io::sink(),
        );
        let out = String::from_utf8_lossy(&out);
        let rows: Vec<&str> = out
            .lines()
            .filter(|line| line.contains("— claude: ") && line.contains("settings"))
            .collect();
        assert_eq!(
            rows,
            vec![format!(
                "doctor: warn — claude: ~/.claude/settings.json cannot take aterm's defaults \
                 ({why}) — an aterm window leaves the file exactly as it is and adds none of \
                 them"
            )],
            "{out}"
        );
        assert!(!out.contains("at its next start"), "no promise: {out}");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (10h) A top-effort key beside a hand-set effort (what adding every unset default makes of
/// `/effort medium`'s per-model save) is a warn naming the effort it overrides and the fix,
/// kept in the default report's short row, since the window never removes it. Controls: the
/// key beside no hand-set effort, or beside a hand-set `xhigh`, is the default, and a
/// hand-set `false` is kept.
#[test]
fn a_top_effort_key_over_an_effort_set_by_hand_is_a_warn_naming_the_fix() {
    use DefaultState::{Default as D, Manual, OverridesEffort, Unset};
    // What adding every unset default makes of
    // `{"modelSettings":{"claude-opus-5-5":{"effortLevel":"medium"}}}`.
    let damaged = &top(
        "{\n  \"modelSettings\": {\n    \"claude-opus-5-5\": {\n      \
         \"effortLevel\": \"medium\"\n    }\n  },\n  \"<top>\": true,\n  \
         \"effortLevel\": \"xhigh\",\n  \"feedbackSurveyRate\": 0\n}\n",
    );
    let named = "modelSettings.claude-opus-5-5.effortLevel = \"medium\"";
    assert_eq!(
        claude_defaults_state(damaged),
        Ok(vec![
            (TOP, OverridesEffort(named.into())),
            ("effortLevel", D),
            ("feedbackSurveyRate", D),
            (CLAUDE_SURVEY_SWITCH, Unset),
            (CLAUDE_DRAFTS_KEY, Unset),
            (CLAUDE_COMPACT_WINDOW_KEY, Unset),
        ])
    );
    assert_eq!(
        claude_defaults_state(&top(r#"{"<top>": true, "effortLevel": "low"}"#)).unwrap()[0],
        (TOP, OverridesEffort("effortLevel = \"low\"".into())),
        "a top-level effort set by hand is overridden the same way"
    );
    for (text, want) in [
        (r#"{"<top>": true}"#, D),
        (
            r#"{"<top>": true, "modelSettings": {"m": {"effortLevel": "xhigh"}}}"#,
            D,
        ),
        (
            r#"{"<top>": false, "effortLevel": "low"}"#,
            Manual("false".into()),
        ),
    ] {
        let text = top(text);
        assert_eq!(
            claude_defaults_state(&text).unwrap()[0],
            (TOP, want),
            "{text}"
        );
    }

    let l = layout("agent-overrides");
    let v283 = crate::vendor_direct::Version::parse("2.1.283")
        .unwrap()
        .build_id();
    install(&l, "claude", v283);
    let home = synthetic_home("agent-overrides");
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
            claude_defaults: Some(claude_defaults_state(damaged)),
            ..Probes::default()
        },
        &mut out,
        &mut std::io::sink(),
    );
    let out = String::from_utf8_lossy(&out).into_owned();
    let warn = format!(
        "doctor: warn — claude: \"{TOP}\": true in ~/.claude/settings.json overrides the \
         effort set by hand ({named}) — Claude Code runs xhigh while it stands — fix: remove \
         \"{TOP}\" from ~/.claude/settings.json to use that effort (a \
         ~/.claude/settings.json.bak-<unix> may hold the file from before)"
    );
    assert!(out.lines().any(|line| line == warn), "{out}");
    assert!(
        out.contains(
            "doctor: ok — claude: effortLevel, feedbackSurveyRate set to aterm's defaults \
             (xhigh effort, the session-quality survey off) in ~/.claude/settings.json"
        ),
        "{out}"
    );
    assert!(
        !out.contains(&format!("ok — claude: {TOP}")),
        "the override is never an ok: {out}"
    );
    assert!(
        !out.contains(&format!("aterm's defaults ({TOP}")),
        "the ok row names only the defaults it lists: {out}"
    );
    // The default report, where a long row is cut to its fact and its fix.
    let shown = present(&out, "", "doctor", Detail::Problems);
    let short: Vec<&str> = shown
        .rest
        .lines()
        .filter(|line| line.contains(&format!("warn — claude: \"{TOP}\": true")))
        .collect();
    assert_eq!(short.len(), 1, "{}", shown.rest);
    assert!(
        short[0].ends_with(&format!(
            " — fix: remove \"{TOP}\" from ~/.claude/settings.json to use that effort"
        )) && short[0].chars().count() <= SHORT_ROW,
        "{short:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (10h) The two refusals the settings text cannot show, on real links in a scratch home: a
/// symlink to a missing file, and a file whose directory this user cannot write (a read-only
/// store link, or the plain file) while a key is left to add. Each is the one refusal warn.
/// Controls: no file is no row, the same text in a writable directory is judged by its keys,
/// and a read-only file that needs nothing added keeps its rows.
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

    // A dotfiles link to a file not there yet.
    let missing = home.join("dotfiles").join("settings.json");
    std::os::unix::fs::symlink(&missing, &settings).unwrap();
    let dangling = probe_claude_settings(&home);
    std::fs::remove_file(&settings).unwrap();

    // A link into a store this user cannot write: a file with keys left to add,
    // and one with every default set.
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
    // The survey off through `env` only: the window still adds
    // `feedbackSurveyRate`, so it still writes.
    let env_only = store.join("env-only.json");
    std::fs::write(
        &env_only,
        top(
            r#"{"env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}, "<top>": true, "effortLevel": "xhigh"}"#,
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
    std::os::unix::fs::symlink(&env_only, &settings).unwrap();
    let env_only_read_only = probe_claude_settings(&home);
    std::fs::remove_file(&settings).unwrap();
    // The plain file in a directory this user cannot write.
    std::fs::write(&settings, "{}").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o555)).unwrap();
    let plain_read_only = probe_claude_settings(&home);
    // Put the permissions back before asserting, so the scratch home can go.
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700)).unwrap();
    // A link to itself: the read fails (a loop), and the window refuses it too.
    std::fs::remove_file(&settings).unwrap();
    std::os::unix::fs::symlink(&settings, &settings).unwrap();
    let looped = probe_claude_settings(&home);
    std::fs::remove_file(&settings).unwrap();

    assert_eq!(
        dangling,
        Some(Err(format!(
            "a symlink to {}, which does not exist",
            missing.display()
        )))
    );
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
    let real = |p: &Path| std::fs::canonicalize(p).unwrap();
    assert_eq!(
        linked_read_only,
        Some(Err(format!(
            "a symlink to {}, whose directory {} this user cannot write",
            real(&bare).display(),
            real(&store).display()
        )))
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
    assert_eq!(
        env_only_read_only,
        Some(Err(format!(
            "a symlink to {}, whose directory {} this user cannot write",
            real(&env_only).display(),
            real(&store).display()
        ))),
        "the window adds feedbackSurveyRate though the env switch covers it"
    );
    assert!(
        matches!(&looped, Some(Err(why)) if why.starts_with("it cannot be read: ")),
        "a file that cannot be read is the refusal, not silence: {looped:?}"
    );
    assert_eq!(
        plain_read_only,
        Some(Err(format!(
            "its directory {} is not writable by this user",
            real(&claude).display()
        )))
    );

    // As the report says it: the one refusal row, no promise.
    let l = layout("claude-links");
    let v283 = crate::vendor_direct::Version::parse("2.1.283")
        .unwrap()
        .build_id();
    install(&l, "claude", v283);
    let now = crate::flow::rfc3339_to_unix("2026-09-28T00:00:00Z").unwrap();
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    for probed in [dangling, linked_read_only, plain_read_only, looped] {
        let Some(Err(why)) = probed.clone() else {
            panic!("a refusal: {probed:?}");
        };
        let mut out: Vec<u8> = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            &Probes {
                claude_defaults: probed,
                ..Probes::default()
            },
            &mut out,
            &mut std::io::sink(),
        );
        let out = String::from_utf8_lossy(&out);
        let rows: Vec<&str> = out
            .lines()
            .filter(|line| line.contains("— claude: ") && line.contains("settings"))
            .collect();
        assert_eq!(
            rows,
            vec![format!(
                "doctor: warn — claude: ~/.claude/settings.json cannot take aterm's defaults \
                 ({why}) — an aterm window leaves the file exactly as it is and adds none of \
                 them"
            )],
            "{out}"
        );
        assert!(!out.contains("at its next start"), "no promise: {out}");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

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
    let foreign = l.prefix.parent().unwrap().join(format!(
        "atpkg-doctor-agent-outside-foreign-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&foreign);
    std::fs::create_dir_all(&foreign).unwrap();
    let exe = foreign.join("claude");
    std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = synthetic_home("agent-outside");
    let now = crate::flow::rfc3339_to_unix("2026-09-24T00:00:00Z").unwrap();
    let run = |path: &std::ffi::OsStr, stub_env: crate::reroute::StubEnv| {
        let mut out: Vec<u8> = Vec::new();
        let ok = run_with(
            &l,
            Some(&home),
            Some(path),
            now,
            None,
            "doctor",
            &Probes {
                stub_env,
                ..Probes::default()
            },
            &mut out,
            &mut std::io::sink(),
        );
        (ok, String::from_utf8_lossy(&out).into_owned())
    };
    let outside = crate::reroute::StubEnv::default();
    let inside = crate::reroute::StubEnv { in_aterm: true };
    let remedy = crate::cli::shell_remedy_command(&l);
    let claude_rows = |out: &str| -> Vec<String> {
        out.lines()
            .filter(|line| line.contains("— claude: "))
            .map(str::to_owned)
            .collect()
    };
    // 1. Outside aterm, the stub laid: an iTerm PATH (no reroute dir), and one whose
    //    reroute dir stands behind the foreign copy.
    let iterm = std::env::join_paths([foreign.clone(), l.bin_dir()]).unwrap();
    let behind =
        std::env::join_paths([foreign.clone(), crate::reroute::dir(&l), l.bin_dir()]).unwrap();
    for (label, path) in [("no reroute dir", &iterm), ("reroute behind", &behind)] {
        let (ok, out) = run(path, outside);
        assert!(ok, "{label}: {out}");
        assert_eq!(
            claude_rows(&out),
            vec![format!(
                "doctor: note — claude: {}",
                crate::state::agent_own_copy_outside_aterm(v280, &exe)
            )],
            "{label}: {out}"
        );
        assert!(
            !out.contains("SHADOWED") && !out.contains(&remedy),
            "{label}: no shadow, no remedy outside aterm: {out}"
        );
    }
    // 2. Inside aterm, the stub first: routed at exec time, an ok.
    let stale =
        std::env::join_paths([crate::reroute::dir(&l), foreign.clone(), l.bin_dir()]).unwrap();
    let (ok, out) = run(&stale, inside);
    assert!(ok, "{out}");
    assert_eq!(
        claude_rows(&out),
        vec![format!(
            "doctor: ok — claude: {}",
            crate::state::agent_routed_in_shell(v280, &stub, &exe)
        )],
        "{out}"
    );
    // 3. The stub absent. Outside aterm it is still the note: with no reroute dir on PATH a
    //    stub decides nothing, and the rc hook's false arm demotes `agents/` there, so its
    //    remedy would do nothing. Inside aterm: shadowed, with the remedy, which works there.
    std::fs::remove_file(&stub).unwrap();
    let (ok, out) = run(&iterm, outside);
    assert!(ok, "{out}");
    assert_eq!(
        claude_rows(&out),
        vec![format!(
            "doctor: note — claude: {}",
            crate::state::agent_own_copy_outside_aterm(v280, &exe)
        )],
        "no stub, outside aterm: {out}"
    );
    assert!(
        !out.contains("SHADOWED")
            && !out.contains(&remedy)
            && !out.contains(&format!("{}:", l.agents_dir().display())),
        "no stub, outside aterm: no shadow, no remedy, agents/ put first nowhere: {out}"
    );
    let (ok, out) = run(&iterm, inside);
    assert!(ok, "{out}");
    assert_eq!(
        claude_rows(&out),
        vec![format!(
            "doctor: warn — claude: {}",
            crate::state::agent_shadowed_in_shell(v280, &exe, &l.agents_dir(), false, &remedy)
        )],
        "no stub, inside aterm: {out}"
    );
    let _ = std::fs::remove_dir_all(&foreign);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// Inspect the intended member's status sentence, not arbitrary path text in
/// the report (the executable itself may live in a `self-update` checkout).
fn member_has_self_update_notice(report: &str, program: &str) -> bool {
    let ok = format!("doctor: ok — {program}:");
    let warn = format!("doctor: warn — {program}:");
    report.lines().any(|line| {
        (line.starts_with(&ok) || line.starts_with(&warn))
            && line.contains(" own updater is off here (")
            && line.ends_with(')')
    })
}

/// (10f)'s drift warn, every wording: what the shim exports (`nothing`, or its own
/// entries), what the build declares, the consequence only when the build declares a
/// self-update switch and the shim exports none (the vendor's product for a
/// vendor-direct program, `its` for any other), and always the one remedy.
#[test]
fn the_drift_warn_says_what_the_shim_exports_and_what_the_build_declares() {
    let admit = |e: &[&str]| {
        crate::shim_env::ShimEnv::admit(&e.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
            .unwrap()
    };
    let shim = Path::new("/p/bin/x");
    let none = crate::shim_env::ShimEnv::NONE;
    let off = admit(&["DISABLE_AUTOUPDATER=1"]);
    let claude = crate::vendor_direct::Version::parse("2.1.280")
        .unwrap()
        .build_id();
    assert_eq!(
        env_drift_line("claude", claude, shim, &none, &off),
        "/p/bin/x exports nothing, but claude 2.1.280 declares DISABLE_AUTOUPDATER=1, so \
         Claude Code's own updater is on here — fix: `aterm pkg repair` re-lays it"
    );
    assert_eq!(
        env_drift_line("ay", 17, shim, &none, &off),
        "/p/bin/x exports nothing, but ay build 17 declares DISABLE_AUTOUPDATER=1, so its \
         own updater is on here — fix: `aterm pkg repair` re-lays it"
    );
    // A switch is exported, just not the declared one: no claim about the updater.
    assert_eq!(
        env_drift_line("ay", 17, shim, &admit(&["DISABLE_AUTOUPDATER=0"]), &off),
        "/p/bin/x exports DISABLE_AUTOUPDATER=0, but ay build 17 declares \
         DISABLE_AUTOUPDATER=1 — fix: `aterm pkg repair` re-lays it"
    );
    // The switch is there; the drift is an extra entry: no updater claim.
    assert_eq!(
        env_drift_line(
            "ay",
            17,
            shim,
            &admit(&["DISABLE_AUTOUPDATER=1", "AY_MODE=x"]),
            &off
        ),
        "/p/bin/x exports DISABLE_AUTOUPDATER=1, AY_MODE=x, but ay build 17 declares \
         DISABLE_AUTOUPDATER=1 — fix: `aterm pkg repair` re-lays it"
    );
    assert_eq!(
        env_drift_line("ay", 17, shim, &none, &admit(&["AY_MODE=x"])),
        "/p/bin/x exports nothing, but ay build 17 declares AY_MODE=x — fix: `aterm pkg \
         repair` re-lays it"
    );
}

/// A managed member whose shim exports its policy's `shim_env` gets one `ok` line (the
/// canonical row plus the trailing "own updater is off here" sentence), never a fault; a
/// plain shim gets no such line; a shadowed one keeps (10d)'s row alone. A vendor program
/// reads by version and source, never an index pin.
#[cfg(unix)]
#[test]
fn a_managed_member_with_a_shim_env_says_self_update_off_as_a_trailing_line() {
    use std::os::unix::fs::PermissionsExt as _;
    let l = layout("shim-env");
    let claude = crate::vendor_direct::Version::parse("2.1.280")
        .unwrap()
        .build_id();
    let dir = install_build_tree(&l, "claude", claude);
    let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    crate::activate::install_tools_env(
        &l,
        &dir,
        &[tool("claude")],
        crate::activate::Aliases::Off,
        &env,
    )
    .unwrap();
    activate_build(&l, &dir).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    let mut programs = std::collections::BTreeMap::new();
    programs.insert(
        "claude".to_string(),
        crate::ProgramStatus {
            installed_build: Some(claude),
            state: crate::state::vendor_managed("2.1.280", "Anthropic"),
            tree_root: String::new(),
        },
    );
    crate::status::write(
        &l,
        &crate::Status {
            schema: 1,
            updated_at: "2026-08-28T00:00:00Z".into(),
            enabled: true,
            index_source: "alabsystems/aterm".into(),
            outcome: "up to date".into(),
            seams: Vec::new(),
            last_success_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            last_pass: String::new(),
            last_pass_at: String::new(),
            last_pass_attempted_index_build: 0,
            last_pass_attempted_at: String::new(),
            pass_seq: 0,
            stale_index_only_pass_seq: 0,
            programs,
            extra: Default::default(),
        },
    )
    .unwrap();
    let home = synthetic_home("shim-env");
    let now = crate::flow::rfc3339_to_unix("2026-08-28T00:00:00Z").unwrap();
    let run_in = |path: &std::ffi::OsStr, stub_env: crate::reroute::StubEnv| {
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let ok = run_with(
            &l,
            Some(&home),
            Some(path),
            now,
            None,
            "doctor",
            &Probes {
                stub_env,
                ..Probes::default()
            },
            &mut out,
            &mut err,
        );
        (ok, String::from_utf8_lossy(&out).into_owned())
    };
    let run = |path: &std::ffi::OsStr| run_in(path, crate::reroute::StubEnv::default());
    let managed_only = std::env::join_paths([l.bin_dir()]).unwrap();
    let (ok, out) = run(&managed_only);
    assert!(ok, "{out}");
    let line = out
        .lines()
        .find(|l| l.contains("ok — claude:"))
        .unwrap_or_else(|| panic!("an ok line for claude:\n{out}"));
    assert_eq!(
        line,
        "doctor: ok — claude: managed 2.1.280 — Anthropic latest — Claude Code's own \
         updater is off here (DISABLE_AUTOUPDATER=1)"
    );
    // Held, then rolled back: the row says why, and the sentence after it claims
    // nothing about the version.
    for why in ["held by local pin", "rolled back from 2.1.281"] {
        let mut st = crate::status::read(&l).unwrap();
        st.programs.get_mut("claude").unwrap().state = crate::state::vendor_kept("2.1.280", why);
        crate::status::write(&l, &st).unwrap();
        let (ok, out) = run(&managed_only);
        assert!(ok, "{out}");
        let line = out.lines().find(|l| l.contains("ok — claude:")).unwrap();
        assert_eq!(
            line,
            format!(
                "doctor: ok — claude: managed 2.1.280 — {why} — Claude Code's own updater \
                 is off here (DISABLE_AUTOUPDATER=1)"
            )
        );
    }
    // Shadowed: (10d)'s row alone — the foreign copy runs, and runs without the env.
    let foreign = l
        .prefix
        .parent()
        .unwrap()
        .join(format!("atpkg-doctor-env-foreign-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&foreign);
    std::fs::create_dir_all(&foreign).unwrap();
    let exe = foreign.join("claude");
    std::fs::write(&exe, b"#!/bin/sh\necho vendor\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    // (Inside aterm a warn; outside it a note — this shell's own
    // copy is the design there. Neither carries the sentence.)
    let ahead = std::env::join_paths([foreign.clone(), l.bin_dir()]).unwrap();
    for (stub_env, row) in [
        (crate::reroute::StubEnv { in_aterm: true }, "warn — claude:"),
        (crate::reroute::StubEnv::default(), "note — claude:"),
    ] {
        let (ok, out) = run_in(&ahead, stub_env);
        assert!(ok, "{out}");
        assert!(out.contains(row), "{stub_env:?}: {out}");
        assert!(!member_has_self_update_notice(&out, "claude"), "{out}");
    }
    // A plain shim: no line about it at all.
    crate::activate::install_tools(&l, &dir, &[tool("claude")], crate::activate::Aliases::Off)
        .unwrap();
    let (ok, out) = run(&managed_only);
    assert!(ok, "{out}");
    assert!(!member_has_self_update_notice(&out, "claude"), "{out}");
    assert!(!out.contains("exports nothing"), "{out}");
    // A plain shim over a build whose sidecar declares the env: a warn with the one remedy,
    // said with a foreign copy ahead too, since the `agents/` twin every tab runs copies it.
    crate::shim_env::write_sidecar(&dir, &env).unwrap();
    let drift = format!(
        "doctor: warn — claude: {} exports nothing, but claude 2.1.280 declares \
         DISABLE_AUTOUPDATER=1, so Claude Code's own updater is on here — fix: `aterm pkg \
         repair` re-lays it",
        l.shim(&tool("claude")).display()
    );
    for path in [&managed_only, &ahead] {
        let (ok, out) = run(path);
        assert!(ok, "a warn, not a fault: {out}");
        assert!(out.lines().any(|line| line == drift), "{drift}\n---\n{out}");
        assert!(!member_has_self_update_notice(&out, "claude"), "{out}");
    }
    // Exporting what the build declares: the ok line again, and no warn.
    crate::activate::install_tools_env(
        &l,
        &dir,
        &[tool("claude")],
        crate::activate::Aliases::Off,
        &env,
    )
    .unwrap();
    let (ok, out) = run(&managed_only);
    assert!(ok, "{out}");
    assert!(member_has_self_update_notice(&out, "claude"), "{out}");
    assert!(!out.contains("exports nothing"), "{out}");
    crate::shim_env::write_sidecar(&dir, &crate::shim_env::ShimEnv::NONE).unwrap();
    let _ = std::fs::remove_dir_all(&foreign);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A Homebrew-style `trust` (p11-kit, §17.11) ahead of the managed bin/ shadows ALab's: the
/// warn keeps the canonical state and gains one trailing sentence naming the alias, only
/// because the alias is laid.
#[cfg(unix)]
#[test]
fn a_shadowed_alab_tool_names_its_alias_as_the_way_to_the_managed_copy() {
    use std::os::unix::fs::PermissionsExt as _;
    let l = layout("shadowed-alias");
    let dir = install_build_tree(&l, "trust", 6808);
    install_shims(
        &l,
        &dir,
        &["trust".to_string()],
        crate::activate::Aliases::Alab,
    )
    .unwrap();
    activate_build(&l, &dir).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    let homebrew = l
        .prefix
        .parent()
        .unwrap()
        .join(format!("atpkg-doctor-homebrew-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&homebrew);
    std::fs::create_dir_all(&homebrew).unwrap();
    let exe = homebrew.join("trust");
    std::fs::write(&exe, b"#!/bin/sh\necho p11-kit\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = synthetic_home("shadowed-alias");
    let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
    let ahead = std::env::join_paths([homebrew.clone(), l.bin_dir()]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let ok = run_with(
        &l,
        Some(&home),
        Some(&ahead),
        now,
        None,
        "doctor",
        &Probes::default(),
        &mut out,
        &mut err,
    );
    let out = String::from_utf8_lossy(&out).into_owned();
    assert!(ok, "a shadow is a warning, not a structural fault:\n{out}");
    let state = crate::state::shadowed(6808, &exe);
    let line = out
        .lines()
        .find(|l| l.contains("warn — trust:"))
        .unwrap_or_else(|| panic!("a warn line for trust:\n{out}"));
    assert!(
        line.contains(&format!("doctor: warn — trust: {state} (")),
        "{line}"
    );
    assert!(
        line.ends_with(" — type alab-trust for the managed one"),
        "the fix-line is the trailing sentence: {line}"
    );
    // And the alias really is what runs the managed copy.
    assert!(
        crate::which(&l, "alab-trust").is_some_and(|t| t.starts_with(&dir)),
        "alab-trust resolves into the managed build"
    );
    let _ = std::fs::remove_dir_all(&homebrew);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// The speaker is the verb typed, so a script keying on the prefix keys on the verb it ran.
#[test]
fn status_never_speaks_as_doctor() {
    let l = layout("speaker");
    install(&l, "ay", 18);
    let home = synthetic_home("speaker");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
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

/// An empty store passes every structural check vacuously and is still not health: the tail
/// names exactly one command, after the byte-stable "found N problem(s)" line.
#[test]
fn an_unhealthy_report_names_one_next_act() {
    let l = layout("next-act");
    let home = synthetic_home("next-act");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    assert!(!run_with(
        &l,
        Some(&home),
        Some(&path),
        0,
        None,
        "doctor",
        &Probes::default(),
        &mut out,
        &mut err
    ));
    let text = String::from_utf8_lossy(&out);
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
    assert!(
        text.contains("doctor: PROBLEM — no ALab programs are installed\n"),
        "the PROBLEM line states the problem; its remedy is the one next act:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// The posture note never says to restart: a staged build applies in-session by the window's
/// overlap handoff and the shells keep running. Both note arms (a newer build staged; a newer
/// bundle on disk) and the quiet case are pinned, and both notes name the window.
#[test]
fn aterm_posture_never_prompts_a_restart() {
    let base = layout("posture");
    // The updater's ledger lives beside the pkg prefix (`<support>/Updates`).
    let lay = Layout {
        prefix: base.prefix.join("pkg"),
    };
    std::fs::create_dir_all(&lay.prefix).unwrap();
    let updates = base.prefix.join("Updates");
    std::fs::create_dir_all(&updates).unwrap();
    let forbidden = ["restart", "relaunch", "reopen"];
    let report_with = |lay: &Layout, installs_by_itself: bool| -> String {
        let mut out = Vec::new();
        report_aterm_posture(lay, &[], installs_by_itself, "atpkg", &mut out);
        String::from_utf8(out).unwrap()
    };
    let report = |lay: &Layout| report_with(lay, true);

    // A strictly-newer build staged while an older one runs: the in-session apply lane.
    std::fs::write(
        updates.join("installed.toml"),
        "build_number = 1788035619\n",
    )
    .unwrap();
    std::fs::write(
        updates.join("status.toml"),
        "current_build = 1788035619\nstaged_build = 1788077184\n",
    )
    .unwrap();
    let text = report(&lay);
    assert_eq!(
        text,
        "atpkg: note — aterm build 1788077184 is downloaded (running build 1788035619) and \
         installs within a minute while an aterm window is open — your shells keep \
         running; with no window open: aterm --window\n"
    );
    let lower = text.to_lowercase();
    for w in forbidden {
        assert!(!lower.contains(w), "{w:?} must never appear: {text}");
    }
    // With `[update] auto_apply = false` the window installs nothing by itself: the note
    // names the press, as `aterm update` does.
    assert_eq!(
        report_with(&lay, false),
        "atpkg: note — aterm build 1788077184 is downloaded (running build 1788035619) — \
         install it from Settings \u{25b8} Software Update; your shells keep running\n"
    );
    // The window's installs of it keep failing (health.toml's apply streak): no install
    // is promised, whichever way `auto_apply` reads — `aterm update`'s words.
    std::fs::write(
        updates.join("health.toml"),
        "apply_failures = 3\napply_failures_for_target = 3\n\
         last_apply_failure_target_build = 1788077184\n",
    )
    .unwrap();
    for by_itself in [true, false] {
        assert_eq!(
            report_with(&lay, by_itself),
            "atpkg: note — aterm build 1788077184 is downloaded (running build 1788035619) \
             but didn\u{2019}t install (3 tries)\n"
        );
    }
    std::fs::write(
        updates.join("health.toml"),
        "apply_failures_for_target = 1\nlast_apply_failure_target_build = 1788077184\n",
    )
    .unwrap();
    assert!(report(&lay).ends_with("but didn\u{2019}t install (1 try)\n"));
    std::fs::write(
        updates.join("health.toml"),
        "apply_failures_for_target = 0\nlast_apply_failure_target_build = 1788077184\n",
    )
    .unwrap();
    assert!(report(&lay).contains("installs within a minute"));
    // An earlier build's failed installs are not this download's: the escalation streak and
    // the per-target count both name build 1788035000, and 1788077184 is still promised.
    std::fs::write(
        updates.join("health.toml"),
        "apply_failures = 3\napply_failures_for_target = 3\n\
         last_apply_failure_target_build = 1788035000\n",
    )
    .unwrap();
    assert!(report(&lay).contains("installs within a minute"));
    std::fs::remove_file(updates.join("health.toml")).unwrap();

    // A newer bundle already on disk (the running process predates it): activation,
    // still in-session and still no reopen.
    std::fs::write(
        updates.join("installed.toml"),
        "build_number = 1788077184\n",
    )
    .unwrap();
    std::fs::write(updates.join("status.toml"), "current_build = 1788035619\n").unwrap();
    let text = report(&lay);
    assert_eq!(
        text,
        "atpkg: note — aterm build 1788077184 is on disk (running build 1788035619) and an \
         aterm window switches to it in place — your shells keep running; with no window \
         open: aterm --window\n"
    );
    let lower = text.to_lowercase();
    for w in forbidden {
        assert!(!lower.contains(w), "{w:?} must never appear: {text}");
    }
    assert_eq!(
        report_with(&lay, false),
        "atpkg: note — aterm build 1788077184 is on disk (running build 1788035619) — \
         install it from Settings \u{25b8} Software Update; your shells keep running\n"
    );

    // Running what is installed, nothing staged: the plain ok line, no note.
    std::fs::write(updates.join("status.toml"), "current_build = 1788077184\n").unwrap();
    let text = report(&lay);
    assert_eq!(text, "atpkg: ok — aterm build 1788077184 installed\n");

    // A source build over an installed bundle: the version comes off the app dir's bundle
    // whose sealed build is the installed one.
    let apps = base.prefix.join("Applications");
    std::fs::create_dir_all(apps.join("aterm.app/Contents")).unwrap();
    std::fs::write(
        apps.join("aterm.app/Contents/Info.plist"),
        "<key>CFBundleVersion</key>\n<string>1788077184</string>\n\
         <key>CFBundleShortVersionString</key>\n<string>0.93.0</string>\n",
    )
    .unwrap();
    let mut out = Vec::new();
    report_aterm_posture(&lay, &[apps], true, "atpkg", &mut out);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "atpkg: ok — aterm 0.93.0 (build 1788077184) installed\n"
    );

    let _ = std::fs::remove_dir_all(&base.prefix);
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
    let report_v = |bundle: Option<u64>, versions: &[(u64, String)]| -> String {
        let mut out = Vec::new();
        report_aterm_posture_at(&updates, bundle, versions, true, "atpkg", &mut out);
        String::from_utf8(out).unwrap()
    };
    let report = |bundle: Option<u64>| report_v(bundle, &[]);
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
    // The hand-placed shape: receipt older than the running build, bundle == running.
    assert_eq!(
        report(Some(1_789_000_876)),
        "atpkg: ok — aterm build 1789000876 installed\n",
        "on disk is derived from the bundle; no downgrade note"
    );
    // …and with the bundle's marketing version in hand, the row names it — the fact a
    // person recognises — beside the build.
    assert_eq!(
        report_v(Some(1_789_000_876), &[(1_789_000_876, "1.2.3".into())]),
        "atpkg: ok — aterm 1.2.3 (build 1789000876) installed\n"
    );
    // A version read off a bundle that is not the installed build (a newer receipt than the
    // running bundle) is not claimed for it: build-only.
    std::fs::write(
        updates.join("installed.toml"),
        "build_number = 1789000900\n",
    )
    .unwrap();
    std::fs::write(updates.join("status.toml"), "current_build = 1789000900\n").unwrap();
    assert_eq!(
        report_v(Some(1_789_000_876), &[(1_789_000_876, "1.2.3".into())]),
        "atpkg: ok — aterm build 1789000900 installed\n"
    );
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
    // The plist reader: the string immediately after the key, shaped as a version.
    let plist = "<key>CFBundleVersion</key>\n<string>1789000876</string>\n\
                 <key>CFBundleShortVersionString</key>\n<string>0.93.0</string>\n";
    assert_eq!(plist_short_version(plist).as_deref(), Some("0.93.0"));
    assert_eq!(plist_bundle_version(plist), Some(1_789_000_876));
    assert_eq!(
        plist_short_version("<key>CFBundleShortVersionString</key>\n<string>x</string>"),
        None,
        "a token that is not version-shaped is not printed"
    );
    assert_eq!(
        plist_short_version("<key>Other</key><string>0.1.0</string>"),
        None
    );
    // Receipt older, no bundle readable (a source build): older than what runs, so no
    // "installed on disk" note, which is for newer.
    assert_eq!(
        report(None),
        "atpkg: ok — aterm build 1786405661 installed\n"
    );
    // …but an app dir's bundle sealed at that build names its version (a source build
    // over an installed app).
    assert_eq!(
        report_v(None, &[(1_786_405_661, "0.93.0".into())]),
        "atpkg: ok — aterm 0.93.0 (build 1786405661) installed\n"
    );
    // A bundle strictly newer than the running process: the activation note.
    let text = report(Some(1_789_999_999));
    assert!(
        text.contains("aterm build 1789999999 is on disk (running build 1789000876)"),
        "{text}"
    );
    assert!(text.contains("switches to it in place"), "{text}");
    // …named by version where a bundle sealed at that build says it.
    let text = report_v(
        Some(1_789_999_999),
        &[
            (1_789_999_999, "0.95.0".into()),
            (1_789_000_876, "0.94.0".into()),
        ],
    );
    assert!(
        text.contains("aterm 0.95.0 is on disk (running 0.94.0)"),
        "{text}"
    );
    // Two builds stamped with one version (a dev bundle): the build numbers tell them apart.
    let text = report_v(
        Some(1_789_999_999),
        &[
            (1_789_999_999, "0.95.0".into()),
            (1_789_000_876, "0.95.0".into()),
        ],
    );
    assert!(
        text.contains("aterm build 1789999999 is on disk (running build 1789000876)"),
        "{text}"
    );
    // A receipt newer than both (the updater installed, the process predates it):
    // the receipt wins the max and the note fires off it.
    std::fs::write(
        updates.join("installed.toml"),
        "build_number = 1790000000\n",
    )
    .unwrap();
    let text = report(Some(1_789_000_876));
    assert!(text.contains("aterm build 1790000000 is on disk"), "{text}");
    let _ = std::fs::remove_dir_all(&base.prefix);
}

/// The plist scan binds the value to the key right before it and parses only an
/// integer build.
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
}

/// `run_with` over `probes` at a fixed clock: whether the report is healthy, and its words.
fn doctor_over(l: &Layout, home: &Path, probes: &Probes) -> (bool, String) {
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let now = crate::flow::rfc3339_to_unix("2026-08-27T00:00:00Z").unwrap();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let ok = run_with(
        l,
        Some(home),
        Some(&path),
        now,
        None,
        "doctor",
        probes,
        &mut out,
        &mut err,
    );
    (ok, String::from_utf8_lossy(&out).into_owned())
}

/// The `devtools` row: off the list is a warning the default report counts and shortens to
/// the fact and the fix (the exit stays healthy); on the list is `ok`; a report that does not
/// read the machine says nothing.
#[test]
fn the_devtools_row_is_a_counted_warning_with_the_owners_fix() {
    use aterm_containment::consent::{
        DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel,
    };
    let l = layout("devtools-row");
    install(&l, "ay", 19);
    let home = synthetic_home("devtools-row");
    let with = |listing: DevToolListing| Probes {
        devtools: Some(crate::devtools::Facts {
            subject: Some("com.aterm.aterm".to_string()),
            list: DevToolList {
                listing,
                label: DevToolListLabel::Read,
                modified_at_s: (listing != DevToolListing::Absent).then_some(1),
                rows: Some(1),
            },
            log: DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest),
            other_responsible: None,
        }),
        ..Probes::default()
    };
    let (ok, out) = doctor_over(&l, &home, &with(DevToolListing::Absent));
    assert!(
        ok,
        "not being on the list is advisory, never structural:\n{out}"
    );
    assert!(
        out.contains(
            "doctor: warn — devtools: aterm (com.aterm.aterm) is not on System Settings \
             \u{25b8} Privacy & Security \u{25b8} Developer Tools \u{2014} macOS holds every \
             new program it starts for a first-run check"
        ),
        "{out}"
    );
    let shown = present(&out, "", "doctor", Detail::Problems).joined();
    assert!(
        shown.contains(
            "doctor: warn \u{2014} devtools: aterm (com.aterm.aterm) is not on System Settings \
             \u{25b8} Privacy & Security \u{25b8} Developer Tools \u{2014} fix: add aterm to \
             Developer Tools\n"
        ),
        "the default report keeps the row whole, fact and fix, counted:\n{shown}"
    );
    assert!(shown.contains("thing(s) to look at"), "{shown}");
    let (ok, out) = doctor_over(&l, &home, &with(DevToolListing::On));
    assert!(ok, "{out}");
    assert!(
        out.contains("doctor: ok — devtools: aterm (com.aterm.aterm) is on System Settings"),
        "{out}"
    );
    let (_, out) = doctor_over(&l, &home, &Probes::default());
    assert!(!out.contains("devtools:"), "no reading, no row:\n{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_workspace_policy_the_installed_targo_refuses_is_a_problem_that_names_the_cure() {
    let l = layout("wspolicy");
    install(&l, "trust", 6808);
    let home = synthetic_home("wspolicy");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let probes = Probes {
        workspace: Some(WorkspacePolicyProbe {
            workspace: PathBuf::from("/work/ty"),
            policy: WorkspacePolicy::UnknownField {
                field: "compiler_timeout_secs".into(),
            },
        }),
        local_seal: None,
        ..Probes::default()
    };
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    assert!(!run_with(
        &l,
        Some(&home),
        Some(&path),
        0,
        None,
        "doctor",
        &probes,
        &mut out,
        &mut err
    ));
    let err = String::from_utf8(err).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        err.contains("`compiler_timeout_secs`"),
        "names the refused key: {err}"
    );
    assert!(
        err.contains("build 6808"),
        "names the installed build: {err}"
    );
    assert!(err.contains(PUBLISH_RUSTC_GROUP), "names the cure: {err}");
    assert!(
        out.contains("next — publish the newer Trust coherence group"),
        "next act: {out}"
    );
}

#[test]
fn an_accepted_workspace_policy_is_ok_and_a_local_seal_only_warns() {
    let l = layout("wsok");
    install(&l, "trust", 6808);
    let home = synthetic_home("wsok");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let local = home.join("trust-d3866677");
    std::fs::create_dir_all(&local).unwrap();
    let mut probes = Probes {
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
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    assert!(run_with(
        &l,
        Some(&home),
        Some(&path),
        0,
        None,
        "doctor",
        &probes,
        &mut out,
        &mut err
    ));
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("[trust] policy the installed trust build 6808 accepts"),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "warn — rustup `trust` names a local toolchain, not the managed one: {} \
             (trustc d3866677)",
            local.display()
        )),
        "{out}"
    );
    assert!(
        out.contains("ln -sfn '")
            && out.contains("' '/Users//me/.rustup/toolchains/trust', then aterm pkg repair"),
        "{out}"
    );
    assert!(!out.contains("features only it has"), "{out}");
    assert!(out.contains("healthy"), "{out}");
    // With the entry really laid at that local toolchain, (9) stays silent: the one
    // entry gets one warning and one fix, not a second re-point line.
    #[cfg(unix)]
    {
        let toolchains = home.join(".rustup").join("toolchains");
        std::fs::create_dir_all(&toolchains).unwrap();
        std::os::unix::fs::symlink(&local, toolchains.join("trust")).unwrap();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            0,
            None,
            "doctor",
            &probes,
            &mut out,
            &mut err,
        );
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.matches("warn — rustup `trust`").count(), 1, "{out}");
        assert!(!out.contains("is NOT the managed store"), "{out}");
    }
    // A target that is gone is said to be gone, never "builds with it".
    if let Some(seal) = probes.local_seal.as_mut() {
        seal.link_target = home.join("gone");
    }
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let _ = run_with(
        &l,
        Some(&home),
        Some(&path),
        0,
        None,
        "doctor",
        &probes,
        &mut out,
        &mut err,
    );
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("which no longer exists"), "{out}");
    assert!(!out.contains("build with it"), "{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A local toolchain older than the store's, or gone, is not a publisher's unpublished seal:
/// the line says it is stale and names `repair`.
#[test]
fn an_older_local_toolchain_is_named_stale_with_repair_as_its_fix() {
    let l = layout("stale-seal");
    install(&l, "trust", 9192);
    let home = synthetic_home("stale-seal");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let probes = Probes {
        local_seal: Some(LocalSealProbe {
            link: PathBuf::from("/Users//me/.rustup/toolchains/trust"),
            link_target: PathBuf::from("/Users//me/trust/build/host/stage2"),
            trustc: "1a2b3c4d5e".into(),
            stale: Some(crate::seam::Stale::Older {
                its: "2026-07-17".into(),
                store: "2026-09-17".into(),
            }),
        }),
        ..Probes::default()
    };
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let _ = run_with(
        &l,
        Some(&home),
        Some(&path),
        0,
        None,
        "doctor",
        &probes,
        &mut out,
        &mut err,
    );
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains(
            "warn — rustup's trust channel resolves to a LOCAL toolchain OLDER than the \
             atpkg store's: /Users//me/trust/build/host/stage2 (trustc 1a2b3c4d5e, \
             2026-07-17; the store's is 2026-09-17)"
        ),
        "{out}"
    );
    assert!(
        out.contains("fix: `aterm pkg repair` re-points it"),
        "{out}"
    );
    assert!(!out.contains("features only it has"), "{out}");
    // A link to a tree that is gone: every `cargo +trust` fails and repair relinks, said once
    // with that one fix, not again by (9) as a foreign link or a channel rustup lacks.
    #[cfg(unix)]
    {
        let toolchains = home.join(".rustup").join("toolchains");
        std::fs::create_dir_all(&toolchains).unwrap();
        std::os::unix::fs::symlink(
            "/Users//me/trust/build/host/stage2",
            toolchains.join("trust"),
        )
        .unwrap();
    }
    let probes = Probes {
        local_seal: Some(LocalSealProbe {
            link: PathBuf::from("/Users//me/.rustup/toolchains/trust"),
            link_target: PathBuf::from("/Users//me/trust/build/host/stage2"),
            trustc: "unknown".into(),
            stale: Some(crate::seam::Stale::Dangling),
        }),
        ..Probes::default()
    };
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let _ = run_with(
        &l,
        Some(&home),
        Some(&path),
        0,
        None,
        "doctor",
        &probes,
        &mut out,
        &mut err,
    );
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains(
            "warn — rustup's trust channel names /Users//me/trust/build/host/stage2, which \
             no longer exists"
        ),
        "{out}"
    );
    assert!(
        out.contains("fix: `aterm pkg repair` re-points it"),
        "{out}"
    );
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
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let bin = l.build_dir("trust", 8589).join("bin");
    (l, home, path, bin)
}

/// The whole report: (exit ok, stdout, stderr).
#[cfg(unix)]
fn whole_report(l: &Layout, home: &Path, path: &OsStr) -> (bool, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ok = run_with(
        l,
        Some(home),
        Some(path),
        0,
        None,
        "doctor",
        &Probes::default(),
        &mut out,
        &mut err,
    );
    (
        ok,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[cfg(unix)]
fn healthy(out: &str) -> bool {
    out.lines().any(|line| line == "doctor: healthy")
}

/// The one "tippy cannot run" warn in `out`, which must exist.
#[cfg(unix)]
fn tippy_warn(out: &str) -> &str {
    let mut warns = out.lines().filter(|line| line.contains("tippy cannot run"));
    let warn = warns
        .next()
        .unwrap_or_else(|| panic!("the warn is printed:\n{out}"));
    assert_eq!(warns.next(), None, "one warn, not two:\n{out}");
    assert!(
        warn.starts_with("doctor: warn — trust build 8589: tippy cannot run — "),
        "{warn}"
    );
    assert!(
        warn.ends_with(&format!(" (the bundle's fix: {TRUST_COMPILER_FIX})")),
        "every refusal carries the one fix that was measured to work: {warn}"
    );
    warn
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

/// The closing line counts each kind of withheld warn under its own words.
#[test]
fn the_not_healthy_line_counts_cannot_run_apart_from_unproven() {
    assert_eq!(withheld_summary(2, 0), "2 managed tool(s) cannot run");
    assert_eq!(
        withheld_summary(0, 1),
        "1 managed tool(s) may not run or cannot be checked"
    );
    assert_eq!(
        withheld_summary(1, 1),
        "1 managed tool(s) cannot run and 1 may not run or cannot be checked"
    );
}

/// (5f) The view: no seam, nothing to say; an attached seam's stock names are clones and the
/// report is healthy. A hard-linked `rustc`, or one with `trustc`'s attributes and other
/// bytes, is named with the repair and withholds "healthy"; re-assertion heals it.
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
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
    assert!(!out.contains("is not a clone of the store's"), "{out}");
    let store_trustc = bin.join("trustc");
    let view_rustc = crate::seam::view_dir(&l, "trust").join("bin").join("rustc");
    assert!(presents(&store_trustc, &view_rustc));
    let warn = format!(
        "doctor: warn — rustup `trust`: {} is not a clone of the store's {} (absent, other \
         bytes, or the hard link a view held before clones), so `rustc +trust` runs \
         something other than the managed trustc, or nothing; fix: `aterm pkg repair` \
         rebuilds the view",
        view_rustc.display(),
        store_trustc.display()
    );

    // A hard link in place of the view's clone.
    std::fs::remove_file(&view_rustc).unwrap();
    std::fs::hard_link(&store_trustc, &view_rustc).unwrap();
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert_not_healthy_exit_0(ok, &out, &err);
    assert!(out.contains(&warn), "{out}");
    // Rebuilt as clones, silently: it presents the same bytes it did.
    let lines = crate::seam::reassert(&l, &rustup);
    assert!(lines.is_empty(), "{lines:?}");
    assert!(presents(&store_trustc, &view_rustc));
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");

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
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert_not_healthy_exit_0(ok, &out, &err);
    assert!(out.contains(&warn), "the bytes can: {out}");

    // What repair does: re-assert, which rebuilds the view — and says so, since the
    // view's `rustc` changed.
    let lines = crate::seam::reassert(&l, &rustup);
    assert!(
        lines.len() == 1 && lines[0].contains("now presents"),
        "{lines:?}"
    );
    assert!(presents(&store_trustc, &view_rustc));
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
}

/// (5f) The view follows a dev-link: against a sysroot checkout the view is compared with the
/// checkout's tools (healthy); a view left presenting the store is named with the repair;
/// unlinked, the store's view is right again.
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
    let view_rustc = crate::seam::view_dir(&l, "trust").join("bin").join("rustc");
    assert!(presents(&bin.join("trustc"), &view_rustc));

    // A checkout shaped like a sysroot, dev-linked — and the seam not yet re-asserted:
    // the view still presents the store, which is now the wrong compiler.
    let checkout = home.join("stage2");
    std::fs::create_dir_all(checkout.join("bin")).unwrap();
    std::fs::create_dir_all(checkout.join("lib")).unwrap();
    for tool in ["trustc", "targo", "tippy"] {
        let executable = checkout.join("bin").join(tool);
        std::fs::write(&executable, format!("dev {tool}")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::linkmode::link(
        &l,
        "trust",
        &checkout,
        &[std::path::PathBuf::from("bin/trustc")],
    )
    .unwrap();
    let (ok, out, err) = whole_report(&l, &home, &path);
    // Two stock names differ (`rustc` and `cargo`; the checkout ships no trustdoc,
    // so `rustdoc` is not compared), each a tool that cannot run.
    assert_withheld_exit_0(ok, &out, &err, "2 managed tool(s) cannot run");
    assert!(
        out.contains(&format!(
            "doctor: warn — rustup `trust`: {} is not the dev-linked checkout's {}",
            view_rustc.display(),
            checkout.join("bin").join("trustc").display()
        )),
        "{out}"
    );

    // What `link` (and repair) does: re-assert, which lays the checkout's view.
    let lines = crate::seam::reassert(&l, &rustup);
    assert!(
        lines.len() == 1 && lines[0].contains("now presents"),
        "{lines:?}"
    );
    assert!(
        std::fs::symlink_metadata(&view_rustc).unwrap().is_file(),
        "the view's rustc is a stub, not a link"
    );
    assert_eq!(
        crate::platform::resolve_shim(&view_rustc).as_deref(),
        Some(checkout.join("bin").join("trustc").as_path()),
        "…that execs the checkout's trustc"
    );
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
    assert!(!out.contains("is not the"), "{out}");

    crate::linkmode::unlink(&l, "trust").unwrap();
    let lines = crate::seam::reassert(&l, &rustup);
    assert!(
        lines.len() == 1 && lines[0].contains("now presents"),
        "{lines:?}"
    );
    assert!(presents(&bin.join("trustc"), &view_rustc));
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
}

/// (5f) A store-less dev-link has a seam too, checked against the checkout, the only
/// thing there is: a view that presents it is not warned about; a stub replaced by a
/// copy is a tool that cannot run, named with the repair.
#[cfg(unix)]
#[test]
fn a_store_less_dev_linked_seam_is_checked_against_the_checkout() {
    let l = layout("view-devlink-nostore");
    let home = synthetic_home("view-devlink-nostore");
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
    let checkout = home.join("stage2");
    std::fs::create_dir_all(checkout.join("bin")).unwrap();
    std::fs::create_dir_all(checkout.join("lib")).unwrap();
    for tool in ["trustc", "targo"] {
        let executable = checkout.join("bin").join(tool);
        std::fs::write(&executable, format!("dev {tool}")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::linkmode::link(
        &l,
        "trust",
        &checkout,
        &[std::path::PathBuf::from("bin/trustc")],
    )
    .unwrap();
    let rustup = home.join(".rustup");
    std::fs::create_dir_all(rustup.join("toolchains")).unwrap();
    let lines = crate::seam::reassert(&l, &rustup);
    assert!(
        lines.len() == 1 && lines[0].contains("attached"),
        "{lines:?}"
    );
    let (_, out, _) = whole_report(&l, &home, &path);
    assert!(!out.contains("is not the"), "{out}");

    let view_rustc = crate::seam::view_dir(&l, "trust").join("bin").join("rustc");
    std::fs::remove_file(&view_rustc).unwrap();
    std::fs::write(&view_rustc, b"a copy").unwrap();
    let (_, out, _) = whole_report(&l, &home, &path);
    assert!(
        out.contains(&format!(
            "doctor: warn — rustup `trust`: {} is not the dev-linked checkout's {}",
            view_rustc.display(),
            checkout.join("bin").join("trustc").display()
        )),
        "{out}"
    );

    // The checkout stops being a sysroot: the seam refuses, and with no store build behind
    // the view doctor names the one fix (`rustc +trust` would run a compiler with no sysroot).
    std::fs::remove_dir_all(checkout.join("lib")).unwrap();
    let lines = crate::seam::reassert(&l, &rustup);
    assert!(
        lines.len() == 1 && lines[0].contains("not a sysroot"),
        "{lines:?}"
    );
    let (_, out, _) = whole_report(&l, &home, &path);
    assert!(
        out.contains("which atpkg cannot present") && out.contains("fix: `aterm pkg unlink trust`"),
        "{out}"
    );
}

/// (5f) tippy refuses a symlinked `trustc`: the report names a tippy that cannot run, with
/// the one fix. A plain `trustc`, or a bundle with no tippy, says nothing.
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
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert_not_healthy_exit_0(ok, &out, &err);
    let warn = tippy_warn(&out);
    assert!(
        warn.contains(&format!(
            "{} is a symbolic link, and tippy requires the selected compiler trustc to be a \
             plain file",
            trustc.display()
        )),
        "{warn}"
    );

    // A plain compiler: healthy, nothing said.
    std::fs::remove_file(&trustc).unwrap();
    std::fs::write(&trustc, b"trustc").unwrap();
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(
        ok && healthy(&out) && !out.contains("tippy cannot run"),
        "{out}{err}"
    );

    // The link back, but no tippy in the bundle: nothing is refused.
    std::fs::remove_file(&trustc).unwrap();
    std::os::unix::fs::symlink("real", &trustc).unwrap();
    std::fs::remove_file(bin.join("tippy")).unwrap();
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(
        ok && healthy(&out) && !out.contains("tippy cannot run"),
        "{out}{err}"
    );
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
    let path = std::env::join_paths([l.bin_dir()]).unwrap();
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

/// The lines of `out` about trust build 8595's exec root.
#[cfg(unix)]
fn exec_root_lines(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|line| line.contains(" — trust build 8595: "))
        .collect()
}

/// (5f)(c)-(f) The exec root over the whole report, every state read-only: no root, the
/// root laid, an alias unrouted, a planted file, a symlinked `compat/trust`, an unreadable
/// `rustc`, stray roots, and a build that stops needing one.
#[cfg(unix)]
#[test]
fn an_affected_trust_build_is_healthy_only_while_its_exec_root_routes_every_shim() {
    use std::os::unix::fs::MetadataExt;
    let (l, home, path, bin) = affected_trust_store("exec-root");
    let differing = crate::compat::needs_root(bin.parent().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(differing, 6, "`rustc_` against `trustc`: six positions");
    let ino = |f: &str| std::fs::symlink_metadata(bin.join(f)).unwrap().ino();
    let root = crate::compat::root_dir(&l, 8595);
    let copy = format!(
        "bin/rustc (inode {}) is a separate file from bin/trustc (inode {}; 6 byte(s) differ)",
        ino("rustc"),
        ino("trustc")
    );
    let fix = "fix: `aterm pkg repair` lays the exec root (a copy-on-write clone — the store \
               is never modified) and routes the shims";

    // Plain shims, no root.
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert_not_healthy_exit_0(ok, &out, &err);
    assert_eq!(
        exec_root_lines(&out),
        vec![format!(
            "doctor: warn — trust build 8595: PATH tippy cannot run — {copy}, which this \
             build's tippy refuses, and no exec root stands at {}; {fix}",
            root.display()
        )],
        "{out}"
    );
    assert!(
        std::fs::symlink_metadata(crate::compat::compat_dir(&l)).is_err(),
        "doctor lays nothing"
    );

    // The pass heals it: root laid, every trust shim routed.
    let report = crate::compat::reconcile(&l, crate::seam::Depth::Shallow);
    assert_eq!(report.built, vec![8595], "{report:?}");
    assert_eq!(report.routed.len(), 5, "{report:?}");
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
    assert_eq!(
        exec_root_lines(&out),
        vec![format!(
            "doctor: note — trust build 8595: {copy}, which its tippy refuses; its 5 trust \
             shim(s) run each tool from {}, where rustc holds trustc's bytes (a \
             copy-on-write clone; the store is untouched) — removed with the build",
            root.display()
        )],
        "{out}"
    );

    // One alias laid plain again, as by a writer that decided no route.
    write_plain_shim(&l, "alab-tippy", &bin.join("tippy"));
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert_not_healthy_exit_0(ok, &out, &err);
    assert_eq!(
        exec_root_lines(&out),
        vec![format!(
            "doctor: warn — trust build 8595: PATH tippy cannot run through 1 of 5 trust \
             shim(s) (e.g. alab-tippy) — {copy}, which this build's tippy refuses, and \
             those shims do not route through the exec root {}; {fix}",
            root.display()
        )],
        "{out}"
    );
    let report = crate::compat::reconcile(&l, crate::seam::Depth::Shallow);
    assert!(
        report.built.is_empty() && report.routed.len() == 1,
        "{report:?}"
    );
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");

    // A file planted in the root's `lib/`: the shims still route, but what they run is
    // not the build. Only a Deep walk sees it, and only a Deep reconcile heals it.
    let planted = root.join("lib").join("planted.dylib");
    std::fs::write(&planted, b"not the build's").unwrap();
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert_unproven_exit_0(ok, &out, &err);
    assert_eq!(
        exec_root_lines(&out),
        vec![format!(
            "doctor: warn — trust build 8595: PATH tippy may not run — {copy}, which this \
             build's tippy refuses, and the exec root {} is not the store build {} file for \
             file (first difference: {}); {fix}",
            root.display(),
            bin.parent().unwrap().display(),
            planted.display()
        )],
        "{out}"
    );
    let report = crate::compat::reconcile(&l, crate::seam::Depth::Deep);
    assert_eq!(report.built, vec![8595], "{report:?}");
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out) && out.contains("doctor: note — trust build 8595: "));
    assert!(err.is_empty(), "{err}");

    // A symbolic link at `compat/trust`, naming the very directory that stood there. No
    // update, repair or gc follows or removes it, so `repair` refuses on every run: every
    // line names the link and says to remove it first, never "run repair".
    let roots = crate::compat::roots_dir(&l);
    let aside = l.prefix.with_file_name(format!(
        "{}-compat-trust-aside",
        l.prefix.file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(&roots, &aside).unwrap();
    std::os::unix::fs::symlink(&aside, &roots).unwrap();
    let link_fix = "fix: remove that link (only the link: whatever it names is left as it \
                    is), then `aterm pkg repair` lays the exec root";
    let blocked = format!(
        "{} is a symbolic link, not a directory — atpkg lays nothing through a link \
         there, and no update, repair or gc removes it, so no exec root is laid at {} and \
         no shim routes through one",
        roots.display(),
        root.display()
    );
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert_unproven_exit_0(ok, &out, &err);
    assert_eq!(
        exec_root_lines(&out),
        vec![format!(
            "doctor: warn — trust build 8595: PATH tippy may not run — {copy}, which this \
             build's tippy refuses, and {blocked}; {link_fix}"
        )],
        "{out}"
    );
    let report = crate::compat::reconcile(&l, crate::seam::Depth::Deep);
    assert_eq!(
        report.errors,
        vec![format!(
            "trust build 8595: {blocked} — its trust shims render plain and run the store \
             path; {link_fix}"
        )],
        "{report:?}"
    );
    assert_eq!(report.routed.len(), 5, "{report:?}");
    let refused =
        crate::compat::ensure_root(&l, bin.parent().unwrap(), crate::seam::Depth::Shallow)
            .unwrap_err();
    assert_eq!(
        crate::compat::not_laid_line(&l, 8595, &refused),
        format!(
            "atpkg: trust build 8595: {blocked} — this build's trust shims run the store \
             path, where its tippy refuses to start; {link_fix}"
        )
    );
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert_not_healthy_exit_0(ok, &out, &err);
    assert_eq!(
        exec_root_lines(&out),
        vec![format!(
            "doctor: warn — trust build 8595: PATH tippy cannot run — {copy}, which this \
             build's tippy refuses, and {blocked}; {link_fix}"
        )],
        "{out}"
    );
    assert!(
        aside.join("8595").join("bin").join("tippy").is_file(),
        "the directory behind the link is left whole"
    );
    std::fs::remove_file(&roots).unwrap();
    std::fs::rename(&aside, &roots).unwrap();
    let report = crate::compat::reconcile(&l, crate::seam::Depth::Deep);
    assert!(
        report.errors.is_empty() && report.built.is_empty() && report.routed.len() == 5,
        "{report:?}"
    );
    let (ok, out, err) = whole_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");

    // (f) The build's `bin/rustc` unreadable: an unanswered question is not "needs none".
    // A warn that withholds "healthy" and names the failed read — and the root, which
    // doctor never touches, still stands for the passes to leave alone.
    if crate::platform::our_uid() != 0 {
        use std::os::unix::fs::PermissionsExt;
        let rustc = bin.join("rustc");
        let mode = std::fs::metadata(&rustc).unwrap().permissions();
        std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (ok, out, err) = read_only_report(&l, &home, &path);
        std::fs::set_permissions(&rustc, mode).unwrap();
        assert_unproven_exit_0(ok, &out, &err);
        let lines = exec_root_lines(&out);
        assert_eq!(lines.len(), 1, "{out}");
        assert!(
            lines[0].starts_with(
                "doctor: warn — trust build 8595: cannot tell whether PATH tippy can run — \
                 cannot compare "
            ) && lines[0].ends_with(
                "; any exec root standing for the build is left as it is; fix: make that \
                 store file readable again (`aterm pkg verify trust` checks the build \
                 against its signed tree)"
            ),
            "{out}"
        );
        assert!(root.is_dir());
    }

    // Roots no build needs: advisory, named with gc, and exactly what gc's sweep takes.
    let gone = crate::compat::root_dir(&l, 9999);
    std::fs::create_dir_all(gone.join("bin")).unwrap();
    let link = crate::compat::root_dir(&l, 42);
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert!(ok && healthy(&out), "advisory, never health:\n{out}{err}");
    let strays: Vec<&str> = out.lines().filter(|l| l.contains("aterm pkg gc")).collect();
    assert_eq!(
        strays,
        vec![
            format!(
                "doctor: warn — {} is not a directory, and atpkg lays only exec root \
                 directories under that name; fix: `aterm pkg gc` removes it",
                link.display()
            ),
            format!(
                "doctor: warn — exec root {} outlives trust build 9999, which is no longer \
                 in the store, and its clones keep that build's reclaimed blocks \
                 allocated; fix: `aterm pkg gc` removes it",
                gone.display()
            ),
        ],
        "{out}"
    );
    let mut swept = crate::compat::sweep(&l).swept;
    swept.sort();
    assert_eq!(swept, vec![link.clone(), gone.clone()]);
    assert!(home.is_dir(), "the symlink was unlinked, not followed");

    // The build stops needing a root: `rustc` a hard link of `trustc`. No (c) line; the
    // root it leaves is a (d) warn until the sweep takes it, then nothing at all.
    std::fs::remove_file(bin.join("rustc")).unwrap();
    std::fs::hard_link(bin.join("trustc"), bin.join("rustc")).unwrap();
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
    assert!(exec_root_lines(&out).is_empty(), "{out}");
    assert_eq!(
        out.lines()
            .filter(|l| l.contains("aterm pkg gc"))
            .collect::<Vec<_>>(),
        vec![format!(
            "doctor: warn — exec root {} stands for trust build 8595, which needs none (its \
             bin/rustc is not a separate file from bin/trustc that its tippy refuses); fix: \
             `aterm pkg gc` removes it",
            root.display()
        )],
        "{out}"
    );
    assert_eq!(crate::compat::sweep(&l).swept, vec![root.clone()]);
    let (ok, out, err) = read_only_report(&l, &home, &path);
    assert!(ok && healthy(&out), "{out}{err}");
    assert!(
        !out.contains("exec root") && exec_root_lines(&out).is_empty(),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (5f)(c)'s example name prefers `tippy` — the tool the line is about — over whichever
/// unrouted shim sorts first; a root that differs names the root, the store build and the
/// first difference each as what it is; and a blocked `compat` names the manual fix.
#[test]
fn the_exec_root_warn_names_tippy_first_and_the_first_difference() {
    use crate::compat::{Inspection, RootState};
    let mut i = Inspection {
        differing: 2428,
        rustc_ino: 11,
        trustc_ino: 12,
        build_dir: PathBuf::from("/p/store/trust/8595"),
        root: PathBuf::from("/p/compat/trust/8595"),
        state: RootState::Matches,
        shims: ["alab-tippy", "targo", "tippy", "trustc"]
            .map(String::from)
            .to_vec(),
        unrouted: ["alab-tippy", "targo", "tippy"].map(String::from).to_vec(),
    };
    let (line, claim) = exec_root_line("doctor", 8595, &i);
    assert_eq!(claim, ToolClaim::CannotRun);
    assert!(
        line.contains(
            "PATH tippy cannot run through 3 of 4 trust shim(s) (e.g. tippy) — bin/rustc \
             (inode 11) is a separate file from bin/trustc (inode 12; 2428 byte(s) differ)"
        ),
        "{line}"
    );
    i.unrouted = vec!["targo".into()];
    assert!(
        exec_root_line("doctor", 8595, &i)
            .0
            .contains("(e.g. targo)")
    );
    // Each path labelled for what it is: the root, the store build, and the first
    // path inside the root that is not the build's.
    i.state = RootState::Differs(PathBuf::from("/p/compat/trust/8595/bin/tippy"));
    let (line, claim) = exec_root_line("status", 8595, &i);
    assert!(
        claim == ToolClaim::Unproven
            && line.starts_with("status: warn — trust build 8595: PATH tippy may not run — "),
        "{line}"
    );
    assert!(
        line.ends_with(
            "and the exec root /p/compat/trust/8595 is not the store build \
             /p/store/trust/8595 file for file (first difference: \
             /p/compat/trust/8595/bin/tippy); fix: `aterm pkg repair` lays the exec root \
             (a copy-on-write clone — the store is never modified) and routes the shims"
        ),
        "{line}"
    );
    // Blocked: a link (or a file) at compat/ or compat/trust. The fix is the manual
    // step, never `repair` alone, which refuses there on every run; `cannot` while no
    // shim carries a route, `may not` while one routed earlier might still run.
    i.state = RootState::Blocked(crate::compat::Blocked {
        at: PathBuf::from("/p/compat/trust"),
        symlink: true,
        root: PathBuf::from("/p/compat/trust/8595"),
    });
    let (line, claim) = exec_root_line("doctor", 8595, &i);
    assert_eq!(claim, ToolClaim::Unproven, "{line}");
    assert_eq!(
        line,
        "doctor: warn — trust build 8595: PATH tippy may not run — bin/rustc (inode 11) is \
         a separate file from bin/trustc (inode 12; 2428 byte(s) differ), which this \
         build's tippy refuses, and /p/compat/trust is a symbolic link, not a directory — \
         atpkg lays nothing through a link there, and no update, repair or gc removes it, \
         so no exec root is laid at /p/compat/trust/8595 and no shim routes through one; \
         fix: remove that link (only \
         the link: whatever it names is left as it is), then `aterm pkg repair` lays the \
         exec root"
    );
    i.unrouted = i.shims.clone();
    i.state = RootState::Blocked(crate::compat::Blocked {
        at: PathBuf::from("/p/compat"),
        symlink: false,
        root: PathBuf::from("/p/compat/trust/8595"),
    });
    let (line, claim) = exec_root_line("doctor", 8595, &i);
    assert_eq!(claim, ToolClaim::CannotRun, "{line}");
    assert!(
        line.contains("PATH tippy cannot run — ")
            && line.ends_with(
                "and /p/compat is not a directory — no update, repair or gc removes it, so no \
                 exec root is laid at /p/compat/trust/8595 and no shim routes through one; \
                 fix: move that file out of the way, then `aterm pkg repair` lays the exec \
                 root"
            ),
        "{line}"
    );
    assert!(
        !line.contains("repair` lays the exec root (a copy-on-write clone"),
        "{line}"
    );
    i.state = RootState::Matches;
    i.unrouted.clear();
    let (line, claim) = exec_root_line("doctor", 8595, &i);
    assert!(
        claim == ToolClaim::Runs && line.starts_with("doctor: note — "),
        "{line}"
    );
    // Every "cannot run" line counts as one; every "may not run" line as unproven.
    for (state, unrouted, claim) in [
        (RootState::Absent, vec![], ToolClaim::CannotRun),
        (
            RootState::Matches,
            vec!["tippy".to_string()],
            ToolClaim::CannotRun,
        ),
    ] {
        i.state = state;
        i.unrouted = unrouted;
        let (line, got) = exec_root_line("doctor", 8595, &i);
        assert_eq!(got, claim, "{line}");
        assert!(line.contains("PATH tippy cannot run "), "{line}");
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
    assert_eq!(workspace_with_trust_table(&nested), Some(root.clone()));
    let plain = synthetic_home("wsplain");
    std::fs::write(plain.join("Cargo.toml"), "[workspace]\n").unwrap();
    assert_eq!(workspace_with_trust_table(&plain), None);
}

/// A `[packages] prefix` naming another store, and a retired opt-out still exported, are
/// each a warn naming the fix; neither line appears when there is nothing to say.
#[test]
fn doctor_warns_an_ignored_prefix_and_an_exported_retired_opt_out() {
    let l = layout("ignored-settings");
    let home = synthetic_home("ignored-settings");
    let run = |probes: &Probes| -> String {
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let (mut out, mut err): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            crate::flow::now_unix(),
            None,
            "doctor",
            probes,
            &mut out,
            &mut err,
        );
        String::from_utf8_lossy(&out).into_owned()
    };
    let quiet = run(&Probes::default());
    assert!(!quiet.contains("is not used by this build"), "{quiet}");
    assert!(!quiet.contains("is exported but does nothing"), "{quiet}");
    let out = run(&Probes {
        ignored_prefix: Some(PathBuf::from("/opt/aterm/pkg")),
        retired_env: vec![RETIRED_OPT_OUTS[0], RETIRED_OPT_OUTS[2]],
        ..Probes::default()
    });
    let warns: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("doctor: warn — "))
        .collect();
    assert!(
        warns.iter().any(|w| w.contains(
            "[packages] prefix = /opt/aterm/pkg is not used \
                                      by this build"
        ) && w.contains("until you remove the line")),
        "{out}"
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("$ATERM_NO_AUTO_UPDATE is exported")
                && w.contains("[update] enabled = false")),
        "{out}"
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("$ATPKG_DISABLE is exported")
                && w.contains("[packages] enabled = false")),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// A floor of 0 is a missing file (`Floor::current`), not an acceptance, so a store that
/// accepted no index says so; the generation rides the index row, and the roster row is said
/// only where it differs from that generation.
#[test]
fn the_floor_rows_claim_only_what_was_accepted() {
    let l = layout("floor-rows");
    let home = synthetic_home("floor-rows");
    let now = crate::flow::rfc3339_to_unix("2026-10-20T00:00:00Z").unwrap();
    let rows = || -> Vec<String> {
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let mut out = Vec::new();
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut std::io::sink(),
        );
        String::from_utf8_lossy(&out)
            .lines()
            .filter(|r| r.contains(" accepted") || r.contains("roster generation"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(rows(), ["doctor: no index accepted yet"]);
    std::fs::write(l.floor(), "42").unwrap();
    assert_eq!(rows(), ["doctor: index 42 accepted"]);
    std::fs::write(l.floor_generation(), "4").unwrap();
    assert_eq!(
        rows(),
        [
            "doctor: index 42 accepted (roster generation 4)",
            "doctor: no roster generation recorded"
        ]
    );
    std::fs::write(l.roster_floor(), "4").unwrap();
    assert_eq!(rows(), ["doctor: index 42 accepted (roster generation 4)"]);
    std::fs::write(l.roster_floor(), "5").unwrap();
    assert_eq!(
        rows(),
        [
            "doctor: index 42 accepted (roster generation 4)",
            "doctor: roster generation 5 accepted"
        ]
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// "Never checked" (no `status.toml`, or a failed pass's record) says only what is true: the
/// index-pinned programs wait for a pass, and the vendor clause names only the programs that
/// update without one (installed, not held, not excluded, on a machine whose packages update
/// on their own). Nothing on stderr.
#[test]
fn doctor_says_never_checked_truthfully_and_only_in_its_report() {
    let l = layout("never-checked");
    let home = synthetic_home("never-checked");
    let now = crate::flow::rfc3339_to_unix("2026-10-20T00:00:00Z").unwrap();
    let run = |probes: &Probes| -> (String, String) {
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let (mut out, mut err): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            probes,
            &mut out,
            &mut err,
        );
        (
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    };
    let line = |text: &str| -> String {
        text.lines()
            .find(|l| l.starts_with("doctor: warn — no "))
            .unwrap_or_else(|| panic!("the never-checked line: {text}"))
            .to_owned()
    };
    let untrue = ["cannot be updated", " until"].concat();
    let defaults = Probes::default();

    // Nothing installed: no vendor program updates, so the line claims none.
    let (out, err) = run(&defaults);
    assert_eq!(
        line(&out),
        "doctor: warn — no index update pass has completed yet — run: aterm pkg update"
    );
    // Both installed: both named.
    for program in ["claude", "codex"] {
        let dir = l.build_dir(program, 2_026_092_201);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        crate::activate::atomic_symlink(&dir, &l.program_current(program)).unwrap();
    }
    let (out1, err1) = run(&defaults);
    assert_eq!(
        line(&out1),
        "doctor: warn — no index update pass has completed yet — run: aterm pkg update; \
         claude and codex update from their vendors without it"
    );
    // A failed pass's record: written, and still no completed index pass.
    crate::status::write(
        &l,
        &crate::Status {
            schema: 1,
            updated_at: "2026-10-19T00:00:00Z".into(),
            outcome: "update failed: index unreachable".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let (out2, err2) = run(&defaults);
    assert_eq!(
        line(&out2),
        "doctor: warn — no index update pass has completed yet — run: aterm pkg update; \
         claude and codex update from their vendors without it"
    );
    // `exclude = ["claude"]`, and codex held by a local pin: neither is named.
    let excluded = Probes {
        excluded: vec!["claude".into()],
        ..Probes::default()
    };
    let (out3, _) = run(&excluded);
    assert!(
        line(&out3).ends_with("; codex updates from its vendor without it"),
        "{out3}"
    );
    crate::pin::set_pinned(&l, "codex", true).unwrap();
    let (out4, _) = run(&excluded);
    assert!(line(&out4).ends_with("— run: aterm pkg update"), "{out4}");
    crate::pin::set_pinned(&l, "codex", false).unwrap();
    // Packages that do not update on their own (`enabled` off, or the manager disarmed): no
    // vendor program updates by itself either.
    let (out5, _) = run(&Probes {
        automatic: false,
        ..Probes::default()
    });
    assert!(line(&out5).ends_with("— run: aterm pkg update"), "{out5}");

    for text in [&out, &err, &out1, &err1, &out2, &err2, &out3, &out4, &out5] {
        assert!(!text.contains(&untrue), "{text}");
    }
    for text in [&err, &err1, &err2] {
        assert!(
            !text.contains("update pass"),
            "the report, not stderr: {text}"
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// (8c) Who watches the vendor heads, as the store's rendezvous says: nothing where no vendor
/// program follows its vendor or packages do not update on their own; nobody watching is a
/// note naming the cost; a window, or a terminal session by its pid, is an `ok`. The report
/// takes neither claim.
#[test]
fn doctor_says_who_watches_the_vendor_heads() {
    use crate::vendor_direct::watch::{Host, claimed};
    let l = layout("head-watcher");
    let home = synthetic_home("head-watcher");
    let now = crate::flow::rfc3339_to_unix("2026-10-20T00:00:00Z").unwrap();
    let run = |probes: &Probes| -> Option<String> {
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let (mut out, mut err): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            probes,
            &mut out,
            &mut err,
        );
        String::from_utf8_lossy(&out)
            .lines()
            .find(|line| line.contains("watches the vendor heads"))
            .map(str::to_owned)
    };
    let defaults = Probes::default();
    assert_eq!(run(&defaults), None, "nothing installed: nothing to say");
    let dir = l.build_dir("claude", 2_026_092_201);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    crate::activate::atomic_symlink(&dir, &l.program_current("claude")).unwrap();
    assert_eq!(
        run(&defaults).as_deref(),
        Some(
            "doctor: note — nothing watches the vendor heads now (no aterm window, and no \
             terminal session running aterm, is open): a new claude release is found by \
             the next update pass instead of within minutes"
        )
    );
    let off = Probes {
        automatic: false,
        ..Probes::default()
    };
    assert_eq!(run(&off), None, "packages that do not update on their own");
    let session = claimed(&l, Host::Session);
    assert_eq!(
        run(&defaults),
        Some(format!(
            "doctor: ok — a terminal session (pid {}) watches the vendor heads, no aterm \
             window being open: a new claude release is found within minutes",
            std::process::id()
        ))
    );
    let window = claimed(&l, Host::Window);
    assert_eq!(
        run(&defaults).as_deref(),
        Some(
            "doctor: ok — an aterm window watches the vendor heads: a new claude release \
             is found within minutes"
        )
    );
    // The report took nothing: the window's hold and the session's seat are as they were.
    assert!(run(&defaults).is_some_and(|line| line.contains("an aterm window watches")));
    drop(window);
    assert!(run(&defaults).is_some_and(|line| line.contains("a terminal session (pid")));
    drop(session);
    assert!(run(&defaults).is_some_and(|line| line.contains("nothing watches")));
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(target_os = "macos")]
/// An index unreached for days (the last pass failed and the last success is that old) and
/// an index build unchanged for a month while reachable (a frozen publisher) are named
/// separately; a record whose last pass reached the index names neither.
#[test]
fn doctor_names_an_unreached_index_and_a_frozen_index_separately() {
    let l = layout("freshness");
    install(&l, "ay", 8256);
    let home = synthetic_home("freshness");
    let now = crate::flow::rfc3339_to_unix("2026-10-20T00:00:00Z").unwrap();
    let run = |status: crate::Status| -> String {
        crate::status::write(&l, &status).unwrap();
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let (mut out, mut err): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
        let _ = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            &Probes::default(),
            &mut out,
            &mut err,
        );
        String::from_utf8_lossy(&out).into_owned()
    };
    let base = crate::Status {
        schema: 1,
        updated_at: "2026-10-19T00:00:00Z".into(),
        enabled: true,
        index_source: "alabsystems/aterm".into(),
        outcome: "up to date".into(),
        seams: Vec::new(),
        last_success_at: "2026-10-19T00:00:00Z".into(),
        last_index_build: 32,
        index_build_changed_at: "2026-10-01T00:00:00Z".into(),
        last_pass: "ok".into(),
        last_pass_at: "2026-10-19T00:00:00Z".into(),
        last_pass_attempted_index_build: 0,
        last_pass_attempted_at: String::new(),
        pass_seq: 0,
        stale_index_only_pass_seq: 0,
        programs: Default::default(),
        extra: Default::default(),
    };
    let out = run(base.clone());
    assert!(!out.contains("has not been reached"), "{out}");
    assert!(!out.contains("publishing looks frozen"), "{out}");
    let out = run(crate::Status {
        last_success_at: "2026-10-10T00:00:00Z".into(),
        last_pass: "failed".into(),
        last_pass_at: "2026-10-19T00:00:00Z".into(),
        ..base.clone()
    });
    assert!(
        out.contains(
            "doctor: warn — the signed index has not been reached for 10 day(s) — \
             `aterm pkg update` says why"
        ),
        "an unreached index is named, and with no cause on record none is guessed:\n{out}"
    );
    assert!(
        !out.contains("publishing looks frozen"),
        "unreachable is not frozen:\n{out}"
    );
    // The cause the last pass wrote (on the cached index, or failed outright) is said as the
    // reason.
    for (outcome, why) in [
        (
            "up to date (index build 32) (index from cache \u{2014} dns lookup failed: \
             github.com (os error 8))",
            "dns lookup failed: github.com (os error 8)",
        ),
        (
            "update failed: the download host refused (HTTP 403)",
            "the download host refused (HTTP 403)",
        ),
    ] {
        let out = run(crate::Status {
            last_success_at: "2026-10-10T00:00:00Z".into(),
            last_pass: "failed".into(),
            last_pass_at: "2026-10-19T00:00:00Z".into(),
            outcome: outcome.into(),
            ..base.clone()
        });
        assert!(
            out.contains(&format!(
                "doctor: warn — the signed index has not been reached for 10 day(s): \
                 {why} — now: aterm pkg update"
            )),
            "{out}"
        );
    }
    // A failure older than the success (a success stamped after the failed pass ended) is no
    // failure.
    let out = run(crate::Status {
        last_success_at: "2026-10-10T00:00:00Z".into(),
        last_pass: "failed".into(),
        last_pass_at: "2026-10-09T00:00:00Z".into(),
        ..base.clone()
    });
    assert!(!out.contains("has not been reached"), "{out}");
    // A pass that ended ok with nothing to check reached nothing, but failed nothing
    // either: an old success after it is not "unreached".
    let out = run(crate::Status {
        last_success_at: "2026-10-10T00:00:00Z".into(),
        last_pass: "ok".into(),
        last_pass_at: "2026-10-19T00:00:00Z".into(),
        ..base.clone()
    });
    assert!(!out.contains("has not been reached"), "{out}");
    let out = run(crate::Status {
        index_build_changed_at: "2026-09-10T00:00:00Z".into(),
        ..base
    });
    assert!(
        out.contains("index build 32 has not changed in 40 day(s)"),
        "a frozen publisher is named:\n{out}"
    );
    assert!(out.contains("publishing looks frozen"), "{out}");
    assert!(!out.contains("has not been reached"), "{out}");
    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}

/// Warnings without a fault read "working", never "healthy"; every printed row fits
/// [`SHORT_ROW`] with its own next step; ok rows and notes wait for `--verbose`, which prints
/// them all under the same verdict.
#[test]
fn the_report_leads_with_its_verdict_and_says_only_the_problems() {
    // A long row a real report carries: its fact, a reason past the width, and the next
    // step at its end.
    let unreached = "doctor: warn — the signed index has not been reached for 10 day(s): \
         the last pass failed: could not fetch https://github.com/alabsystems/aterm/releases/\
         download/index/index.toml (connection timed out after 30 s, retried 3 times) — now: \
         aterm pkg update";
    assert!(
        unreached.chars().count() > SHORT_ROW,
        "the row must be cut: {unreached}"
    );
    let path_row = "doctor: warn — managed bin/ is not on PATH here; a new aterm shell \
         adds it, or add: export PATH=\"$PATH:/Users//someone-with-a-long-name/Library/\
         Application Support/aterm/pkg/bin\"";
    assert!(
        path_row.chars().count() > SHORT_ROW,
        "the row must be cut: {path_row}"
    );
    assert_eq!(
        present(path_row, "", "doctor", Detail::Problems)
            .joined()
            .lines()
            .nth(1),
        Some(
            "doctor: warn — managed bin/ is not on PATH here — add: export \
             PATH=\"$PATH:/Users//someone-with-a-long-name/Library/Application \
             Support/aterm/pkg/bin\""
        ),
        "the line to paste survives the cut"
    );
    let report = format!(
        "doctor: this atpkg is 0.91.0 at /x\n\
         doctor: ok — prefix /p\n\
         doctor: warn — no index update pass has completed yet — run: aterm pkg update\n\
         doctor: note — Trust builds use the ay pinned inside the trust bundle\n\
         {unreached}\n\
         doctor: 12 program(s) active\n\
         doctor: healthy\n"
    );
    let out = present(&report, "", "doctor", Detail::Problems).joined();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        "doctor: working — 12 program(s) installed; 2 thing(s) to look at"
    );
    assert_eq!(
        lines[1],
        "doctor: warn — no index update pass has completed yet — run: aterm pkg update"
    );
    assert_eq!(
        lines[2],
        "doctor: warn — the signed index has not been reached for 10 day(s): the last pass \
         failed: could not fetch https://github.com/alabsystems… — run aterm pkg update",
        "the fact and its next step survive the cut"
    );
    assert_eq!(
        lines[3],
        "doctor: the full report: aterm pkg doctor --verbose"
    );
    assert_eq!(lines.len(), 4, "{out}");
    assert!(
        lines.iter().all(|l| l.chars().count() <= SHORT_ROW),
        "{out}"
    );
    assert!(
        !out.contains("healthy"),
        "never healthy beside a warning: {out}"
    );

    let all = present(&report, "", "doctor", Detail::Everything).joined();
    assert!(all.starts_with("doctor: working — 12 program(s)"), "{all}");
    assert!(
        all.contains("doctor: ok — prefix /p") && all.contains(unreached),
        "{all}"
    );
    assert!(!all.contains("doctor: healthy"), "{all}");
}

/// Nothing flagged is `healthy`, with nothing below it; a fault keeps its own verdict and the
/// one next act; a FAIL row's indented detail follows it.
#[test]
fn a_clean_report_is_healthy_and_a_faulty_one_keeps_its_verdict_and_next_act() {
    let clean = "status: ok — prefix /p\nstatus: 3 program(s) active\nstatus: healthy\n";
    assert_eq!(
        present(clean, "", "status", Detail::Problems).joined(),
        "status: healthy — 3 program(s) installed and working\n\
         status: the full report: aterm pkg status --verbose\n"
    );
    let faulty = format!(
        "doctor: FAIL — trust: shim missing\n\
         doctor:   trust: {}\n\
         doctor: ok — prefix /p\n\
         doctor: found 1 problem(s)\n\
         doctor: next — aterm pkg update\n",
        "x".repeat(300)
    );
    let out = present(&faulty, "", "doctor", Detail::Problems).joined();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "doctor: found 1 problem(s)");
    assert_eq!(lines[1], "doctor: FAIL — trust: shim missing");
    assert!(lines[2].starts_with("doctor:   trust: xxx") && lines[2].ends_with('…'));
    assert_eq!(lines[2].chars().count(), SHORT_ROW);
    assert_eq!(lines[3], "doctor: next — aterm pkg update");

    // A recorded problem listed after other rows still follows its verdict, and a
    // declined store says why it is empty.
    let declined = format!(
        "doctor: PROBLEM — the toolset is incomplete; 3 program(s) active\n\
         doctor: ok — ty: removed on purpose\n\
         doctor:   trust: error: disk full\n\
         doctor: {DECLINED_LINE} (`aterm pkg install --default-set` reinstalls it)\n\
         doctor: found 1 problem(s)\n"
    );
    let out = present(&declined, "", "doctor", Detail::Problems).joined();
    assert!(out.contains("doctor:   trust: error: disk full"), "{out}");
    assert!(out.contains(DECLINED_LINE), "{out}");
    assert!(
        !out.contains("removed on purpose"),
        "an ok row waits for --verbose: {out}"
    );
}

/// A structural fault (a stderr row of [`run_with`]) follows the verdict, cut to one short
/// row and still on stderr; a fact ending at a `;` or a sentence is kept whole with its
/// `Fix:`, and a `;` inside a parenthetical does not end the fact.
#[test]
fn a_structural_fault_follows_the_verdict_in_one_short_row() {
    let long_path = format!("/Users//x/{}/pkg/bin/foo", "d".repeat(150));
    let builds = (9100..9130)
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let faults = format!(
        "doctor: FAIL \u{2014} broken bin shim {long_path}\n\
         doctor: FAIL \u{2014} trust: its bin/ shims are split across builds {builds} \
         \u{2014} fix: aterm pkg repair, then aterm pkg update trust\n"
    );
    // A row in the shape the parser must handle, not a row `run_with` writes today.
    let report = "doctor: ok \u{2014} prefix /p\n\
         doctor: PROBLEM \u{2014} no ALab programs are installed. Fix: aterm pkg install \
         --default-set (the signed index serves ten programs for this machine and none of \
         them has a build under the prefix yet)\n\
         doctor: found 3 problem(s)\n\
         doctor: next \u{2014} aterm pkg install --default-set\n";
    let shown = present(report, &faults, "doctor", Detail::Problems);
    assert_eq!(shown.verdict, "doctor: found 3 problem(s)\n");
    let fault_rows: Vec<&str> = shown.faults.lines().collect();
    assert_eq!(fault_rows.len(), 2, "{}", shown.faults);
    assert!(
        fault_rows[0].starts_with("doctor: FAIL \u{2014} broken bin shim /Users//x/")
            && fault_rows[0].ends_with('\u{2026}'),
        "{}",
        fault_rows[0]
    );
    assert!(
        fault_rows[1].starts_with("doctor: FAIL \u{2014} trust: its bin/ shims are split")
            && fault_rows[1]
                .ends_with("\u{2026} \u{2014} fix: aterm pkg repair, then aterm pkg update trust"),
        "the fact yields room to the fix: {}",
        fault_rows[1]
    );
    assert!(
        fault_rows.iter().all(|r| r.chars().count() <= SHORT_ROW),
        "{fault_rows:?}"
    );
    assert_eq!(
        shown.rest.lines().next(),
        Some(
            "doctor: PROBLEM \u{2014} no ALab programs are installed \u{2014} fix: aterm pkg \
             install --default-set"
        )
    );
    assert!(
        shown
            .rest
            .ends_with("doctor: the full report: aterm pkg doctor --verbose\n"),
        "{}",
        shown.rest
    );
    // Under --verbose every fault is whole.
    let all = present(report, &faults, "doctor", Detail::Everything);
    assert_eq!(all.faults, faults);

    // A `;` inside a parenthetical does not end the fact.
    let row = format!(
        "reroute stub {} missing (the next aterm session lays it; `aterm pkg repair` \
         lays it now; nothing else does)",
        "c".repeat(60)
    );
    let short = short_row("doctor", "warn", &row);
    assert!(short.chars().count() <= SHORT_ROW, "{short}");
    assert!(
        !short.contains("lays it \u{2014}") && short.ends_with("run `aterm pkg repair`"),
        "{short}"
    );
}

/// A channel head past the local index (floor 42, the host answering for 43) is said, not
/// just "healthy". The stamps come from the probe's real writer
/// ([`crate::index_probe::successor_with`]) over a fake HEAD; nothing touches the network.
#[test]
fn doctor_says_when_the_channel_head_is_past_the_local_index() {
    use crate::index_probe::NEAR_LOCK;
    use aterm_update_core::{HeadAnswer, HttpError};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let l = layout("channel-head");
    install(&l, "ay", 8256);
    let home = synthetic_home("channel-head");
    let now = crate::flow::rfc3339_to_unix("2026-09-22T18:00:00Z").unwrap();
    let at =
        |secs_ago: u64| UNIX_EPOCH + Duration::from_secs(u64::try_from(now).unwrap() - secs_ago);
    crate::status::write(
        &l,
        &crate::Status {
            schema: 1,
            updated_at: "2026-09-22T17:58:00Z".into(),
            enabled: true,
            index_source: "alabsystems/aterm".into(),
            outcome: "up to date".into(),
            seams: Vec::new(),
            last_success_at: "2026-09-22T17:58:00Z".into(),
            last_index_build: 42,
            index_build_changed_at: "2026-09-21T09:00:00Z".into(),
            last_pass: String::new(),
            last_pass_at: String::new(),
            last_pass_attempted_index_build: 0,
            last_pass_attempted_at: String::new(),
            pass_seq: 0,
            stale_index_only_pass_seq: 0,
            programs: Default::default(),
            extra: Default::default(),
        },
    )
    .unwrap();
    std::fs::write(l.floor(), "42").unwrap();
    // The probe's real writer, over a fake release host.
    let probe = |published: Option<u64>, fail: bool| {
        let mut head = |url: &str| -> Result<HeadAnswer, HttpError> {
            if fail {
                return Err(HttpError::Transport("offline".into()));
            }
            let hit =
                published.is_some_and(|b| url.ends_with(&format!("/atpkg-index-{b}/index.toml")));
            Ok(HeadAnswer {
                code: if hit { 302 } else { 404 },
                location: hit.then(|| "https://release-assets.githubusercontent.com/object".into()),
            })
        };
        crate::index_probe::successor_with(&l, "alabsystems", "aterm", SystemTime::now(), &mut head)
    };
    let age_stamps = |secs_ago: u64| {
        if let Ok(file) = std::fs::File::options()
            .write(true)
            .open(l.prefix.join(NEAR_LOCK))
        {
            file.set_modified(at(secs_ago)).unwrap();
        }
    };
    let clear = || {
        let _ = std::fs::remove_file(l.prefix.join(NEAR_LOCK));
    };
    let run = |index_head: bool| -> (bool, String) {
        let path = std::env::join_paths([l.bin_dir()]).unwrap();
        let (mut out, mut err): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
        let ok = run_with(
            &l,
            Some(&home),
            Some(&path),
            now,
            None,
            "doctor",
            &Probes {
                index_head,
                ..Probes::default()
            },
            &mut out,
            &mut err,
        );
        (ok, String::from_utf8_lossy(&out).into_owned())
    };
    // The channel-head rows — not the floor row (`index 42 accepted …`) beside them.
    let head_lines = |out: &str| -> Vec<String> {
        out.lines()
            .filter(|l| l.contains("newer than 42") || l.contains("index 42 is not"))
            .map(str::to_string)
            .collect()
    };

    // The §14 cache a pass writes from what the channel served, selected or not, by its real
    // writer under the source key the real fetcher uses.
    let cache_holds = |builds: &[u64]| {
        let candidates: Vec<crate::select::Candidate> = builds
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
    };
    let drop_cache = || {
        let _ = std::fs::remove_file(l.prefix.join("index-cache.toml"));
    };

    // (a) Index 43 is published: the owner's state, with no downloaded candidates on
    // record. Said, as a warn with the remedy.
    assert_eq!(
        probe(Some(43), false),
        crate::index_probe::Probe::Published(43)
    );
    age_stamps(120);
    let (ok, out) = run(true);
    assert!(ok, "advisory, never structural:\n{out}");
    assert!(
        out.contains("ok — 0 day(s) since the last completed update pass")
            && out.contains("doctor: index 42 accepted\n"),
        "the lines the owner read are still there:\n{out}"
    );
    assert!(
        !out.contains("doctor: roster generation"),
        "no generation recorded, none named:\n{out}"
    );
    let published_line = "doctor: warn — index 42 is not the newest: the index probe found a \
                          newer ALab index (seen 2 min ago; an unverified hint — no signed \
                          pass has landed it on this machine) — run: aterm pkg update";
    assert_eq!(head_lines(&out), [published_line], "{out}");

    clear();
    assert_eq!(
        probe(Some(43), false),
        crate::index_probe::Probe::Published(43)
    );
    age_stamps(120);

    // (a2) The same hint, but the last pass that reached the index held only 42 and older:
    // the channel had no complete newer release then, and "run: aterm pkg update" alone
    // would loop ("already current" / "not the newest"), so the line says why.
    cache_holds(&[42, 41, 40, 39]);
    let (_, out) = run(true);
    assert_eq!(
        head_lines(&out),
        [
            "doctor: warn — index 42 is not the newest: the index probe found a \
          newer ALab index (seen 2 min ago; an unverified hint), but the last signed \
          update pass reached the index 2 min ago and found no complete signed \
          release newer than 42 there — if the new one was up by then, its signed files \
          are not all uploaded and no update can land it yet; otherwise run: aterm pkg \
          update"
        ],
        "{out}"
    );

    // (a3) The last pass downloaded 43 and the floor is still 42: it was refused, and another
    // update is not the remedy, whatever the probe says.
    cache_holds(&[43, 42, 41, 40]);
    let refused = "doctor: warn — index 42 is not the newest: index 43 was downloaded \
                   but not accepted — its signatures did not verify here, or its pass \
                   stopped early; if `aterm pkg update` still ends on index 42, the \
                   release needs fixing, not this machine";
    let (ok, out) = run(true);
    assert!(ok, "{out}");
    assert_eq!(head_lines(&out), [refused], "{out}");

    // (b) Nothing newer, asked just now: ok, said as what was checked.
    clear();
    drop_cache();
    assert_eq!(probe(None, false), crate::index_probe::Probe::Missing);
    age_stamps(30);
    let (_, out) = run(true);
    assert_eq!(
        head_lines(&out),
        ["doctor: ok — no index newer than 42 is published (checked 30 s ago)"],
        "{out}"
    );
    assert!(!out.contains("not the newest"), "{out}");
    // ...and a refused download outranks the probe's "nothing newer".
    cache_holds(&[43, 42]);
    let (_, out) = run(true);
    assert_eq!(head_lines(&out), [refused], "{out}");
    drop_cache();

    // (d) The same answer three hours old — no window asking: its age, not an ok, and
    // the time a pass last reached the index (what `aterm pkg update` does move).
    age_stamps(3 * 3600);
    let (_, out) = run(true);
    assert_eq!(
        head_lines(&out),
        [
            "doctor: note — no index newer than 42 was published when last checked, 3 h \
          ago (an aterm window re-checks while open); the last signed update pass reached \
          the index 2 min ago"
        ],
        "{out}"
    );

    // A failed check is unknown, never "newest".
    clear();
    assert_eq!(probe(None, true), crate::index_probe::Probe::Deferred);
    age_stamps(60);
    let (_, out) = run(true);
    assert_eq!(
        head_lines(&out),
        [
            "doctor: note — the last check for an index newer than 42 got no usable \
          answer (60 s ago), so whether one is published is unknown here; the last \
          signed update pass reached the index 2 min ago"
        ],
        "{out}"
    );

    // (c) No false alarm once the index landed: a "published" stamp written under 42 says
    // nothing about floor 43, and a cache holding 43 is not "newer than 43".
    clear();
    assert_eq!(
        probe(Some(43), false),
        crate::index_probe::Probe::Published(43)
    );
    cache_holds(&[43, 42]);
    std::fs::write(l.floor(), "43").unwrap();
    let (_, out) = run(true);
    assert!(!out.contains("not the newest"), "{out}");
    assert!(
        out.contains(
            "doctor: note — no check for an index newer than 43 is recorded on this \
             machine (an aterm window checks while open); the last signed update pass \
             reached the index 2 min ago"
        ),
        "{out}"
    );
    drop_cache();

    // (e) A source the probe does not watch reports nothing at all.
    std::fs::write(l.floor(), "42").unwrap();
    let (_, out) = run(false);
    assert!(head_lines(&out).is_empty(), "{out}");
    assert!(!out.contains("release host"), "{out}");

    // No pass has recorded reaching the index: the note says that, not a time.
    clear();
    let mut status = crate::status::read(&l).unwrap();
    status.last_success_at.clear();
    crate::status::write(&l, &status).unwrap();
    let (_, out) = run(true);
    assert_eq!(
        head_lines(&out),
        [
            "doctor: note — no check for an index newer than 42 is recorded on this \
          machine (an aterm window checks while open); no update pass has recorded \
          reaching the signed index"
        ],
        "{out}"
    );

    let _ = std::fs::remove_dir_all(&l.prefix);
    let _ = std::fs::remove_dir_all(&home);
}
