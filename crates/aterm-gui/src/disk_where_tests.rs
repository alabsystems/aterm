// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn build_dir(dir: &Path, bytes: usize) {
    std::fs::create_dir_all(dir.join("deps")).expect("deps");
    std::fs::write(
        dir.join("CACHEDIR.TAG"),
        aterm_agent::harness::disk::CACHEDIR_SIGNATURE,
    )
    .expect("tag");
    std::fs::write(dir.join("deps").join("out.rlib"), vec![7u8; bytes]).expect("output");
}

fn tree(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("readable").flatten() {
            let meta = entry.metadata().expect("metadata");
            if meta.is_dir() {
                stack.push(entry.path());
            }
            out.push((entry.path(), meta.len()));
        }
    }
    out.sort();
    out
}

fn found(path: &Path) -> discover::Found {
    discover::Found {
        path: path.to_path_buf(),
        origin: Origin::Checkout {
            root: path.parent().expect("parent").to_path_buf(),
        },
    }
}

#[test]
fn a_survey_runs_on_its_own_thread_at_most_hourly() {
    let slot = Arc::new(Where::default());
    let t0 = Instant::now();
    let w0 = 1_792_000_000_000_i64;
    let every_ms = EVERY.as_millis() as i64;
    let caller = std::thread::current().id();
    let handle = start_with(&slot, (t0, Some(w0)), move || {
        let me = std::thread::current();
        assert_ne!(me.id(), caller, "never on the caller's thread");
        assert_eq!(me.name(), Some(THREAD));
        Found {
            hogs: vec![Hog {
                path: "~/a/target".into(),
                bytes: 1,
                ..Hog::default()
            }],
            at_unix_ms: Some(1),
            cut: false,
        }
    })
    .expect("the first survey starts");
    assert!(start_with(&slot, (t0, Some(w0)), Found::default).is_none());
    handle.join().expect("the survey's assertions held");
    let (last, running) = slot.read();
    assert!(!running);
    assert_eq!(last.expect("its rows").hogs[0].path, "~/a/target");

    let within = t0 + EVERY - Duration::from_secs(1);
    let within_ms = w0 + every_ms - 1_000;
    assert!(start_with(&slot, (within, Some(within_ms)), Found::default).is_none());
    assert!(start_with(&slot, (within, None), Found::default).is_none());
    let later = t0 + EVERY;
    let handle = start_with(&slot, (later, Some(within_ms)), || {
        panic!("a survey that fails")
    })
    .expect("an hour on, another");
    assert!(handle.join().is_err());
    let (last, running) = slot.read();
    assert!(!running);
    assert_eq!(last.expect("kept").hogs.len(), 1);
    assert!(start_with(&slot, (later, Some(within_ms)), Found::default).is_none());
    let woke = later + Duration::from_secs(1);
    let handle = start_with(&slot, (woke, Some(within_ms + every_ms)), Found::default)
        .expect("an hour on the wall clock, another");
    handle.join().expect("the survey ends");
}

fn worktree(repo: &Path, name: &str, wt: &Path) {
    let record = repo.join(".git").join("worktrees").join(name);
    std::fs::create_dir_all(&record).expect("worktree record");
    std::fs::create_dir_all(wt).expect("worktree");
    std::fs::write(wt.join(".git"), "gitdir: x\n").expect(".git file");
    std::fs::write(
        record.join("gitdir"),
        format!("{}\n", wt.join(".git").display()),
    )
    .expect("gitdir");
}

#[test]
fn a_survey_reads_only_and_never_enters_guarded_folders() {
    let dir = aterm_tempfile::tempdir().expect("scratch home");
    let root = std::fs::canonicalize(dir.path()).expect("canonical root");
    let home = root.join("home");
    let guarded = checkout::HOME_SKIP[0];
    build_dir(&home.join("proj").join("target"), 40_000);
    for wt in ["a", "b", "c"] {
        build_dir(&home.join("wt").join(wt).join("target"), 10_000);
    }
    let guarded_dir = home.join(guarded).join("proj");
    build_dir(&guarded_dir.join("target"), 90_000);
    let outside = root.join("elsewhere").join("wt");
    worktree(&home.join("proj"), "out", &outside);
    build_dir(&outside.join("target"), 20_000);
    let inside = home.join(guarded).join("wt");
    worktree(&home.join("proj"), "in", &inside);
    build_dir(&inside.join("target"), 80_000);
    let before = tree(&root);

    let roots = roots(
        &home,
        vec![guarded_dir.clone(), PathBuf::from("relative")],
        vec![guarded_dir.join("target")],
        &aterm_sysprobe::volumes::table(),
        discover::device_of(&home).expect("scratch home's device"),
    )
    .expect("scratch home is on a known local volume");
    assert!(roots.cwds.is_empty() && roots.agent_target_dirs.is_empty());
    let found = discover::discover(&roots, Budget::SHIPPED);
    let (hogs, cut) = rows(
        &found.found,
        &home,
        || false,
        aterm_agent::harness::disk::target::total_bytes,
    );
    assert!(!cut);
    let paths: Vec<&str> = hogs.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(paths.len(), 3, "{hogs:?} of {found:?} under {roots:?}");
    assert!(paths.contains(&"~/proj/target"), "{hogs:?}");
    let beside = outside.join("target");
    let beside = aterm_sysprobe::paths::plain(&beside).display().to_string();
    assert!(!beside.starts_with(r"\\?\"), "{beside}");
    assert!(paths.contains(&beside.as_str()), "{hogs:?}");
    let wt = hogs.iter().find(|h| h.path == "~/wt").expect("grouped");
    assert_eq!(wt.remedy, "3 target folders");
    assert!(wt.bytes >= 30_000, "{wt:?}");
    // Below the scratch root: Windows keeps the root itself in a guarded
    // folder (AppData\Local\Temp).
    let scratch = aterm_sysprobe::paths::plain(&root).display().to_string();
    assert!(
        !paths.iter().any(|p| p
            .strip_prefix(scratch.as_str())
            .unwrap_or(p)
            .contains(guarded)),
        "a guarded folder was entered: {hogs:?}"
    );
    assert_eq!(tree(&root), before, "the survey removed or changed nothing");
}

#[test]
#[cfg(unix)]
fn roots_are_only_on_homes_own_local_volume() {
    use aterm_sysprobe::volumes::Mount;
    let mount = |on: &str, local: bool, automounted: bool, fstype: &str| Mount {
        on: PathBuf::from(on),
        from: String::new(),
        fstype: fstype.to_owned(),
        local,
        automounted,
        removable: false,
        read_only: false,
        container: None,
    };
    let table = Table {
        mounts: vec![
            mount("/", true, false, "apfs"),
            mount("/Volumes/USB", true, false, "msdos"),
            mount("/Users//x/share", false, false, "nfs"),
            mount("/net", false, true, "autofs"),
        ],
        complete: true,
        firmlinks: Vec::new(),
    };
    let home = Path::new("/Users//x");
    let kept = |p: &str| placed(Path::new(p), home, &table);
    assert!(kept("/Users//x/proj"), "on home's own volume");
    assert!(kept("/srv/build/target"), "elsewhere on home's volume");
    for p in [
        "/Volumes/USB/proj",
        "/Users//x/share/proj",
        "/net/server/proj",
        "relative/proj",
        "/Users//x/../y/proj",
    ] {
        assert!(!kept(p), "{p}");
    }
    let guarded = format!("/Users//x/{}/proj", checkout::HOME_SKIP[0]);
    assert!(!kept(&guarded), "{guarded}");
    let incomplete = Table {
        complete: false,
        ..table.clone()
    };
    assert!(!placed(Path::new("/Users//x/proj"), home, &incomplete));
    let remote_home = Path::new("/Users//x/share/me");
    assert!(!placed(
        Path::new("/Users//x/share/me/proj"),
        remote_home,
        &table
    ));
    let roots = roots(
        home,
        vec![
            PathBuf::from("/Users//x/proj"),
            PathBuf::from("/Volumes/USB/proj"),
        ],
        vec![PathBuf::from("/net/server/target")],
        &table,
        7,
    )
    .expect("captured local home");
    assert_eq!(roots.home.as_deref(), Some(home));
    assert_eq!(roots.device, Some(7));
    assert_eq!(roots.cwds, [PathBuf::from("/Users//x/proj")]);
    assert!(roots.agent_target_dirs.is_empty());
    assert!(super::roots(home, Vec::new(), Vec::new(), &incomplete, 7).is_none());
    assert!(super::roots(remote_home, Vec::new(), Vec::new(), &table, 7).is_none());
    for unplaced in [Path::new("/net/me"), Path::new("relative")] {
        assert!(super::roots(unplaced, Vec::new(), Vec::new(), &table, 7).is_none());
    }
}

#[test]
fn sizing_stops_at_its_budget_and_keeps_partial_sizes() {
    let home = Path::new("/h");
    let dirs: Vec<PathBuf> = (0..12).map(|i| home.join(format!("p{i}/target"))).collect();
    let mut all: Vec<discover::Found> = dirs.iter().map(|d| found(d)).collect();
    all.push(found(&home.join("p0/target/nested")));

    let mut asked = Vec::new();
    let mut calls = 0;
    let (hogs, cut) = rows(
        &all,
        home,
        || {
            calls += 1;
            calls > 3
        },
        |p| {
            asked.push(p.to_path_buf());
            (1000 * asked.len() as u64, p.ends_with("p1/target"))
        },
    );
    assert_eq!(
        asked,
        dirs[..3],
        "nothing is sized once the budget is spent"
    );
    assert!(cut, "a survey whose budget ran out says so");
    assert_eq!(
        hogs.iter()
            .map(|h| (h.path.as_str(), h.bytes, h.partial))
            .collect::<Vec<_>>(),
        [
            ("~/p2/target", 3000, false),
            ("~/p1/target", 2000, true),
            ("~/p0/target", 1000, false),
        ]
    );
    let (hogs, cut) = rows(
        &all,
        home,
        || false,
        |p| (p.as_os_str().len() as u64, false),
    );
    assert!(!cut);
    assert_eq!(hogs.len(), ROWS);
    assert!(hogs.windows(2).all(|w| w[0].bytes >= w[1].bytes));
}

/// A discovery its search budget cut short is a cut survey, whose rows may miss the largest
/// folders, however its sizing went.
#[test]
fn a_search_cut_short_makes_a_cut_survey() {
    let home = Path::new("/h");
    let cut = |why: Option<&str>| discover::Discovered {
        found: vec![found(&home.join("p0/target"))],
        notes: Vec::new(),
        cut: why.map(str::to_owned),
        dangling: Vec::new(),
    };
    let (hogs, cut_short) = found_rows(&cut(Some("wall budget")), home, || false, |_| (100, false));
    assert_eq!(
        hogs.iter()
            .map(|h| (h.path.as_str(), h.bytes))
            .collect::<Vec<_>>(),
        [("~/p0/target", 100)]
    );
    assert!(cut_short);
    let (_, cut_short) = found_rows(&cut(None), home, || false, |_| (100, false));
    assert!(!cut_short, "searched whole and sized in time");
}
