// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const HOUR: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn file(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Set `path`'s modification time to `at`, without following a link.
fn stamp(path: &Path, at: SystemTime) {
    std::fs::File::open(path).unwrap().set_modified(at).unwrap();
}

/// Every file and folder of `dir`, and `dir`, last written at `at`.
fn age_tree(dir: &Path, at: SystemTime) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.file_type().unwrap().is_dir() {
            age_tree(&entry.path(), at);
        }
        stamp(&entry.path(), at);
    }
    stamp(dir, at);
}

/// A cargo-shaped cache at `dir`: a signed tag and one unit of output.
fn cache(dir: &Path) {
    file(
        &dir.join(TAG),
        "Signature: 8a477f597d28d172789f06886806bc55\n# a cache directory tag\n",
    );
    file(&dir.join("debug/deps/libunit.rlib"), "rlib");
}

/// A campaign scope `name` under `home` with evidence, a clone, a cargo home and a target
/// directory, and with `receipt` (a file name) when it is finished.
fn scope(home: &Path, name: &str, receipt: Option<&str>) -> PathBuf {
    let unit = home.join(SCOPES).join(name);
    file(&unit.join("normal.log"), "evidence");
    file(&unit.join("clone/src/main.rs"), "fn main() {}");
    cache(&unit.join("cargo-home/registry"));
    cache(&unit.join("clone/target"));
    if let Some(receipt) = receipt {
        file(
            &unit.join(receipt),
            "{\"schema\": 1, \"landed\": \"a6dac72\"}",
        );
    }
    unit
}

fn planned(home: &Path, now: SystemTime, pressed: bool) -> Vec<(PathBuf, Unused)> {
    plan(home, now, pressed)
        .into_iter()
        .map(|one| (one.path, one.why))
        .collect()
}

fn inert() -> fence::Fence<'static> {
    fence::Fence::default()
}

/// THE RULE OF A FINISHED SCOPE: its tagged caches go once it and they stood unwritten a day,
/// and its receipt, evidence and clone stay. NEGATIVE CONTROLS, each beside it in one home and
/// each kept whole by the plan and by the pass: the same scope with no receipt; one whose own
/// folder was written an hour ago; one a cache of which was written an hour ago, whose other
/// cache stays with it; one a note deep in which was written an hour ago; one under another
/// authority's custody marker; one `aterm pkg machine cache retain` keeps; one a process uses;
/// and, in the finished scope itself, a `target` with no tag and one whose tag lacks the
/// signature.
#[test]
fn a_finished_scopes_tagged_caches_go_and_everything_else_stays() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let now = SystemTime::now();
    let old = now - 2 * DAY;

    let done = scope(&home, "done", Some("DELIVERED_ARM.json"));
    file(&done.join("untagged/target/debug/x"), "x");
    file(&done.join("unsigned").join(TAG), "not a signature\n");
    file(&done.join("unsigned/debug/x"), "x");
    let open = scope(&home, "open", None);
    let fresh = scope(&home, "fresh", Some("MAIN_LANDED.json"));
    let busy = scope(&home, "busy", Some("private-landed.json"));
    let kept = scope(&home, "kept", Some("LANDED.json"));
    file(&kept.join(super::super::registered::MARKER), "unreadable");
    let used = scope(&home, "used", Some("delivery.json"));
    let noted = scope(&home, "noted", Some("LANDED.json"));
    file(&noted.join("evidence/run/notes/next.md"), "still at it");
    let retained = scope(&home, "retained", Some("LANDED.json"));
    for unit in [&done, &open, &fresh, &busy, &kept, &used, &noted, &retained] {
        age_tree(unit, old);
    }
    stamp(&fresh, now - HOUR);
    stamp(&busy.join("clone/target/debug/deps"), now - HOUR);
    stamp(&noted.join("evidence/run/notes/next.md"), now - HOUR);
    {
        use std::os::unix::fs::PermissionsExt as _;
        for dir in [home.as_path(), retained.as_path()] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let exact = retained.canonicalize().unwrap();
        super::super::registered::retain(&home, &exact).expect("a scope can be retained");
        age_tree(&retained, old);
    }

    let goes = vec![
        done.join("cargo-home/registry"),
        done.join("clone/target"),
        used.join("cargo-home/registry"),
        used.join("clone/target"),
    ];
    let mut listed: Vec<PathBuf> = planned(&home, now, false)
        .into_iter()
        .map(|(path, why)| {
            assert!(
                matches!(why, Unused::Leftover { finished: true, after, idle }
                    if after == FINISHED_QUIET && idle >= after),
                "{}: {why:?}",
                path.display()
            );
            path
        })
        .collect();
    listed.sort();
    let mut expected = goes.clone();
    expected.sort();
    assert_eq!(listed, expected, "the plan names a process-used unit too");

    let held = used.join("normal.log");
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let users = dirs
            .iter()
            .any(|dir| held.starts_with(dir))
            .then(|| aterm_uds::holders::PathUse {
                pid: 23,
                how: "fd",
                path: held.clone(),
            })
            .into_iter()
            .collect();
        Ok(aterm_uds::holders::PathScan {
            users,
            ..aterm_uds::holders::PathScan::default()
        })
    };
    let mut asked = Vec::new();
    let went = reclaim_on(
        &home,
        now,
        false,
        &mut |path, _| {
            asked.push(path.to_path_buf());
            true
        },
        &fence::Fence::new(&processes),
    );
    let mut gone: Vec<PathBuf> = went.iter().map(|(path, _)| path.clone()).collect();
    gone.sort();
    assert_eq!(
        gone,
        vec![done.join("cargo-home/registry"), done.join("clone/target")],
        "{went:#?}"
    );
    assert_eq!(asked.len(), 2, "no intent for a kept cache: {asked:#?}");
    for path in &gone {
        assert!(!path.exists(), "{} went", path.display());
    }
    for stays in [
        done.join("DELIVERED_ARM.json"),
        done.join("normal.log"),
        done.join("clone/src/main.rs"),
        done.join("untagged/target/debug/x"),
        done.join("unsigned/debug/x"),
        open.join("clone/target/debug/deps/libunit.rlib"),
        open.join("cargo-home/registry/debug/deps/libunit.rlib"),
        fresh.join("clone/target/debug/deps/libunit.rlib"),
        busy.join("clone/target/debug/deps/libunit.rlib"),
        busy.join("cargo-home/registry").join(TAG),
        noted.join("clone/target").join(TAG),
        noted.join("cargo-home/registry").join(TAG),
        retained.join("clone/target").join(TAG),
        retained.join("cargo-home/registry").join(TAG),
        kept.join("clone/target/debug/deps/libunit.rlib"),
        used.join("clone/target/debug/deps/libunit.rlib"),
        used.join("cargo-home/registry").join(TAG),
    ] {
        assert!(stays.exists(), "{} stays", stays.display());
    }
    assert!(
        std::fs::read_dir(done.join("clone"))
            .unwrap()
            .flatten()
            .all(|entry| staged_from(&entry.file_name()).is_none()),
        "no stage is left behind"
    );
}

/// A unit with no receipt waits a week while the volume has room and a day below the floor;
/// the worktree roots' folder, the user cache folder and aterm's own `.ubd` cache folder are
/// places too. NEGATIVE CONTROL: the same trees two days old are no candidates with room, and
/// the shared cache's own root under aterm's `.ubd` folder is never one of this sweep's.
#[test]
fn a_unit_with_no_receipt_waits_a_week_or_a_day_below_the_floor() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let now = SystemTime::now();
    let roots = [
        home.join(".ubd/trust-cohort-root"),
        home.join(".cache/trust-cg-target.noindex"),
        home.join("Library/Caches/aterm/.ubd/ty-package.noindex"),
    ];
    for root in &roots {
        cache(&root.join("target"));
        age_tree(root, now - 2 * DAY);
    }
    let shared = super::super::cache_root(&home).join("Users/x/repo");
    cache(&shared.join(super::super::LEAF));
    age_tree(&super::super::cache_root(&home), now - 30 * DAY);

    assert_eq!(
        planned(&home, now, false),
        Vec::new(),
        "two days, with room"
    );
    let pressed: Vec<PathBuf> = planned(&home, now, true)
        .into_iter()
        .map(|(path, why)| {
            assert!(
                matches!(why, Unused::Leftover { finished: false, after, .. }
                    if after == FINISHED_QUIET),
                "{why:?}"
            );
            path
        })
        .collect();
    let mut expected: Vec<PathBuf> = roots.iter().map(|root| root.join("target")).collect();
    expected.sort();
    let mut sorted = pressed;
    sorted.sort();
    assert_eq!(sorted, expected);

    for root in &roots {
        age_tree(root, now - 8 * DAY);
    }
    let calm = planned(&home, now, false);
    assert_eq!(calm.len(), 3, "{calm:#?}");
    assert!(calm.iter().all(|(_, why)| matches!(
        why,
        Unused::Leftover { finished: false, after, .. } if *after == LEFTOVER_IDLE
    )));
    let went = reclaim_on(&home, now, false, &mut |_, _| true, &inert());
    assert_eq!(went.len(), 3, "{went:#?}");
    assert!(
        shared.join(super::super::LEAF).join(TAG).exists(),
        "the shared cache is its own sweep's"
    );
}

/// A cache a build holds a cargo lock in stays, with no intent on record. NEGATIVE CONTROL:
/// the lock let go and the unit quiet again (the first pass wrote in it, taking the other
/// cache), the same pass takes it.
#[test]
fn a_cache_whose_cargo_lock_a_build_holds_stays() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let now = SystemTime::now();
    let unit = scope(&home, "locked", Some("LANDED.json"));
    let lock = unit.join("clone/target/debug/.cargo-lock");
    file(&lock, "");
    age_tree(&unit, now - 2 * DAY);
    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).unwrap()).unwrap();
    let mut asked = Vec::new();
    let went = reclaim_on(
        &home,
        now,
        false,
        &mut |path, _| {
            asked.push(path.to_path_buf());
            true
        },
        &inert(),
    );
    assert_eq!(asked, vec![unit.join("cargo-home/registry")], "{went:#?}");
    assert!(unit.join("clone/target").join(TAG).exists());
    drop(held);
    let went = reclaim_on(&home, now, false, &mut |_, _| true, &inert());
    assert!(
        went.is_empty(),
        "the pass's own write keeps the unit a day: {went:#?}"
    );
    age_tree(&unit, now - 2 * DAY);
    let went = reclaim_on(&home, now, false, &mut |_, _| true, &inert());
    assert_eq!(went.len(), 1, "{went:#?}");
    assert!(!unit.join("clone/target").exists());
}

/// A stage an earlier pass left in a unit is finished by the next one. NEGATIVE CONTROL: a
/// folder that only resembles one (no cache name before the suffix, or another suffix) stays.
#[test]
fn a_stage_an_earlier_pass_left_is_finished() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let now = SystemTime::now();
    let unit = home.join(SCOPES).join("half");
    file(&unit.join("LANDED.json"), "{\"landed\": true}");
    let stage = unit.join(format!("target{STAGE}"));
    let second = unit.join(format!("target{STAGE}-1"));
    file(&stage.join("debug/deps/left.rlib"), "left");
    file(&second.join("debug/deps/left.rlib"), "left");
    file(&unit.join(STAGE).join("x"), "no cache name");
    file(&unit.join("target.removing/x"), "another tool's name");
    age_tree(&unit, now - 2 * DAY);

    let listed = planned(&home, now, false);
    assert_eq!(
        listed,
        vec![
            (stage.clone(), Unused::Unfinished),
            (second.clone(), Unused::Unfinished)
        ]
    );
    let went = reclaim_on(&home, now, false, &mut |_, _| true, &inert());
    assert_eq!(went.len(), 2, "{went:#?}");
    assert!(!stage.exists() && !second.exists());
    assert!(unit.join(STAGE).join("x").exists());
    assert!(unit.join("target.removing/x").exists());
    assert!(unit.join("LANDED.json").exists());
}

/// A unit that is itself a tagged cache (a target directory made directly in a place) goes
/// whole once unwritten a week, and a stage of one an earlier pass left in the place is finished
/// and is no unit. NEGATIVE CONTROLS: the same unit two days old stays, as does one whose
/// cargo lock a build holds and a place's folder that only resembles a stage.
#[test]
fn a_unit_that_is_itself_a_tagged_cache_goes_whole() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let now = SystemTime::now();
    let place = home.join(".cache");
    let (old, young, locked) = (
        place.join("probe-target.noindex"),
        place.join("young-target.noindex"),
        place.join("locked-target.noindex"),
    );
    for unit in [&old, &young, &locked] {
        cache(unit);
    }
    let lock = locked.join("debug/.cargo-lock");
    file(&lock, "");
    let stage = place.join(format!("gone-target.noindex{STAGE}"));
    file(&stage.join("debug/deps/left.rlib"), "left");
    file(&place.join(STAGE).join("x"), "no cache name");
    for (unit, days) in [(&old, 8), (&young, 2), (&locked, 8), (&stage, 8)] {
        age_tree(unit, now - days * DAY);
    }
    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).unwrap()).unwrap();

    assert!(
        places(&home)[1].units().iter().all(|unit| unit != &stage),
        "a stage is no unit"
    );
    let mut listed: Vec<PathBuf> = planned(&home, now, false)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    listed.sort();
    assert_eq!(listed, [stage.clone(), locked.clone(), old.clone()]);
    let went = reclaim_on(&home, now, false, &mut |_, _| true, &inert());
    let mut gone: Vec<PathBuf> = went.into_iter().map(|(path, _)| path).collect();
    gone.sort();
    assert_eq!(gone, [stage.clone(), old.clone()]);
    assert!(!old.exists() && !stage.exists());
    assert!(young.join(TAG).exists() && locked.join(TAG).exists());
    assert!(place.join(STAGE).join("x").exists());
    assert!(
        std::fs::read_dir(&place)
            .unwrap()
            .flatten()
            .all(|entry| staged_from(&entry.file_name()).is_none()),
        "no stage is left behind"
    );
    drop(held);
}

/// What counts as a receipt: a JSON object in a `.json` file whose name holds the word.
/// NEGATIVE CONTROLS: the word inside another word, another extension, a script that records a
/// delivery, and a file that is no JSON object.
#[test]
fn a_receipt_is_a_json_object_named_landed_or_delivered() {
    let dir = aterm_tempfile::tempdir().unwrap();
    for (name, text, says) in [
        ("DELIVERED_ARM.json", "{\"build\": 2026110901}", true),
        ("private-landed.json", "{\"landed\": \"a6dac72\"}", true),
        ("MAIN_LANDED_RECEIPT.json", "{\"main\": \"1ad8b4f\"}", true),
        ("PUBLIC_DELIVERY.json", "{\"public_url\": \"u\"}", true),
        ("islanded.json", "{\"a\": 1}", false),
        ("undelivered.json", "{\"a\": 1}", false),
        ("landed.txt", "{\"a\": 1}", false),
        ("record-delivery.py", "print()", false),
        ("LANDED.json", "", false),
        ("landed.json", "[1, 2]", false),
        ("delivered.json", "{}", false),
    ] {
        let unit = dir.path().join(name.replace('.', "_"));
        file(&unit.join(name), text);
        assert_eq!(receipt(&unit).is_some(), says, "{name}: {text}");
    }
}

/// The whole pass and the whole plan (`aterm pkg machine cache`, `cache clean`, the disk
/// watch's sweep) take the leftovers in. NEGATIVE CONTROL: the scope with no receipt stays.
#[test]
fn the_home_plan_and_the_home_pass_take_the_leftovers_in() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let now = SystemTime::now();
    let done = scope(&home, "done", Some("DELIVERED_SOURCE.json"));
    let open = scope(&home, "open", None);
    age_tree(&done, now - 2 * DAY);
    age_tree(&open, now - 2 * DAY);
    let room = |_: &Path| Some((2u64, 1u64));
    let listed: Vec<PathBuf> = super::super::plan_home_on(&home, now, &room)
        .into_iter()
        .map(|one| one.path)
        .collect();
    assert_eq!(
        listed,
        vec![done.join("cargo-home/registry"), done.join("clone/target")]
    );
    let went = super::super::reclaim_home_report_on(&home, now, &room, &inert());
    assert_eq!(went.len(), 2, "{went:#?}");
    assert!(!done.join("clone/target").exists());
    assert!(done.join("DELIVERED_SOURCE.json").exists());
    assert!(open.join("clone/target").join(TAG).exists());
}
