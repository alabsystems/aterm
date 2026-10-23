// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk report and apply path ([`NOW`], [`now_after_write`], [`lay_target`]).

use super::*;
use crate::harness::host_path::abs;

const NOW: i64 = 1_789_603_200;

/// A fresh directory under the system temp dir, removed on drop.
pub(crate) struct Tmp(PathBuf);

impl Tmp {
    pub(crate) fn new(tag: &str) -> Tmp {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "aterm-harness-disk-{tag}-{}-{:?}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
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

/// `now`, `days` past the real wall clock: the files a test just wrote carry
/// the real clock, so the survey is moved later rather than the files back-dated.
pub(crate) fn now_after_write(days: i64) -> i64 {
    let real = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    real + days * 86_400
}

/// A build directory as cargo lays one out, at `dir`: the two root markers
/// (`CACHEDIR.TAG` with its signature, `.rustc_info.json`) and no root lock;
/// one profile, `debug/`, holding all three build locks, `.fingerprint/`,
/// `deps/` (an rlib), `build/` and `incremental/` (8 KiB).
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
    let at = |rel: &str| rel.split('/').fold(profile.to_path_buf(), |p, c| p.join(c));
    for d in [
        "deps",
        ".fingerprint/lay-2bcb77ac5af7c69c",
        "build",
        "incremental/lay-1x2y3z/s-abc-def",
    ] {
        std::fs::create_dir_all(at(d)).expect("profile dirs");
    }
    for lock in CARGO_LOCKS {
        std::fs::write(profile.join(lock), b"").expect("lock");
    }
    std::fs::write(profile.join("deps").join("liblay.rlib"), vec![7u8; 4096]).expect("rlib");
    std::fs::write(
        at(".fingerprint/lay-2bcb77ac5af7c69c/lib-lay"),
        b"0123456789abcdef",
    )
    .expect("fingerprint");
    std::fs::write(
        at("incremental/lay-1x2y3z/s-abc-def/query-cache.bin"),
        vec![9u8; 8192],
    )
    .expect("query cache");
}

/// [`lay_target`] at `<tmp>/<name>`, answered canonical (the report names
/// the real path; macOS's temp dir sits behind the `/var` link).
pub(crate) fn synthetic_target(tmp: &Tmp, name: &str) -> PathBuf {
    let dir = lay_target(&tmp.path().join(name));
    std::fs::canonicalize(&dir).expect("canonical")
}

/// The real remover under the default stale window at `now`, on any device.
#[cfg(unix)]
fn real(now: i64) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    remover(
        Judge {
            now,
            threshold_days: DEFAULT_TARGET_STALE_DAYS,
            device: None,
            volume: None,
        },
        idle::Sessions::default,
    )
}

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

fn applying() -> Config {
    Config {
        apply: true,
        ..Config::default()
    }
}

#[cfg(unix)]
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

    let (row, removed) = &done.removed[0];
    let line = removal_row(now, "sid-1", row, removed);
    assert!(line.contains("\"kind\":\"removed\""), "{line}");
    assert!(line.contains("\"witness\":\"stale-build-dir\""), "{line}");
    assert!(line.contains("CACHEDIR.TAG"), "{line}");
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

    let bare = tmp.path().join("bare");
    lay_profile(&bare.join("debug"));
    let scanned =
        scan_target(&bare, now, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory");
    assert!(scanned.marker.is_none());
    let mut survey = Survey::new(now);
    survey.targets.push(scanned);
    let rep = report(&survey, applying());
    assert!(rep.rows_of(Class::CargoTargets).is_empty(), "{rep:?}");

    let fresh = synthetic_target(&tmp, "fresh");
    let now_fresh = now_after_write(0);
    let scanned =
        scan_target(&fresh, now_fresh, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory");
    let mut survey = Survey::new(now_fresh);
    survey.targets.push(scanned);
    let rep = report(&survey, applying());
    assert!(rep.rows_of(Class::CargoTargets).is_empty(), "{rep:?}");

    let busy = synthetic_target(&tmp, "busy");
    let held = target::Build::holds(&busy.join("debug/.cargo-build-lock"));
    let mut survey = Survey::new(now);
    survey
        .targets
        .push(scan_target(&busy, now, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory"));
    let rep = report(&survey, applying());
    assert!(rep.rows_of(Class::CargoTargets).is_empty(), "{rep:?}");
    drop(held);

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
}

#[test]
fn a_version_dir_that_is_not_the_link_target_is_reported_and_the_live_one_is_not() {
    let mut survey = Survey::new(NOW);
    for (name, bytes) in [("2.1.259", 200), ("2.1.265", 300)] {
        survey.versions.push(VersionDir {
            path: PathBuf::from("/v").join(name),
            bytes,
            bytes_partial: false,
            kept: None,
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
        kept: None,
    });
    survey.live_version = None;
    let rep = report(&survey, applying());
    let rows = rep.rows_of(Class::ClaudeStaleVersions);
    assert_eq!(rows.len(), 1, "it is still REPORTED");
    assert!(
        !rows[0].removable,
        "but it has no witness, so it is blocked"
    );

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
    for build in ["2026091501", "2026091601", "2026091701", "2026091801"] {
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
    let named: Vec<&Path> = rows.iter().map(|r| r.path.as_path()).collect();
    assert_eq!(
        named,
        [
            Path::new("/store/claude/2026091501"),
            Path::new("/store/claude/2026091801")
        ],
        "what atpkg's retention takes: {rep:?}"
    );
    assert!(
        !named.contains(&Path::new("/store/claude/2026091601")),
        "the rollback target is atpkg's to keep"
    );
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

#[cfg(unix)]
#[test]
fn report_only_is_the_default_and_apply_without_a_class_does_nothing() {
    let tmp = Tmp::new("default");
    let dir = synthetic_target(&tmp, "target");
    let now = now_after_write(DEFAULT_TARGET_STALE_DAYS + 1);
    let mut survey = Survey::new(now);
    survey
        .targets
        .push(scan_target(&dir, now, DEFAULT_TARGET_STALE_DAYS, false).expect("a real directory"));

    let rep = report(&survey, Config::default());
    assert!(!rep.config.apply, "disk.apply defaults to false");
    assert_eq!(rep.rows_of(Class::CargoTargets).len(), 1, "still REPORTED");
    assert!(rep.headline().contains("apply=off"), "{}", rep.headline());

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

    let line = denial_row(now, "sid-1", &done.denials[0]);
    assert!(line.contains("\"kind\":\"denial\""), "{line}");
    assert!(line.contains("\"reason\":\"apply-off\""), "{line}");
    assert!(line.contains("\"class\":\"cargo-targets\""), "{line}");
}

#[test]
fn a_path_outside_the_list_is_refused_with_a_denial_row() {
    let reported = PathBuf::from(abs("/v/2.1.259"));
    let mut survey = Survey::new(NOW);
    survey.versions.push(VersionDir {
        path: reported.clone(),
        bytes: 200,
        bytes_partial: false,
        kept: None,
    });
    survey.live_version = Some(PathBuf::from(abs("/v/2.1.265")));
    let rep = report(&survey, applying());

    let outside = abs("/etc/passwd");
    let err = guard(&rep, None, Class::ClaudeStaleVersions, Path::new(&outside))
        .expect_err("a path nothing reported has no witness");
    assert_eq!(err.code(), "not-reported");
    let line = denial_row(NOW, "sid-1", &err);
    assert!(line.contains("\"kind\":\"denial\""), "{line}");
    assert!(line.contains("\"reason\":\"not-reported\""), "{line}");
    assert!(line.contains(&outside), "{line}");
    assert!(!line.contains('\n'), "one row is one line: {line}");

    assert_eq!(
        guard(&rep, None, Class::CargoTargets, &reported)
            .expect_err("a cargo-targets grant cannot reach a versions row")
            .code(),
        "not-reported"
    );

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

    guard(&rep, None, Class::ClaudeStaleVersions, &reported).expect("the reported row passes");
}

#[test]
fn nothing_under_the_transcripts_root_is_ever_removed_even_when_it_is_on_the_report() {
    let mut survey = Survey::new(NOW);
    let root = PathBuf::from(abs("/home/a/.claude/projects"));
    survey.transcripts_root = Some(root.clone());
    survey.versions.push(VersionDir {
        path: root.join("repo"),
        bytes: 999,
        bytes_partial: false,
        kept: None,
    });
    survey.live_version = Some(PathBuf::from(abs("/v/2.1.265")));
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

    assert_eq!(
        guard(
            &rep,
            survey.transcripts_root.as_deref(),
            Class::ClaudeStaleVersions,
            &root
        )
        .expect_err("the root itself")
        .code(),
        "transcript"
    );
}

/// WINDOWS: THE TRANSCRIPTS FENCE HOLDS IN EVERY SPELLING. The root is
/// spelled plain (`home.join(".claude").join("projects")`) and a row as
/// `canonicalize` spells it (`\\?\C:\…`, the case on disk) or in capitals:
/// both are refused, on the report or not.
#[cfg(windows)]
#[test]
fn the_transcripts_fence_holds_in_every_windows_spelling() {
    let tmp = Tmp::new("transcripts-win");
    let root = tmp.path().join(".claude").join("projects");
    let session = root.join("C--Users-x-repo");
    std::fs::create_dir_all(&session).expect("transcripts");
    std::fs::write(session.join("s.jsonl"), b"{}\n").expect("a transcript");
    let shorty = root.join("ABC~1");
    std::fs::create_dir_all(&shorty).expect("an 8.3-shaped name");
    let beside = tmp.path().join("elsewhere").join("ABC~1");
    std::fs::create_dir_all(&beside).expect("beside");
    let rep = report(&Survey::new(NOW), applying());
    let refused = |p: &Path| guard_path(Some(&root), p).map_err(|r| r.code());
    let verbatim = std::fs::canonicalize(session.join("s.jsonl")).expect("canonical");
    let capitals = PathBuf::from(session.to_string_lossy().to_uppercase());
    for p in [&verbatim, &capitals, &shorty, &shorty.join("missing")] {
        assert_eq!(refused(p), Err("transcript"), "{p:?}");
        let code = guard(&rep, Some(&root), Class::CargoTargets, p).map_err(|r| r.code());
        assert_eq!(code, Err("transcript"), "{p:?}");
    }
    assert_eq!(refused(&beside), Ok(()));
    assert_eq!(refused(&tmp.path().join("elsewhere")), Ok(()));
    assert_eq!(
        refused(&tmp.path().join("elsewhere").join("ABC~1").join("missing")),
        Err("transcript"),
        "an ancestor not read is not ruled out"
    );
    assert_eq!(placed_under(&shorty, &root), Some(true));
    assert_eq!(placed_under(&beside, &root), Some(false));
}

/// WINDOWS: THE TRANSCRIPTS FENCE SEES THROUGH A LINKED `~\.claude`. The
/// root comes as the home names it, through a junction to another folder of
/// the same drive, where the key alone reads a canonical row under the real
/// folder as beside it: the row is refused all the same, on the report or
/// not, by the root's file identity among the row's ancestors.
#[cfg(windows)]
#[test]
fn the_transcripts_fence_sees_through_a_linked_claude_folder() {
    let tmp = Tmp::new("transcripts-linked");
    let real = tmp.path().join("data").join("claude");
    let session = real.join("projects").join("C--Users-x-repo");
    std::fs::create_dir_all(&session).expect("transcripts");
    std::fs::write(session.join("s.jsonl"), b"{}\n").expect("a transcript");
    let beside = tmp.path().join("data").join("other");
    std::fs::create_dir_all(&beside).expect("beside");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    atpkg::platform::atomic_symlink(&real, &home.join(".claude")).expect("a junction");
    let root = home.join(".claude").join("projects");
    let rep = report(&Survey::new(NOW), applying());
    let code = |root: &Path, p: &Path| guard_path(Some(root), p).map_err(|r| r.code());

    let row = std::fs::canonicalize(&session).expect("canonical");
    assert_eq!(
        aterm_sysprobe::paths::is_under(&row, &root),
        Some(false),
        "the key alone reads the row beside the linked root"
    );
    assert_eq!(code(&root, &row), Err("transcript"));
    assert_eq!(
        guard(&rep, Some(&root), Class::CargoTargets, &row).map_err(|r| r.code()),
        Err("transcript")
    );
    let beside = std::fs::canonicalize(&beside).expect("canonical");
    assert_eq!(code(&root, &beside), Ok(()));

    let missing = tmp.path().join("nobody").join(".claude").join("projects");
    let shorty = tmp.path().join("elsewhere").join("ABC~1");
    std::fs::create_dir_all(&shorty).expect("an 8.3-shaped name");
    assert_eq!(
        code(&missing, &shorty),
        Ok(()),
        "a root not there holds nothing"
    );
    assert_eq!(code(&missing, &missing.join("x")), Err("transcript"));
}

#[test]
fn a_removal_that_fails_is_a_denial_row_and_does_not_abandon_the_rest() {
    let mut survey = Survey::new(NOW);
    for name in ["2.1.259", "2.1.260"] {
        survey.versions.push(VersionDir {
            path: PathBuf::from(abs("/v")).join(name),
            bytes: 10,
            bytes_partial: false,
            kept: None,
        });
    }
    survey.live_version = Some(PathBuf::from(abs("/v/2.1.265")));
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

/// A STALE VERSION IN A REAL CLOUD SYNC ROOT STAYS. A scratch folder registered as a cloud
/// sync root ([`aterm_dirfd::seam::SyncRoot`]) holds a versions folder whose version file and
/// version folder are in-sync placeholders; [`remove_tree`] refuses both and every byte stays,
/// whatever placeholder mode the calling thread is in.
#[cfg(windows)]
#[test]
fn remove_tree_refuses_a_version_in_a_cloud_sync_root() {
    let tmp = Tmp::new("win-sync-versions");
    let sync = std::fs::canonicalize(tmp.path())
        .expect("canonical")
        .join("synced");
    let versions = sync.join("versions");
    std::fs::create_dir_all(versions.join("2.0.0-dir")).expect("dir");
    std::fs::write(versions.join("2.0.0"), b"exe").expect("file");
    std::fs::write(versions.join("2.0.0-dir").join("f"), b"x").expect("file");
    let root = aterm_dirfd::seam::SyncRoot::register(&sync).expect("a sync root");
    for made in [
        versions.clone(),
        versions.join("2.0.0"),
        versions.join("2.0.0-dir"),
        versions.join("2.0.0-dir").join("f"),
    ] {
        root.convert(&made).expect("a placeholder");
    }
    for p in [versions.join("2.0.0"), versions.join("2.0.0-dir")] {
        let err = remove_tree(&p).expect_err("in a sync root");
        assert!(err.to_string().contains("cloud sync root"), "{err}");
    }
    assert_eq!(std::fs::read(versions.join("2.0.0")).expect("kept"), b"exe");
    assert_eq!(
        std::fs::read(versions.join("2.0.0-dir").join("f")).expect("kept"),
        b"x"
    );

    aterm_dirfd::seam::leave_placeholders_disguised(true);
    let file = remove_tree(&versions.join("2.0.0"));
    let dir = remove_tree(&versions.join("2.0.0-dir"));
    aterm_dirfd::seam::leave_placeholders_disguised(false);
    file.expect("disguised: the file goes");
    dir.expect("disguised: the folder goes");
    assert!(!versions.join("2.0.0").exists());
    assert!(!versions.join("2.0.0-dir").exists());
    drop(root);
}

/// A VERSION WHOSE FOLDER CANNOT BE PINNED STAYS, WHATEVER THE REASON: a versions folder
/// reached through a junction (no cloud sync root anywhere: `aterm_dirfd::pin` answers `LOOP`
/// at the junction) holds a version file and a version folder; [`remove_tree`] of each,
/// spelled through the junction, refuses, names why, and every byte stays — the refusal that
/// fired for a sync root alone let this through to a delete by path, which followed the
/// junction.
#[cfg(windows)]
#[test]
fn remove_tree_refuses_a_version_whose_folder_is_reached_through_a_junction() {
    let tmp = Tmp::new("win-junction-versions");
    let base = tmp.path().to_path_buf();
    let versions = base.join("real").join("versions");
    std::fs::create_dir_all(versions.join("2.0.0-dir").join("sub")).expect("dir");
    std::fs::write(versions.join("2.0.0"), b"exe").expect("file");
    std::fs::write(versions.join("2.0.0-dir").join("sub").join("f"), b"x").expect("file");
    let elsewhere = base.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("dir");
    std::fs::write(elsewhere.join("g"), b"y").expect("file");
    let link = base.join("link");
    atpkg::platform::atomic_symlink(&base.join("real"), &link).expect("a junction");
    atpkg::platform::atomic_symlink(&elsewhere, &versions.join("2.0.1")).expect("a junction");

    let through = link.join("versions");
    for p in [through.join("2.0.0"), through.join("2.0.0-dir")] {
        let err = remove_tree(&p).expect_err("its folder is reached through a junction");
        assert!(
            err.to_string()
                .contains("could not be opened from its volume's root"),
            "{err}"
        );
    }
    let err = remove_tree(&versions.join("2.0.1")).expect_err("a junction itself");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    assert!(err.to_string().contains("symlink or junction"), "{err}");
    assert_eq!(std::fs::read(versions.join("2.0.0")).expect("kept"), b"exe");
    assert_eq!(
        std::fs::read(versions.join("2.0.0-dir").join("sub").join("f")).expect("kept"),
        b"x"
    );
    assert_eq!(std::fs::read(elsewhere.join("g")).expect("kept"), b"y");

    remove_tree(&versions.join("2.0.0")).expect("the real path: the file goes");
    remove_tree(&versions.join("2.0.0-dir")).expect("the real path: the folder goes");
    assert!(!versions.join("2.0.0").exists());
    assert!(!versions.join("2.0.0-dir").exists());
    assert!(std::fs::symlink_metadata(&link).is_ok_and(|m| m.is_symlink()));
    assert!(std::fs::symlink_metadata(versions.join("2.0.1")).is_ok_and(|m| m.is_symlink()));
    assert_eq!(std::fs::read(elsewhere.join("g")).expect("kept"), b"y");
}

/// A VERSION OFF A FIXED DISK STAYS: the volume fence every reclaim clears first holds for a
/// Claude Code version too.
#[cfg(windows)]
#[test]
fn remove_tree_refuses_a_version_off_a_fixed_disk() {
    let tmp = Tmp::new("win-drive-versions");
    let versions = tmp.path().join("versions");
    std::fs::create_dir_all(versions.join("2.0.0-dir").join("sub")).expect("dir");
    std::fs::write(versions.join("2.0.0"), b"exe").expect("file");
    std::fs::write(versions.join("2.0.0-dir").join("sub").join("f"), b"x").expect("file");
    for (kind, why) in [(Some(false), "removable"), (None, "could not be read")] {
        target::pretend_drive_kind(Some(kind));
        for p in [versions.join("2.0.0"), versions.join("2.0.0-dir")] {
            let err = remove_tree(&p).expect_err("off a fixed disk");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
            let err = err.to_string();
            assert!(err.contains(why) && err.contains("fixed disk"), "{err}");
        }
        assert_eq!(std::fs::read(versions.join("2.0.0")).expect("kept"), b"exe");
        assert_eq!(
            std::fs::read(versions.join("2.0.0-dir").join("sub").join("f")).expect("kept"),
            b"x"
        );
    }

    target::pretend_drive_kind(None);
    remove_tree(&versions.join("2.0.0")).expect("a fixed disk: the file goes");
    remove_tree(&versions.join("2.0.0-dir")).expect("a fixed disk: the folder goes");
    assert!(!versions.join("2.0.0").exists());
    assert!(!versions.join("2.0.0-dir").exists());
}

#[test]
fn the_safelist_is_closed_and_its_spellings_round_trip() {
    for c in Class::ALL {
        assert_eq!(Class::parse(c.as_str()), Some(*c));
    }
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
    assert_eq!(age_days(NOW, NOW + 999 * 86_400), 0);
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
        kept: None,
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

    let line = report_row(NOW, "sid-1", &rep);
    assert!(line.contains("\"kind\":\"report\""), "{line}");
    assert!(line.contains("\"rows\":1"), "{line}");
    assert!(line.contains("\"warn\":true"), "{line}");
}

/// THE FREE FIGURE IS ONE SYSCALL ON THE MOUNT POINT, never a fork: this
/// test's own temp directory reads a figure no larger than its volume, a path
/// that does not exist yet reads its nearest ancestor's volume (where it would
/// be written), and a spelling through a link (`/tmp` on macOS) reads the
/// volume it resolves to.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_free_figure_is_read_on_the_volume_a_path_resolves_to() {
    let tmp = Tmp::new("free-figure");
    let table = aterm_sysprobe::volumes::table();
    let here = volume_of(tmp.path(), &table).expect("a local volume");
    assert!(here.free > 0 && here.free <= here.total, "{here:?}");
    let ghost = volume_of(&tmp.path().join("not/yet/here"), &table).expect("its ancestor");
    assert_eq!(ghost.key, here.key);
    assert_eq!(ghost.mount, here.mount);
    let linked = volume_of(&std::env::temp_dir(), &table).expect("the temp root");
    assert_eq!(linked.mount, here.mount);
    assert!(free_bytes(tmp.path()).is_some());
    assert!(free_bytes(Path::new("relative/path")).is_none());
}

/// A DIRECTORY SPELLED WITH 8.3 SHORT NAMES READS ITS VOLUME (live1's first
/// finding: a tab or an agent working under a short `%TEMP%` read
/// `NoMount`, so no volume and no disk line): the short spelling of a real
/// long-named folder reads the volume its long spelling reads, and so does
/// a directory under it that is not made yet (the 8.3 branch's review,
/// rev4-6: `await sys vol=` naming a folder still to be made, or a tab whose
/// directory was removed, read `NoMount`).
#[cfg(windows)]
#[test]
fn a_short_spelled_directory_reads_its_volume() {
    let tmp = Tmp::new("short-volume");
    let folder = std::fs::canonicalize(tmp.path())
        .expect("canonical")
        .join("a folder with a long name");
    std::fs::create_dir(&folder).expect("the long-named folder");
    let short = aterm_sysprobe::short_spelling(&folder).unwrap_or_else(|| {
        panic!(
            "PREMISE: the volume generates 8.3 short names; none for {}",
            folder.display()
        )
    });
    let table = aterm_sysprobe::volumes::table();
    assert!(
        aterm_sysprobe::volumes::mount_of(&short, &table).is_none(),
        "{short:?} places on no mount as spelled"
    );
    let long = volume_of(&folder, &table).expect("the long spelling's volume");
    let read = volume_of(&short, &table).expect("the short spelling's volume");
    assert_eq!((&read.mount, &read.key), (&long.mount, &long.key));
    let ghost = short.join("not-made-yet").join("deeper");
    let read = volume_of(&ghost, &table).expect("a directory still to be made");
    assert_eq!((&read.mount, &read.key), (&long.mount, &long.key));
}

#[test]
fn a_volume_is_placed_with_the_error_boxes_off() {
    let tmp = Tmp::new("volume-quiet");
    let table = aterm_sysprobe::volumes::table();
    let unguarded = !cfg!(windows);
    VOLUME_ERRORS_QUIET.set(None);
    assert_eq!(aterm_sysprobe::errors_quiet(), unguarded, "no guard before");
    assert!(volume_of(tmp.path(), &table).is_ok(), "a local volume");
    assert_eq!(VOLUME_ERRORS_QUIET.get(), Some(true), "inside the resolve");
    assert_eq!(
        aterm_sysprobe::errors_quiet(),
        unguarded,
        "the caller's mode restored"
    );
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

/// One build directory is one row, however it was named: a `target` link and
/// the `target.noindex` it names canonicalize to one directory, which the
/// survey holds once, so the report does not count its bytes twice and an
/// apply does not reclaim it twice.
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

/// A build directory nested in another (`o/target/nested/target`) is surveyed
/// twice; only the inner survey decides the nested profile (its row, or its
/// note keeping it while a build holds a lock at that survey), and the outer
/// row neither counts it nor, applied, takes it.
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
            let note = |dir: &Path| -> Option<&str> {
                let head = format!("cargo-targets: {} — ", dir.display());
                rep.notes.iter().find_map(|x| x.strip_prefix(head.as_str()))
            };
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
                    !why.contains("a build holds"),
                    "{case}: the outer directory speaks for its own profile only: {why}"
                );
            }
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

/// The automatic grant: once the volume is LOW (below the warning) the idle
/// build cache goes — tier 2 — with `disk.apply` off, since the automatic
/// removal is its own grant, and nothing of another class does; free space is
/// measured again before it and the tier stops at the warning, so an hourly
/// pass at `low` cannot take every idle cache.
#[cfg(unix)]
#[test]
fn from_low_the_stale_target_goes_re_measured_and_nothing_else_does() {
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
        let versions = tmp.path().join("versions");
        std::fs::create_dir_all(versions.join("2.1.1")).expect("old version");
        survey.versions.push(VersionDir {
            path: versions.join("2.1.1"),
            bytes: 4096,
            bytes_partial: false,
            kept: None,
        });
        survey.live_version = Some(versions.join("2.1.2"));
        survey
    };

    for free in [Some(DEFAULT_WARN_FREE_GIB * GIB), Some(500 * GIB), None] {
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
    let off = Config {
        auto_free_gib: 0,
        ..config
    };
    assert_eq!(auto_plan(&report(&survey_at(Some(GIB)), off)), None);

    let low = Some(20 * GIB);
    let rep = report(&survey_at(low), config);
    let plan = pass_plan(&rep).expect("low");
    assert_eq!(
        plan.iter().map(|p| p.target).collect::<Vec<_>>(),
        [Some(DEFAULT_WARN_FREE_GIB * GIB)],
        "tier 2 stops at the warning"
    );
    let mut calls = 0;
    let done = apply_auto(
        &rep,
        None,
        &mut counting(&mut calls),
        &mut || Some(DEFAULT_WARN_FREE_GIB * GIB),
        &mut |_| true,
    )
    .expect("planned");
    assert_eq!(calls, 0, "{done:?}");
    assert_eq!(done.stopped.as_ref().map(|s| s.by), Some(StopBy::Measured));
    assert_eq!(done.stopped.as_ref().and_then(|s| s.tier), Some(2));

    let rows = auto_plan(&rep).expect("low");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].class, Class::CargoTargets);
    assert!(
        rep.rows
            .iter()
            .any(|r| r.class == Class::ClaudeStaleVersions && r.removable),
        "the other class had a removable row the floor left alone: {rep:?}"
    );
    let done = apply_auto(&rep, None, &mut real(now), &mut || low, &mut |_| true).expect("applied");
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

#[test]
fn thresholds_are_the_larger_of_the_knob_and_the_volumes_share() {
    let big = 460 * GIB;
    let t = Config::default().thresholds(Some(big));
    assert_eq!(t.floor, (big / 100 * AUTO_FREE_PCT).div_ceil(GIB) * GIB);
    assert_eq!(t.floor / GIB, 23);
    assert_eq!(t.warn, 40 * GIB);
    assert_eq!(t.emergency, big / 100);
    assert_eq!(t.target, t.floor + PRESSURE_MARGIN_GIB * GIB);
    for (free, files, want) in [
        (Some(t.warn), None, Level::Ok),
        (Some(t.warn - 1), None, Level::Low),
        (Some(t.floor), None, Level::Low),
        (Some(t.floor - 1), None, Level::Short),
        (Some(t.emergency), None, Level::Short),
        (Some(t.emergency - 1), None, Level::Emergency),
        (
            Some(t.warn),
            Some(EMERGENCY_FILES_FREE - 1),
            Level::Emergency,
        ),
        (Some(t.warn), Some(EMERGENCY_FILES_FREE), Level::Ok),
        (None, None, Level::Unknown),
        (None, Some(EMERGENCY_FILES_FREE), Level::Unknown),
        (None, Some(0), Level::Emergency),
    ] {
        assert_eq!(t.level(free, files), want, "free {free:?} files {files:?}");
    }

    let small = Config::default().thresholds(Some(40 * GIB));
    assert_eq!(
        (small.floor, small.warn, small.emergency),
        (4 * GIB, 8 * GIB, 4 * GIB)
    );
    let unknown = Config::default().thresholds(None);
    assert_eq!(unknown.floor, DEFAULT_AUTO_FREE_GIB * GIB);

    let off = Config {
        auto_free_gib: 0,
        ..Config::default()
    };
    assert_eq!(off.for_volume(Some(big)).auto_free_gib, 0);
    let t = off.thresholds(Some(big));
    assert_eq!(
        t.level(Some(t.emergency), None),
        Level::Low,
        "off: never Short"
    );
    assert_eq!(t.level(Some(0), None), Level::Emergency);
}

#[test]
fn the_lines_on_four_volumes_hold_the_unwritten_defaults_to_a_small_volumes_share() {
    let owners_mac = 1_948_404_040 * 1024;
    for (total, warn, floor, emergency) in [
        (128_000_000_000, 23, 10, 4),
        (256_000_000_000, 40, 12, 4),
        (owners_mac, 64, 32, 8),
        (4_000_000_000_000, 64, 32, 8),
    ] {
        let t = Config::default().thresholds(Some(total));
        assert_eq!(
            (t.warn / GIB, t.floor / GIB, t.emergency / GIB),
            (warn, floor, emergency),
            "{total}"
        );
        assert!(t.warn * 100 / total <= SMALL_WARN_MAX_PCT.max(WARN_FREE_PCT));
    }
    let written = Config::default().thresholds_on(
        Some(128_000_000_000),
        Written {
            auto_free_gib: true,
            warn_free_gib: true,
        },
    );
    assert_eq!((written.warn / GIB, written.floor / GIB), (40, 10));
    let tiny = Config::default().thresholds(Some(10 * GIB));
    assert_eq!(tiny.floor, GIB, "held, never off");
    assert_eq!(tiny.level(Some(GIB / 2), None), Level::Emergency);
}

#[test]
fn the_volume_share_is_capped_and_a_written_knob_is_read_as_written() {
    let huge = 4096 * GIB;
    let t = Config::default().thresholds(Some(huge));
    assert_eq!(
        (t.floor, t.warn, t.emergency),
        (
            AUTO_FREE_SHARE_MAX_GIB * GIB,
            WARN_FREE_SHARE_MAX_GIB * GIB,
            EMERGENCY_FREE_SHARE_MAX_GIB * GIB
        )
    );
    let owner = Config {
        auto_free_gib: 5,
        warn_free_gib: 20,
        ..Config::default()
    };
    let both = Written {
        auto_free_gib: true,
        warn_free_gib: true,
    };
    for total in [40 * GIB, 460 * GIB, huge] {
        let c = owner.on_volume(Some(total), both);
        assert_eq!((c.auto_free_gib, c.warn_free_gib), (5, 20), "{total}");
        let t = owner.thresholds_on(Some(total), both);
        assert_eq!((t.floor, t.warn), (5 * GIB, 20 * GIB), "{total}");
        assert!(t.emergency <= t.floor, "{total}");
    }
    let raised = owner.on_volume(Some(huge), Written::default());
    assert_eq!(
        (raised.auto_free_gib, raised.warn_free_gib),
        (AUTO_FREE_SHARE_MAX_GIB, WARN_FREE_SHARE_MAX_GIB)
    );
}

#[test]
fn claude_codes_windows_temp_root_is_under_its_temp() {
    let (temp, tmp) = (abs("/Users//u/AppData/Local/Temp"), abs("/t"));
    let root = |vars: &[(&str, &str)]| {
        discover::windows_claude_tmp(|name| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| std::ffi::OsString::from(v))
        })
    };
    let under = |dir: &str| Some(PathBuf::from(dir).join("claude"));
    assert_eq!(root(&[("TEMP", &temp), ("TMP", &tmp)]), under(&temp));
    assert_eq!(root(&[("TEMP", ""), ("TMP", &tmp)]), under(&tmp));
    assert_eq!(root(&[("TMP", &tmp)]), under(&tmp));
    assert_eq!(root(&[("TEMP", "Temp"), ("TMP", &tmp)]), None);
    assert_eq!(root(&[]), None);
    #[cfg(windows)]
    {
        let live = discover::claude_tmp_root(discover::own_uid());
        assert!(live.is_absolute() && live.ends_with("claude"), "{live:?}");
    }
}

#[test]
fn the_claude_census_passes_over_its_own_process() {
    use discover::{Exe, Exec, Process};
    let me = std::process::id();
    let other = me.wrapping_add(1);
    let claude = |pid: u32| Process {
        pid,
        exe: Some(Exe::At(PathBuf::from("/opt/x/bin/claude"))),
        exec: Some(Exec {
            path: "/opt/x/bin/claude".to_owned(),
            argv: vec!["claude".to_owned()],
            config: None,
            home: None,
            stored_login: true,
        }),
        cwd: Some(PathBuf::from("/proj")),
        started: None,
    };
    let unread = |pid: u32| Process {
        pid,
        exe: Some(Exe::Unnamed),
        ..Process::default()
    };
    let mut asked = Vec::new();
    let census = discover::census_of_running(
        [me, other],
        |pid| {
            asked.push(pid);
            Some(claude(pid))
        },
        None,
        None,
    )
    .expect("read whole");
    assert_eq!(asked, [other], "this process is never read");
    let pids: Vec<u32> = census.claudes.iter().map(|c| c.pid).collect();
    assert_eq!(pids, [other]);
    assert_eq!(
        discover::census_of_running([me], |pid| Some(unread(pid)), None, None),
        Ok(discover::Running::default())
    );
    let refused = discover::census_of_running([other], |pid| Some(unread(pid)), None, None);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.starts_with(&format!("process {other} "))),
        "{refused:?}"
    );
}

fn idle_whole(tmp: &Tmp, now: i64) -> (PathBuf, PathBuf, Row) {
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git").join("logs")).expect("git");
    std::fs::write(repo.join(".git").join("index"), b"DIRC").expect("index");
    std::fs::write(repo.join(".git").join("logs").join("HEAD"), b"0 1 a\n").expect("head");
    std::fs::create_dir_all(repo.join("src")).expect("src");
    std::fs::write(repo.join("src").join("main.rs"), b"fn main() {}\n").expect("source");
    let target = lay_target(&repo.join("target"));
    let canon = |p: &Path| std::fs::canonicalize(p).expect("canonical");
    let (repo, target) = (canon(&repo), canon(&target));
    let row = Row {
        class: Class::CargoTargets,
        path: target.clone(),
        bytes: 8192,
        bytes_partial: false,
        witness: Witness::IdleWhole {
            marker: "CACHEDIR.TAG".to_owned(),
            checkout: repo.clone(),
            profiles: 1,
            last_used: now,
            age_s: 8 * DAY_S,
            window_days: 7,
            device: target::dev_of(&target).expect("the build directory's device"),
        },
        removable: true,
        blocked: None,
    };
    (repo, target, row)
}

fn remove_idle_whole(
    row: &Row,
    now: i64,
    sessions: &dyn Fn() -> idle::Sessions,
    tables: &dyn Fn() -> aterm_sysprobe::volumes::Table,
) -> std::io::Result<Removed> {
    remover_among(
        Judge {
            now,
            threshold_days: DEFAULT_TARGET_STALE_DAYS,
            device: None,
            volume: target::VolumeGuid::of_path_now(&row.path),
        },
        |_: &[PathBuf], _: &[u32]| Ok(aterm_uds::holders::PathScan::default()),
        |_| Ok(discover::Running::default()),
        sessions,
        tables,
    )(row)
}

#[test]
fn a_session_that_moves_into_the_checkout_after_the_survey_keeps_it() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-sessions-now");
    let (repo, target, row) = idle_whole(&tmp, now);
    let remove = |sessions: &dyn Fn() -> idle::Sessions| {
        remove_idle_whole(&row, now, sessions, &aterm_sysprobe::volumes::table)
    };
    let rlib = target.join("debug").join("deps").join("liblay.rlib");

    let in_src = || idle::Sessions {
        cwds: vec![repo.join("src")],
        unknown: None,
    };
    let refused = remove(&in_src).expect_err("a session works in the checkout");
    assert!(
        refused.to_string().contains("a session works in"),
        "{refused}"
    );

    let why = "tab 7 reported no working directory this window can place, so any checkout may \
               be one's";
    let unknown = || idle::Sessions {
        cwds: Vec::new(),
        unknown: Some(why.to_owned()),
    };
    let refused = remove(&unknown).expect_err("a tab's directory is unknown");
    assert!(refused.to_string().contains(why), "{refused}");

    let reads = std::cell::Cell::new(0usize);
    let moves_in = || {
        reads.set(reads.get() + 1);
        idle::Sessions {
            cwds: if reads.get() == 1 {
                Vec::new()
            } else {
                vec![target.join("debug")]
            },
            unknown: None,
        }
    };
    let refused = remove(&moves_in).expect_err("a session moved in under the locks");
    assert!(
        refused.to_string().contains("a session works in"),
        "{refused}"
    );
    assert!(
        reads.get() >= 2,
        "read again under the locks: {}",
        reads.get()
    );
    assert!(rlib.exists(), "nothing was emptied");

    let removed = remove(&idle::Sessions::default).expect("no session in it");
    assert!(
        removed.units.iter().any(|u| u.ends_with("debug")),
        "{removed:?}"
    );
    assert!(!rlib.exists(), "the profile was emptied");
}

#[test]
fn the_removal_reads_its_sessions_on_a_mount_table_read_whole() {
    use aterm_sysprobe::volumes::{self, Table};
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-sessions-table");
    let (_, target, row) = idle_whole(&tmp, now);
    let beside = tmp.path().join("beside");
    std::fs::create_dir_all(&beside).expect("beside");
    let rlib = target.join("debug").join("deps").join("liblay.rlib");
    let reads = std::cell::Cell::new(0usize);
    let tables = |shape: &dyn Fn(usize, Table) -> Table| {
        reads.set(reads.get() + 1);
        shape(reads.get(), volumes::table())
    };
    let cut_first = |n: usize, t: Table| Table {
        complete: t.complete && n > 1,
        ..t
    };
    let refused = remove_idle_whole(&row, now, &idle::Sessions::default, &|| tables(&cut_first))
        .expect_err("the first table was not read whole");
    assert!(refused.to_string().contains("not read whole"), "{refused}");
    assert_eq!(reads.get(), 1, "refused before the locks");
    assert!(rlib.exists(), "nothing was emptied");

    let in_beside = || idle::Sessions {
        cwds: vec![beside.clone()],
        unknown: None,
    };
    #[cfg(windows)]
    {
        let on = volumes::mount_of(&target, &volumes::table())
            .expect("a mount holds it")
            .on
            .clone();
        let remote_after_first = |n: usize, mut t: Table| {
            for m in t.mounts.iter_mut().filter(|m| n > 1 && m.on == on) {
                m.local = false;
            }
            t
        };
        reads.set(0);
        let refused = remove_idle_whole(&row, now, &in_beside, &|| tables(&remote_after_first))
            .expect_err("a session beside it, not resolved under the locks");
        assert!(
            refused.to_string().contains("a session works in"),
            "{refused}"
        );
        assert!(reads.get() >= 2, "read under the locks: {}", reads.get());
        assert!(rlib.exists(), "nothing was emptied");
    }

    let removed = remove_idle_whole(&row, now, &in_beside, &volumes::table)
        .expect("a session beside it keeps nothing");
    assert!(
        removed.units.iter().any(|u| u.ends_with("debug")),
        "{removed:?}"
    );
    assert!(!rlib.exists(), "the profile was emptied");
}

#[test]
fn a_session_cwd_on_a_share_or_an_automount_is_never_opened() {
    use aterm_sysprobe::paths::is_under;
    let tmp = Tmp::new("idle-cwd-on");
    let dir = tmp.path().join("work");
    std::fs::create_dir_all(&dir).expect("work");
    let canonical = std::fs::canonicalize(&dir).expect("canonical");
    let table = aterm_sysprobe::volumes::table();
    let on = aterm_sysprobe::volumes::mount_of(&dir, &table)
        .expect("a mount holds it")
        .on
        .clone();
    let at = |cwd: &idle::SessionCwd, want: &Path| {
        matches!(cwd, idle::SessionCwd::Placed(p)
            if is_under(p, want) == Some(true) && is_under(want, p) == Some(true))
    };
    let read = idle::SessionCwd::on(&dir, Some(&table));
    assert!(at(&read, &canonical), "{read:?}");
    assert_eq!(
        idle::SessionCwd::on(&dir, None),
        idle::SessionCwd::Unresolved(dir.clone())
    );
    let cut = aterm_sysprobe::volumes::Table {
        complete: false,
        ..table.clone()
    };
    assert_eq!(
        idle::SessionCwd::on(&dir, Some(&cut)),
        idle::SessionCwd::Unresolved(dir.clone())
    );
    let far = tmp.path().join("far");
    std::fs::create_dir_all(&far).expect("far");
    let far_canonical = std::fs::canonicalize(&far).expect("canonical");
    let via = tmp.path().join("via");
    atpkg::platform::atomic_symlink(&far, &via).expect("a link (a junction on Windows)");
    let followed = idle::SessionCwd::on(&via, Some(&table));
    assert!(at(&followed, &far_canonical), "{followed:?}");
    let mut shared = table.clone();
    shared.mounts.push(aterm_sysprobe::volumes::Mount {
        on: far_canonical,
        from: "nas:/far".to_owned(),
        fstype: "nfs".to_owned(),
        local: false,
        automounted: false,
        removable: false,
        read_only: false,
        container: None,
    });
    assert_eq!(
        idle::SessionCwd::on(&via, Some(&shared)),
        idle::SessionCwd::Unresolved(via.clone()),
        "a link onto a share is not followed"
    );
    for remote in [true, false] {
        let mut t = table.clone();
        for m in t.mounts.iter_mut().filter(|m| m.on == on) {
            if remote {
                m.local = false;
            } else {
                m.automounted = true;
            }
        }
        assert_eq!(
            idle::SessionCwd::on(&dir, Some(&t)),
            idle::SessionCwd::Unresolved(dir.clone()),
            "remote {remote}, else automounted"
        );
    }
}

#[test]
fn the_text_form_spells_paths_plain_and_the_row_keeps_its_own() {
    let verbatim = PathBuf::from(r"\\?\C:\Users\u\repo\target");
    let row = Row {
        class: Class::ClaudeStaleVersions,
        path: verbatim.clone(),
        bytes: 1024,
        bytes_partial: false,
        witness: Witness::NotLinkTarget {
            live: PathBuf::from(r"\\?\UNC\srv\share\v\2.1.3"),
        },
        removable: false,
        blocked: Some(
            r"process 9 uses \\?\C:\Users\u\repo\target\debug\x.d (its open file)".to_owned(),
        ),
    };
    let hog = Hog {
        path: verbatim.clone(),
        bytes: 1 << 20,
        bytes_partial: false,
        why: r"kept by \\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}\x".to_owned(),
    };
    let line = row.line();
    let hog_line = hog.line(Some(Path::new(r"C:\Users\u")));
    let hog_verbatim_home = hog.line(Some(Path::new(r"\\?\C:\Users\u")));
    assert_eq!(row.path, verbatim, "the row keeps its own spelling");
    if cfg!(windows) {
        assert!(!line.contains(r"\\?\"), "{line}");
        assert!(line.contains(r" C:\Users\u\repo\target — "), "{line}");
        assert!(
            line.contains(r"uses C:\Users\u\repo\target\debug\x.d"),
            "{line}"
        );
        assert!(line.contains(r"target \\srv\share\v\2.1.3"), "{line}");
        assert!(hog_line.starts_with(r"~\repo\target 1.0 MiB"), "{hog_line}");
        assert_eq!(hog_verbatim_home, hog_line);
        assert!(
            hog_line.contains(r"kept by \\?\Volume{6140fc3b"),
            "no plain spelling: {hog_line}"
        );
    } else {
        let text = r"uses \\?\C:\x and \\?\UNC\srv\share";
        assert_eq!(shown(text), text);
        assert!(line.contains(r"\\?\C:\Users\u\repo\target"), "{line}");
    }
}
