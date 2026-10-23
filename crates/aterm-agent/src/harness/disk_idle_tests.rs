// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier 3 on faithful cargo layouts.

use super::super::tests::{Tmp, lay_profile, lay_target, now_after_write};
use super::super::{
    CACHEDIR_SIGNATURE, Class, Config, Roots, Row, Witness, report, scan_with, target,
};
use super::*;
use crate::harness::disk::discover::{Found, Origin};
use aterm_uds::holders::{PathScan, PathUse};

const DAY: i64 = 86_400;

/// Set the mtime of `path` to `at` (unix seconds).
fn set_mtime(path: &Path, at: i64) {
    let when =
        std::time::UNIX_EPOCH + Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .expect("set mtime");
}

/// A checkout at `<tmp>/repo`: `.git/index`, `.git/logs/HEAD`, a manifest
/// and a source file, and its build directory `target/` (one profile,
/// `debug/`) with `doc/` and `criterion/` at its root.
fn checkout(tmp: &Tmp) -> (PathBuf, PathBuf) {
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git/logs")).expect("git");
    std::fs::write(repo.join(".git/index"), b"DIRC").expect("index");
    std::fs::write(repo.join(".git/logs/HEAD"), b"0 1 a\n").expect("head");
    std::fs::create_dir_all(repo.join("src")).expect("src");
    std::fs::write(repo.join("Cargo.toml"), b"[package]\n").expect("manifest");
    std::fs::write(repo.join("src/main.rs"), b"fn main() {}\n").expect("source");
    let target = lay_target(&repo.join("target"));
    std::fs::create_dir_all(target.join("doc/lay")).expect("doc");
    std::fs::write(target.join("doc/lay/index.html"), b"<html>").expect("doc");
    std::fs::create_dir_all(target.join("criterion/bench")).expect("criterion");
    std::fs::write(target.join("criterion/bench/base.json"), b"{}").expect("baseline");
    let canon = |p: PathBuf| std::fs::canonicalize(p).expect("canonical");
    (canon(repo), canon(target))
}

/// No process uses anything.
fn nobody(_: &[PathBuf], _: &[u32]) -> std::io::Result<PathScan> {
    Ok(PathScan::default())
}

/// Survey `target` (found under `repo`) for tier 3 at `now`, below the floor,
/// with `links` as the link folders, through `processes`.
fn survey(
    tmp: &Tmp,
    repo: &Path,
    target: &Path,
    now: i64,
    edit: impl FnOnce(&mut Roots),
    processes: super::super::discover::Processes<'_>,
) -> super::super::Report {
    let config = Config {
        idle_build_dirs: true,
        ..Config::default()
    };
    let mut roots = Roots {
        volume: Some(tmp.path().to_path_buf()),
        found: vec![Found {
            path: target.to_path_buf(),
            origin: Origin::Checkout {
                root: repo.to_path_buf(),
            },
        }],
        mounts: Some(aterm_sysprobe::volumes::table()),
        idle_whole: true,
        link_dirs: vec![tmp.path().join("bin")],
        ..Roots::default()
    };
    edit(&mut roots);
    let s = scan_with(
        &roots,
        now,
        super::super::Trigger::Tick,
        config,
        Some(1 << 30),
        processes,
    );
    report(&s, config)
}

fn whole(rep: &super::super::Report) -> Vec<&Row> {
    rep.rows
        .iter()
        .filter(|r| matches!(r.witness, Witness::IdleWhole { .. }))
        .collect()
}

fn kept_why(rep: &super::super::Report, dir: &Path) -> Option<String> {
    let prefix = format!("cargo-targets: {} — tier 3 keeps it whole: ", dir.display());
    rep.notes
        .iter()
        .find_map(|n| n.strip_prefix(&prefix).map(str::to_owned))
}

/// EVERY CLAUSE CLEAR, the build directory is a tier-3 row (`tier=3`, its
/// profiles' bytes); ONE CLAUSE UNMET at a time keeps it, the note saying
/// which: a git index written an hour ago, a working-tree file written today,
/// a session working in the checkout, a process with its working directory
/// there, a profile written two days ago, only `.rustc_info.json` for a
/// marker, a volume mounted inside it, and an installed link resolving into
/// its `debug/`.
#[test]
fn every_clause_clear_makes_a_whole_idle_row_and_each_unmet_one_keeps_it() {
    let now = now_after_write(8);
    let hour_ago = now - 3_600;

    let tmp = Tmp::new("idle-clear");
    let (repo, target) = checkout(&tmp);
    std::fs::create_dir_all(tmp.path().join("bin")).expect("bin");
    std::os::unix::fs::symlink("/usr/bin/true", tmp.path().join("bin/elsewhere")).expect("link");
    let rep = survey(&tmp, &repo, &target, now, |_| {}, &nobody);
    let rows = whole(&rep);
    assert_eq!(rows.len(), 1, "{:#?}", rep.notes);
    assert!(rows[0].line().contains("tier=3"), "{}", rows[0].line());
    assert!(rows[0].removable, "{:?}", rows[0].blocked);
    assert!(
        rows[0].bytes >= 8192,
        "the profile's contents: {}",
        rows[0].bytes
    );

    type Case = (&'static str, Box<dyn Fn(&Tmp, &Path, &Path)>);
    let cases: Vec<Case> = vec![
        (
            "git index",
            Box::new(move |_, repo, _| set_mtime(&repo.join(".git/index"), hour_ago)),
        ),
        (
            "working tree",
            Box::new(move |_, repo, _| set_mtime(&repo.join("src/main.rs"), hour_ago)),
        ),
        (
            "written into",
            Box::new(move |_, _, target| {
                lay_profile(&target.join("release"));
                for c in target::CLOCK_DIRS {
                    let d = target.join("release").join(c);
                    if d.exists() {
                        set_mtime(&d, now - 2 * DAY);
                    }
                }
            }),
        ),
        (
            "only a signed CACHEDIR.TAG",
            Box::new(|_, _, target| {
                std::fs::write(target.join("CACHEDIR.TAG"), b"not signed").expect("tag");
            }),
        ),
        (
            "installed link",
            Box::new(|tmp, _, target| {
                std::fs::create_dir_all(tmp.path().join("bin")).expect("bin");
                std::fs::write(target.join("debug/tool"), b"installed tool").expect("tool");
                std::os::unix::fs::symlink(target.join("debug/tool"), tmp.path().join("bin/tool"))
                    .expect("link");
            }),
        ),
        (
            "installed link",
            Box::new(|tmp, _, target| {
                std::fs::write(target.join("debug/tool"), b"installed tool").expect("tool");
                std::os::unix::fs::symlink(target.join("debug"), tmp.path().join("bin"))
                    .expect("installed directory alias");
            }),
        ),
        (
            "could not be resolved",
            Box::new(|tmp, _, _| {
                std::fs::create_dir_all(tmp.path().join("bin")).expect("bin");
                std::os::unix::fs::symlink(
                    "../repo/target/debug/missing",
                    tmp.path().join("bin/tool"),
                )
                .expect("unresolved relative installed link");
            }),
        ),
    ];
    for (word, make) in cases {
        let tmp = Tmp::new("idle-kept");
        let (repo, target) = checkout(&tmp);
        make(&tmp, &repo, &target);
        let rep = survey(&tmp, &repo, &target, now, |_| {}, &nobody);
        assert!(whole(&rep).is_empty(), "{word}: {:#?}", rep.rows);
        let why = kept_why(&rep, &target).unwrap_or_default();
        assert!(why.contains(word), "{word}: {why:?} {:#?}", rep.notes);
    }

    for remote in [true, false] {
        let tmp = Tmp::new("idle-unknown-mount");
        let (repo, target) = checkout(&tmp);
        std::fs::create_dir_all(tmp.path().join("bin")).expect("bin");
        std::os::unix::fs::symlink("../repo/target/debug/missing", tmp.path().join("bin/tool"))
            .expect("unresolved installed link");
        let rep = survey(
            &tmp,
            &repo,
            &target,
            now,
            |roots| {
                let table = roots.mounts.as_mut().expect("captured mount table");
                let on = aterm_sysprobe::volumes::mount_of(tmp.path(), table)
                    .expect("a mount holds the fixture")
                    .on
                    .clone();
                for mount in &mut table.mounts {
                    if mount.on == on {
                        if remote {
                            mount.local = false;
                        } else {
                            mount.automounted = true;
                        }
                    }
                }
            },
            &nobody,
        );
        assert!(
            whole(&rep).is_empty()
                && kept_why(&rep, &target).is_some_and(|why| why.contains("could not be resolved"))
                && target.join("debug/deps/liblay.rlib").exists(),
            "UNKNOWN_INBOUND_DESTINATION_MUST_NOT_ADMIT_WHOLE_REMOVAL: {:#?}",
            rep.notes
        );
    }

    let tmp = Tmp::new("idle-session");
    let (repo, target) = checkout(&tmp);
    let rep = survey(
        &tmp,
        &repo,
        &target,
        now,
        |r| r.session_cwds = vec![repo.join("src")],
        &nobody,
    );
    assert!(kept_why(&rep, &target).is_some_and(|w| w.contains("a session works")));
    let inside = repo.join("src");
    let busy = move |_: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        Ok(PathScan {
            users: vec![PathUse {
                pid: 4242,
                how: "cwd",
                path: inside.clone(),
            }],
            ..PathScan::default()
        })
    };
    let rep = survey(&tmp, &repo, &target, now, |_| {}, &busy);
    assert!(
        kept_why(&rep, &target).is_some_and(|w| w.contains("process 4242")),
        "{:#?}",
        rep.notes
    );

    let mut mounted = aterm_sysprobe::volumes::table();
    mounted.mounts.push(aterm_sysprobe::volumes::Mount {
        on: target.join("mnt"),
        from: "/dev/disk9s1".to_owned(),
        fstype: "apfs".to_owned(),
        local: true,
        automounted: false,
        removable: false,
        read_only: false,
        container: None,
    });
    let rep = survey(
        &tmp,
        &repo,
        &target,
        now,
        |r| r.mounts = Some(mounted),
        &nobody,
    );
    assert!(
        kept_why(&rep, &target).is_some_and(|w| w.contains("mounted inside it")),
        "{:#?}",
        rep.notes
    );
    let cut = aterm_sysprobe::volumes::Table {
        complete: false,
        ..aterm_sysprobe::volumes::table()
    };
    let rep = survey(&tmp, &repo, &target, now, |r| r.mounts = Some(cut), &nobody);
    assert!(kept_why(&rep, &target).is_some_and(|w| w.contains("not read whole")));

    let rep = survey(
        &tmp,
        &repo,
        &target,
        now,
        |r| {
            r.found[0].origin = Origin::Bootstrap {
                repo: repo.clone(),
                slim: false,
            }
        },
        &nobody,
    );
    assert!(whole(&rep).is_empty() && kept_why(&rep, &target).is_none());
}

/// Off by `disk.idle_build_dirs` the row is listed, report-only, saying why;
/// `idle_build_days` below one turns it off too.
#[test]
fn the_row_is_report_only_while_tier_3_is_off() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-off");
    let (repo, target) = checkout(&tmp);
    let roots = Roots {
        volume: Some(tmp.path().to_path_buf()),
        found: vec![Found {
            path: target.clone(),
            origin: Origin::Checkout { root: repo.clone() },
        }],
        mounts: Some(aterm_sysprobe::volumes::table()),
        idle_whole: true,
        ..Roots::default()
    };
    for config in [
        Config {
            idle_build_dirs: false,
            ..Config::default()
        },
        Config {
            idle_build_dirs: true,
            idle_build_days: 0,
            ..Config::default()
        },
    ] {
        let s = scan_with(
            &roots,
            now,
            super::super::Trigger::OnDemand,
            config,
            Some(1 << 30),
            &nobody,
        );
        let rep = report(&s, config);
        let rows = whole(&rep);
        assert_eq!(rows.len(), 1, "{config:?}: {:#?}", rep.notes);
        assert!(!rows[0].removable);
        assert!(
            rows[0]
                .blocked
                .as_deref()
                .is_some_and(|b| b.contains("idle_build_"))
        );
        assert!(rep.idle_whole_up_to() >= rows[0].bytes);
        assert_eq!(
            super::super::pass_plan(&rep).map(|p| p
                .iter()
                .any(|p| p.row.tier() == Some(super::super::Tier::IdleWhole))),
            Some(false)
        );
    }
    assert!(Config::default().idle_whole_on());
}

/// The working-tree walk is bounded: one that runs out of time keeps the
/// directory (a walk cut short proves nothing).
#[test]
fn a_working_tree_walk_cut_short_keeps_it() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-walk");
    let (repo, target) = checkout(&tmp);
    assert!(
        tree_quiet(
            &repo,
            std::slice::from_ref(&target),
            now,
            7 * DAY,
            TREE_WALL
        )
        .is_ok()
    );
    let cut = tree_quiet(&repo, &[target], now, 7 * DAY, Duration::ZERO);
    assert!(cut.is_err_and(|w| w.contains("ran out of time")));
}

/// THE REMOVAL EMPTIES EACH PROFILE AND KEEPS IT: afterwards `debug/` and its
/// three lock files are there and nothing else of it; `doc/`, `criterion/`,
/// `CACHEDIR.TAG` and `.rustc_info.json` are untouched.
#[test]
fn the_removal_empties_each_profile_under_its_locks_and_keeps_the_rest() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-reclaim");
    let (_, target) = checkout(&tmp);
    lay_profile(&target.join("release"));
    std::fs::write(target.join("release/lay"), vec![1u8; 4096]).expect("an uplifted binary");
    std::fs::write(
        target.join("release/deps/lay-0123456789abcdef"),
        vec![1u8; 4096],
    )
    .expect("its deps/ twin");
    let judge = target::Judge {
        now,
        threshold_days: 1,
        device: None,
        volume: None,
    };
    let seen = now;
    let mut held: Vec<target::Build> = Vec::new();
    let mut first: Option<PathBuf> = None;
    let profiles = [target.join("debug"), target.join("release")];
    let done = target::reclaim_profiles_with(
        &target,
        &judge,
        seen,
        7 * DAY,
        &mut || Ok(()),
        &mut || Ok(()),
        &mut |stage, path| {
            if stage == target::Stage::Released && first.is_none() {
                first = Some(path.to_path_buf());
                let other = profiles.iter().find(|p| p.as_path() != path).expect("two");
                held.push(target::Build::holds(&other.join(".cargo-lock")));
            }
        },
    )
    .expect("the first profile is emptied");
    drop(held);
    let first = first.expect("one profile went");
    let other = profiles.iter().find(|p| **p != first).expect("two").clone();
    assert_eq!(done.units, vec![first.clone()]);
    assert!(
        done.skipped
            .iter()
            .any(|(p, why)| p == &other && why.contains("a build holds")),
        "{done:?}"
    );
    let mut left: Vec<String> = std::fs::read_dir(&first)
        .expect("the profile kept")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    let mut locks: Vec<String> = target::CARGO_LOCKS
        .iter()
        .map(|l| (*l).to_owned())
        .collect();
    locks.sort();
    assert_eq!(left, locks, "the profile directory and its lock files stay");
    assert!(
        other.join("deps/liblay.rlib").exists(),
        "the held profile is kept whole"
    );
    assert!(target.join("doc/lay/index.html").exists());
    assert!(target.join("criterion/bench/base.json").exists());
    assert!(
        std::fs::read_to_string(target.join("CACHEDIR.TAG"))
            .expect("tag")
            .starts_with(CACHEDIR_SIGNATURE)
    );
    assert!(done.bytes >= 8192, "{done:?}");

    let done =
        target::reclaim_profiles(&target, &judge, seen, 7 * DAY, &mut || Ok(()), &mut || {
            Ok(())
        })
        .expect("emptied");
    assert!(done.units.contains(&other), "{done:?}");
    assert!(!target.join("release/lay").exists() && target.join("release/.cargo-lock").exists());
    let _ = Class::CargoTargets;
}

/// A profile written since the survey, or a mount found under the locks,
/// refuses the removal before anything is deleted; a path through a link is
/// refused whole.
#[test]
fn the_removal_reads_the_clock_and_the_mount_table_again_under_the_locks() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-recheck");
    let (_, target) = checkout(&tmp);
    let judge = target::Judge {
        now,
        threshold_days: 1,
        device: None,
        volume: None,
    };
    let refused = target::reclaim_profiles(
        &target,
        &judge,
        now - 9 * DAY,
        7 * DAY,
        &mut || Ok(()),
        &mut || Ok(()),
    )
    .expect_err("written since the survey");
    assert!(
        refused.to_string().contains("since the survey"),
        "{refused}"
    );
    let refused = target::reclaim_profiles(
        &target,
        &judge,
        now,
        7 * DAY,
        &mut || Err("a volume is mounted inside it".to_owned()),
        &mut || Ok(()),
    )
    .expect_err("a mount");
    assert!(refused.to_string().contains("mounted"), "{refused}");
    assert!(target.join("debug/deps/liblay.rlib").exists());
    let link = tmp.path().join("linked");
    std::os::unix::fs::symlink(tmp.path().join("repo"), &link).expect("link");
    let through = target::reclaim_profiles(
        &link.join("target"),
        &judge,
        now,
        7 * DAY,
        &mut || Ok(()),
        &mut || Ok(()),
    );
    assert!(through.is_err());
    assert!(target.join("debug/deps/liblay.rlib").exists());
}

/// A build directory a search from `$HOME` found (`~/repo/target`, its
/// discovery root `~`) serves `~/repo`, the git worktree above it — not `~`:
/// a session working in `~` itself does not keep it, one in `~/repo` does.
#[test]
fn a_build_directory_found_from_home_serves_the_checkout_above_it() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-home");
    let (repo, target) = checkout(&tmp);
    let home = std::fs::canonicalize(tmp.path()).expect("home");
    assert_eq!(
        checkout_of(&Origin::Checkout { root: home.clone() }, &target),
        Some(repo.clone())
    );
    let from_home = |r: &mut Roots| {
        r.found[0].origin = Origin::Checkout { root: home.clone() };
        r.home = Some(home.clone());
    };
    let rep = survey(
        &tmp,
        &repo,
        &target,
        now,
        |r| {
            from_home(r);
            r.session_cwds = vec![home.clone()];
        },
        &nobody,
    );
    assert_eq!(whole(&rep).len(), 1, "{:#?}", rep.notes);
    let rep = survey(
        &tmp,
        &repo,
        &target,
        now,
        |r| {
            from_home(r);
            r.session_cwds = vec![repo.clone()];
        },
        &nobody,
    );
    assert!(kept_why(&rep, &target).is_some_and(|w| w.contains("a session works")));
    std::fs::remove_dir_all(repo.join(".git")).expect("no git");
    let rep = survey(&tmp, &repo, &target, now, from_home, &nobody);
    assert!(
        kept_why(&rep, &target).is_some_and(|w| w.contains("no checkout")),
        "{:#?}",
        rep.notes
    );
}

/// TIER 3 EMPTIES ONLY CARGO'S OWN OUTPUTS of a profile: its caches,
/// `examples/`, its dep-info files and its uplifted artifacts (a byte-identical
/// copy of its hashed `deps/` twin, a hard link to it, a `.dSYM` link into
/// `deps/`).
#[test]
fn the_removal_takes_only_cargos_outputs_and_keeps_the_rest_of_a_profile() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-allow");
    let (_, target) = checkout(&tmp);
    let p = target.join("debug");
    let w = |rel: &str, bytes: usize| {
        let at = p.join(rel);
        std::fs::create_dir_all(at.parent().expect("folder")).expect("folder");
        std::fs::write(&at, vec![3u8; bytes]).expect("write");
    };
    w("deps/lay-0123456789abcdef", 4096);
    w("lay", 4096);
    w("lay.d", 64);
    w("deps/liblay-0123456789abcdef.rlib", 2048);
    w("liblay.rlib", 2048);
    w("deps/hl-0123456789abcdef", 100);
    std::fs::hard_link(p.join("deps/hl-0123456789abcdef"), p.join("hl")).expect("hard link");
    std::fs::create_dir_all(p.join("deps/lay-0123456789abcdef.dSYM")).expect("dsym");
    std::os::unix::fs::symlink("deps/lay-0123456789abcdef.dSYM", p.join("lay.dSYM"))
        .expect("dsym link");
    w("examples/demo", 16);
    w("lay.db", 4096);
    w("bundle/osx/Lay.app/Contents/Info.plist", 32);
    w("notes.txt", 10);
    w("deps/other-0123456789abcdef", 10);
    w("other", 11);
    w("deps/custom-0123456789abcdef", 4096);
    let custom_bytes = vec![4u8; 4096];
    std::fs::write(p.join("custom"), &custom_bytes).expect("custom bytes");
    let judge = target::Judge {
        now,
        threshold_days: 1,
        device: None,
        volume: None,
    };
    let done =
        target::reclaim_profiles(
            &target,
            &judge,
            now,
            7 * DAY,
            &mut || Ok(()),
            &mut || Ok(()),
        )
        .expect("emptied");
    for gone in [
        "deps",
        ".fingerprint",
        "build",
        "incremental",
        "examples",
        "lay",
        "lay.d",
        "liblay.rlib",
        "hl",
        "lay.dSYM",
    ] {
        assert!(
            std::fs::symlink_metadata(p.join(gone)).is_err(),
            "{gone} is cargo's and goes"
        );
    }
    for kept in [
        "lay.db",
        "bundle/osx/Lay.app/Contents/Info.plist",
        "notes.txt",
        "other",
    ] {
        assert!(p.join(kept).exists(), "{kept} is not cargo's and stays");
    }
    for l in target::CARGO_LOCKS {
        assert!(p.join(l).exists(), "{l}");
    }
    for kept in ["lay.db", "bundle", "notes.txt", "other"] {
        assert!(
            done.skipped
                .iter()
                .any(|(at, why)| at == &p.join(kept) && why.contains("not one of cargo's outputs")),
            "{kept} said: {:?}",
            done.skipped
        );
    }
    assert!(done.units.contains(&p), "{done:?}");
    assert_eq!(
        std::fs::read(p.join("custom")).ok(),
        Some(custom_bytes),
        "SAME_SIZE_DIFFERENT_BYTES_MUST_KEEP_CUSTOM_UPLIFT"
    );
    assert!(
        done.skipped.iter().any(|(at, _)| at == &p.join("custom")),
        "the kept custom uplift is reported"
    );
}

/// The uplift rule, pure: `<stem><ext>` and its hashed twin
/// `<stem>-<hash><ext>`, `-` read as `_`.
#[test]
fn an_uplift_is_named_by_its_hashed_twin() {
    use super::super::target::uplift_twin;
    assert!(uplift_twin("lay", "lay-0123456789abcdef"));
    assert!(uplift_twin("my-tool", "my_tool-0123456789abcdef"));
    assert!(uplift_twin("liblay.rlib", "liblay-0123456789abcdef.rlib"));
    assert!(uplift_twin("lay.dSYM", "lay-0123456789abcdef.dSYM"));
    assert!(!uplift_twin("lay.db", "lay-0123456789abcdef"));
    assert!(!uplift_twin("lay", "lay-0123456789abcdef.d"));
    assert!(!uplift_twin("lay", "lay-123"));
    assert!(!uplift_twin("lay", "lay-0123456789abcdeg"));
    assert!(!uplift_twin("other", "lay-0123456789abcdef"));
    assert!(!uplift_twin(".hidden", "-0123456789abcdef.hidden"));
}

/// Clause 4 is read again UNDER EACH PROFILE'S LOCKS, the first included,
/// right before its delete: a `between` that refuses (a `cargo test` started
/// from an idle profile after the survey, cargo having given its lock up)
/// keeps every profile, nothing deleted, and it is asked only once the locks
/// are held.
#[test]
fn the_process_check_runs_under_the_locks_before_the_first_delete() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-under");
    let (_, target) = checkout(&tmp);
    let judge = target::Judge {
        now,
        threshold_days: 1,
        device: None,
        volume: None,
    };
    let order = std::cell::RefCell::new(Vec::new());
    let refused = target::reclaim_profiles_with(
        &target,
        &judge,
        now,
        7 * DAY,
        &mut || Ok(()),
        &mut || {
            order.borrow_mut().push("between");
            Err("process 4242 uses deps/lay-test (its executable)".to_owned())
        },
        &mut |stage, _| {
            if stage == target::Stage::Locked {
                order.borrow_mut().push("locked");
            }
            if stage == target::Stage::Deleting {
                order.borrow_mut().push("deleting");
            }
        },
    )
    .expect_err("a process in it keeps it");
    assert!(refused.to_string().contains("process 4242"), "{refused}");
    assert_eq!(*order.borrow(), ["locked", "between"]);
    assert!(target.join("debug/deps/liblay.rlib").exists());
    let done =
        target::reclaim_profiles(
            &target,
            &judge,
            now,
            7 * DAY,
            &mut || Ok(()),
            &mut || Ok(()),
        )
        .expect("emptied");
    assert!(done.units.contains(&target.join("debug")));
}

/// THE OWNER'S VERB NEVER TAKES A TIER-3 ROW, whatever `disk.apply` says: its
/// plan for `cargo-targets` lists the whole idle build directory as reported,
/// never removed.
#[test]
fn the_verb_reports_tier_3_and_never_takes_it() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-verb");
    let (repo, target) = checkout(&tmp);
    let mut rep = survey(&tmp, &repo, &target, now, |_| {}, &nobody);
    assert_eq!(whole(&rep).len(), 1, "{:#?}", rep.notes);
    rep.config.apply = true;
    match super::super::plan(&rep, Some(Class::CargoTargets)) {
        super::super::Plan::Remove(_, rows) => assert!(
            rows.iter()
                .all(|r| r.tier() != Some(super::super::Tier::IdleWhole)),
            "{rows:#?}"
        ),
        other => panic!("{other:?}"),
    }
    rep.demand = Some(1 << 30);
    assert!(super::super::pass_plan(&rep).is_some_and(|p| {
        p.iter()
            .any(|p| p.row.tier() == Some(super::super::Tier::IdleWhole))
    }));
}

/// CLAUSE 1's CHECKOUT is the build directory's own: a `target` reached
/// through a link from `~/a-wt` into `~/z-main/target.noindex` is not judged
/// by `~/a-wt`'s quiet (refused, said); an agent's `CARGO_TARGET_DIR` is never
/// tier 3's; a build directory with no git checkout above it is refused.
#[test]
fn tier_3_judges_the_checkout_the_build_directory_lies_in() {
    let now = now_after_write(8);
    let tmp = Tmp::new("idle-linked");
    let (repo, target) = checkout(&tmp);
    let wt = tmp.path().join("a-wt");
    std::fs::create_dir_all(wt.join(".git")).expect("a-wt");
    let wt = std::fs::canonicalize(wt).expect("canonical");
    let rep = survey(
        &tmp,
        &repo,
        &target,
        now,
        |r| r.found[0].origin = Origin::Checkout { root: wt.clone() },
        &nobody,
    );
    assert!(whole(&rep).is_empty());
    assert!(
        kept_why(&rep, &target).is_some_and(|w| w.contains("not the one it was found from")),
        "{:#?}",
        rep.notes
    );
    let rep = survey(&tmp, &repo, &target, now, |_| {}, &nobody);
    assert_eq!(whole(&rep).len(), 1, "{:#?}", rep.notes);
    let rep = survey(
        &tmp,
        &repo,
        &target,
        now,
        |r| r.found[0].origin = Origin::AgentTargetDir,
        &nobody,
    );
    assert!(whole(&rep).is_empty() && kept_why(&rep, &target).is_none());
    std::fs::remove_dir_all(repo.join(".git")).expect("no git");
    let rep = survey(&tmp, &repo, &target, now, |_| {}, &nobody);
    assert!(
        kept_why(&rep, &target).is_some_and(|w| w.contains("no git checkout")),
        "{:#?}",
        rep.notes
    );
}

/// Links are resolved ONE COMPONENT AT A TIME, and only on local,
/// non-automounted mounts of a table read whole: a link chain resolves; the
/// same chain where the table calls its volume remote (a stalled share) or
/// automounted is never followed (`None`), nor is anything on a table read
/// short.
#[test]
fn links_are_resolved_only_on_local_mounts() {
    let tmp = Tmp::new("idle-resolve");
    let base = std::fs::canonicalize(tmp.path()).expect("canonical");
    std::fs::create_dir_all(base.join("real/bin")).expect("real");
    std::fs::write(base.join("real/bin/tool"), b"x").expect("tool");
    std::os::unix::fs::symlink("real", base.join("via")).expect("via");
    std::fs::create_dir_all(base.join("links")).expect("links");
    std::os::unix::fs::symlink(base.join("via/bin/tool"), base.join("links/tool")).expect("link");
    let table = aterm_sysprobe::volumes::table();
    assert_eq!(
        resolve_local(&base.join("links/tool"), &table),
        Some(base.join("real/bin/tool"))
    );
    let on = aterm_sysprobe::volumes::mount_of(&base, &table)
        .expect("a mount holds it")
        .on
        .clone();
    let physical = base.join("physical/bin");
    std::fs::create_dir_all(&physical).expect("physical bin");
    std::fs::write(base.join("physical/tool"), b"x").expect("physical tool");
    std::os::unix::fs::symlink("../tool", physical.join("tool")).expect("relative tool link");
    let alias = base.join("bin-alias");
    std::os::unix::fs::symlink("physical/bin", &alias).expect("directory alias");
    let listed =
        inbound_links(std::slice::from_ref(&alias), Some(&table)).expect("resolved directory");
    assert!(
        listed.contains(&physical),
        "the installed directory itself is referenced"
    );
    assert!(
        listed.contains(&base.join("physical/tool")),
        "the relative target is resolved physically"
    );
    assert!(
        inbound_links(&[base.join("absent-bin")], Some(&table))
            .expect("positively missing folder is skipped")
            .is_empty()
    );
    let empty = base.join("empty-bin");
    std::fs::create_dir(&empty).expect("empty bin");
    let empty_alias = base.join("empty-bin-alias");
    std::os::unix::fs::symlink("empty-bin", &empty_alias).expect("empty directory alias");
    let mut unresolved_remote = Vec::new();
    let mut unresolved_enumeration = Vec::new();
    for remote in [true, false] {
        let mut t = table.clone();
        for m in &mut t.mounts {
            if m.on == on {
                if remote {
                    m.local = false;
                } else {
                    m.automounted = true;
                }
            }
        }
        assert_eq!(resolve_local(&base.join("links/tool"), &t), None);
        unresolved_remote
            .push(inbound_links(std::slice::from_ref(&base.join("links")), Some(&t)).is_err());
        unresolved_enumeration
            .push(inbound_links(std::slice::from_ref(&empty_alias), Some(&t)).is_err());
    }
    let cut = aterm_sysprobe::volumes::Table {
        complete: false,
        ..table.clone()
    };
    assert_eq!(resolve_local(&base.join("links/tool"), &cut), None);
    assert!(inbound_links(&[base.join("links")], Some(&cut)).is_err());

    let unknown = base.join("unknown-links");
    std::fs::create_dir(&unknown).expect("owned link folder");
    let link = unknown.join("tool");
    std::os::unix::fs::symlink("../real/bin/missing", &link).expect("dangling local link");
    let missing = inbound_links(std::slice::from_ref(&unknown), Some(&table));
    std::fs::remove_file(&link).expect("retire owned fixture link");
    std::os::unix::fs::symlink("tool", &link).expect("cyclic local link");
    let cycle = inbound_links(std::slice::from_ref(&unknown), Some(&table));
    assert!(
        missing.is_err()
            && cycle.is_err()
            && unresolved_remote.iter().all(|refused| *refused)
            && unresolved_enumeration.iter().all(|refused| *refused),
        "UNKNOWN_INBOUND_LINK_MUST_KEEP_CANDIDATES: missing={missing:?}, cycle={cycle:?}, remote={unresolved_remote:?}, directories={unresolved_enumeration:?}"
    );
}

/// A walk ends only on a local folder ([`resolve_local`]): `/` where the
/// table calls its mount remote holds no component for a step to place, and
/// resolves to nothing — as a link onto a remote root does, which ends the
/// walk there (the old walk answered the root it ended on).
#[test]
fn a_walk_that_ends_on_a_remote_root_resolves_to_nothing() {
    let root = |local: bool| aterm_sysprobe::volumes::Table {
        mounts: vec![aterm_sysprobe::volumes::Mount {
            on: PathBuf::from("/"),
            from: "nas:/".to_owned(),
            fstype: if local { "ext4" } else { "nfs" }.to_owned(),
            local,
            automounted: false,
            removable: false,
            read_only: false,
            container: None,
        }],
        complete: true,
        firmlinks: Vec::new(),
    };
    assert_eq!(resolve_local(Path::new("/"), &root(false)), None);
    assert_eq!(
        resolve_local(Path::new("/"), &root(true)),
        Some(PathBuf::from("/"))
    );
}

/// The other-user fence of a whole removal past a sizing walk cut short: a
/// walk that runs past its bound, or a folder it cannot read, rules nothing
/// out (`Some`).
#[test]
fn an_ownership_walk_cut_short_rules_nothing_out() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = Tmp::new("idle-foreign");
    let (_, target) = checkout(&tmp);
    assert_eq!(target::foreign_walk(&target, 1_000_000, TREE_WALL), None);
    assert!(target::foreign_walk(&target, 3, TREE_WALL).is_some_and(|w| w.contains("more than 3")),);
    let shut = target.join("shut");
    std::fs::create_dir_all(shut.join("x")).expect("shut");
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let unread = target::foreign_walk(&target, 1_000_000, TREE_WALL);
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert!(
        unread.is_some_and(|w| w.contains("could not be read")),
        "an unreadable folder rules nothing out"
    );
}
