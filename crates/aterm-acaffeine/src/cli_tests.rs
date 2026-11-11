// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::HOUR;

fn words(line: &str) -> Vec<OsString> {
    line.split_whitespace().map(OsString::from).collect()
}

fn parsed(line: &str) -> Result<Command, String> {
    parse(&words(line))
}

/// The judgement every verb that acts makes first ([`mac::login_paths`]): a process is the
/// login's only under the login's own home (one directory, however it is spelled) and no
/// private state root; with no HOME there is nothing to judge. This test binary has a
/// private root, so the live check refuses it.
#[cfg(target_os = "macos")]
#[test]
fn only_the_logins_roots_run_the_mug() {
    let dir = crate::test_scratch_dir("cli");
    let (login, other, link) = (
        dir.path().join("login"),
        dir.path().join("other"),
        dir.path().join("link"),
    );
    std::fs::create_dir(&login).expect("login");
    std::fs::create_dir(&other).expect("other");
    std::os::unix::fs::symlink(&login, &link).expect("link");
    assert_eq!(mac::login_refusal(false, Some(&login), Some(&login)), None);
    assert_eq!(
        mac::login_refusal(false, Some(&link), Some(&login)),
        None,
        "one directory, spelled twice"
    );
    let why = mac::login_refusal(false, Some(&other), Some(&login)).expect("another home");
    assert!(
        why.starts_with(&format!("HOME is {}", other.display())),
        "{why}"
    );
    let why = mac::login_refusal(true, Some(&login), Some(&login)).expect("a private root");
    assert!(why.contains("ATERM_STATE_HOME"), "{why}");
    assert_eq!(
        mac::login_refusal(false, None, Some(&login)),
        None,
        "no HOME to judge"
    );
    let why = mac::login_paths().expect_err("a test binary's private root");
    assert!(why.contains("ATERM_STATE_HOME"), "{why}");
}

/// A start finds the mug holding the lock; else, while this build's job runs a process that
/// holds none (a mug starting, or one ending after a Quit), it looks again until a mug holds
/// the lock or the job's process is gone and the job is started, and past its time takes the
/// job's process for the mug; a job it started it waits for until a mug holds the lock with
/// its label written, or its time is up ([`mac::ensure_with`]).
#[cfg(target_os = "macos")]
#[test]
fn a_start_waits_out_a_jobs_process_that_holds_no_lock() {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use crate::instance::Holder;
    use crate::launchd::Started;
    use mac::{Running, ensure_with};
    let later = Instant::now() + Duration::from_secs(60);
    let named = |pid| Holder {
        pid,
        label: Some("com.aterm.aterm.acaffeine.dev.0123abcd".to_string()),
    };
    let bounded = |waits: &Cell<u32>| {
        waits.set(waits.get() + 1);
        assert!(waits.get() < 10, "a wait that ends");
    };

    let (kicked, looks, waits) = (Cell::new(false), Cell::new(0), Cell::new(0));
    let ended = ensure_with(
        || kicked.get().then(|| named(9)),
        || {
            looks.set(looks.get() + 1);
            kicked.set(looks.get() == 3);
            Ok(if kicked.get() {
                Started::Kicked
            } else {
                Started::AlreadyRunning { pid: 7 }
            })
        },
        later,
        || bounded(&waits),
    );
    assert_eq!(
        ended,
        Ok(Running::Started),
        "the ending mug's job, started again"
    );
    assert_eq!((looks.get(), waits.get()), (3, 2));

    let (mugs, waits) = (Cell::new(0), Cell::new(0));
    let starting = ensure_with(
        || {
            mugs.set(mugs.get() + 1);
            (mugs.get() == 3).then(|| named(9))
        },
        || Ok(Started::AlreadyRunning { pid: 8 }),
        later,
        || bounded(&waits),
    );
    assert_eq!(
        starting,
        Ok(Running::Already(9)),
        "a starting mug takes the lock"
    );

    let (mugs, waits) = (Cell::new(0), Cell::new(0));
    let started = ensure_with(
        || {
            mugs.set(mugs.get() + 1);
            match mugs.get() {
                1 => None,
                2 => Some(Holder {
                    pid: 9,
                    label: None,
                }),
                _ => Some(named(9)),
            }
        },
        || Ok(Started::Bootstrapped),
        later,
        || bounded(&waits),
    );
    assert_eq!(started, Ok(Running::Started));
    assert_eq!(
        (mugs.get(), waits.get()),
        (3, 1),
        "the started job is waited for, a pause between looks, until its mug names its label"
    );

    let past = ensure_with(
        || None,
        || Ok(Started::AlreadyRunning { pid: 7 }),
        Instant::now(),
        || panic!("no wait past its time"),
    );
    assert_eq!(past, Ok(Running::Already(7)));
    for started in [Started::Kicked, Started::Bootstrapped] {
        assert_eq!(
            ensure_with(
                || None,
                || Ok(started),
                Instant::now(),
                || panic!("no wait")
            ),
            Ok(Running::Started),
            "a started job whose mug is not up in time"
        );
    }
    let held = ensure_with(
        || Some(named(5)),
        || panic!("the mug holding the lock runs"),
        later,
        || panic!("no wait"),
    );
    assert_eq!(held, Ok(Running::Already(5)));
    assert_eq!(
        ensure_with(|| None, || Err("refused".to_string()), later, || {}),
        Err("refused".to_string())
    );
}

/// One change of mode and its start or end of the mug at a time: a second waits out the
/// first's turn up to its time, and takes it once the first lets go; the turn's folder is
/// made if missing, and a folder that cannot be made says why, at once ([`mac::change_in`]).
#[cfg(target_os = "macos")]
#[test]
fn a_change_waits_for_anothers_turn() {
    use std::time::Duration;
    let dir = crate::test_scratch_dir("cli");
    let folder = dir.path().join("state").join("acaffeine");
    let first = mac::change_in(&folder, Duration::ZERO).expect("the first turn");
    assert!(folder.is_dir(), "made");
    let start = std::time::Instant::now();
    let Err(why) = mac::change_in(&folder, Duration::from_millis(200)) else {
        panic!("another's turn");
    };
    assert!(start.elapsed() >= Duration::from_millis(200), "waited");
    assert!(
        why.starts_with("another start or quit still held "),
        "{why}"
    );
    drop(first);
    assert!(
        mac::change_in(&folder, Duration::ZERO).is_ok(),
        "taken once the first let go"
    );
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"").expect("a file where the folder goes");
    let start = std::time::Instant::now();
    let Err(why) = mac::change_in(&blocked.join("acaffeine"), Duration::from_secs(60)) else {
        panic!("no folder");
    };
    assert!(start.elapsed() < Duration::from_secs(60), "no wait");
    assert!(why.starts_with(&blocked.display().to_string()), "{why}");
    assert!(!why.contains("still held"), "{why}");
}

/// A `serve` that will not run lets a bridge an update restart left go, and exits 0; one
/// that cannot find the mug's files touches none.
#[cfg(target_os = "macos")]
#[test]
fn a_serve_refusal_lets_a_left_bridge_go() {
    let dir = crate::test_scratch_dir("cli");
    let paths = crate::paths::Paths {
        state_dir: dir.path().join("state"),
        home: dir.path().join("home"),
        logs_dir: dir.path().join("logs"),
        local_volume: |_| true,
    };
    std::fs::create_dir_all(&paths.state_dir).expect("state dir");
    std::fs::write(paths.bridge_file(), "4242\n").expect("bridge file");
    assert_eq!(mac::refuse(None, "a test refusal"), ExitCode::SUCCESS);
    assert!(paths.bridge_file().exists());
    assert_eq!(
        mac::refuse(Some(&paths), "a test refusal"),
        ExitCode::SUCCESS
    );
    assert!(!paths.bridge_file().exists(), "the bridge was left holding");
}

#[cfg(target_os = "macos")]
fn scratch(dir: &std::path::Path) -> crate::paths::Paths {
    crate::paths::Paths {
        state_dir: dir.join("state"),
        home: dir.join("home"),
        logs_dir: dir.join("logs"),
        local_volume: |_| true,
    }
}

/// `serve`'s lock: one something that is no mug still holds is started again by launchd
/// (69), with why recorded and a bridge an update restart left holding on; a name that
/// cannot hold a lock at all (a directory, a link, a FIFO) ends 0 with why recorded, so
/// launchd does not start it every minute to the same refusal, copying the app each time.
#[cfg(target_os = "macos")]
#[test]
fn a_serve_that_cannot_take_the_lock_retries_only_what_can_clear_up() {
    let dir = crate::test_scratch_dir("cli");
    let paths = scratch(dir.path());
    std::fs::create_dir_all(&paths.state_dir).expect("state dir");
    std::fs::write(paths.bridge_file(), "4242\n").expect("bridge file");
    let held = crate::instance::Instance::try_hold(&paths.lock_file())
        .expect("hold")
        .expect("free");
    let code = mac::hold(&paths).expect_err("held by this test, which is no mug");
    assert_eq!(code, ExitCode::from(crate::launchd::EXIT_RETRY));
    let why = crate::trouble::read(&paths).expect("recorded").why;
    assert!(why.contains("which is no mug"), "{why}");
    assert!(paths.bridge_file().exists(), "the bridge holds on");
    drop(held);
    assert!(mac::hold(&paths).is_ok(), "a free lock is taken");

    type Plant = fn(&std::path::Path);
    let plants: [(&str, Plant); 3] = [
        ("a directory", |lock| {
            std::fs::create_dir(lock).expect("dir")
        }),
        ("a link", |lock| {
            let target = lock.with_extension("target");
            std::fs::write(&target, b"").expect("target");
            std::os::unix::fs::symlink(&target, lock).expect("link");
        }),
        ("a FIFO", |lock| {
            use std::os::unix::ffi::OsStrExt as _;
            let name = std::ffi::CString::new(lock.as_os_str().as_bytes()).expect("no NUL");
            // SAFETY: a NUL-terminated path the call reads and does not retain.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
        }),
    ];
    for (name, plant) in plants {
        let dir = crate::test_scratch_dir("cli");
        let paths = scratch(dir.path());
        std::fs::create_dir_all(&paths.state_dir).expect("state dir");
        plant(&paths.lock_file());
        let code = mac::hold(&paths).expect_err(name);
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "{name}: never started again by launchd"
        );
        let why = crate::trouble::read(&paths).map(|trouble| trouble.why);
        assert!(why.is_some(), "{name}: recorded for doctor and the window");
    }
}

/// The copy refusals no start again cures (a FIFO in the bundle, a link at the code
/// directory) are told from a full disk and a passing shortage, which launchd retries.
#[cfg(target_os = "macos")]
#[test]
fn a_copy_refusal_that_lasts_is_told_from_one_that_clears_up() {
    use std::io::{Error, ErrorKind};
    let dir = crate::test_scratch_dir("cli");
    let app = dir.path().join("X.app");
    let exe = app.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(exe.parent().expect("MacOS")).expect("bundle");
    std::fs::write(&exe, b"exe").expect("exe");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("elsewhere");
    let link = dir.path().join("code-link");
    std::os::unix::fs::symlink(&elsewhere, &link).expect("link");
    let refused = crate::code::prepare_copy(&exe, &link, "l").expect_err("a link");
    assert!(mac::lasts(&refused), "{refused}");
    let fifo = app.join("Contents/pipe");
    {
        use std::os::unix::ffi::OsStrExt as _;
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("no NUL");
        // SAFETY: a NUL-terminated path the call reads and does not retain.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
    }
    let refused = crate::code::prepare_copy(&exe, &dir.path().join("code"), "l")
        .expect_err("a FIFO in the bundle");
    assert!(mac::lasts(&refused), "{refused}");
    for clears in [
        Error::from(ErrorKind::StorageFull),
        Error::from_raw_os_error(libc::ENOSPC),
        Error::from_raw_os_error(libc::ENOENT),
        Error::from_raw_os_error(libc::ENOMEM),
    ] {
        assert!(!mac::lasts(&clears), "{clears}");
    }
}

/// Keeping this Mac awake (the window's watch, the converge pass) never undoes a person's
/// off: over a saved off it starts nothing, writes no mode and turns no Enable at Startup on.
/// With nothing saved it starts the mug and puts an installed app's job (here a
/// `~/Applications` install's) at every login; an Enable at Startup the person turned off
/// stays off; a dev build's is left as it is. 0.145's launch, which saved forever, fails it.
#[cfg(target_os = "macos")]
#[test]
fn keeping_awake_over_a_persons_off_starts_and_writes_nothing() {
    use std::cell::{Cell, RefCell};
    thread_local! {
        static STARTED: Cell<u32> = const { Cell::new(0) };
        static STARTUP: RefCell<Vec<bool>> = const { RefCell::new(Vec::new()) };
    }
    let doors = mac::KeepDoors {
        prune_orphans: |_| {},
        set_startup: |paths, label, origin, on| {
            STARTUP.with_borrow_mut(|calls| calls.push(on));
            crate::launchd::set_startup(paths, label, origin, on)
        },
        ensure: |_, _, _| {
            STARTED.set(STARTED.get() + 1);
            Ok(mac::Running::Started)
        },
    };
    let at = |dir: &std::path::Path, installed: bool| {
        let paths = scratch(dir);
        let origin = if installed {
            paths
                .home
                .join("Applications/aterm.app/Contents/MacOS/aterm")
        } else {
            dir.join("worktree/target/debug/aterm")
        };
        std::fs::create_dir_all(origin.parent().expect("a directory")).expect("mkdir");
        std::fs::write(&origin, b"exe").expect("exe");
        mac::Here {
            label: crate::launchd::label_for(&origin),
            paths,
            origin,
        }
    };
    let calls = || (STARTED.get(), STARTUP.with_borrow(Clone::clone));
    let reset = || {
        STARTED.set(0);
        STARTUP.set(Vec::new());
    };

    let dir = crate::test_scratch_dir("cli");
    let here = at(dir.path(), true);
    let file = here.paths.state_file();
    crate::state::save(&file, Mode::Off).expect("the person's off");
    let saved = std::fs::read(&file).expect("saved");
    reset();
    assert_eq!(mac::keep_awake_at(&here, NOW, &doors), Kept::Off);
    assert_eq!(
        calls(),
        (0, Vec::new()),
        "nothing started, no startup turned on"
    );
    assert_eq!(
        std::fs::read(&file).expect("kept"),
        saved,
        "no mode written"
    );
    assert!(!here.paths.startup_plist(&here.label).exists());

    let dir = crate::test_scratch_dir("cli");
    let here = at(dir.path(), true);
    reset();
    assert_eq!(mac::keep_awake_at(&here, NOW, &doors), Kept::Running(None));
    assert_eq!(calls(), (1, vec![true]), "started, and put at every login");
    assert!(here.paths.startup_plist(&here.label).is_file());
    assert!(!here.paths.state_file().exists(), "no mode written");

    let dir = crate::test_scratch_dir("cli");
    let here = at(dir.path(), true);
    crate::launchd::choose_startup(&here.paths, &here.label, &here.origin, false)
        .expect("the person's startup off");
    reset();
    assert_eq!(mac::keep_awake_at(&here, NOW, &doors), Kept::Running(None));
    assert_eq!(
        calls(),
        (1, Vec::new()),
        "the person's startup off stays off"
    );

    let dir = crate::test_scratch_dir("cli");
    let here = at(dir.path(), false);
    reset();
    assert_eq!(mac::keep_awake_at(&here, NOW, &doors), Kept::Running(None));
    assert_eq!(
        calls(),
        (1, Vec::new()),
        "a dev build's job is not put at login"
    );
}

/// The converge pass's keep-awake retires the old `aterm caffeine` name's login jobs no aterm
/// will run again (here one whose aterm is gone and one whose aterm says it has no old verb)
/// before anything else, whatever the installed app or the mode, so a pass that starts the mug
/// on a Mac where no window opens after a restart stops them too; then it keeps the installed
/// app's mug as before.
#[cfg(target_os = "macos")]
#[test]
fn the_converge_pass_retires_the_old_names_jobs_whatever_it_keeps() {
    use std::cell::Cell;
    thread_local! {
        static STARTED: Cell<u32> = const { Cell::new(0) };
    }
    let keep = mac::KeepDoors {
        prune_orphans: |_| {},
        set_startup: |_, _, _, _| Ok(()),
        ensure: |_, _, _| {
            STARTED.set(STARTED.get() + 1);
            Ok(mac::Running::Started)
        },
    };
    let doors = mac::InstalledDoors {
        retire_old: |paths| {
            crate::legacy::retire_jobs_with(
                paths,
                std::path::Path::new("/nonexistent/this-aterm"),
                |_| Some(false),
                |_| crate::launchd::Unload::Unloaded,
            )
            .failed
        },
        installed: |paths| {
            crate::launchd::Installed::At(
                paths
                    .home
                    .join("Applications/aterm.app/Contents/MacOS/aterm"),
            )
        },
        now: || Ok(NOW),
        keep,
    };
    let absent = mac::InstalledDoors {
        installed: |_| crate::launchd::Installed::Absent("not installed".to_string()),
        ..doors
    };
    let old_jobs = |dir: &std::path::Path| {
        let paths = scratch(dir);
        std::fs::create_dir_all(paths.launch_agents()).expect("agents");
        let other = dir.join("0.145.0/aterm");
        std::fs::create_dir_all(other.parent().expect("a folder")).expect("mkdir");
        std::fs::write(&other, b"exe").expect("exe");
        let plists = [
            ("com.aterm.aterm.caffeine", other),
            (
                "com.aterm.aterm.caffeine.dev.0000000a",
                dir.join("gone/aterm"),
            ),
        ]
        .map(|(label, program)| {
            let text = crate::launchd::plist_text(label, &program, &paths.log_file(), None)
                .replace("<string>acaffeine</string>", "<string>caffeine</string>");
            let plist = paths.startup_plist(label);
            std::fs::write(&plist, text).expect("plist");
            plist
        });
        (paths, plists)
    };
    let retired = |plists: &[std::path::PathBuf]| plists.iter().all(|p| !p.exists());

    let dir = crate::test_scratch_dir("cli");
    let (paths, plists) = old_jobs(dir.path());
    STARTED.set(0);
    assert_eq!(
        mac::keep_installed_awake_with(paths, &absent),
        Kept::NotHere("not installed".to_string())
    );
    assert!(retired(&plists), "retired with no installed app");
    assert_eq!(STARTED.get(), 0);

    let dir = crate::test_scratch_dir("cli");
    let (paths, plists) = old_jobs(dir.path());
    crate::state::save(&paths.state_file(), Mode::Off).expect("the person's off");
    STARTED.set(0);
    assert_eq!(mac::keep_installed_awake_with(paths, &doors), Kept::Off);
    assert!(retired(&plists), "retired over the person's off");
    assert_eq!(STARTED.get(), 0);

    let dir = crate::test_scratch_dir("cli");
    let (paths, plists) = old_jobs(dir.path());
    STARTED.set(0);
    assert_eq!(
        mac::keep_installed_awake_with(paths, &doors),
        Kept::Running(None)
    );
    assert!(retired(&plists), "retired, and the mug started");
    assert_eq!(STARTED.get(), 1);
}

/// When a mug already runs, `sync.awake` reads and retires the old `aterm caffeine` login jobs
/// no aterm will run again without starting a mug, while keeping an 0.144 job whose `aterm`
/// still answers `help caffeine` (`Some(true)`).
#[cfg(target_os = "macos")]
#[test]
fn retired_old_jobs_reads_and_retires_only_dead_or_post_rename_login_items() {
    use std::cell::RefCell;
    let dir = crate::test_scratch_dir("cli");
    let paths = scratch(dir.path());
    std::fs::create_dir_all(paths.launch_agents()).expect("agents");
    let post_rename = dir.path().join("0.145.0/aterm");
    let pre_rename = dir.path().join("0.144.0/aterm");
    for bin in [&post_rename, &pre_rename] {
        std::fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
        std::fs::write(bin, b"exe").expect("exe");
    }
    for (label, program) in [
        ("com.aterm.aterm.caffeine", post_rename.clone()),
        (
            "com.aterm.aterm.caffeine.dev.0000000a",
            dir.path().join("gone/aterm"),
        ),
        ("com.aterm.aterm.caffeine.dev.0000000b", pre_rename.clone()),
    ] {
        let text = crate::launchd::plist_text(label, &program, &paths.log_file(), None)
            .replace("<string>acaffeine</string>", "<string>caffeine</string>");
        std::fs::write(paths.startup_plist(label), text).expect("plist");
    }
    let this_exe = std::path::Path::new("/nonexistent/this-aterm");
    let has_verb = |program: &std::path::Path| Some(program == pre_rename);
    let found = mac::retired_old_jobs_with(&paths, this_exe, has_verb);
    assert_eq!(
        found,
        [
            "com.aterm.aterm.caffeine",
            "com.aterm.aterm.caffeine.dev.0000000a"
        ]
    );
    assert!(
        paths.startup_plist("com.aterm.aterm.caffeine").exists(),
        "reading retired jobs writes nothing"
    );
    let unloaded = RefCell::new(Vec::new());
    let said = mac::retire_old_jobs_only_with(&paths, this_exe, has_verb, |label| {
        unloaded.borrow_mut().push(label.to_owned());
        crate::launchd::Unload::Unloaded
    })
    .expect("retires dead and 0.145.0 jobs");
    assert_eq!(
        said,
        "retired old `aterm caffeine` login items: com.aterm.aterm.caffeine, \
         com.aterm.aterm.caffeine.dev.0000000a"
    );
    assert_eq!(
        *unloaded.borrow(),
        [
            "com.aterm.aterm.caffeine",
            "com.aterm.aterm.caffeine.dev.0000000a"
        ]
    );
    assert!(!paths.startup_plist("com.aterm.aterm.caffeine").exists());
    assert!(
        !paths
            .startup_plist("com.aterm.aterm.caffeine.dev.0000000a")
            .exists()
    );
    assert!(
        paths
            .startup_plist("com.aterm.aterm.caffeine.dev.0000000b")
            .exists(),
        "NEGATIVE CONTROL: 0.144 job whose aterm still has `caffeine` is kept"
    );
    assert_eq!(
        mac::retired_old_jobs_with(&paths, this_exe, has_verb),
        Vec::<String>::new()
    );
}

/// An exceptions entry an earlier process raised (before a quit, or an update's relaunch) is
/// cleared by the next process's watch once the trouble is gone, though that process never
/// raised it; a lasting trouble is raised once for each why; a process that is not the
/// login's says nothing.
#[test]
fn the_entry_an_earlier_process_raised_is_cleared_after_a_relaunch() {
    let why = "the menu bar refused the mug".to_string();
    let mut before = EntryWatch::default();
    let running = Kept::Running(Some(7));
    assert_eq!(
        before.step(&running, Some(why.clone())),
        Some(EntryWord::Raise(why.clone()))
    );
    assert!(before.raised());
    assert_eq!(before.step(&running, Some(why)), None, "raised once");
    assert_eq!(
        before.step(&running, Some("the disk is full".to_string())),
        Some(EntryWord::Raise("the disk is full".to_string())),
        "a new why replaces it"
    );
    drop(before);
    for kept in [
        Kept::Running(Some(8)),
        Kept::Running(None),
        Kept::Off,
        Kept::Failed("launchctl bootstrap: 5".to_string()),
    ] {
        let mut relaunched = EntryWatch::default();
        assert_eq!(
            relaunched.step(&kept, None),
            Some(EntryWord::Clear),
            "{kept:?}"
        );
        assert!(!relaunched.raised());
    }
    let mut elsewhere = EntryWatch::default();
    assert_eq!(
        elsewhere.step(&Kept::NotHere("a private root".to_string()), None),
        None
    );
}

/// An ordinary launch never undoes a person's off: a saved off starts nothing and turns
/// nothing on, an Enable at Startup the person turned off stays off, and only the installed
/// app's job is put at every login.
#[test]
fn a_launch_keeps_a_persons_off() {
    let plan = |on, installed, startup_on, turned_off| {
        let plan = keep_plan(on, installed, startup_on, turned_off);
        (plan.start, plan.turn_startup_on)
    };
    assert_eq!(
        plan(true, true, false, false),
        (true, true),
        "installed, nothing chosen"
    );
    assert_eq!(plan(true, true, true, false), (true, false), "on already");
    assert_eq!(
        plan(true, true, false, true),
        (true, false),
        "the person's startup off"
    );
    assert_eq!(
        plan(true, false, false, false),
        (true, false),
        "a dev build's job"
    );
    for installed in [true, false] {
        assert_eq!(
            plan(false, installed, false, false),
            (false, false),
            "the person's off"
        );
    }
}

/// The window raises a trouble once it has lasted, and never one of a person's off or of a
/// process that is not the login's.
#[test]
fn only_a_lasting_trouble_of_a_kept_mug_is_raised() {
    let trouble = crate::trouble::Trouble {
        since: 1_000,
        why: "the menu bar refused the mug".to_string(),
    };
    let lasted = 1_000 + crate::trouble::LASTING_SECS;
    for kept in [
        Kept::Running(Some(7)),
        Kept::Failed("launchctl".to_string()),
    ] {
        assert_eq!(
            lasting_trouble(&kept, Some(&trouble), lasted),
            Some(trouble.why.clone()),
            "{kept:?}"
        );
        assert_eq!(lasting_trouble(&kept, Some(&trouble), lasted - 1), None);
        assert_eq!(lasting_trouble(&kept, None, lasted), None);
    }
    for kept in [Kept::Off, Kept::NotHere("a private root".to_string())] {
        assert_eq!(
            lasting_trouble(&kept, Some(&trouble), lasted),
            None,
            "{kept:?}"
        );
    }
}

#[test]
fn every_command_parses() {
    assert_eq!(parsed(""), Ok(Command::Start));
    assert_eq!(parsed("start"), Ok(Command::Start));
    assert_eq!(parsed("on"), Ok(Command::On { span: None }));
    assert_eq!(parsed("off"), Ok(Command::Off));
    assert_eq!(parsed("status"), Ok(Command::Status));
    assert_eq!(parsed("lid"), Ok(Command::Lid(None)));
    assert_eq!(parsed("lid on"), Ok(Command::Lid(Some(true))));
    assert_eq!(parsed("lid off"), Ok(Command::Lid(Some(false))));
    assert!(parsed("lid yes").is_err());
    assert!(parsed("lid on now").is_err());
    assert_eq!(parsed("startup"), Ok(Command::Startup(None)));
    assert_eq!(parsed("startup on"), Ok(Command::Startup(Some(true))));
    assert_eq!(parsed("startup off"), Ok(Command::Startup(Some(false))));
    assert_eq!(parsed("quit"), Ok(Command::Quit { own: false }));
    assert_eq!(parsed("quit own"), Ok(Command::Quit { own: true }));
    assert!(parsed("quit all").is_err());
    assert_eq!(parsed("serve"), Ok(Command::Serve));
    assert_eq!(parsed("bridge"), Ok(Command::Bridge));
    assert!(parsed("bridge now").is_err());
}

/// `on` hands its word to `crate::model::parse_span`, whose own tests hold the table.
#[test]
fn on_takes_a_span_word() {
    assert_eq!(
        parsed("on 8h"),
        Ok(Command::On {
            span: Some(8 * HOUR)
        })
    );
    let why = parsed("on 1x").expect_err("not a span");
    assert!(why.contains("\"1x\""), "{why}");
}

#[test]
fn help_is_asked_for_by_word_or_flag() {
    for line in [
        "-h",
        "--help",
        "help",
        "on --help",
        "status -h",
        "bogus --help",
    ] {
        assert_eq!(parsed(line), Ok(Command::Help), "{line:?}");
    }
    assert!(parsed("startup help").is_err());
}

#[test]
fn any_option_is_refused() {
    for line in [
        "--aterm-front-door-routing-probe",
        "-x",
        "on -1h",
        "status --json",
        "serve --foreground",
        "-",
        "--",
    ] {
        let why = parsed(line).expect_err(line);
        assert!(why.starts_with("unknown option "), "{line:?}: {why}");
    }
}

#[test]
fn unknown_words_and_extra_operands_are_refused() {
    for line in ["bogus", "ON"] {
        let why = parsed(line).expect_err(line);
        assert!(why.starts_with("unknown command "), "{line:?}: {why}");
    }
    for line in [
        "off now",
        "start now",
        "on 1h 2h",
        "status x",
        "startup on off",
        "quit own x",
        "serve x",
    ] {
        let why = parsed(line).expect_err(line);
        assert!(why.starts_with("unexpected argument "), "{line:?}: {why}");
    }
    let why = parsed("startup maybe").expect_err("maybe");
    assert!(why.starts_with("startup takes "), "{why}");
    let why = parsed("quit x").expect_err("x");
    assert!(why.starts_with("quit takes own"), "{why}");
}

#[cfg(unix)]
#[test]
fn a_word_that_is_not_utf8_is_unknown() {
    use std::os::unix::ffi::OsStringExt as _;
    let word = OsString::from_vec(vec![b'o', 0xff]);
    let why = parse(&[word]).expect_err("not a command");
    assert!(why.starts_with("unknown command "), "{why}");
}

/// A refused command line ends `main_entry` with 2 before any platform work: the front
/// door's routing probe passes an option no verb knows.
#[test]
fn a_refused_command_line_exits_2_from_the_entry() {
    for line in [
        "--aterm-front-door-routing-probe",
        "bogus",
        "off now",
        "on 0h",
        "startup maybe",
        "serve --foreground",
    ] {
        assert_eq!(
            main_entry(words(line), "0.0.0"),
            ExitCode::from(2),
            "{line:?}"
        );
    }
}

/// A word from the command line reaches the error line escaped: one line, no escape
/// sequence a terminal would act on.
#[test]
fn the_error_line_is_one_line_naming_help() {
    let word = OsString::from("bo\ngus\u{1b}[2J");
    let line = usage_error(&parse(&[word]).expect_err("bad"));
    assert!(!line.contains('\n'), "{line}");
    assert!(!line.contains('\u{1b}'), "{line}");
    assert!(line.starts_with("aterm acaffeine: "), "{line}");
    assert!(line.contains("aterm acaffeine --help"), "{line}");
}

const NOW: Clocks = Clocks {
    since_boot: 300_000,
    wall: 1_790_000_000,
};

#[test]
fn status_says_the_mode_the_mug_startup_and_label() {
    assert_eq!(
        status_text(Mode::Off, NOW, None, Startup::Off, "l.x", false),
        "acaffeine=off\nuntil=-\nmenu=stopped\nstartup=off\nlabel=l.x\nlid=off\n"
    );
    assert_eq!(
        status_text(Mode::Forever, NOW, None, Startup::On, "l.x", false),
        "acaffeine=off\nuntil=-\nmenu=stopped\nstartup=on\nlabel=l.x\nlid=off\n"
    );
    assert_eq!(
        status_text(Mode::Forever, NOW, Some(42), Startup::Blocked, "l.x", false),
        "acaffeine=on\nuntil=forever\nmenu=running\npid=42\nstartup=blocked\nlabel=l.x\nlid=off\n"
    );
    let timed = Mode::activate(Some(HOUR), NOW);
    assert_eq!(
        status_text(timed, NOW, Some(7), Startup::Off, "l.x", false),
        format!(
            "acaffeine=on\nuntil={}\nmenu=running\npid=7\nstartup=off\nlabel=l.x\nlid=off\n",
            NOW.wall + HOUR
        )
    );
    assert_eq!(
        status_text(timed, NOW + HOUR, Some(7), Startup::Off, "l.x", false),
        "acaffeine=off\nuntil=-\nmenu=running\npid=7\nstartup=off\nlabel=l.x\nlid=off\n"
    );
    assert_eq!(
        status_text(Mode::Forever, NOW, Some(42), Startup::On, "l.x", true),
        "acaffeine=on\nuntil=forever\nmenu=running\npid=42\nstartup=on\nlabel=l.x\nlid=on\n"
    );
    assert_eq!(lid_line(true), "lid=on\n");
    assert_eq!(lid_line(false), "lid=off\n");
}

/// Ten minutes into `on 1h`, the wall clock set back or forward a day: `status` says the
/// span is on and ends 50 minutes from the clock as it reads now.
#[test]
fn status_says_the_end_as_the_wall_clock_reads_now() {
    let timed = Mode::activate(Some(HOUR), NOW);
    for wall in [NOW.wall - 24 * HOUR + 600, NOW.wall + 24 * HOUR + 600] {
        let later = Clocks {
            since_boot: NOW.since_boot + 600,
            wall,
        };
        assert_eq!(
            status_text(timed, later, Some(7), Startup::Off, "l.x", false),
            format!(
                "acaffeine=on\nuntil={}\nmenu=running\npid=7\nstartup=off\nlabel=l.x\nlid=off\n",
                wall + HOUR - 600
            )
        );
    }
}

#[test]
fn quit_own_leaves_another_builds_mug_running_or_between_runs() {
    let running = |label: Option<&str>| crate::instance::Holder {
        pid: 7,
        label: label.map(str::to_owned),
    };
    assert!(!another_builds_mug(
        Some(&running(Some("l.here"))),
        None,
        "l.here"
    ));
    assert!(another_builds_mug(
        Some(&running(Some("l.old"))),
        None,
        "l.here"
    ));
    assert!(another_builds_mug(Some(&running(None)), None, "l.here"));
    assert!(!another_builds_mug(
        Some(&running(Some("l.here"))),
        Some("l.old"),
        "l.here"
    ));
    assert!(another_builds_mug(None, Some("l.old"), "l.here"));
    assert!(!another_builds_mug(None, Some("l.here"), "l.here"));
    assert!(!another_builds_mug(None, None, "l.here"));
}

#[test]
fn on_and_startup_answer_in_one_line() {
    assert_eq!(
        on_line(Mode::Forever, NOW),
        "acaffeine=requested until=forever\n"
    );
    assert_eq!(
        on_line(Mode::activate(Some(60), NOW), NOW),
        format!("acaffeine=requested until={}\n", NOW.wall + 60)
    );
    assert_eq!(startup_line(Startup::On), "startup=on\n");
    assert_eq!(startup_line(Startup::Off), "startup=off\n");
    assert_eq!(startup_line(Startup::Blocked), "startup=blocked\n");
    assert_eq!(startup_line(Startup::Disabled), "startup=disabled\n");
    assert_eq!(until_word(Mode::Off, NOW), "-");
}
