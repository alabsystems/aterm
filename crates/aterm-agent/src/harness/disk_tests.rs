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
//! The negative controls are the point of most of these: a build directory
//! with no marker is not a candidate however old it is; a fresh lock file is
//! not staleness; an unresolvable live symlink turns the whole version class
//! blocked rather than guessing; a delegated class removes nothing even with
//! `disk.apply` on; and apply with no class named removes nothing and never
//! calls the remover at all.

use super::*;

/// 2026-09-21T00:00:00Z. Fixed; never `SystemTime::now()`.
const NOW: i64 = 1_789_603_200;

/// A fresh directory under the system temp dir, removed on drop.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
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

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `now` far enough past the files a test just wrote that both clocks clear
/// the threshold. The files carry the real wall clock, so the only honest
/// reading is "the survey happened later".
fn now_after_write(days: i64) -> i64 {
    let real = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    real + days * 86_400
}

/// A synthetic target directory: the two markers a build tool leaves, and a
/// lock file. Written for real, because the marker check reads bytes.
fn synthetic_target(tmp: &Tmp, name: &str) -> PathBuf {
    let dir = tmp.path().join(name);
    std::fs::create_dir_all(dir.join("debug")).expect("target dir");
    std::fs::write(
        dir.join("CACHEDIR.TAG"),
        format!("{CACHEDIR_SIGNATURE}\n# a cargo target directory\n"),
    )
    .expect("CACHEDIR.TAG");
    std::fs::write(dir.join(CARGO_LOCK_FILE), b"").expect("lock");
    std::fs::write(dir.join(RUSTC_INFO), b"{}").expect("rustc info");
    std::fs::write(dir.join("debug").join("blob"), vec![7u8; 4096]).expect("blob");
    dir
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

    let scanned = scan_target(&dir, now, DEFAULT_TARGET_STALE_DAYS).expect("a real directory");
    assert_eq!(
        scanned.marker.as_deref(),
        Some("CACHEDIR.TAG"),
        "the signed tag is the marker that proves a build tool laid this"
    );
    assert!(
        !scanned.live_build,
        "nothing at the root was touched inside the threshold"
    );
    assert!(scanned.bytes >= 4096, "the blob is counted: {scanned:?}");

    let mut survey = Survey::new(now);
    survey.targets.push(scanned);
    let rep = report(&survey, applying());
    let rows = rep.rows_of(Class::CargoTargets);
    assert_eq!(rows.len(), 1, "one candidate: {rep:?}");
    let row = rows[0].clone();
    assert!(row.removable, "a stale target with a marker is removable");
    assert!(
        matches!(&row.witness, Witness::StaleBuildDir { marker, lock_age_days, root_age_days, threshold_days }
            if marker == "CACHEDIR.TAG"
                && *lock_age_days >= DEFAULT_TARGET_STALE_DAYS
                && *root_age_days >= DEFAULT_TARGET_STALE_DAYS
                && *threshold_days == DEFAULT_TARGET_STALE_DAYS),
        "the witness names the marker and both clocks: {:?}",
        row.witness
    );

    // NOW REMOVE IT, through the real primitive.
    let done = apply(
        &rep,
        survey.transcripts_root.as_deref(),
        Some(Class::CargoTargets),
        &mut remove_tree,
    );
    assert_eq!(done.removed.len(), 1, "exactly the one row: {done:?}");
    assert!(done.denials.is_empty(), "nothing refused: {done:?}");
    assert!(done.freed_bytes >= 4096);
    assert!(!dir.exists(), "the tree is gone");

    // THE ROW CARRIES THE WITNESS. A record of what went without why it was
    // safe is a receipt, not a record.
    let line = removal_row(now, "sid-1", &done.removed[0]);
    assert!(line.contains("\"kind\":\"removed\""), "{line}");
    assert!(line.contains("\"witness\":\"stale-build-dir\""), "{line}");
    assert!(line.contains("CACHEDIR.TAG"), "{line}");
    assert!(line.contains("costs a rebuild"), "{line}");
    assert!(line.contains(&dir.display().to_string()), "{line}");
    assert!(!line.contains('\n'), "one row is one line: {line}");
}

#[test]
fn a_target_without_a_marker_or_with_a_fresh_clock_is_never_a_candidate() {
    let tmp = Tmp::new("negatives");
    let now = now_after_write(DEFAULT_TARGET_STALE_DAYS + 1);

    // NEGATIVE 1: old enough, but nothing proves a build tool laid it.
    let bare = tmp.path().join("bare");
    std::fs::create_dir_all(&bare).expect("dir");
    std::fs::write(bare.join(CARGO_LOCK_FILE), b"").expect("lock");
    let scanned = scan_target(&bare, now, DEFAULT_TARGET_STALE_DAYS).expect("a real directory");
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
        scan_target(&fresh, now_fresh, DEFAULT_TARGET_STALE_DAYS).expect("a real directory");
    assert!(scanned.live_build, "the proxy reads a fresh root as live");
    let mut survey = Survey::new(now_fresh);
    survey.targets.push(scanned);
    let rep = report(&survey, applying());
    assert!(rep.rows_of(Class::CargoTargets).is_empty(), "{rep:?}");
    assert!(
        rep.notes.iter().any(|n| n.contains("a build is live")),
        "{:?}",
        rep.notes
    );

    // NEGATIVE 3: a marker and old clocks, but the injected live-build fact
    // says a build is running. The fact wins over both clocks.
    let mut td = scan_target(
        &synthetic_target(&tmp, "busy"),
        now,
        DEFAULT_TARGET_STALE_DAYS,
    )
    .expect("a real directory");
    td.live_build = true;
    let mut survey = Survey::new(now);
    survey.targets.push(td);
    assert!(
        report(&survey, applying())
            .rows_of(Class::CargoTargets)
            .is_empty()
    );

    // NEGATIVE 4: no lock file to date it by.
    let mut td = scan_target(
        &synthetic_target(&tmp, "nolock"),
        now,
        DEFAULT_TARGET_STALE_DAYS,
    )
    .expect("a real directory");
    td.lock_mtime = None;
    let mut survey = Survey::new(now);
    survey.targets.push(td);
    let rep = report(&survey, applying());
    assert!(rep.rows_of(Class::CargoTargets).is_empty());
    assert!(rep.notes.iter().any(|n| n.contains(CARGO_LOCK_FILE)));
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
    let done = apply(&rep, None, Some(Class::ClaudeStaleVersions), &mut |_| {
        calls += 1;
        Ok(())
    });
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
    let done = apply(&rep, None, Some(Class::AtpkgGc), &mut |_| {
        calls += 1;
        Ok(())
    });
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
        .push(scan_target(&dir, now, DEFAULT_TARGET_STALE_DAYS).expect("a real directory"));

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
    let done = apply(&rep, None, None, &mut |_| {
        calls += 1;
        Ok(())
    });
    assert_eq!(calls, 0, "the remover is never even called");
    assert!(done.removed.is_empty());
    assert_eq!(done.denials.len(), 1);
    assert_eq!(done.denials[0].code(), "no-class");
    assert!(dir.exists(), "the directory is untouched");

    // A class named, but `disk.apply` off: still nothing.
    let done = apply(&rep, None, Some(Class::CargoTargets), &mut |_| {
        calls += 1;
        Ok(())
    });
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
        &mut |_| {
            calls += 1;
            Ok(())
        },
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
    let done = apply(&rep, None, Some(Class::ClaudeStaleVersions), &mut |p| {
        if p.ends_with("2.1.259") {
            Err(std::io::Error::other("busy"))
        } else {
            Ok(())
        }
    });
    assert_eq!(done.removed.len(), 1, "the other row still went");
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
    survey.trigger = Trigger::UpdateDone;
    survey.versions.push(VersionDir {
        path: PathBuf::from("/v/2.1.259"),
        bytes: 7,
        bytes_partial: true,
    });
    survey.live_version = Some(PathBuf::from("/v/2.1.265"));
    let rep = report(&survey, Config::default());
    let json = rep.to_json();
    assert!(json.contains("\"kind\":\"disk\""), "{json}");
    assert!(json.contains("\"trigger\":\"update-done\""), "{json}");
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
    let survey = scan(&roots, NOW, Trigger::Tick, Config::default(), Some(GIB));
    assert_eq!(survey.versions.len(), 2);
    assert_eq!(survey.free_bytes, Some(GIB));
    assert_eq!(survey.trigger, Trigger::Tick);
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
