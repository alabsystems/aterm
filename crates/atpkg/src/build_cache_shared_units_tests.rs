// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn file(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![7u8; 64 * 1024]).unwrap();
}

/// Set `path`'s access and modification times to `at`.
fn stamp(path: &Path, at: SystemTime) {
    let file = open_to_stamp(path);
    file.set_times(std::fs::FileTimes::new().set_accessed(at).set_modified(at))
        .unwrap();
}

/// `path`, a file or a folder, opened so its times can be set: any open on Unix.
#[cfg(unix)]
fn open_to_stamp(path: &Path) -> std::fs::File {
    std::fs::File::open(path).unwrap()
}

/// `path`, a file or a folder, opened so its times can be set: Windows sets them only through a
/// handle that may write attributes, and opens a folder only with backup semantics.
#[cfg(windows)]
fn open_to_stamp(path: &Path) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap()
}

/// Every file and folder of `dir` last touched at `at`.
fn age_tree(dir: &Path, at: SystemTime) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.file_type().unwrap().is_dir() {
            age_tree(&entry.path(), at);
        }
        stamp(&entry.path(), at);
    }
    stamp(dir, at);
}

/// A unit as targo lays one (`shared_unit_dir`): its `.lock`, its `out/` and its `fingerprint/`,
/// every part of it last touched at `at`.
fn unit(path: &Path, at: SystemTime) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join(LOCK), b"").unwrap();
    file(&path.join("out/libdep-0123.rlib"));
    file(&path.join("fingerprint/lib-dep.json"));
    age_tree(path, at);
    path.to_path_buf()
}

/// Each path of `went` under `root`, its parts joined by `/` whatever the host's separator.
fn names(went: &[(PathBuf, Unused)], root: &Path) -> Vec<String> {
    went.iter()
        .map(|(path, _)| {
            let parts: Vec<_> = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect();
            parts.join("/")
        })
        .collect()
}

/// A build's lock on `unit`, shared as a build reading it takes it.
fn reading(unit: &Path) -> crate::lock::Flock {
    crate::lock::Flock::try_lock_shared(crate::metadata_io::open_regular(&unit.join(LOCK)).unwrap())
        .unwrap()
}

/// Whether `path`'s access time is below its write time: armed, so a read moves it ([`arm`]).
fn armed(path: &Path) -> bool {
    let meta = std::fs::symlink_metadata(path).unwrap();
    meta.accessed().unwrap() < meta.modified().unwrap()
}

/// A scratch home and its store, at the home's `.cargo` ([`root`]).
fn home_and_store(dir: &Path) -> (PathBuf, PathBuf) {
    let home = dir.join("home");
    let root = home.join(".cargo/trust/native-dependencies-v1");
    assert_eq!(super::root(&home), root);
    (home, root)
}

/// THE CALM RULE OVER THE STORE, BY LAST USE: a registry unit and a Git unit
/// (`build/git/<commit>/…`) built a month ago and unread since go, each renamed aside in its own
/// folder and removed, each journalled while it still stands; a stage an earlier pass left is
/// finished; a unit written yesterday stays; and the folders the units sat in stay for the next
/// build. A pass two days earlier, when none was a month old, took nothing and armed every
/// unit's files, so a build that today only reads one unit's fingerprint (as a fresh build does)
/// moves its access time. NEGATIVE CONTROL: that unit, as old as the ones that go, stays; a
/// pass carries its read into its `.lock` before arming its files again, so a month less two
/// days later it still stays, and a month and a day later it goes.
#[test]
fn a_shared_unit_unused_a_month_goes_and_one_a_build_used_stays() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (home, root) = home_and_store(dir.path());
    let now = SystemTime::now();
    let month = now - RECLAIM_IDLE - DAY;
    let serde = unit(&root.join("build/serde/0123abcd"), month);
    let git = unit(&root.join("build/git/4f2e9c/dep/89ab"), month);
    let fresh = unit(&root.join("build/itoa/5555"), now - DAY);
    let read = unit(&root.join("build/libc/7777"), month);
    let fingerprint = read.join("fingerprint/lib-dep.json");
    assert!(
        !armed(&fingerprint),
        "as targo writes it, a read would not move it"
    );

    assert!(reclaim(&home, now - 2 * DAY, &mut |_, _| true).is_empty());
    for kept in [&serde, &git, &fresh, &read] {
        assert!(
            armed(&kept.join("out/libdep-0123.rlib")),
            "{}",
            kept.display()
        );
        assert!(
            armed(&kept.join("fingerprint/lib-dep.json")),
            "{}",
            kept.display()
        );
    }
    std::fs::read(&fingerprint).unwrap();
    assert!(
        !armed(&fingerprint),
        "the build's read moved the armed access time"
    );
    let stage = root.join("build/memchr").join(STAGE);
    file(&stage.join("out/libmemchr.rlib"));

    let planned = plan(&root, now);
    assert_eq!(planned.len(), 2, "{planned:?}");
    assert_eq!(planned[0].path, stage);
    assert_eq!(planned[0].why, Unused::Unfinished);
    assert_eq!(planned[1].path, root);
    assert!(!planned[1].held);
    assert_eq!(planned[1].paths, vec![git.clone(), serde.clone()]);
    assert!(matches!(
        planned[1].why,
        Unused::SharedUnits { units: 2, after, .. } if after == RECLAIM_IDLE
    ));

    let mut asked = Vec::new();
    let went = reclaim(&home, now, &mut |path, why| {
        assert!(
            path.exists(),
            "journalled before it moves: {}",
            path.display()
        );
        asked.push((path.to_path_buf(), why));
        true
    });
    assert_eq!(
        names(&went, &root),
        [
            "build/memchr/.aterm-reclaim",
            "build/git/4f2e9c/dep/89ab",
            "build/serde/0123abcd"
        ]
    );
    assert_eq!(asked.len(), 3, "each removal is journalled first");
    assert!(went.iter().skip(1).all(|(_, why)| matches!(
        why,
        Unused::SharedUnits { units: 1, idle, after } if *idle >= *after && *after == RECLAIM_IDLE
    )));
    for gone in [&serde, &git, &stage] {
        assert!(!gone.exists(), "{}", gone.display());
    }
    for kept in [&fresh, &read] {
        assert!(kept.join(LOCK).is_file(), "{}", kept.display());
    }
    assert!(armed(&fingerprint), "the pass armed the read file again");
    assert!(root.join("build/serde").is_dir(), "the unit's folder stays");
    assert!(
        std::fs::read_dir(root.join("build/serde"))
            .unwrap()
            .next()
            .is_none(),
        "no stage is left beside it"
    );
    assert!(reclaim(&home, now, &mut |_, _| true).is_empty());

    // The read stays on record in the unit's `.lock` once its files are armed again.
    let went = reclaim(&home, now + RECLAIM_IDLE - 2 * DAY, &mut |_, _| true);
    assert!(went.is_empty(), "{went:?}");
    let went = reclaim(&home, now + RECLAIM_IDLE + DAY, &mut |_, _| true);
    assert_eq!(names(&went, &root), ["build/itoa/5555", "build/libc/7777"]);
}

/// OVER THE BUDGET THE LEAST RECENTLY USED UNIT GOES FIRST: of three units unused a day, a week
/// and a month, a budget that asks for one more takes the month's, and once its record declines
/// the pass ends with the rest in place. NEGATIVE CONTROL: a unit used within the hour stays
/// even while the budget asks for everything.
#[test]
fn over_the_budget_the_least_recently_used_unit_goes_first() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (home, root) = home_and_store(dir.path());
    let now = SystemTime::now();
    let day = unit(&root.join("build/a/1"), now - DAY);
    let week = unit(&root.join("build/b/2"), now - 7 * DAY);
    let month = unit(&root.join("build/c/3"), now - 30 * DAY);
    let hot = unit(&root.join("build/d/4"), now - Duration::from_secs(60));

    let mut room = 1;
    let went = reclaim_lru(&home, now, &mut |_, _| {
        let more = room > 0;
        room -= 1;
        more
    });
    assert_eq!(names(&went, &root), ["build/c/3"]);
    assert!(!month.exists());
    assert!(day.exists() && week.exists());

    let went = reclaim_lru(&home, now, &mut |_, _| true);
    assert_eq!(names(&went, &root), ["build/b/2", "build/a/1"]);
    assert!(
        hot.join(LOCK).is_file(),
        "a unit used within the hour stays"
    );
}

/// A UNIT A BUILD USES DURING THE PASS STAYS: the pass reads each unit's use before it removes
/// any, so a build that reads a unit's fingerprint while the pass removes an older unit (here,
/// as the older one's intent is journalled) keeps it: its use is read again once the pass holds
/// its lock, and it is neither journalled nor moved. NEGATIVE CONTROL: with no build reading it,
/// the same pass takes both.
#[test]
fn a_shared_unit_a_build_uses_during_the_pass_stays() {
    for used in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (home, root) = home_and_store(dir.path());
        let now = SystemTime::now();
        let old = unit(&root.join("build/a/1"), now - 30 * DAY);
        let younger = unit(&root.join("build/b/2"), now - 7 * DAY);
        let mut asked = Vec::new();
        let went = reclaim_lru(&home, now, &mut |path, _| {
            if used && path == old {
                std::fs::read(younger.join("fingerprint/lib-dep.json")).unwrap();
            }
            asked.push(path.to_path_buf());
            true
        });
        if used {
            assert_eq!(names(&went, &root), ["build/a/1"]);
            assert_eq!(asked, std::slice::from_ref(&old));
            assert!(younger.join(LOCK).is_file());
        } else {
            assert_eq!(names(&went, &root), ["build/a/1", "build/b/2"]);
            assert!(!younger.exists());
        }
    }
}

/// A UNIT A BUILD HOLDS STAYS: a month-old unit whose lock a build holds, shared (reading it) or
/// exclusive (writing it), is neither journalled nor moved, and a plan lists no row for it; the
/// pass records it as in use when it found it held. NEGATIVE CONTROL: once the build lets its
/// lock go, a pass a month after that use takes it.
#[test]
fn a_shared_unit_whose_lock_a_build_holds_stays() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (home, root) = home_and_store(dir.path());
    let now = SystemTime::now();
    let held = unit(&root.join("build/serde/0123abcd"), now - RECLAIM_IDLE - DAY);

    let build = reading(&held);
    let mut asked = 0;
    let went = reclaim(&home, now, &mut |_, _| {
        asked += 1;
        true
    });
    assert!(went.is_empty(), "{went:?}");
    assert_eq!(asked, 0, "no intent is recorded for a unit a build holds");
    assert!(held.join(LOCK).is_file());
    assert!(plan(&root, now).is_empty());
    let lru = reclaim_lru(&home, now, &mut |_, _| true);
    assert!(lru.is_empty(), "{lru:?}");
    drop(build);

    let writer =
        crate::lock::Flock::try_lock(crate::metadata_io::open_regular(&held.join(LOCK)).unwrap())
            .unwrap();
    assert!(reclaim(&home, now, &mut |_, _| true).is_empty());
    assert!(held.join(LOCK).is_file());
    drop(writer);

    // It was in use when the pass found it held.
    assert!(reclaim(&home, now, &mut |_, _| true).is_empty());
    // CONTROL: a month after that use, with no build holding it, it goes.
    let went = reclaim(&home, now + RECLAIM_IDLE, &mut |_, _| true);
    assert_eq!(names(&went, &root), ["build/serde/0123abcd"]);
    assert!(!held.exists());
}

/// ON WINDOWS A BUILD BETWEEN THE LOCK AND THE RENAME KEEPS ITS UNIT: Windows renames no folder
/// holding an open file, so a pass lets go of a month-unused unit's lock, its intent on record,
/// just before its rename aside. A build that takes the lock then (shared, as a reading build
/// does) and holds it keeps the unit, its rename refused; one that takes it, reads the unit's
/// fingerprint and lets go keeps it too, its read seen at the stage and the unit put back. Each
/// intent closes with nothing gone. NEGATIVE CONTROL: a build that takes the lock and lets go
/// without reading leaves the unit to go.
#[cfg(windows)]
#[test]
fn on_windows_a_build_between_the_lock_and_the_rename_keeps_its_unit() {
    use super::super::Refusal;
    use std::cell::RefCell;
    use std::rc::Rc;
    for build in ["holds", "reads", "lets go"] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (home, root) = home_and_store(dir.path());
        let now = SystemTime::now();
        let old = unit(&root.join("build/serde/0123abcd"), now - RECLAIM_IDLE - DAY);
        let holding: Rc<RefCell<Option<crate::lock::Flock>>> = Rc::default();
        let held = Rc::clone(&holding);
        super::super::seams::on_staging(move |unit| {
            // The pass let its lock go: a build takes it, shared.
            let lock = reading(unit);
            match build {
                "holds" => *held.borrow_mut() = Some(lock),
                "reads" => {
                    std::fs::read(unit.join("fingerprint/lib-dep.json")).unwrap();
                    drop(lock);
                }
                _ => drop(lock),
            }
        });
        let went = reclaim(&home, now, &mut |_, _| true);
        super::super::seams::clear();
        drop(holding.borrow_mut().take());
        assert_eq!(names(&went, &root), ["build/serde/0123abcd"], "{build}");
        let why = &went[0].1;
        match build {
            "holds" => assert!(
                matches!(
                    why,
                    Unused::Refused {
                        by: Refusal::Rename(_)
                    }
                ),
                "{build}: {why:?}"
            ),
            "reads" => assert!(
                matches!(
                    why,
                    Unused::Refused {
                        by: Refusal::Stopped
                    }
                ),
                "{build}: {why:?}"
            ),
            _ => assert!(
                matches!(why, Unused::SharedUnits { units: 1, .. }),
                "{build}: {why:?}"
            ),
        }
        let stays = build != "lets go";
        assert_eq!(
            old.join("fingerprint/lib-dep.json").is_file(),
            stays,
            "{build}"
        );
        assert_eq!(old.join(LOCK).is_file(), stays, "{build}");
        let stages: Vec<_> = std::fs::read_dir(root.join("build/serde"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .filter(|name| name != "0123abcd")
            .collect();
        assert!(stages.is_empty(), "{build}: no stage is left: {stages:?}");
    }
}

/// THE STORE IS LISTED WITH ITS BYTES: `aterm pkg machine cache` (`plan_home`) lists the store
/// of a home's `.cargo` as one candidate whose paths are the units a pass takes, so its line
/// carries their bytes on disk; the shared build cache and the tooling cache stay listed beside
/// it. NEGATIVE CONTROL: a store whose units were all used within the month is not listed.
#[test]
fn the_shared_store_is_listed_with_its_bytes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let root = home.join(".cargo/trust/native-dependencies-v1");
    assert_eq!(super::root(&home), root);
    let now = SystemTime::now();
    let old = unit(&root.join("build/serde/0123abcd"), now - RECLAIM_IDLE - DAY);
    let young = unit(&root.join("build/itoa/5555"), now - DAY);

    let planned = super::super::plan_home_on(&home, now, &|_| None);
    let rows: Vec<&Planned> = planned.iter().filter(|p| p.path == root).collect();
    assert_eq!(rows.len(), 1, "{planned:?}");
    assert_eq!(rows[0].paths, vec![old.clone()]);
    let bytes = super::super::allocated(&rows[0].paths);
    assert!(bytes >= 2 * 64 * 1024, "{bytes}");
    assert_eq!(bytes, super::super::allocated(std::slice::from_ref(&old)));

    // CONTROL: once every unit was used within the month, nothing of the store is a candidate.
    age_tree(&old, now - DAY);
    let planned = super::super::plan_home_on(&home, now, &|_| None);
    assert!(
        planned.iter().all(|p| !p.path.starts_with(&root)),
        "{planned:?}"
    );
    assert!(young.exists());
}

/// THE STORE'S ROOT IS THE CARGO HOME'S, as Targo selects it: `CARGO_HOME`, else `TARGO_HOME`,
/// else the home's `.cargo`, a relative value against the working directory, `.` and `..`
/// folded; a set but empty `CARGO_HOME` wins over `TARGO_HOME` and falls back to `.cargo`, as
/// Trust's `homedir` does. NEGATIVE CONTROL: with no variable set the home's `.cargo` holds it.
#[test]
fn the_shared_stores_root_is_the_cargo_homes() {
    let (home, cwd) = (native("/Users//someone"), native("/work/repo"));
    let (home, cwd) = (home.as_path(), cwd.as_path());
    let with = |vars: &[(&str, &str)]| {
        let vars: Vec<(String, OsString)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), native(v).into_os_string()))
            .collect();
        root_with(home, Some(cwd), &move |name| {
            vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        })
    };
    let store = |base: &str| native(base).join("trust/native-dependencies-v1");
    assert_eq!(with(&[]), store("/Users//someone/.cargo"));
    assert_eq!(
        with(&[("CARGO_HOME", "/opt/cargo"), ("TARGO_HOME", "/opt/targo")]),
        store("/opt/cargo")
    );
    assert_eq!(with(&[("TARGO_HOME", "/opt/targo")]), store("/opt/targo"));
    assert_eq!(
        with(&[("CARGO_HOME", ""), ("TARGO_HOME", "/opt/targo")]),
        store("/Users//someone/.cargo")
    );
    assert_eq!(
        with(&[("CARGO_HOME", "../cargo/./home")]),
        store("/work/cargo/home")
    );
    // This process's own home is read by its environment; another home is read at its own
    // `.cargo`, whatever this process's environment names.
    let env = |name: &str| (name == "CARGO_HOME").then(|| native("/opt/cargo").into_os_string());
    assert_eq!(root_for(home, Some(home), &env), store("/opt/cargo"));
    assert_eq!(
        root_for(home, Some(native("/Users//other").as_path()), &env),
        store("/Users//someone/.cargo")
    );
    assert_eq!(root_for(home, None, &env), store("/Users//someone/.cargo"));
    assert_eq!(super::root(home), store("/Users//someone/.cargo"));
}

/// `unix`, a fixture's path, as this platform spells it: an absolute one on a drive on Windows
/// (`C:\opt\cargo`), where `/opt/cargo` names no drive and so takes the working directory's.
fn native(unix: &str) -> PathBuf {
    if cfg!(windows) && unix.starts_with('/') {
        PathBuf::from(format!("C:{}", unix.replace('/', "\\")))
    } else {
        PathBuf::from(unix)
    }
}

/// A STORE A BUILD NOTED IS READ AND EVICTED by the passes that run without the build's
/// environment: a build whose `CARGO_HOME` is not the home's `.cargo` notes its store once
/// ([`note_store`]), the home's own store is never noted, and the noted store is read beside the
/// home's ([`roots`]): the calm pass takes its month-unused unit, both through
/// [`super::super::reclaim_roots`] and, over the budget, [`super::super::reclaim_over_budget`];
/// the listing lists it. Once the store is gone the calm pass drops its note. NEGATIVE CONTROL:
/// a store noted in a folder a pass never looks into (`~/Documents`) is not read, its unit stays
/// and its note stays; a note that names no absolute path is dropped.
#[cfg(unix)]
#[test]
fn a_store_a_build_noted_is_read_and_evicted() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (home, own) = home_and_store(dir.path());
    let now = SystemTime::now();
    let month = now - RECLAIM_IDLE - DAY;
    let store = dir.path().join("cargo-home/trust/native-dependencies-v1");
    let notes_dir = home.join(NOTED);

    note_store(&home, &own).unwrap();
    assert!(!notes_dir.exists(), "the home's own store is never noted");
    note_store(&home, &store).unwrap();
    note_store(&home, &store).unwrap();
    assert_eq!(std::fs::read_dir(&notes_dir).unwrap().count(), 1);
    assert_eq!(
        roots(&home),
        std::slice::from_ref(&own),
        "a store not yet made is not read"
    );

    let first = unit(&store.join("build/serde/0123abcd"), month);
    assert_eq!(roots(&home), [own.clone(), store.clone()]);
    let planned = super::super::plan_home_on(&home, now, &|_| None);
    assert!(
        planned
            .iter()
            .any(|p| p.path == store && p.paths == [first.clone()]),
        "{planned:?}"
    );
    let went = super::super::reclaim_roots(
        &home,
        now,
        super::super::ReclaimAfter::CALM.into(),
        None,
        true,
        &super::super::fence::Fence::default(),
        &mut |_, _| true,
    );
    assert!(went.iter().any(|(path, _)| *path == first), "{went:?}");
    assert!(!first.exists());

    if cfg!(target_os = "macos") {
        let second = unit(&store.join("build/itoa/5555"), now - DAY);
        let went =
            super::super::reclaim_over_budget(&home, now, &|_, _| false, None, &mut |_, _| true);
        assert_eq!(went.len(), 1, "{went:?}");
        assert_eq!(went[0].0, second);
    }

    std::fs::remove_dir_all(dir.path().join("cargo-home")).unwrap();
    assert!(reclaim(&home, now, &mut |_, _| true).is_empty());
    assert!(
        !notes_dir.join(noted_name(&store)).exists(),
        "a gone store's note is dropped"
    );

    // CONTROL: a store noted under `~/Documents` is never read, and its note stays.
    let unseen = home.join("Documents/cargo/trust/native-dependencies-v1");
    let kept = unit(&unseen.join("build/serde/0123abcd"), month);
    note_store(&home, &unseen).unwrap();
    assert_eq!(roots(&home), std::slice::from_ref(&own));
    assert!(reclaim(&home, now, &mut |_, _| true).is_empty());
    assert!(kept.join(LOCK).is_file());
    assert!(notes_dir.join(noted_name(&unseen)).is_file());
    // CONTROL: a note that names no absolute path is dropped.
    std::fs::write(notes_dir.join("0000000000000000"), b"relative/store").unwrap();
    reclaim(&home, now, &mut |_, _| true);
    assert!(!notes_dir.join("0000000000000000").exists());
}
