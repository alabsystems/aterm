// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-status-{label}-"))
}

#[test]
fn the_pass_end_stamps_say_what_the_pass_did() {
    use aterm_update_core::pkg_check::{PassOutcome, Stamps};
    let dir = crate::testdir::dir("atpkg-status-fresh-");
    let layout = Layout {
        prefix: dir.clone(),
    };
    stamp_success(&layout, "2026-09-15T10:00:00Z").unwrap();
    stamp_pass_end(&layout, "2026-09-15T10:00:00Z", PassOutcome::Ok).unwrap();
    stamp_index_build(&layout, "2026-09-15T10:00:00Z", Some(32)).unwrap();
    let s = read(&layout).unwrap();
    assert_eq!(s.last_success_at, "2026-09-15T10:00:00Z");
    assert_eq!(s.last_pass, "ok");
    assert_eq!(s.last_index_build, 32);
    assert_eq!(s.index_build_changed_at, "2026-09-15T10:00:00Z");
    assert!(!Stamps::read(&layout.status()).last_failed());
    stamp_pass_end(&layout, "2026-09-16T10:00:00Z", PassOutcome::Failed).unwrap();
    stamp_index_build(&layout, "2026-09-16T10:00:00Z", Some(32)).unwrap();
    let s = read(&layout).unwrap();
    assert_eq!(s.last_success_at, "2026-09-15T10:00:00Z");
    assert_eq!(s.last_pass, "failed");
    assert_eq!(s.last_pass_at, "2026-09-16T10:00:00Z");
    assert_eq!(s.index_build_changed_at, "2026-09-15T10:00:00Z");
    assert_eq!(s.updated_at, "2026-09-16T10:00:00Z");
    assert!(Stamps::read(&layout.status()).last_failed());
    stamp_success(&layout, "2026-09-17T10:00:00Z").unwrap();
    stamp_pass_end(&layout, "2026-09-17T10:00:00Z", PassOutcome::Ok).unwrap();
    stamp_index_build(&layout, "2026-09-17T10:00:00Z", Some(33)).unwrap();
    let s = read(&layout).unwrap();
    assert_eq!(s.last_success_at, "2026-09-17T10:00:00Z");
    assert_eq!(s.last_pass, "ok");
    assert!(
        !Stamps::read(&layout.status()).last_failed(),
        "the success heals"
    );
    assert_eq!(s.last_index_build, 33);
    assert_eq!(s.index_build_changed_at, "2026-09-17T10:00:00Z");
    stamp_index_build(&layout, "2026-09-18T10:00:00Z", None).unwrap();
    let s = read(&layout).unwrap();
    assert_eq!(s.last_index_build, 33);
    assert_eq!(s.index_build_changed_at, "2026-09-17T10:00:00Z");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pass_end_binds_its_index_target_and_clears_unknown_successors() {
    use aterm_update_core::pkg_check::{PassOutcome, Stamps};
    let l = layout("pass-target");
    stamp_index_build(&l, "2026-09-24T12:00:00Z", Some(44)).unwrap();
    stamp_pass_end_with_index(&l, "2026-09-24T12:01:00Z", PassOutcome::Ok, 45).unwrap();
    let status = read(&l).unwrap();
    assert_eq!(status.last_index_build, 44);
    assert_eq!(status.last_pass_attempted_index_build, 45);
    assert_eq!(status.last_pass_at, status.last_pass_attempted_at);
    assert!(status.last_pass_at.ends_with(".000000000Z"));
    let stamps = Stamps::read(&l.status());
    assert_eq!(stamps.last_pass_target.map(|(_, build)| build), Some(45));
    assert!(!stamps.completed_older_index_pass(45));
    assert!(stamps.completed_older_index_pass(46));

    let mut old_writer = status;
    old_writer.last_pass = "failed".to_string();
    old_writer.last_pass_at = "2026-09-24T12:01:00Z".to_string();
    write(&l, &old_writer).unwrap();
    let stale = Stamps::read(&l.status());
    assert_eq!(stale.last_pass_target, None);
    assert!(!stale.completed_older_index_pass(46));

    stamp_pass_end(&l, "2026-09-24T12:02:00Z", PassOutcome::Failed).unwrap();
    let next = Stamps::read(&l.status());
    assert_eq!(read(&l).unwrap().last_pass_attempted_index_build, 0);
    assert_eq!(
        next.last_pass_target, None,
        "the old target is not borrowed"
    );
    assert!(!next.completed_older_index_pass(46));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A failed write reclaims its `status.toml.tmp-<pid>`, which nothing else sweeps.
#[cfg(unix)]
#[test]
fn failed_write_reclaims_its_temp() {
    let l = layout("failed-write");
    std::fs::create_dir(l.status()).unwrap();
    let error = write(
        &l,
        &Status {
            schema: 1,
            ..Default::default()
        },
    )
    .expect_err("a rename onto a directory cannot succeed");
    assert_eq!(
        error.raw_os_error(),
        Some(21),
        "the staged failure must be the rename's, not an earlier one: {error}"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&l.prefix)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .map(|e| e.file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "a failed status write must reclaim its temp, not strand it: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn read_absent_or_corrupt_is_none() {
    let l = layout("absent");
    assert!(read(&l).is_none(), "absent status reads as None");
    assert_eq!(read_checked(&l).unwrap(), None);
    std::fs::write(l.status(), "this is not valid toml {{{").unwrap();
    assert!(read(&l).is_none(), "corrupt status is never load-bearing");
    let error = read_checked(&l).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("status.toml is invalid"));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn rewriting_writers_heal_an_unreadable_record() {
    let l = layout("unreadable");
    assert_eq!(
        seed_for_rewrite(&l).unwrap().schema,
        1,
        "a virgin store seeds a fresh record: absent is not unreadable"
    );
    let aside = |l: &Layout| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&l.prefix)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("status.toml.corrupt-"))
            .collect();
        v.sort();
        v
    };
    assert!(aside(&l).is_empty());
    let whole = "schema = 1\nupdated_at = \"2026-09-16T00:00:00Z\"\nenabled = true\n\
                 index_source = \"o/r\"\noutcome = \"up to date\"\n";
    for (label, bytes) in [
        (
            "garbage",
            "schema = 1\nseams = [\"rustup:trust\"]\nthis is not valid toml {{{\n",
        ),
        ("truncated", &whole[..whole.len() / 2]),
        ("empty", ""),
    ] {
        std::fs::write(l.status(), bytes).unwrap();
        assert!(read_checked(&l).is_err(), "{label}: unreadable as written");
        stamp_success(&l, "2026-09-16T00:00:00Z").expect("the stamp heals and lands");
        assert!(!never_checked(&l), "{label}: the stamp landed");
        let kept = aside(&l);
        let newest = kept.last().expect("kept aside");
        assert_eq!(
            std::fs::read_to_string(l.prefix.join(newest)).unwrap(),
            bytes,
            "{label}: the unreadable bytes are kept, byte for byte"
        );
        for name in kept {
            std::fs::remove_file(l.prefix.join(name)).unwrap();
        }
    }
    stamp_index_build(&l, "2026-09-16T00:00:00Z", Some(7)).unwrap();
    assert_eq!(read(&l).unwrap().last_index_build, 7);
    std::fs::write(l.status(), "garbage {{{").unwrap();
    {
        let _held = hold(&l);
        stamp_success(&l, "2026-09-17T00:00:00Z").unwrap();
        let on_disk = read_disk(&l).expect("a rebuilt record is on disk");
        assert_eq!(on_disk.map(|s| s.schema), Some(1));
    }
    assert_eq!(read(&l).unwrap().last_success_at, "2026-09-17T00:00:00Z");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A read that failed (a permission, an I/O error) is not corruption: the healthy record
/// is left where it is, never moved aside or replaced.
#[cfg(unix)]
#[test]
fn a_record_that_could_not_be_read_is_left_alone() {
    let l = layout("unreadable-perm");
    stamp_success(&l, "2026-09-16T00:00:00Z").unwrap();
    std::fs::set_permissions(l.status(), std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(l.status()).is_ok() {
        let _ = std::fs::remove_dir_all(&l.prefix);
        aterm_uds::host::could_not_run(
            "a mode-0 file read anyway: root, or a file system that ignores modes",
        );
    }
    let error = stamp_success(&l, "2026-09-17T00:00:00Z").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
    std::fs::set_permissions(l.status(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(read(&l).unwrap().last_success_at, "2026-09-16T00:00:00Z");
    let aside = std::fs::read_dir(&l.prefix)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
        .count();
    assert_eq!(aside, 0, "nothing kept aside");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_unknown_field_survives_a_rewrite() {
    let l = layout("unknown-field");
    let newer = "schema = 1\nupdated_at = \"2026-09-16T00:00:00Z\"\nenabled = true\n\
                 index_source = \"o/r\"\noutcome = \"up to date\"\n\
                 a_future_field = \"kept\"\na_future_count = 7\n\n\
                 [a_future_table]\nnested = true\n\n\
                 [programs.ay]\ninstalled_build = 18\nstate = \"active\"\n";
    std::fs::write(l.status(), newer).unwrap();
    stamp_success(&l, "2026-09-17T00:00:00Z").unwrap();
    let back = read(&l).unwrap();
    assert_eq!(back.last_success_at, "2026-09-17T00:00:00Z");
    assert_eq!(
        back.extra
            .get("a_future_field")
            .and_then(aterm_toml::Value::as_str),
        Some("kept")
    );
    assert_eq!(
        back.extra
            .get("a_future_count")
            .and_then(aterm_toml::Value::as_integer),
        Some(7)
    );
    assert!(
        back.extra.contains_key("a_future_table"),
        "{:?}",
        back.extra
    );
    assert!(
        !back.extra.contains_key("programs"),
        "known keys are never extra"
    );
    assert_eq!(back.programs["ay"].installed_build, Some(18));
    let text = std::fs::read_to_string(l.status()).unwrap();
    assert!(text.contains("a_future_field = \"kept\""), "{text}");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// While a store is held the stamps land in memory, where readers in this process see
/// them; the file is written once when the hold ends, and not at all if nothing changed.
#[cfg(unix)]
#[test]
fn a_held_record_is_written_once_when_the_hold_ends() {
    use std::os::unix::fs::MetadataExt as _;
    let l = layout("held");
    stamp_success(&l, "2026-09-16T00:00:00Z").unwrap();
    let ino = std::fs::metadata(l.status()).unwrap().ino();
    {
        let _held = hold(&l);
        let _inner = hold(&l);
        stamp_success(&l, "2026-09-17T00:00:00Z").unwrap();
        stamp_index_build(&l, "2026-09-17T00:00:00Z", Some(9)).unwrap();
        let mut s = seed_for_rewrite(&l).unwrap();
        s.outcome = "up to date (index build 9)".into();
        write(&l, &s).unwrap();
        assert_eq!(
            read(&l).unwrap().last_index_build,
            9,
            "readers see the held record"
        );
        assert_eq!(
            std::fs::metadata(l.status()).unwrap().ino(),
            ino,
            "nothing on disk yet"
        );
    }
    let back = read(&l).unwrap();
    assert_eq!(back.last_success_at, "2026-09-17T00:00:00Z");
    assert_eq!(back.last_index_build, 9);
    assert_eq!(back.outcome, "up to date (index build 9)");
    let written = std::fs::metadata(l.status()).unwrap().ino();
    assert_ne!(written, ino, "written once, at the end");
    {
        let _held = hold(&l);
        let _ = read(&l);
    }
    assert_eq!(
        std::fs::metadata(l.status()).unwrap().ino(),
        written,
        "a hold nothing wrote into writes nothing"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A row change is written through even while held: a pass killed after activating a
/// build must leave its row and signed root on disk.
#[cfg(unix)]
#[test]
fn a_held_row_change_reaches_the_disk_at_once() {
    use std::os::unix::fs::MetadataExt as _;
    let l = layout("held-row");
    stamp_success(&l, "2026-09-16T00:00:00Z").unwrap();
    let held = hold(&l);
    let mut s = seed_for_rewrite(&l).unwrap();
    s.programs.insert(
        "ay".into(),
        ProgramStatus {
            installed_build: Some(19),
            state: "managed 19 — pinned by index 44".into(),
            tree_root: "r19".into(),
        },
    );
    s.outcome = "installed ay build 19".into();
    write(&l, &s).unwrap();
    let on_disk = read_disk(&l).unwrap().unwrap();
    assert_eq!(
        on_disk.programs["ay"].tree_root, "r19",
        "the row is on disk"
    );
    let ino = std::fs::metadata(l.status()).unwrap().ino();
    stamp_success(&l, "2026-09-17T00:00:00Z").unwrap();
    assert_eq!(
        std::fs::metadata(l.status()).unwrap().ino(),
        ino,
        "a stamp after it waits"
    );
    std::mem::forget(held);
    let on_disk = read_disk(&l).unwrap().unwrap();
    assert_eq!(on_disk.programs["ay"].installed_build, Some(19));
    assert_eq!(on_disk.last_success_at, "2026-09-16T00:00:00Z");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn status_program_count_is_bounded_on_read_and_write() {
    let l = layout("program-cap");
    let mut programs = BTreeMap::new();
    for index in 0..=MAX_STATUS_PROGRAMS {
        programs.insert(format!("p{index}"), ProgramStatus::default());
    }
    let status = Status {
        schema: 1,
        programs,
        ..Default::default()
    };
    let write_error = write(&l, &status).unwrap_err();
    assert_eq!(write_error.kind(), io::ErrorKind::InvalidData);
    assert!(write_error.to_string().contains("limit is"));

    std::fs::write(l.status(), status.to_toml().unwrap()).unwrap();
    let read_error = read_checked(&l).unwrap_err();
    assert_eq!(read_error.kind(), io::ErrorKind::InvalidData);
    assert!(read_error.to_string().contains("limit is"));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn seams_round_trip_and_default_empty() {
    let l = layout("seams");
    let mut programs = BTreeMap::new();
    programs.insert(
        "trust".to_string(),
        ProgramStatus {
            installed_build: Some(6808),
            state: "managed 6808 — pinned by index 9".into(),
            tree_root: "r".into(),
        },
    );
    programs.insert("ay".to_string(), ProgramStatus::default());
    let s = Status {
        schema: 1,
        updated_at: "2026-08-29T00:00:00Z".into(),
        enabled: true,
        index_source: "o/r".into(),
        outcome: "up to date".into(),
        seams: vec!["rustup:trust".into()],
        last_success_at: String::new(),
        last_index_build: 0,
        index_build_changed_at: String::new(),
        last_pass: String::new(),
        last_pass_at: String::new(),
        last_pass_attempted_index_build: 0,
        last_pass_attempted_at: String::new(),
        pass_seq: 3,
        stale_index_only_pass_seq: 0,
        programs,
        refused_shims: BTreeMap::new(),
        extra: Default::default(),
    };
    write(&l, &s).unwrap();
    let back = read(&l).expect("status reads back");
    assert_eq!(back, s);
    assert_eq!(back.seams, vec!["rustup:trust".to_string()]);
    let text = std::fs::read_to_string(l.status()).unwrap();
    let seams_at = text.find("seams = [").expect("seams array on disk");
    let table_at = text.find("[programs.").expect("program tables on disk");
    assert!(
        seams_at < table_at,
        "seams must precede the program tables:\n{text}"
    );

    let legacy = "schema = 1\nupdated_at = \"2026-08-01T00:00:00Z\"\nenabled = true\n\
         index_source = \"o/r\"\noutcome = \"up to date\"\n\n[programs.trust]\n\
         installed_build = 6808\nstate = \"managed 6808 — pinned by index 9\"\n\
         tree_root = \"r\"\n";
    std::fs::write(l.status(), legacy).unwrap();
    let back = read_checked(&l).unwrap().expect("legacy record parses");
    assert!(back.seams.is_empty(), "no key ⇒ no seams");
    assert_eq!(back.programs["trust"].installed_build, Some(6808));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn refused_shims_are_recorded_per_build_bounded_and_dropped_with_the_build() {
    let l = layout("refused-shims");
    let names = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    std::fs::create_dir_all(l.build_dir("ay", 18)).unwrap();
    std::fs::create_dir_all(l.build_dir("ay", 19)).unwrap();
    std::fs::create_dir_all(l.build_dir("ny", 4)).unwrap();

    record_refused_shims(&l, "ay", 18, &[]).unwrap();
    assert!(
        !l.status().exists(),
        "a landing that refused nothing writes nothing"
    );

    record_refused_shims(&l, "ay", 18, &names(&["pwsh", "git"])).unwrap();
    let long = "x".repeat(MAX_REFUSED_NAME_CHARS + 9);
    let many: Vec<String> = std::iter::once(long.clone())
        .chain((0..MAX_REFUSED_RECORDED + 3).map(|i| format!("a/{i}")))
        .collect();
    record_refused_shims(&l, "ny", 4, &many).unwrap();
    record_refused_shims(&l, "ay", 19, &names(&["zsh"])).unwrap();
    let back = read_checked(&l).unwrap().expect("recorded");
    assert_eq!(
        back.refused_for("ay", 18),
        Some(&RefusedShims {
            build: 18,
            names: names(&["pwsh", "git"]),
            count: 2,
        })
    );
    assert_eq!(
        back.refused_for("ay", 19).map(|r| r.names.clone()),
        Some(names(&["zsh"]))
    );
    assert_eq!(back.refused_for("ay", 17), None);
    let ny = back.refused_for("ny", 4).expect("ny's entry");
    assert_eq!(ny.names.len(), MAX_REFUSED_RECORDED);
    assert_eq!(ny.count, u64::try_from(many.len()).unwrap());
    assert_eq!(ny.names[0].chars().count(), MAX_REFUSED_NAME_CHARS);
    assert!(ny.names[0].ends_with('\u{2026}') && long.starts_with(&ny.names[0][..60]));

    #[cfg(unix)]
    {
        let writes = counted::writes(&l);
        record_refused_shims(&l, "ay", 18, &names(&["pwsh", "git"])).unwrap();
        assert_eq!(
            counted::writes(&l),
            writes,
            "an unchanged entry is not rewritten"
        );
    }

    for build in 20..20 + u64::try_from(MAX_REFUSED_BUILDS).unwrap() {
        std::fs::create_dir_all(l.build_dir("ay", build)).unwrap();
        record_refused_shims(&l, "ay", build, &names(&["fish"])).unwrap();
    }
    let back = read_checked(&l).unwrap().expect("recorded");
    let builds: Vec<u64> = back.refused_shims["ay"].iter().map(|r| r.build).collect();
    assert_eq!(
        builds,
        (20..20 + u64::try_from(MAX_REFUSED_BUILDS).unwrap()).collect::<Vec<_>>()
    );

    std::fs::remove_dir_all(l.build_dir("ny", 4)).unwrap();
    for build in 20..20 + u64::try_from(MAX_REFUSED_BUILDS).unwrap() {
        record_refused_shims(&l, "ay", build, &[]).unwrap();
    }
    let back = read_checked(&l).unwrap().expect("recorded");
    assert!(back.refused_shims.is_empty(), "{:?}", back.refused_shims);
    assert!(
        !std::fs::read_to_string(l.status())
            .unwrap()
            .contains("refused_shims"),
        "an empty map is not written"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_record_over_the_refused_shims_bounds_is_neither_written_nor_read() {
    let l = layout("refused-bounds");
    let entry = |build: u64| RefusedShims {
        build,
        names: vec!["pwsh".to_string()],
        count: 1,
    };
    let at_bounds = Status {
        schema: 1,
        refused_shims: (0..MAX_STATUS_PROGRAMS)
            .map(|i| (format!("p{i}"), vec![entry(1)]))
            .chain(std::iter::once((
                "p0".to_string(),
                (0..u64::try_from(MAX_REFUSED_BUILDS).unwrap())
                    .map(entry)
                    .collect(),
            )))
            .collect(),
        ..Default::default()
    };
    write(&l, &at_bounds).unwrap();
    assert_eq!(read_checked(&l).unwrap().as_ref(), Some(&at_bounds));
    let good = std::fs::read_to_string(l.status()).unwrap();

    let mut programs = at_bounds.clone();
    programs
        .refused_shims
        .insert("one-more".to_string(), vec![entry(1)]);
    let mut builds = at_bounds.clone();
    builds
        .refused_shims
        .get_mut("p1")
        .unwrap()
        .extend((2..=u64::try_from(MAX_REFUSED_BUILDS).unwrap() + 1).map(entry));
    for (what, over) in [("programs", &programs), ("builds", &builds)] {
        let err = write(&l, over).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{what}: {err}");
        assert_eq!(
            std::fs::read_to_string(l.status()).unwrap(),
            good,
            "{what}: a refused write leaves the record"
        );
        std::fs::write(l.status(), over.to_toml().unwrap()).unwrap();
        let err = read_checked(&l).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{what}: {err}");
        std::fs::write(l.status(), &good).unwrap();
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn never_checked_holds_until_a_successful_pass_stamps_last_success_at() {
    let l = layout("never-checked");
    assert!(never_checked(&l), "no status.toml at all");
    let mut programs = BTreeMap::new();
    programs.insert(
        "*index*".to_string(),
        ProgramStatus {
            installed_build: None,
            state: "error: index unreachable".into(),
            tree_root: String::new(),
        },
    );
    write(
        &l,
        &Status {
            schema: 1,
            updated_at: "2026-09-10T06:40:53Z".into(),
            outcome: "update failed: index unreachable".into(),
            programs,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        never_checked(&l),
        "a failed pass moves updated_at but is not a check that ran"
    );
    let legacy = "schema = 1\nupdated_at = \"2026-08-01T00:00:00Z\"\nenabled = true\n\
         index_source = \"o/r\"\noutcome = \"up to date\"\n";
    std::fs::write(l.status(), legacy).unwrap();
    assert!(never_checked(&l), "pre-field record: empty last_success_at");
    stamp_success(&l, "2026-09-10T07:00:00Z").unwrap();
    assert!(!never_checked(&l));
    let back = read(&l).unwrap();
    assert_eq!(back.last_success_at, "2026-09-10T07:00:00Z");
    assert_eq!(back.updated_at, "2026-09-10T07:00:00Z");
    assert_eq!(back.outcome, "up to date", "the sentence is not touched");
    let text = std::fs::read_to_string(l.status()).unwrap();
    assert!(text.contains("last_success_at = \"2026-09-10T07:00:00Z\""));
    std::fs::write(l.status(), "schema = 1\nlast_success_at = \"  \"\n").unwrap();
    assert!(never_checked(&l));
    std::fs::remove_file(l.status()).unwrap();
    stamp_success(&l, "2026-09-10T08:00:00Z").unwrap();
    assert!(!never_checked(&l));
    assert_eq!(read(&l).unwrap().schema, 1);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn the_full_pass_rule_conforms_to_its_derived_model() {
    use super::full_pass_rule_tier1::{legacy_stamps, looks, realize, records};
    use aterm_models_atpkg::atpkg_full_pass_rule_model;
    use aterm_update_core::pkg_check;
    let model = atpkg_full_pass_rule_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let fixed = records(&model);
    assert!(fixed.len() > 20, "{}", fixed.len());
    crate::testdir::each_side_by_side(&fixed, |i, state| {
        let now = crate::flow::now_unix();
        let (l, sink) = realize(&format!("tier1-{i}"), state, true, now);
        let stamps = pass_stamps(&l, now);
        let look = looks(&model, "SessionLook", state);
        assert_eq!(stamps.last_failed(), look["verdict"] == 1, "{state:?}");
        assert_eq!(stamps.in_flight, state["running"] >= 1, "{state:?}");
        assert_eq!(
            pkg_check::full_pass_owed(&stamps, now).is_some(),
            look["queued"] > 0,
            "{state:?} {stamps:?}"
        );
        drop(sink);
        let _ = std::fs::remove_dir_all(&l.prefix);
    });
    let old = records(&buggy);
    assert!(old.len() > 5, "{}", old.len());
    crate::testdir::each_side_by_side(&old, |i, state| {
        let now = crate::flow::now_unix();
        let (l, sink) = realize(&format!("tier1-old-{i}"), state, false, now);
        let legacy = legacy_stamps(&l, now);
        let look = looks(&buggy, "SessionLook", state);
        assert_eq!(legacy.last_failed(), look["verdict"] == 1, "{state:?}");
        assert_eq!(
            pkg_check::full_pass_owed(&legacy, now).is_some(),
            look["queued"] > 0,
            "{state:?} {legacy:?}"
        );
        let upgraded = pass_stamps(&l, now);
        assert!(!upgraded.last_failed(), "by its success alone: {state:?}");
        assert_eq!(
            pkg_check::full_pass_owed(&upgraded, now).is_some(),
            state["running"] == 0 && (state["ever_ok"] == 0 || state["ok_age"] >= 3),
            "{state:?} {upgraded:?}"
        );
        drop(sink);
        let _ = std::fs::remove_dir_all(&l.prefix);
    });
    let defect = super::full_pass_rule_tier1::walk(
        &model,
        &[
            "SessionLook",
            "TakeLockFresh",
            "PassSucceeds",
            "Tick",
            "Tick",
            "Tick",
            "OtherWrite",
        ],
    );
    let now = crate::flow::now_unix();
    let (l, _) = realize("tier1-defect", &defect, true, now);
    assert!(pkg_check::full_pass_owed(&pass_stamps(&l, now), now).is_some());
    assert!(looks(&model, "SessionLook", &defect)["queued"] > 0);
    let legacy = legacy_stamps(&l, now);
    assert!(
        legacy.last_failed(),
        "the old reader read the write as a failed pass"
    );
    assert!(
        pkg_check::full_pass_owed(&legacy, now).is_none(),
        "and held the pass back"
    );
    assert_eq!(looks(&buggy, "SessionLook", &defect)["queued"], 0);
    let _ = std::fs::remove_dir_all(&l.prefix);
    let failed = fixed
        .iter()
        .find(|s| s["rec"] == 2 && s["ever_ok"] == 1 && s["running"] == 0)
        .expect("a failed pass after a success is reachable");
    let (l, _) = realize("tier1-unrecorded", failed, false, now);
    assert!(
        !pass_stamps(&l, now).last_failed(),
        "unrecorded, the failure is lost"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}
