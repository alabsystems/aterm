// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(unix)]
use crate::launchd::{Unjudged, Unload};

#[test]
fn only_a_label_the_old_name_made_is_one() {
    assert!(is_label(INSTALLED_LABEL));
    assert!(is_label("com.aterm.aterm.caffeine.dev.0123abcd"));
    for bad in [
        "",
        crate::launchd::INSTALLED_LABEL,
        "com.aterm.aterm.acaffeine.dev.0123abcd",
        "com.aterm.aterm.caffeine.dev",
        "com.aterm.aterm.caffeine.dev.0123ABCD",
        "com.aterm.aterm.caffeine.dev.0123abc",
        "com.aterm.aterm.caffeine.x",
    ] {
        assert!(!is_label(bad), "{bad:?}");
    }
}

#[test]
fn only_serve_alone_is_the_old_jobs_command() {
    let words = |w: &[&str]| w.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(is_serve(&words(&["serve"])));
    for other in [
        &[][..],
        &["status"],
        &["serve", "x"],
        &["bridge"],
        &["--help"],
    ] {
        assert!(!is_serve(&words(other)), "{other:?}");
    }
}

#[cfg(unix)]
fn scratch(root: &Path) -> Paths {
    Paths {
        state_dir: root.join("state"),
        home: root.join("home"),
        logs_dir: root.join("logs"),
        local_volume: |_| true,
    }
}

/// An old build's login plist for `label`, running `program caffeine serve`.
#[cfg(unix)]
fn plant(paths: &Paths, label: &str, program: &Path) -> PathBuf {
    let text = crate::launchd::plist_text(label, program, &paths.log_file(), None)
        .replace("<string>acaffeine</string>", "<string>caffeine</string>");
    assert!(text.contains("<string>caffeine</string>"), "{text}");
    let plist = paths.startup_plist(label);
    std::fs::create_dir_all(paths.launch_agents()).expect("agents");
    std::fs::write(&plist, text).expect("plist");
    plist
}

/// `paths` with `~/Library/LaunchAgents` read as on no disk of this Mac: a network home.
#[cfg(unix)]
fn network(paths: &Paths) -> Paths {
    Paths {
        local_volume: |_| false,
        ..paths.clone()
    }
}

/// The old job `label` under `paths`, running `program`, kept on a network home, with what its
/// unload did (`None` where its program is here and nothing was unloaded).
#[cfg(unix)]
fn kept_job(paths: &Paths, label: &str, program: &Path, unload: Option<Unload>) -> Unjudged {
    Unjudged {
        label: label.to_string(),
        plist: paths.startup_plist(label),
        program: program.to_path_buf(),
        unload,
    }
}

/// Whether `lines` are one kept line ([`kept_on_shared_home`]) for each of `jobs`, its label,
/// plist, program and what its unload did, ending with what that unload did ([`unload_said`])
/// where there was one, and nothing else.
#[cfg(unix)]
fn says_kept(lines: &[String], jobs: &[Unjudged]) -> bool {
    lines.len() == jobs.len()
        && jobs.iter().all(|job| {
            let line = kept_on_shared_home(job);
            job.unload
                .as_ref()
                .is_none_or(|unload| line.ends_with(&unload_said(unload)))
                && lines.iter().filter(|said| **said == line).count() == 1
        })
}

/// A kept line names the job and its program, and, for a program gone here, ends with what
/// the unload did, a clause apart for each outcome: an idle job unloaded, one whose process
/// still runs left loaded (naming that process), one launchd had not loaded, and a failure
/// (saying why). A program here adds nothing.
#[test]
#[cfg(unix)]
fn a_kept_line_says_what_the_unload_did() {
    let root = crate::test_scratch_dir("legacy");
    let paths = network(&scratch(root.path()));
    let label = "com.aterm.aterm.caffeine.dev.0000000a";
    let program = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let here = kept_on_shared_home(&kept_job(&paths, label, program, None));
    assert!(here.starts_with(&format!("kept {label} ")), "{here}");
    assert!(
        here.contains(&paths.startup_plist(label).display().to_string())
            && here.contains(&program.display().to_string()),
        "{here}"
    );
    let outcomes = [
        Unload::Unloaded,
        Unload::Running(4242),
        Unload::NotLoaded,
        Unload::Failed("launchctl refused".to_string()),
    ];
    let said: Vec<String> = outcomes.iter().map(unload_said).collect();
    for (unload, clause) in outcomes.iter().zip(&said) {
        let line = kept_on_shared_home(&kept_job(&paths, label, program, Some(unload.clone())));
        assert_eq!(line, format!("{here}{clause}"), "{unload:?}");
        assert!(!clause.is_empty(), "{unload:?}");
    }
    for (i, a) in said.iter().enumerate() {
        for b in &said[i + 1..] {
            assert_ne!(a, b, "each outcome is told apart");
        }
    }
    assert!(
        said[1].contains("4242"),
        "a job left loaded names its process"
    );
    assert!(!said[0].contains("4242"), "one unloaded has none");
    assert!(said[3].contains("launchctl refused"), "a failure says why");
}

#[test]
#[cfg(unix)]
fn an_old_login_job_retires_when_its_program_is_gone_or_this_aterm() {
    let root = crate::test_scratch_dir("legacy");
    let paths = scratch(root.path());
    let exe = root.path().join("aterm");
    std::fs::write(&exe, "this aterm").expect("exe");
    let link = root.path().join("aterm-link");
    std::os::unix::fs::symlink(&exe, &link).expect("symlink");
    let other = root.path().join("other-aterm");
    std::fs::write(&other, "another build").expect("exe");
    let gone = root.path().join("removed-worktree/aterm");
    let ejected = Path::new("/Volumes/Ejected/aterm.app/Contents/MacOS/aterm");
    let asked = std::cell::RefCell::new(Vec::new());
    let has_verb = |program: &Path| {
        asked.borrow_mut().push(program.to_path_buf());
        (program == other).then_some(true)
    };
    assert_eq!(
        retired_jobs_with(&paths, &exe, has_verb),
        [],
        "no LaunchAgents at all"
    );

    let mine = plant(&paths, INSTALLED_LABEL, &exe);
    let linked = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000a", &link);
    let orphan = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000b", &gone);
    plant(&paths, "com.aterm.aterm.caffeine.dev.0000000c", &other);
    plant(&paths, "com.aterm.aterm.caffeine.dev.0000000d", ejected);
    plant(&paths, "com.example.other", &exe);
    let new_name = crate::launchd::plist_text(
        crate::launchd::INSTALLED_LABEL,
        &exe,
        &paths.log_file(),
        None,
    );
    std::fs::write(
        paths.startup_plist(crate::launchd::INSTALLED_LABEL),
        new_name,
    )
    .expect("plist");
    let status = paths.startup_plist("com.aterm.aterm.caffeine.dev.0000000e");
    std::fs::write(
        &status,
        std::fs::read_to_string(&mine)
            .expect("plist")
            .replace("<string>serve</string>", "<string>status</string>"),
    )
    .expect("plist");
    let planted = paths.startup_plist("com.aterm.aterm.caffeine.dev.0000000f");
    std::os::unix::fs::symlink(&mine, &planted).expect("symlink");

    assert_eq!(
        retired_jobs_with(&paths, &exe, has_verb),
        [
            (INSTALLED_LABEL.to_string(), mine),
            ("com.aterm.aterm.caffeine.dev.0000000a".to_string(), linked),
            ("com.aterm.aterm.caffeine.dev.0000000b".to_string(), orphan),
        ],
        "the job of another build with the old verb, one on a volume, the new name's, a \
         status job and a link stay"
    );
    let mut asked = asked.into_inner();
    asked.sort();
    let mut others = vec![ejected.to_path_buf(), other];
    others.sort();
    assert_eq!(
        asked, others,
        "only another program is asked, never this aterm or one that is gone"
    );

    let in_home = plant(
        &paths,
        "com.aterm.aterm.caffeine.dev.00000010",
        &paths.home.join("worktree/aterm"),
    );
    let answers_no = paths.home.join("Applications/aterm");
    std::fs::create_dir_all(answers_no.parent().expect("a folder")).expect("mkdir");
    std::fs::write(&answers_no, "0.145.0").expect("exe");
    let without_verb = plant(&paths, "com.aterm.aterm.caffeine.dev.00000011", &answers_no);
    let asked = std::cell::RefCell::new(Vec::new());
    let says_no = |program: &Path| {
        asked.borrow_mut().push(program.to_path_buf());
        Some(false)
    };
    assert_eq!(
        retired_jobs_with(&network(&paths), &exe, says_no),
        [
            ("com.aterm.aterm.caffeine.dev.00000010".to_string(), in_home),
            (
                "com.aterm.aterm.caffeine.dev.00000011".to_string(),
                without_verb
            ),
        ],
        "on a network home only a program inside it is judged: one outside may be another \
         Mac's"
    );
    assert_eq!(
        asked.into_inner(),
        [answers_no],
        "no program outside a network home is run to ask"
    );
}

/// An old login job whose program is another aterm that says it has no old verb (0.145.0,
/// whose job exits 2 and is started again every ten seconds) retires. One whose program still
/// has the old verb (0.144.0's own mug) stays, and so does one whose program gives no answer.
#[test]
#[cfg(unix)]
fn an_old_login_job_of_another_aterm_without_the_old_verb_retires() {
    let root = crate::test_scratch_dir("legacy");
    let paths = scratch(root.path());
    let exe = root.path().join("aterm");
    std::fs::write(&exe, "this aterm").expect("exe");
    let program = |name: &str| {
        let program = root.path().join(name).join("aterm");
        std::fs::create_dir_all(program.parent().expect("a folder")).expect("mkdir");
        std::fs::write(&program, name).expect("exe");
        program
    };
    let no_verb = program("0.145.0");
    let old_verb = program("0.144.0");
    let silent = program("hung");
    let retired = plant(&paths, INSTALLED_LABEL, &no_verb);
    let mug = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000a", &old_verb);
    let unanswered = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000b", &silent);
    let has_verb = |program: &Path| {
        if program == no_verb {
            Some(false)
        } else if program == old_verb {
            Some(true)
        } else {
            None
        }
    };
    assert_eq!(
        retired_jobs_with(&paths, &exe, has_verb),
        [(INSTALLED_LABEL.to_string(), retired.clone())]
    );

    let unloaded = std::cell::RefCell::new(Vec::new());
    let failed = tidy_with(&paths, &exe, has_verb, |label| {
        unloaded.borrow_mut().push(label.to_string());
        Unload::Unloaded
    });
    assert_eq!(failed, Vec::<String>::new());
    assert!(!retired.exists(), "the job that can only fail goes");
    assert!(mug.is_file(), "an old build's own mug stays");
    assert!(unanswered.is_file(), "no answer keeps it");
    assert_eq!(unloaded.into_inner(), [INSTALLED_LABEL]);
}

/// The live probe asks the old name's help topic: 0.145.0 answers yes to `help acaffeine`,
/// so a probe of the new name would keep the job that exits 2 every ten seconds.
#[cfg(unix)]
#[test]
fn the_old_verb_probe_asks_help_caffeine() {
    assert_eq!(old_verb_probe_args(), ["help", "caffeine"]);
}

/// A copy of aterm in the old code folder: one a mug said it ran from, or one macOS may still
/// be assessing (new, with no such word).
#[cfg(unix)]
fn copy(paths: &Paths, name: &str, ran: bool) -> PathBuf {
    let copy = code_dir(paths).join("l").join(name);
    std::fs::create_dir_all(copy.join("aterm.app")).expect("copy");
    if ran {
        std::fs::write(copy.join(aterm_uds::deadline::RAN), "").expect("ran");
    }
    copy
}

#[test]
#[cfg(unix)]
fn tidy_retires_the_old_jobs_and_discards_the_old_copies() {
    let root = crate::test_scratch_dir("legacy");
    let paths = scratch(root.path());
    let exe = root.path().join("aterm");
    std::fs::write(&exe, "this aterm").expect("exe");
    let other = root.path().join("other-aterm");
    std::fs::write(&other, "another build").expect("exe");
    let mine = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000a", &exe);
    let orphan = plant(
        &paths,
        "com.aterm.aterm.caffeine.dev.0000000b",
        &root.path().join("gone/aterm"),
    );
    let kept = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000c", &other);
    let ran = copy(&paths, "1-1", true);
    let state = state_file(&paths);
    std::fs::write(&state, "mode=on\n").expect("state");

    let unloaded = std::cell::RefCell::new(Vec::new());
    let has_verb = |program: &Path| {
        assert_eq!(program, other, "only another program is asked");
        Some(true)
    };
    let failed = tidy_with(&paths, &exe, has_verb, |label| {
        unloaded.borrow_mut().push(label.to_string());
        Unload::Unloaded
    });
    assert_eq!(failed, Vec::<String>::new());
    assert!(!mine.exists() && !orphan.exists(), "both retired");
    assert!(
        kept.is_file(),
        "another build with the old verb keeps its job"
    );
    assert_eq!(
        unloaded.into_inner(),
        [
            "com.aterm.aterm.caffeine.dev.0000000a",
            "com.aterm.aterm.caffeine.dev.0000000b"
        ],
        "each retired job, and no other, is unloaded"
    );
    assert!(!ran.exists() && !code_dir(&paths).exists(), "the copies go");
    assert!(state.is_file(), "an old build writes it without the lock");
    assert_eq!(std::fs::read(lock_file(&paths)).expect("lock"), b"");

    let mine = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000a", &exe);
    let gone = root.path().join("gone/aterm");
    let elsewhere = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000b", &gone);
    let in_home = plant(
        &paths,
        "com.aterm.aterm.caffeine.dev.0000000d",
        &paths.home.join("gone/aterm"),
    );
    let removed = root.path().join("removed-app/aterm");
    let running = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000e", &removed);
    let unloaded = std::cell::RefCell::new(Vec::new());
    let failed = tidy_with(
        &network(&paths),
        &exe,
        |program| {
            panic!(
                "{} is outside a network home, never asked",
                program.display()
            )
        },
        |label| {
            unloaded.borrow_mut().push(label.to_string());
            if label == "com.aterm.aterm.caffeine.dev.0000000e" {
                Unload::Running(4242)
            } else {
                Unload::Unloaded
            }
        },
    );
    assert!(
        says_kept(
            &failed,
            &[
                kept_job(&paths, "com.aterm.aterm.caffeine.dev.0000000a", &exe, None),
                kept_job(
                    &paths,
                    "com.aterm.aterm.caffeine.dev.0000000b",
                    &gone,
                    Some(Unload::Unloaded)
                ),
                kept_job(
                    &paths,
                    "com.aterm.aterm.caffeine.dev.0000000c",
                    &other,
                    None
                ),
                kept_job(
                    &paths,
                    "com.aterm.aterm.caffeine.dev.0000000e",
                    &removed,
                    Some(Unload::Running(4242))
                ),
            ]
        ),
        "on a network home each job whose program lies outside it is kept and said to be, \
         once each tidy, and one whose program is gone here with what its unload did: an \
         idle one unloaded, one whose process still runs left loaded: {failed:#?}"
    );
    assert!(mine.is_file() && elsewhere.is_file() && kept.is_file() && running.is_file());
    assert!(!in_home.exists(), "a job inside the home is judged");
    assert_eq!(
        unloaded.into_inner(),
        [
            "com.aterm.aterm.caffeine.dev.0000000d",
            "com.aterm.aterm.caffeine.dev.0000000b",
            "com.aterm.aterm.caffeine.dev.0000000e"
        ],
        "the job retired is unloaded, and so is each one kept whose program is gone here, \
         which this Mac cannot run"
    );
}

#[test]
#[cfg(unix)]
fn the_old_serve_lets_the_bridge_go_and_retires_its_job_taking_no_lock() {
    let root = crate::test_scratch_dir("legacy");
    let paths = scratch(root.path());
    let exe = root.path().join("aterm");
    std::fs::write(&exe, "this aterm").expect("exe");
    let other = root.path().join("other-aterm");
    std::fs::write(&other, "another build").expect("exe");
    let mine = plant(&paths, INSTALLED_LABEL, &exe);
    let kept = plant(&paths, "com.aterm.aterm.caffeine.dev.0000000c", &other);
    let ran = copy(&paths, "1-1", true);
    let bridge = bridge_file(&paths);
    std::fs::write(&bridge, "4242\n").expect("a bridge left by a restart");

    let unloaded = std::cell::RefCell::new(Vec::new());
    let has_verb = |program: &Path| (program == other).then_some(true);
    let failed = serve_retires(&paths, Ok(exe.clone()), has_verb, |label| {
        unloaded.borrow_mut().push(label.to_string());
        Unload::Unloaded
    });
    assert_eq!(failed, Vec::<String>::new());
    assert!(
        !bridge.exists(),
        "the bridge sees its file gone and lets go"
    );
    assert!(!mine.exists() && kept.is_file(), "its own job retires");
    assert_eq!(unloaded.into_inner(), [INSTALLED_LABEL]);
    assert!(ran.is_dir(), "the copies are the window's to discard");
    assert!(!lock_file(&paths).exists(), "no lock is opened");

    std::fs::write(&bridge, "4242\n").expect("bridge");
    let mine = plant(&paths, INSTALLED_LABEL, &exe);
    let failed = serve_retires(
        &paths,
        Err(std::io::Error::other("no path")),
        |_| panic!("nothing is asked without this aterm's path"),
        |_| panic!("nothing is retired without this aterm's path"),
    );
    assert_eq!(failed, ["this executable's path cannot be read: no path"]);
    assert!(!bridge.exists(), "the bridge goes first");
    assert!(mine.is_file());
    assert!(!lock_file(&paths).exists(), "no lock is opened");

    let in_home = plant(
        &paths,
        "com.aterm.aterm.caffeine.dev.00000011",
        &paths.home.join("removed-worktree/aterm"),
    );
    let gone = root.path().join("removed-elsewhere/aterm");
    let elsewhere = plant(&paths, "com.aterm.aterm.caffeine.dev.00000012", &gone);
    let removed = root.path().join("removed-app/aterm");
    let running = plant(&paths, "com.aterm.aterm.caffeine.dev.00000013", &removed);
    let unloaded = std::cell::RefCell::new(Vec::new());
    let failed = serve_retires(
        &network(&paths),
        Ok(exe.clone()),
        |program| {
            panic!(
                "{} is outside a network home, never asked",
                program.display()
            )
        },
        |label| {
            unloaded.borrow_mut().push(label.to_string());
            if label == "com.aterm.aterm.caffeine.dev.00000013" {
                Unload::Running(4242)
            } else {
                Unload::Unloaded
            }
        },
    );
    assert!(
        says_kept(
            &failed,
            &[
                kept_job(&paths, INSTALLED_LABEL, &exe, None),
                kept_job(
                    &paths,
                    "com.aterm.aterm.caffeine.dev.0000000c",
                    &other,
                    None
                ),
                kept_job(
                    &paths,
                    "com.aterm.aterm.caffeine.dev.00000012",
                    &gone,
                    Some(Unload::Unloaded)
                ),
                kept_job(
                    &paths,
                    "com.aterm.aterm.caffeine.dev.00000013",
                    &removed,
                    Some(Unload::Running(4242))
                ),
            ]
        ),
        "on a network home its own job and every other whose program lies outside the home, \
         there or gone, are kept and said to be, one gone with what its unload did: \
         {failed:#?}"
    );
    assert!(mine.is_file() && kept.is_file() && elsewhere.is_file() && running.is_file());
    assert!(!in_home.exists(), "a job inside the home is judged");
    assert_eq!(
        unloaded.into_inner(),
        [
            "com.aterm.aterm.caffeine.dev.00000011",
            "com.aterm.aterm.caffeine.dev.00000012",
            "com.aterm.aterm.caffeine.dev.00000013"
        ],
        "the job retired is unloaded, and so is each one kept whose program is gone here"
    );
    assert_eq!(
        retire_jobs_with(&paths, &exe, has_verb, |_| Unload::Unloaded).kept,
        [],
        "a home on this Mac keeps none so"
    );
}

#[test]
#[cfg(unix)]
fn the_old_copies_go_only_while_no_old_mug_holds_the_lock() {
    let root = crate::test_scratch_dir("legacy");
    let paths = scratch(root.path());
    let lock = lock_file(&paths);
    assert_eq!(discard_copies(&paths), Vec::<String>::new());
    assert!(!lock.exists(), "no copies opens no lock");

    let ran = copy(&paths, "1-1", true);
    let held = crate::instance::Instance::try_hold(&lock)
        .expect("lock")
        .expect("free");
    assert_eq!(discard_copies(&paths), Vec::<String>::new());
    assert!(ran.is_dir(), "an old mug holds the lock");
    drop(held);

    let ended = format!("4242\n{INSTALLED_LABEL}\n");
    std::fs::write(&lock, &ended).expect("a mug that ended for an update restart");
    let young = copy(&paths, "1-2", false);
    assert_eq!(
        discard_copies(&paths),
        [kept_line(&young)],
        "a copy kept is said, as aterm's next launch removes it"
    );
    assert!(!kept_line(&young).contains("quit"), "no verb removes it");
    assert!(!ran.exists(), "a copy a mug ran from goes");
    assert!(young.is_dir(), "a copy macOS may still be assessing stays");
    assert_eq!(
        std::fs::read_to_string(&lock).expect("lock"),
        ended,
        "the lock stays, and the hold left its text as it was"
    );
}
