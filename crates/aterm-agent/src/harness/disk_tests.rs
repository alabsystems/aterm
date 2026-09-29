// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk report and the one apply path (design §5.5).
//!
//! Nothing here reads the wall clock: `now` is a literal in every test, and
//! the two tests that touch a real directory move `now` FORWARD past the
//! files they just wrote rather than back-dating the files — the threshold is
//! a comparison, so moving either side of it proves the same thing and only
//! one of them needs a syscall this crate does not have.
//!
//! Every build-directory fixture is laid out AS CARGO LAYS ONE OUT
//! ([`lay_target`]): the build locks inside each profile and NONE at the
//! root (cargo never writes one there — none of this machine's build
//! directories has one), `.fingerprint/`, `deps/`, `build/` and
//! `incremental/` in the profile. A fixture that planted a root
//! `.cargo-lock` is what let the old witness pass here while it could never
//! pass on a real build directory.
//!
//! The negative controls are the point of most of these: a build directory
//! with no marker is not a candidate however old it is; a fresh profile is
//! not idle; a profile a build holds locked is not a candidate; an
//! unresolvable live symlink turns the whole version class
//! blocked rather than guessing; a delegated class removes nothing even with
//! `disk.apply` on; and apply with no class named removes nothing and never
//! calls the remover at all.

use super::*;

/// 2026-09-21T00:00:00Z. Fixed; never `SystemTime::now()`.
const NOW: i64 = 1_789_603_200;

/// A fresh directory under the system temp dir, removed on drop.
pub(crate) struct Tmp(PathBuf);

impl Tmp {
    pub(crate) fn new(tag: &str) -> Tmp {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "aterm-harness-disk-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Tmp(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        // A test may leave a read-only directory behind (the undeletable
        // entry); make everything writable again first.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut stack = vec![self.0.clone()];
            while let Some(d) = stack.pop() {
                let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755));
                if let Ok(rd) = std::fs::read_dir(&d) {
                    for e in rd.flatten() {
                        if e.file_type().is_ok_and(|t| t.is_dir()) {
                            stack.push(e.path());
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `now` far enough past the files a test just wrote that both clocks clear
/// the threshold. The files carry the real wall clock, so the only honest
/// reading is "the survey happened later".
pub(crate) fn now_after_write(days: i64) -> i64 {
    let real = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    real + days * 86_400
}

/// A build directory AS CARGO LAYS ONE OUT, at `dir`: the two root markers
/// (`CACHEDIR.TAG` with its signature, `.rustc_info.json`) and NO root lock;
/// one profile, `debug/`, holding all three build locks, `.fingerprint/`,
/// `deps/` (an rlib), `build/` and `incremental/` (a session's query cache,
/// 8 KiB). Written for real, because the marker check reads bytes and the
/// reclaim takes real locks.
pub(crate) fn lay_target(dir: &Path) -> PathBuf {
    lay_profile(&dir.join("debug"));
    std::fs::write(
        dir.join("CACHEDIR.TAG"),
        format!("{CACHEDIR_SIGNATURE}\n# a cargo target directory\n"),
    )
    .expect("CACHEDIR.TAG");
    std::fs::write(dir.join(RUSTC_INFO), b"{}").expect("rustc info");
    dir.to_path_buf()
}

/// One cargo profile at `profile`: every build lock, `.fingerprint/`,
/// `deps/liblay.rlib` (4 KiB), `build/`, and one incremental session holding
/// an 8 KiB query cache.
pub(crate) fn lay_profile(profile: &Path) {
    for d in [
        "deps",
        ".fingerprint/lay-2bcb77ac5af7c69c",
        "build",
        "incremental/lay-1x2y3z/s-abc-def",
    ] {
        std::fs::create_dir_all(profile.join(d)).expect("profile dirs");
    }
    for lock in CARGO_LOCKS {
        std::fs::write(profile.join(lock), b"").expect("lock");
    }
    std::fs::write(profile.join("deps").join("liblay.rlib"), vec![7u8; 4096]).expect("rlib");
    std::fs::write(
        profile.join(".fingerprint/lay-2bcb77ac5af7c69c/lib-lay"),
        b"0123456789abcdef",
    )
    .expect("fingerprint");
    std::fs::write(
        profile.join("incremental/lay-1x2y3z/s-abc-def/query-cache.bin"),
        vec![9u8; 8192],
    )
    .expect("query cache");
}

/// [`lay_target`] at `<tmp>/<name>`, answered CANONICAL (the report names
/// the real path; macOS's temp dir sits behind the `/var` link).
pub(crate) fn synthetic_target(tmp: &Tmp, name: &str) -> PathBuf {
    let dir = lay_target(&tmp.path().join(name));
    std::fs::canonicalize(&dir).expect("canonical")
}

/// The real remover under the default stale window at `now`, on any device.
fn real(now: i64) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    remover(Judge {
        now,
        threshold_days: DEFAULT_TARGET_STALE_DAYS,
        device: None,
    })
}

/// A remover that counts its calls and removes nothing, answering the row's
/// own size as freed.
fn counting(calls: &mut usize) -> impl FnMut(&Row) -> std::io::Result<Removed> + '_ {
    move |row: &Row| {
        *calls += 1;
        Ok(Removed {
            bytes: row.bytes,
            ..Removed::default()
        })
    }
}

/// The re-measure of a pass with no pressure row: never read (a pressure
/// row reaching it is a test's mistake, said loudly).
pub(crate) fn no_pressure() -> Option<u64> {
    panic!("free space was measured again, but this pass has no pressure row")
}

/// A config with apply ON, for the tests that mean to remove something.
fn applying() -> Config {
    Config {
        apply: true,
        ..Config::default()
    }
}

// -- the report ----------------------------------------------------------------

#[test]
fn a_synthetic_stale_target_is_reported_and_then_removed_with_its_witness_in_the_row() {
    let tmp = Tmp::new("stale-target");
    let dir = synthetic_target(&tmp, "target");
    let now = now_after_write(DEFAULT_TARGET_STALE_DAYS + 1);

    let scanned =
        scan_target(&dir, now, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory");
    assert_eq!(
        scanned.marker.as_deref(),
        Some("CACHEDIR.TAG"),
        "the signed tag is the marker that proves a build tool laid this"
    );
    assert_eq!(scanned.path, dir, "the report names the canonical path");
    assert_eq!(
        scanned
            .profiles
            .iter()
            .map(|p| p.path.clone())
            .collect::<Vec<_>>(),
        vec![dir.join("debug")],
        "the profile is found by its own locks: {scanned:?}"
    );
    assert!(
        !dir.join(".cargo-lock").exists(),
        "no root lock, as cargo lays it"
    );

    let mut survey = Survey::new(now);
    survey.targets.push(scanned);
    let rep = report(&survey, applying());
    let rows = rep.rows_of(Class::CargoTargets);
    assert_eq!(rows.len(), 1, "one candidate: {rep:?}");
    let row = rows[0].clone();
    assert!(row.removable, "an idle profile under a marker is removable");
    assert!(
        row.bytes >= 8192,
        "the row's size is the incremental cache alone: {row:?}"
    );
    assert!(
        row.bytes < 8192 + 4096 * 4,
        "and not the rlib or the rest of the tree: {row:?}"
    );
    assert!(
        matches!(&row.witness, Witness::StaleBuildDir { marker, profiles: 1, idle_days, threshold_days, leaves }
            if marker == "CACHEDIR.TAG"
                && *idle_days >= DEFAULT_TARGET_STALE_DAYS
                && *threshold_days == DEFAULT_TARGET_STALE_DAYS
                && leaves.is_empty()),
        "the witness names the marker, the idle profile and its clock: {:?}",
        row.witness
    );

    // NOW RECLAIM IT, through the real primitive.
    let done = apply(
        &rep,
        survey.transcripts_root.as_deref(),
        Some(Class::CargoTargets),
        &mut real(now),
        &mut no_pressure,
        &mut |_| true,
    );
    assert_eq!(done.removed.len(), 1, "exactly the one row: {done:?}");
    assert!(done.denials.is_empty(), "nothing refused: {done:?}");
    assert!(done.freed_bytes >= 8192, "{done:?}");
    assert!(dir.exists(), "the build directory itself stays");
    assert!(
        !dir.join("debug/incremental").exists(),
        "its idle incremental/ is gone"
    );
    assert!(
        dir.join("debug/deps/liblay.rlib").exists(),
        "deps/ is never touched"
    );

    // THE ROW CARRIES THE WITNESS. A record of what went without why it was
    // safe is a receipt, not a record.
    let (row, removed) = &done.removed[0];
    let line = removal_row(now, "sid-1", row, removed);
    assert!(line.contains("\"kind\":\"removed\""), "{line}");
    assert!(line.contains("\"witness\":\"stale-build-dir\""), "{line}");
    assert!(line.contains("CACHEDIR.TAG"), "{line}");
    assert!(line.contains("only incremental/ goes"), "{line}");
    assert!(line.contains(&dir.display().to_string()), "{line}");
    assert!(
        line.contains(&format!("\"freed_bytes\":{}", removed.bytes)),
        "{line}"
    );
    assert!(
        line.contains("debug/incremental"),
        "the unit that went: {line}"
    );
    assert!(!line.contains('\n'), "one row is one line: {line}");
}

#[test]
fn a_target_without_a_marker_or_with_a_fresh_clock_is_never_a_candidate() {
    let tmp = Tmp::new("negatives");
    let now = now_after_write(DEFAULT_TARGET_STALE_DAYS + 1);

    // NEGATIVE 1: old enough, a profile in it, but nothing proves a build
    // tool laid it.
    let bare = tmp.path().join("bare");
    lay_profile(&bare.join("debug"));
    let scanned =
        scan_target(&bare, now, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory");
    assert!(scanned.marker.is_none());
    let mut survey = Survey::new(now);
    survey.targets.push(scanned);
    let rep = report(&survey, applying());
    assert!(rep.rows_of(Class::CargoTargets).is_empty(), "{rep:?}");
    assert!(
        rep.notes.iter().any(|n| n.contains("cannot prove it laid")),
        "the note says WHY: {:?}",
        rep.notes
    );

    // NEGATIVE 2: a marker, but the clocks are fresh — `now` is the real
    // clock, so the files this test just wrote are seconds old.
    let fresh = synthetic_target(&tmp, "fresh");
    let now_fresh = now_after_write(0);
    let scanned =
        scan_target(&fresh, now_fresh, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory");
    let mut survey = Survey::new(now_fresh);
    survey.targets.push(scanned);
    let rep = report(&survey, applying());
    assert!(rep.rows_of(Class::CargoTargets).is_empty(), "{rep:?}");
    assert!(
        rep.notes
            .iter()
            .any(|n| n.contains("every profile was written into 0d ago")),
        "{:?}",
        rep.notes
    );

    // NEGATIVE 3: a marker and an idle profile, but a build HOLDS one of its
    // locks. The held lock wins over the clock.
    let busy = synthetic_target(&tmp, "busy");
    let held = target::Build::holds(&busy.join("debug/.cargo-build-lock"));
    let mut survey = Survey::new(now);
    survey
        .targets
        .push(scan_target(&busy, now, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory"));
    let rep = report(&survey, applying());
    assert!(rep.rows_of(Class::CargoTargets).is_empty(), "{rep:?}");
    assert!(
        rep.notes.iter().any(|n| n.contains("a build holds")),
        "{:?}",
        rep.notes
    );
    drop(held);

    // NEGATIVE 4: a marker but no cargo profile (no build lock beside
    // deps/): nothing to date and nothing cargo laid.
    let nolock = synthetic_target(&tmp, "nolock");
    for lock in CARGO_LOCKS {
        std::fs::remove_file(nolock.join("debug").join(lock)).expect("unlock");
    }
    let mut survey = Survey::new(now);
    survey.targets.push(
        scan_target(&nolock, now, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory"),
    );
    let rep = report(&survey, applying());
    assert!(rep.rows_of(Class::CargoTargets).is_empty());
    assert!(
        rep.notes.iter().any(|n| n.contains("no cargo profile")),
        "{:?}",
        rep.notes
    );
}

#[test]
fn a_version_dir_that_is_not_the_link_target_is_reported_and_the_live_one_is_not() {
    let mut survey = Survey::new(NOW);
    for (name, bytes) in [("2.1.259", 200), ("2.1.265", 300)] {
        survey.versions.push(VersionDir {
            path: PathBuf::from("/v").join(name),
            bytes,
            bytes_partial: false,
        });
    }
    survey.live_version = Some(PathBuf::from("/v/2.1.265"));
    let rep = report(&survey, applying());
    let rows = rep.rows_of(Class::ClaudeStaleVersions);
    assert_eq!(rows.len(), 1, "the live one is not a candidate: {rep:?}");
    assert_eq!(rows[0].path, PathBuf::from("/v/2.1.259"));
    assert!(rows[0].removable);
    assert!(
        matches!(&rows[0].witness, Witness::NotLinkTarget { live } if live == Path::new("/v/2.1.265")),
        "the witness NAMES the live target so a reader can check it"
    );
    assert_eq!(rep.reclaimable(Class::ClaudeStaleVersions), 200);
}

#[test]
fn an_unresolvable_live_symlink_blocks_every_version_row_rather_than_guessing() {
    let mut survey = Survey::new(NOW);
    survey.versions.push(VersionDir {
        path: PathBuf::from("/v/2.1.259"),
        bytes: 200,
        bytes_partial: false,
    });
    survey.live_version = None;
    let rep = report(&survey, applying());
    let rows = rep.rows_of(Class::ClaudeStaleVersions);
    assert_eq!(rows.len(), 1, "it is still REPORTED");
    assert!(
        !rows[0].removable,
        "but it has no witness, so it is blocked"
    );
    assert!(rep.notes.iter().any(|n| n.contains("did not resolve")));

    let mut calls = 0;
    let done = apply(
        &rep,
        None,
        Some(Class::ClaudeStaleVersions),
        &mut counting(&mut calls),
        &mut no_pressure,
        &mut |_| true,
    );
    assert_eq!(calls, 0, "nothing was removed");
    assert!(done.removed.is_empty());
}

#[test]
fn a_superseded_store_build_is_reported_and_delegated_never_removed_here() {
    let mut survey = Survey::new(NOW);
    for build in ["2026091601", "2026091701"] {
        survey.store.push(StoreBuild {
            program: "claude".to_owned(),
            build: build.to_owned(),
            path: PathBuf::from("/store/claude").join(build),
            bytes: 100,
            bytes_partial: false,
        });
    }
    survey
        .live_builds
        .insert("claude".to_owned(), "2026091701".to_owned());
    let rep = report(&survey, applying());
    let rows = rep.rows_of(Class::AtpkgGc);
    assert_eq!(rows.len(), 1, "only the superseded one: {rep:?}");
    assert!(
        matches!(&rows[0].witness, Witness::Superseded { program, live }
            if program == "claude" && live == "2026091701")
    );
    assert!(!rows[0].removable, "the store is another writer's tree");
    assert!(
        rows[0]
            .blocked
            .as_deref()
            .is_some_and(|b| b.contains("aterm pkg gc")),
        "the row NAMES the command that owns it: {:?}",
        rows[0].blocked
    );

    // Even with apply on and the class named, this removes nothing.
    let mut calls = 0;
    let done = apply(
        &rep,
        None,
        Some(Class::AtpkgGc),
        &mut counting(&mut calls),
        &mut no_pressure,
        &mut |_| true,
    );
    assert_eq!(calls, 0);
    assert_eq!(done.denials.len(), 1);
    assert_eq!(done.denials[0].code(), "delegated");
}

#[test]
fn free_space_warns_only_on_a_figure_it_actually_has() {
    let mut survey = Survey::new(NOW);
    survey.free_bytes = Some(10 * GIB);
    assert!(report(&survey, Config::default()).warn, "10 GiB < 40 GiB");

    survey.free_bytes = Some(100 * GIB);
    assert!(!report(&survey, Config::default()).warn);

    // NEGATIVE: a failed query FAILS OPEN — no warning on a number we do not
    // have, and the headline says `unknown` rather than 0.
    survey.free_bytes = None;
    let rep = report(&survey, Config::default());
    assert!(!rep.warn);
    assert!(
        rep.headline().contains("free=unknown"),
        "{}",
        rep.headline()
    );
    assert!(
        rep.to_json().contains("\"free_bytes\":null"),
        "{}",
        rep.to_json()
    );
}

// -- apply: the default, the fences ---------------------------------------------

#[test]
fn report_only_is_the_default_and_apply_without_a_class_does_nothing() {
    let tmp = Tmp::new("default");
    let dir = synthetic_target(&tmp, "target");
    let now = now_after_write(DEFAULT_TARGET_STALE_DAYS + 1);
    let mut survey = Survey::new(now);
    survey
        .targets
        .push(scan_target(&dir, now, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory"));

    // The DEFAULT config: apply off.
    let rep = report(&survey, Config::default());
    assert!(!rep.config.apply, "disk.apply defaults to false");
    assert_eq!(rep.rows_of(Class::CargoTargets).len(), 1, "still REPORTED");
    assert!(rep.headline().contains("apply=off"), "{}", rep.headline());

    // No class named: report-only, whatever the switch says.
    assert!(matches!(
        plan(&rep, None),
        Plan::ReportOnly(Refusal::NoClass)
    ));
    let mut calls = 0;
    let mut steps = Vec::new();
    let done = apply(
        &rep,
        None,
        None,
        &mut counting(&mut calls),
        &mut no_pressure,
        &mut |s| {
            steps.push(step_row(now, "sid-1", s));
            true
        },
    );
    assert_eq!(calls, 0, "the remover is never even called");
    assert_eq!(
        steps.len(),
        1,
        "the refusal is told as it happens: {steps:?}"
    );
    assert!(steps[0].contains("\"reason\":\"no-class\""), "{steps:?}");
    assert!(done.removed.is_empty());
    assert_eq!(done.denials.len(), 1);
    assert_eq!(done.denials[0].code(), "no-class");
    assert!(dir.exists(), "the directory is untouched");

    // A class named, but `disk.apply` off: still nothing.
    let done = apply(
        &rep,
        None,
        Some(Class::CargoTargets),
        &mut counting(&mut calls),
        &mut no_pressure,
        &mut |_| true,
    );
    assert_eq!(calls, 0);
    assert_eq!(done.denials[0].code(), "apply-off");
    assert!(dir.exists());

    // And the denial row records the refusal rather than dropping it.
    let line = denial_row(now, "sid-1", &done.denials[0]);
    assert!(line.contains("\"kind\":\"denial\""), "{line}");
    assert!(line.contains("\"reason\":\"apply-off\""), "{line}");
    assert!(line.contains("\"class\":\"cargo-targets\""), "{line}");
}

#[test]
fn a_path_outside_the_list_is_refused_with_a_denial_row() {
    let mut survey = Survey::new(NOW);
    survey.versions.push(VersionDir {
        path: PathBuf::from("/v/2.1.259"),
        bytes: 200,
        bytes_partial: false,
    });
    survey.live_version = Some(PathBuf::from("/v/2.1.265"));
    let rep = report(&survey, applying());

    // A path that is simply not on the report.
    let outside = Path::new("/etc/passwd");
    let err = guard(&rep, None, Class::ClaudeStaleVersions, outside)
        .expect_err("a path nothing reported has no witness");
    assert_eq!(err.code(), "not-reported");
    let line = denial_row(NOW, "sid-1", &err);
    assert!(line.contains("\"kind\":\"denial\""), "{line}");
    assert!(line.contains("\"reason\":\"not-reported\""), "{line}");
    assert!(line.contains("/etc/passwd"), "{line}");
    assert!(!line.contains('\n'), "one row is one line: {line}");

    // A reported path, but under the WRONG class: the grant does not carry.
    assert_eq!(
        guard(&rep, None, Class::CargoTargets, Path::new("/v/2.1.259"))
            .expect_err("a cargo-targets grant cannot reach a versions row")
            .code(),
        "not-reported"
    );

    // A relative path and a `..` walk are refused before anything is looked up.
    assert_eq!(
        guard(
            &rep,
            None,
            Class::ClaudeStaleVersions,
            Path::new("v/2.1.259")
        )
        .expect_err("relative")
        .code(),
        "not-anchored"
    );
    assert_eq!(
        guard(
            &rep,
            None,
            Class::ClaudeStaleVersions,
            Path::new("/v/../v/2.1.259")
        )
        .expect_err("dot-dot")
        .code(),
        "not-anchored"
    );

    // The path that IS on the report passes the same fence.
    guard(
        &rep,
        None,
        Class::ClaudeStaleVersions,
        Path::new("/v/2.1.259"),
    )
    .expect("the reported row passes");
}

#[test]
fn nothing_under_the_transcripts_root_is_ever_removed_even_when_it_is_on_the_report() {
    // A report that (wrongly) carries a transcript path as a removable row:
    // the SECOND fence is what this test is about, so the first is bypassed.
    let mut survey = Survey::new(NOW);
    survey.transcripts_root = Some(PathBuf::from("/home/a/.claude/projects"));
    survey.versions.push(VersionDir {
        path: PathBuf::from("/home/a/.claude/projects/repo"),
        bytes: 999,
        bytes_partial: false,
    });
    survey.live_version = Some(PathBuf::from("/v/2.1.265"));
    let rep = report(&survey, applying());
    assert_eq!(
        rep.rows_of(Class::ClaudeStaleVersions).len(),
        1,
        "the crafted row is on the report"
    );

    let mut calls = 0;
    let done = apply(
        &rep,
        survey.transcripts_root.as_deref(),
        Some(Class::ClaudeStaleVersions),
        &mut counting(&mut calls),
        &mut no_pressure,
        &mut |_| true,
    );
    assert_eq!(calls, 0, "the remover is never reached");
    assert!(done.removed.is_empty());
    assert_eq!(done.denials.len(), 1);
    assert_eq!(done.denials[0].code(), "transcript");
    let line = denial_row(NOW, "sid-1", &done.denials[0]);
    assert!(line.contains("under any flag"), "{line}");

    // The root itself is refused too, not only what is under it.
    assert_eq!(
        guard(
            &rep,
            survey.transcripts_root.as_deref(),
            Class::ClaudeStaleVersions,
            Path::new("/home/a/.claude/projects")
        )
        .expect_err("the root itself")
        .code(),
        "transcript"
    );
}

#[test]
fn a_removal_that_fails_is_a_denial_row_and_does_not_abandon_the_rest() {
    let mut survey = Survey::new(NOW);
    for name in ["2.1.259", "2.1.260"] {
        survey.versions.push(VersionDir {
            path: PathBuf::from("/v").join(name),
            bytes: 10,
            bytes_partial: false,
        });
    }
    survey.live_version = Some(PathBuf::from("/v/2.1.265"));
    let rep = report(&survey, applying());
    let mut steps = Vec::new();
    let done = apply(
        &rep,
        None,
        Some(Class::ClaudeStaleVersions),
        &mut |row: &Row| {
            if row.path.ends_with("2.1.259") {
                Err(std::io::Error::other("busy"))
            } else {
                Ok(Removed {
                    bytes: row.bytes,
                    ..Removed::default()
                })
            }
        },
        &mut no_pressure,
        &mut |s| {
            steps.push(step_row(NOW, "", s));
            true
        },
    );
    assert_eq!(done.removed.len(), 1, "the other row still went");
    // Each removal's intent is told BEFORE it, its outcome after.
    let kinds: Vec<&str> = steps
        .iter()
        .map(|l| {
            ["removing", "removed", "denial"]
                .into_iter()
                .find(|k| l.contains(&format!("\"kind\":\"{k}\"")))
                .unwrap_or("?")
        })
        .collect();
    assert_eq!(
        kinds,
        ["removing", "denial", "removing", "removed"],
        "{steps:?}"
    );
    assert_eq!(done.freed_bytes, 10, "only what actually went is counted");
    assert_eq!(done.denials.len(), 1);
    assert_eq!(done.denials[0].code(), "remove-failed");
}

#[test]
fn remove_tree_refuses_a_symlink() {
    let tmp = Tmp::new("symlink");
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).expect("dir");
    std::fs::write(real.join("f"), b"x").expect("file");
    let link = tmp.path().join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    #[cfg(unix)]
    {
        let err = remove_tree(&link).expect_err("a symlink reaches a tree with no witness");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(real.exists(), "and the target survives");
        remove_tree(&real).expect("the real tree removes");
        assert!(!real.exists());
    }
    #[cfg(not(unix))]
    let _ = link;
}

// -- the vocabulary -------------------------------------------------------------

#[test]
fn the_safelist_is_closed_and_its_spellings_round_trip() {
    for c in Class::ALL {
        assert_eq!(Class::parse(c.as_str()), Some(*c));
        assert!(!c.describe().is_empty());
    }
    // NEGATIVE: no prefix matching, no case folding, no empty name — a typo
    // that resolved to a different class would be a grant nobody gave.
    for bad in ["", "cargo", "Cargo-Targets", "cargo-targets ", "everything"] {
        assert_eq!(Class::parse(bad), None, "{bad:?}");
    }
    assert_eq!(Class::CargoTargets.delegate(), None);
    assert_eq!(
        Class::AtpkgGc.delegate().map(Delegate::command),
        Some("aterm pkg gc")
    );
    assert_eq!(
        Class::ClaudePurge.delegate().map(Delegate::command),
        Some("claude project purge --dry-run")
    );
}

#[test]
fn age_days_never_reads_a_future_mtime_as_old() {
    assert_eq!(age_days(NOW, NOW), 0);
    assert_eq!(age_days(NOW, NOW - 86_399), 0);
    assert_eq!(age_days(NOW, NOW - 86_400), 1);
    assert_eq!(age_days(NOW, NOW - 14 * 86_400), 14);
    // NEGATIVE: a clock that moved backwards must not age a directory into
    // the removable set.
    assert_eq!(age_days(NOW, NOW + 999 * 86_400), 0);
}

/// The shipped idle window is ONE day (2026-09-28; it was 14, sized for the
/// whole-directory removal): what it guards is an idle profile's
/// `incremental/` alone, taken automatically only below the floor. The
/// settings help serves the same figure, and a negative setting falls back
/// to it. A profile written into under 24 hours ago is never idle at it; one
/// a full day old is.
#[test]
fn the_idle_window_defaults_to_one_day() {
    assert_eq!(DEFAULT_TARGET_STALE_DAYS, 1);
    assert_eq!(Config::default().target_stale_days, 1);
    assert_eq!(
        Config::default_shown("target_stale_days").as_deref(),
        Some("1")
    );
    assert_eq!(
        Config::negative_reading("target_stale_days").as_deref(),
        Some("the default, 1")
    );
    let window = Config::default().target_stale_days;
    assert!(age_days(NOW, NOW - 86_399) < window, "built within the day");
    assert!(age_days(NOW, NOW - 86_400) >= window, "a day old");
}

#[test]
fn human_bytes_is_coarse_on_purpose_and_exact_under_a_kib() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(1023), "1023 B");
    assert_eq!(human_bytes(1024), "1.0 KiB");
    assert_eq!(human_bytes(GIB), "1.0 GiB");
    assert_eq!(human_bytes(1024 * GIB), "1.0 TiB");
}

#[test]
fn the_purge_row_is_surfaced_and_never_run_from_here() {
    let mut survey = Survey::new(NOW);
    survey.purge_available = true;
    survey.transcripts_root = Some(PathBuf::from("/home/a/.claude/projects"));
    let rep = report(&survey, applying());
    let rows = rep.rows_of(Class::ClaudePurge);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].removable);
    assert!(matches!(
        &rows[0].witness,
        Witness::VendorReports { command } if *command == "claude project purge --dry-run"
    ));
    assert!(matches!(
        plan(&rep, Some(Class::ClaudePurge)),
        Plan::Denied(Refusal::Delegated(Class::ClaudePurge, _))
    ));

    // NEGATIVE: with the vendor command absent there is no row at all.
    let mut survey = Survey::new(NOW);
    survey.purge_available = false;
    assert!(
        report(&survey, applying())
            .rows_of(Class::ClaudePurge)
            .is_empty()
    );
}

#[test]
fn the_report_json_carries_every_row_with_its_witness() {
    let mut survey = Survey::new(NOW);
    survey.free_bytes = Some(12 * GIB);
    survey.trigger = Trigger::OnDemand;
    survey.versions.push(VersionDir {
        path: PathBuf::from("/v/2.1.259"),
        bytes: 7,
        bytes_partial: true,
    });
    survey.live_version = Some(PathBuf::from("/v/2.1.265"));
    let rep = report(&survey, Config::default());
    let json = rep.to_json();
    assert!(json.contains("\"kind\":\"disk\""), "{json}");
    assert!(json.contains("\"trigger\":\"on-demand\""), "{json}");
    assert!(json.contains("\"warn\":true"), "{json}");
    assert!(json.contains("\"apply_allowed\":false"), "{json}");
    assert!(json.contains("\"witness\":\"not-link-target\""), "{json}");
    assert!(json.contains("\"bytes_partial\":true"), "{json}");
    assert!(!json.contains('\n'), "one object, one line");

    // The report row is the durable measurement, and it is not the rows.
    let line = report_row(NOW, "sid-1", &rep);
    assert!(line.contains("\"kind\":\"report\""), "{line}");
    assert!(line.contains("\"rows\":1"), "{line}");
    assert!(line.contains("\"warn\":true"), "{line}");
}

#[test]
fn the_df_reading_is_anchored_on_capacity_and_never_on_a_field_count() {
    let plain = "Filesystem 1024-blocks      Used Available Capacity  Mounted on\n\
                 /dev/disk3s5 971350180 120000000 700000000      15%    /System/Volumes/Data\n";
    assert_eq!(parse_df_kb(plain), Some(700_000_000 * 1024));

    // THE CASE A FIELD COUNT GETS WRONG: macOS autofs names the source
    // `map auto_home`, so the row has seven fields and the fourth is `Used`.
    let autofs = "Filesystem    1024-blocks Used Available Capacity  Mounted on\n\
                  map auto_home           0    0         0   100%    /System/Volumes/Data/home\n";
    assert_eq!(parse_df_kb(autofs), Some(0));

    // NEGATIVE: no anchor, no row, a non-numeric cell, and a mount name that
    // merely contains a `%` all answer None rather than a plausible guess.
    assert_eq!(parse_df_kb(""), None);
    assert_eq!(parse_df_kb("Filesystem 1024-blocks\n"), None);
    assert_eq!(parse_df_kb("h\n/dev/d 1 2 3 4 /mnt\n"), None);
    assert_eq!(parse_df_kb("h\n/dev/d 1 2 three 15% /mnt\n"), None);
    assert_eq!(parse_df_kb("h\n/dev/50%share 1 2 3 /mnt\n"), None);
}

#[test]
fn dir_bytes_counts_a_tree_and_does_not_follow_a_symlink() {
    let tmp = Tmp::new("bytes");
    let root = tmp.path().join("tree");
    std::fs::create_dir_all(root.join("a").join("b")).expect("dirs");
    std::fs::write(root.join("f1"), vec![0u8; 100]).expect("f1");
    std::fs::write(root.join("a").join("f2"), vec![0u8; 200]).expect("f2");
    std::fs::write(root.join("a").join("b").join("f3"), vec![0u8; 300]).expect("f3");
    let (bytes, partial) = dir_bytes(&root);
    assert_eq!(bytes, 600, "every level is counted");
    assert!(!partial);

    #[cfg(unix)]
    {
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("dir");
        std::fs::write(elsewhere.join("big"), vec![0u8; 5000]).expect("big");
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).expect("symlink");
        let (bytes, _) = dir_bytes(&root);
        assert_eq!(bytes, 600, "a symlink is not walked and not counted");
    }
}

#[test]
fn scan_reads_the_versions_dir_and_resolves_the_live_link() {
    let tmp = Tmp::new("scan");
    let versions = tmp.path().join("versions");
    std::fs::create_dir_all(versions.join("2.1.259")).expect("dir");
    std::fs::create_dir_all(versions.join("2.1.265")).expect("dir");
    std::fs::write(versions.join("2.1.265").join("claude"), b"bin").expect("bin");
    let link = tmp.path().join("claude");
    #[cfg(unix)]
    std::os::unix::fs::symlink(versions.join("2.1.265").join("claude"), &link).expect("symlink");

    let roots = Roots {
        versions_dir: Some(versions.clone()),
        live_link: Some(link),
        ..Roots::default()
    };
    let survey = scan(&roots, NOW, Trigger::OnDemand, Config::default(), Some(GIB));
    assert_eq!(survey.versions.len(), 2);
    assert_eq!(survey.free_bytes, Some(GIB));
    assert_eq!(survey.trigger, Trigger::OnDemand);
    #[cfg(unix)]
    {
        // The link points at the BINARY; the class is about the version
        // DIRECTORY, so the resolved target is walked up to it.
        let live = survey.live_version.clone().expect("the link resolved");
        assert!(
            live.ends_with("2.1.265"),
            "the version directory, not the binary: {}",
            live.display()
        );
        let rep = report(&survey, applying());
        let rows = rep.rows_of(Class::ClaudeStaleVersions);
        assert_eq!(rows.len(), 1, "only the one that is not live: {rep:?}");
        assert!(rows[0].path.ends_with("2.1.259"));
    }
    let _ = &versions;
}

/// ONE BUILD DIRECTORY IS ONE ROW, HOWEVER IT WAS NAMED: a `target` link and
/// the `target.noindex` it names (or two agents' cwds that differ by a
/// linked component) canonicalize to one directory, which the survey holds
/// once — so the report does not count its bytes twice and an apply does not
/// reclaim it and then fail on it a second time.
#[cfg(unix)]
#[test]
fn a_build_directory_named_twice_is_one_row() {
    let tmp = Tmp::new("twice");
    let repo = tmp.path().join("repo");
    let twin = lay_target(&repo.join("target.noindex"));
    std::os::unix::fs::symlink("target.noindex", repo.join("target")).expect("the link");
    let now = now_after_write(DEFAULT_TARGET_STALE_DAYS + 1);
    let roots = Roots {
        targets: vec![repo.join("target"), twin.clone()],
        ..Roots::default()
    };
    let survey = scan(&roots, now, Trigger::OnDemand, Config::default(), Some(GIB));
    assert_eq!(survey.targets.len(), 1, "{:?}", survey.targets);
    let rep = report(&survey, applying());
    assert_eq!(rep.rows_of(Class::CargoTargets).len(), 1, "{rep:?}");
    let done = apply(
        &rep,
        None,
        Some(Class::CargoTargets),
        &mut real(now),
        &mut no_pressure,
        &mut |_| true,
    );
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert!(done.denials.is_empty(), "{done:?}");
}

/// ONE VERDICT PER PROFILE IN THE IDLE PASS TOO, THE INNERMOST BUILD
/// DIRECTORY'S. A build directory named inside another (`o/target` holding
/// `o/target/nested/target`) is surveyed twice, one survey after the other,
/// and each probes the nested profile's locks: a build holding one at only
/// one of the two surveys is read free by one and held by the other. The
/// idle rows are per build directory, so until 2026-09-28 the OUTER row
/// counted the nested profile as well: held at the inner survey, a
/// removable outer row counting it beside "cargo-targets: …/nested/target —
/// a build holds …"; held at the outer survey with the outer directory's
/// own profile recent, the outer directory's note that "a build holds" it
/// beside the inner row counting it; held at neither, the profile counted by
/// both rows, and an apply that took it through the outer row and then
/// failed on it as a denial. Now only the inner directory's survey decides
/// that profile — its row, or its note keeping it — and the outer row
/// neither counts it nor, applied, takes it.
///
/// Both ways the outer directory's own profile can stand: idle (a row of
/// its own either way) and recent (no row of its own). No clock decides
/// which survey sees the build: the surveys run one after the other and
/// the lock is held around exactly one ([`target::Build`]).
///
/// NEGATIVE CONTROL: held at neither, each idle profile is counted once, by
/// its own directory's row, and an apply takes each once, with no denial.
#[cfg(unix)]
#[test]
fn the_innermost_build_directory_decides_a_nested_profile_in_the_idle_pass() {
    use super::pressure_tests::last_used;
    /// Which survey a build holds the nested profile's `.cargo-lock` at.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum HeldAt {
        Neither,
        Outer,
        Inner,
    }
    let now = now_after_write(DEFAULT_TARGET_STALE_DAYS + 1);
    for own_idle in [true, false] {
        for build in [HeldAt::Neither, HeldAt::Outer, HeldAt::Inner] {
            let case = format!(
                "outer's own profile {}, build held at {build:?}",
                if own_idle { "idle" } else { "recent" }
            );
            let tmp = Tmp::new("idle-verdict");
            let outer = lay_target(&tmp.path().join("o/target"));
            let inner = lay_target(&outer.join("nested/target"));
            // The nested profile's cache is 64 KiB larger than the outer
            // one's, so a row's bytes say which profiles it counts.
            std::fs::write(
                inner.join("debug/incremental/lay-1x2y3z/s-abc-def/more.bin"),
                vec![5u8; 65_536],
            )
            .expect("a larger cache");
            if !own_idle {
                last_used(&outer.join("debug"), now - 2 * 3_600);
            }
            let canon = |p: &Path| std::fs::canonicalize(p).expect("canonical");
            let (outer, inner) = (canon(&outer), canon(&inner));
            let (o, n) = (outer.join("debug"), inner.join("debug"));
            let size = |p: &Path| target::cache_bytes(&p.join(INCREMENTAL), None).0;
            let (bo, bn) = (size(&o), size(&n));
            assert!(bo > 0 && bn > bo, "{case}: {bo} {bn}");
            // One survey of `dir`, a build holding the nested profile's lock
            // around it when `held` — taken before, released after.
            let survey_of = |dir: &Path, held: bool| {
                let _build = held.then(|| target::Build::holds(&n.join(".cargo-lock")));
                let roots = Roots {
                    targets: vec![dir.to_path_buf()],
                    ..Roots::default()
                };
                scan(
                    &roots,
                    now,
                    Trigger::OnDemand,
                    Config::default(),
                    Some(500 * GIB),
                )
            };
            let mut survey = survey_of(&outer, build == HeldAt::Outer);
            let second = survey_of(&inner, build == HeldAt::Inner);
            survey.targets.extend(second.targets);
            assert_eq!(survey.targets.len(), 2, "{case}");
            let rep = report(&survey, applying());

            // The profiles a removable row counts, read off its bytes.
            let counts = |r: &Row| -> Vec<PathBuf> {
                match r.bytes {
                    b if b == bo => vec![o.clone()],
                    b if b == bn => vec![n.clone()],
                    b if b == bo + bn => vec![o.clone(), n.clone()],
                    b => panic!("{case}: {b} bytes is no set of these profiles: {r:?}"),
                }
            };
            let removable: Vec<&Row> = rep
                .rows_of(Class::CargoTargets)
                .into_iter()
                .filter(|r| r.removable)
                .collect();
            // The build directory's note, when it has one.
            let note = |dir: &Path| -> Option<&str> {
                let head = format!("cargo-targets: {} — ", dir.display());
                rep.notes.iter().find_map(|x| x.strip_prefix(head.as_str()))
            };
            // The profiles a note keeps because a build holds one of their
            // locks.
            let kept: Vec<PathBuf> = rep
                .notes
                .iter()
                .filter_map(|x| x.split_once("a build holds "))
                .map(|(_, lock)| {
                    let lock = lock.split(" — ").next().unwrap_or(lock);
                    Path::new(lock)
                        .parent()
                        .expect("a lock's profile")
                        .to_path_buf()
                })
                .collect();

            // No removable row counts a profile a note keeps, and none is
            // counted twice.
            let counted: Vec<PathBuf> = removable.iter().flat_map(|r| counts(r)).collect();
            for p in &counted {
                assert!(
                    !kept.contains(p),
                    "{case}: {} is both counted and kept: {rep:?}",
                    p.display()
                );
                assert_eq!(
                    counted.iter().filter(|q| *q == p).count(),
                    1,
                    "{case}: {} counted twice: {rep:?}",
                    p.display()
                );
            }
            // The outer row, when it has one, counts its own profile alone
            // and leaves the nested build directory to its own survey.
            let outer_row = removable.iter().find(|r| r.path == outer);
            assert_eq!(outer_row.is_some(), own_idle, "{case}: {rep:?}");
            if let Some(r) = outer_row {
                assert_eq!(counts(r), vec![o.clone()], "{case}: {rep:?}");
                assert!(
                    matches!(&r.witness, Witness::StaleBuildDir { profiles: 1, leaves, .. } if *leaves == vec![inner.clone()]),
                    "{case}: {r:?}"
                );
            } else {
                let why = note(&outer).unwrap_or_else(|| panic!("{case}: {:?}", rep.notes));
                assert!(
                    why.contains(&format!("under the {DEFAULT_TARGET_STALE_DAYS}d threshold"))
                        && !why.contains("a build holds"),
                    "{case}: the outer directory speaks for its own profile only: {why}"
                );
            }
            // The inner directory decides the nested profile: its row, or,
            // held at its survey, its note keeping it.
            let inner_row = removable.iter().find(|r| r.path == inner);
            if build == HeldAt::Inner {
                assert!(inner_row.is_none(), "{case}: {rep:?}");
                assert_eq!(
                    note(&inner),
                    Some(format!("a build holds {}", n.join(".cargo-lock").display()).as_str()),
                    "{case}: {:?}",
                    rep.notes
                );
                assert_eq!(kept, vec![n.clone()], "{case}: {:?}", rep.notes);
            } else {
                let r = inner_row.unwrap_or_else(|| panic!("{case}: {rep:?}"));
                assert_eq!(counts(r), vec![n.clone()], "{case}: {rep:?}");
                assert!(note(&inner).is_none(), "{case}: {:?}", rep.notes);
                assert!(kept.is_empty(), "{case}: {:?}", rep.notes);
            }

            // APPLIED, the build long gone: each row takes exactly what it
            // counts, once, and a profile its note kept stays.
            let done = apply(
                &rep,
                None,
                Some(Class::CargoTargets),
                &mut real(now),
                &mut no_pressure,
                &mut |_| true,
            );
            assert!(done.denials.is_empty(), "{case}: {done:?}");
            assert_eq!(done.removed.len(), removable.len(), "{case}: {done:?}");
            for (row, removed) in &done.removed {
                let mut units = removed.units.clone();
                units.sort();
                let want: Vec<PathBuf> = counts(row).iter().map(|p| p.join(INCREMENTAL)).collect();
                assert_eq!(units, want, "{case}: {done:?}");
            }
            for p in [&o, &n] {
                assert_eq!(
                    p.join(INCREMENTAL).exists(),
                    !counted.contains(p),
                    "{case}: {}",
                    p.display()
                );
            }
        }
    }
}

// -- the automatic floor --------------------------------------------------------

/// THE AUTOMATIC GRANT: below `auto_free_gib` the idle build cache goes —
/// with `disk.apply` OFF, since the floor is its own grant — and nothing of
/// another class does; at the floor, above it, or on an unknown free figure,
/// nothing is planned and the remover is never called. Before the floor
/// existed nothing removed anything until a person named a class (the
/// 21.8 h full-disk stall of the harness audit).
#[test]
fn below_the_floor_the_stale_target_goes_and_nothing_else_does() {
    let tmp = Tmp::new("auto-floor");
    let dir = synthetic_target(&tmp, "target");
    let now = now_after_write(DEFAULT_TARGET_STALE_DAYS + 1);
    let config = Config::default();
    assert!(!config.apply, "the floor does not need the verb's switch");
    let survey_at = |free: Option<u64>| {
        let mut survey = Survey::new(now);
        survey.free_bytes = free;
        survey
            .targets
            .push(scan_target(&dir, now, config.target_stale_days, false).expect("the target"));
        // Another class's stale row, which the floor must never reach.
        let versions = tmp.path().join("versions");
        std::fs::create_dir_all(versions.join("2.1.1")).expect("old version");
        survey.versions.push(VersionDir {
            path: versions.join("2.1.1"),
            bytes: 4096,
            bytes_partial: false,
        });
        survey.live_version = Some(versions.join("2.1.2"));
        survey
    };

    // At and above the floor, and unknown: nothing.
    for free in [Some(DEFAULT_AUTO_FREE_GIB * GIB), Some(500 * GIB), None] {
        let rep = report(&survey_at(free), config);
        assert_eq!(auto_plan(&rep), None, "free={free:?}");
        let mut calls = 0;
        let done = apply_auto(
            &rep,
            None,
            &mut counting(&mut calls),
            &mut no_pressure,
            &mut |_| true,
        );
        assert_eq!(done, None);
        assert_eq!(calls, 0, "free={free:?}: the remover was called");
    }
    // Off: nothing, however low.
    let off = Config {
        auto_free_gib: 0,
        ..config
    };
    assert_eq!(auto_plan(&report(&survey_at(Some(GIB)), off)), None);

    // Below it: the stale target, and only it.
    let rep = report(&survey_at(Some(GIB)), config);
    let rows = auto_plan(&rep).expect("below the floor");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].class, Class::CargoTargets);
    assert!(
        rep.rows
            .iter()
            .any(|r| r.class == Class::ClaudeStaleVersions && r.removable),
        "the other class had a removable row the floor left alone: {rep:?}"
    );
    let done =
        apply_auto(&rep, None, &mut real(now), &mut no_pressure, &mut |_| true).expect("applied");
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert!(done.denials.is_empty(), "{done:?}");
    assert!(
        !dir.join("debug/incremental").exists(),
        "the idle profile's incremental/ is gone"
    );
    assert!(dir.join("debug/deps").exists(), "and nothing else of it");
    assert!(
        tmp.path().join("versions/2.1.1").exists(),
        "the other class's directory stayed"
    );
}
