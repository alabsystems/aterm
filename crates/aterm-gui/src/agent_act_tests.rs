// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use std::cell::{Cell, RefCell};

use super::*;

/// The process the fake kernel reports for every pid: what a test sets ([`set`]). This scaffolding
/// is unix only, as the tests that use it are: their programs are copies of `/bin/sleep`.
#[cfg(unix)]
#[derive(Clone, Default)]
struct Proc {
    exe: Option<PathBuf>,
    start_us: Option<u64>,
    team: Option<String>,
    script: Option<PathBuf>,
}

#[cfg(unix)]
thread_local! {
    static PROC: RefCell<Proc> = RefCell::new(Proc::default());
    /// Each read of the start moves it this much: a process that execs or is replaced while
    /// the verdict is read.
    static DRIFT: Cell<u64> = const { Cell::new(0) };
    /// How many verdicts were read: each asks for the places once.
    static VERDICTS: Cell<usize> = const { Cell::new(0) };
    /// How many times the team was read: a `codesign` run each, live.
    static TEAM_READS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(unix)]
fn set(proc: Proc) {
    PROC.with_borrow_mut(|p| *p = proc);
}

#[cfg(unix)]
const FAKE: Kernel = Kernel {
    exe: |_| PROC.with_borrow(|p| p.exe.clone()),
    file: |_, exe| FileId::of(exe),
    start_us: |_| {
        let drift = DRIFT.get();
        PROC.with_borrow_mut(|p| {
            let at = p.start_us;
            p.start_us = at.map(|s| s + drift);
            at
        })
    },
    team: |_| {
        TEAM_READS.set(TEAM_READS.get() + 1);
        PROC.with_borrow(|p| p.team.clone())
    },
    script: |_| PROC.with_borrow(|p| p.script.clone()),
};

/// A scratch machine: Claude Code's versions directory, an install in it, a copy of `sleep` and
/// a lab stand-in script.
#[cfg(unix)]
struct World {
    _dir: aterm_tempfile::TempDir,
    versions: PathBuf,
    install: PathBuf,
    sleep: PathBuf,
    fake: PathBuf,
}

#[cfg(unix)]
impl World {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = std::fs::canonicalize(dir.path()).expect("canonical scratch");
        let versions = agent_image::claude_versions(&root.join("home"));
        std::fs::create_dir_all(&versions).expect("versions");
        let install = versions.join("2.1.399");
        std::fs::copy("/bin/sleep", &install).expect("an install");
        let sleep = root.join("sleep");
        std::fs::copy("/bin/sleep", &sleep).expect("a copy of sleep");
        let fake = root.join("lab/claude");
        std::fs::create_dir_all(fake.parent().expect("lab/")).expect("lab");
        std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").expect("the stand-in");
        Self {
            _dir: dir,
            versions,
            install,
            sleep,
            fake,
        }
    }

    fn places(&self) -> impl Fn() -> Where + '_ {
        move || {
            VERDICTS.set(VERDICTS.get() + 1);
            Where {
                versions: Some(self.versions.clone()),
                roots: None,
                standins: FileId::of(&self.fake).into_iter().collect(),
            }
        }
    }

    fn running(&self, exe: &Path, start_us: u64) -> Proc {
        Proc {
            exe: Some(exe.to_path_buf()),
            start_us: Some(start_us),
            ..Proc::default()
        }
    }
}

/// A sid no other test's statics hold.
#[cfg(unix)]
fn sid(name: &str) -> String {
    format!("s-act-{name}-{}", std::process::id())
}

#[cfg(unix)]
const GROUP: i32 = 4242;

/// The kernel decides, never the name: an install by its vendor's layout, the vendor's team, a lab
/// stand-in's script are each confirmed; a copy of another program named `claude`, and a leader
/// nothing can be read of, are observed. NEGATIVE CONTROL (by hand, run): with `read_with`
/// confirming every admitted agent (today's name-only admission), the observed rows say `on`.
#[cfg(unix)]
#[test]
fn the_kernel_confirms_an_agent_and_a_name_alone_is_observed() {
    let w = World::new();
    let places = w.places();
    let rows: [(&str, Proc, Act, Option<Proof>); 5] = [
        (
            "install",
            w.running(&w.install, 1),
            Act::On,
            Some(Proof::Layout),
        ),
        (
            "team",
            Proc {
                team: Some(agent_image::CLAUDE_TEAM.to_string()),
                ..w.running(&w.sleep, 1)
            },
            Act::On,
            Some(Proof::Team),
        ),
        (
            "stand-in",
            Proc {
                script: Some(w.fake.clone()),
                ..w.running(Path::new("/bin/sh"), 1)
            },
            Act::On,
            Some(Proof::Seam),
        ),
        ("named only", w.running(&w.sleep, 1), Act::Observe, None),
        ("unreadable", Proc::default(), Act::Observe, None),
    ];
    for (name, proc, act, proof) in rows {
        set(proc);
        let verdict = read_with(&sid(name), Program::Claude, GROUP, &FAKE, &places);
        assert_eq!(verdict, Verdict { act, proof }, "{name}");
        assert_eq!(cached(&sid(name), GROUP), Some(act), "{name}");
        assert_eq!(observed(&sid(name), GROUP), act == Act::Observe, "{name}");
    }
    set(Proc {
        team: Some(agent_image::CLAUDE_TEAM.to_string()),
        ..w.running(&w.sleep, 1)
    });
    assert_eq!(
        read_with(&sid("team-agy"), Program::Agy, GROUP, &FAKE, &places).act,
        Act::Observe,
        "Claude Code's team confirms no agy"
    );
    set(Proc {
        script: Some(PathBuf::from("lab/claude")),
        ..w.running(Path::new("/bin/sh"), 1)
    });
    assert_eq!(
        read_with(&sid("relative"), Program::Claude, GROUP, &FAKE, &places).act,
        Act::Observe,
        "a script named by a relative path is never read"
    );
}

/// The verdict is kept per process: the same leader, start and file keep it unread; an exec (a
/// new file at the same start) and a new process (a new start) are read again, and a later
/// confirmation lifts the observation. NEGATIVE CONTROL (by hand, run): keeping the verdict by
/// sid alone, the exec row stays observed.
#[cfg(unix)]
#[test]
fn the_verdict_is_kept_per_process_and_read_again_after_an_exec() {
    let w = World::new();
    let places = w.places();
    let s = sid("kept");
    VERDICTS.set(0);
    set(w.running(&w.sleep, 10));
    assert_eq!(
        read_with(&s, Program::Claude, GROUP, &FAKE, &places).act,
        Act::Observe
    );
    assert_eq!(
        read_with(&s, Program::Claude, GROUP, &FAKE, &places).act,
        Act::Observe
    );
    assert_eq!(VERDICTS.get(), 1, "the same process is not read again");
    set(w.running(&w.install, 10));
    assert_eq!(
        read_with(&s, Program::Claude, GROUP, &FAKE, &places).act,
        Act::On,
        "the exec into the install is read again and lifts the observation"
    );
    assert_eq!(VERDICTS.get(), 2);
    set(w.running(&w.sleep, 11));
    assert_eq!(
        read_with(&s, Program::Claude, GROUP, &FAKE, &places).act,
        Act::Observe
    );
    assert_eq!(VERDICTS.get(), 3, "a new process is read again");
    assert_eq!(
        read_with(&s, Program::Agy, GROUP, &FAKE, &places).act,
        Act::Observe
    );
    assert_eq!(VERDICTS.get(), 4, "another agent named is read again");
    assert_eq!(cached(&s, GROUP + 1), None, "another group has no verdict");
}

/// A read whose key moved under it (the process exec'd or was replaced while its image was read)
/// is answered but not kept: the next read reads again. NEGATIVE CONTROL (by hand, run): keeping
/// every verdict read, the moved key's verdict is kept.
#[cfg(unix)]
#[test]
fn a_key_that_moves_during_the_read_is_not_kept() {
    let w = World::new();
    let places = w.places();
    let moving = sid("moving");
    set(w.running(&w.install, 20));
    DRIFT.set(1);
    let verdict = read_with(&moving, Program::Claude, GROUP, &FAKE, &places);
    DRIFT.set(0);
    assert_eq!(verdict.act, Act::On);
    assert_eq!(cached(&moving, GROUP), None, "a key that moved is not kept");
    read_with(&moving, Program::Claude, GROUP, &FAKE, &places);
    assert_eq!(cached(&moving, GROUP), Some(Act::On), "a still key is kept");
}

/// What `status supervisor_act=` prints: `-` for no agent admitted and before any read, then
/// what the host's read kept; and the session log's word for each act is one of its closed
/// `why=` words.
#[cfg(unix)]
#[test]
fn status_prints_the_kept_act_and_the_log_says_it_in_closed_words() {
    let w = World::new();
    let places = w.places();
    let s = sid("status");
    assert_eq!(status_word(&s, None, GROUP), "-");
    assert_eq!(
        status_word(&s, Some(Program::Claude), GROUP),
        "-",
        "no read yet: no act to say"
    );
    assert!(
        !observed(&s, GROUP),
        "nothing read: the needs are as they were"
    );
    set(w.running(&w.install, 30));
    read_with(&s, Program::Claude, GROUP, &FAKE, &places);
    assert_eq!(status_word(&s, Some(Program::Claude), GROUP), "on");
    for act in Act::ALL {
        assert_eq!(
            aterm_slog::vocab::lookup(aterm_slog::vocab::WHY_WORDS, why(act)),
            Some(why(act)),
            "{act:?}"
        );
    }
    assert_ne!(why(Act::On), why(Act::Observe));
}

/// `ctl help status` names exactly the words `status supervisor_act=` prints: the act's own
/// enumeration and `-`.
#[test]
fn help_status_names_exactly_the_supervisor_act_words() {
    let help = aterm_types::control_verbs::spec("status")
        .expect("the status verb")
        .help_line();
    let lists: Vec<&str> = help.split(" supervisor_act=<").skip(1).collect();
    assert_eq!(
        lists.len(),
        1,
        "help status names one supervisor_act= vocabulary"
    );
    let named: std::collections::BTreeSet<&str> = lists[0]
        .split_once('>')
        .expect("the vocabulary closes")
        .0
        .split('|')
        .collect();
    let mut written: std::collections::BTreeSet<&str> =
        Act::ALL.into_iter().map(Act::word).collect();
    written.insert("-");
    assert_eq!(named, written);
}

/// Each move of a hosted session's act is told once, in words: `meta field=supervisor ...
/// why=unconfirmed`, then `why=confirmed` when a later read confirms the agent. A session gone
/// and one whose foreground is no agent are observed. NEGATIVE CONTROL (by hand, run): with
/// `take_told` always news, a second `unconfirmed` mark is written.
#[cfg(unix)]
#[test]
fn a_hosted_sessions_act_is_told_once_per_move_in_words() {
    let w = World::new();
    let places = w.places();
    let h = crate::session_store::test_handle(71);
    let store = crate::session_store::new_store();
    store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .register(h.clone());
    let s = h.ctx.self_id.as_str().to_string();
    assert_eq!(hosted_with(&store, "s-gone", &FAKE, &places), Act::Observe);
    assert_eq!(
        hosted_with(&store, &s, &FAKE, &places),
        Act::Observe,
        "a foreground that is no agent"
    );
    {
        let mut timeline = h
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        timeline.note_foreground_group(GROUP);
        timeline.set_program(GROUP, Some("claude".to_string()));
    }
    crate::session_timeline::claim_supervisor(
        &h.ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    let whys = || -> Vec<String> {
        let timeline = h
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        timeline
            .slog()
            .log()
            .window()
            .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).expect("a record fits"))
            .filter(|l| l.contains(" meta ") && l.contains(" why="))
            .collect()
    };
    set(w.running(&w.sleep, 40));
    assert_eq!(hosted_with(&store, &s, &FAKE, &places), Act::Observe);
    assert_eq!(hosted_with(&store, &s, &FAKE, &places), Act::Observe);
    let told = whys();
    assert_eq!(told.len(), 1, "{told:#?}");
    assert!(told[0].contains("field=supervisor"), "{told:#?}");
    assert!(told[0].contains("why=unconfirmed"), "{told:#?}");
    assert!(told[0].contains("by=harness"), "{told:#?}");
    assert!(
        !told[0].contains(w.sleep.to_str().expect("utf-8")),
        "{told:#?}"
    );
    set(w.running(&w.install, 40));
    assert_eq!(hosted_with(&store, &s, &FAKE, &places), Act::On);
    let told = whys();
    assert_eq!(told.len(), 2, "{told:#?}");
    assert!(told[1].contains("why=confirmed"), "{told:#?}");
}

/// The roster pass tells a session's act once the host holds its claim, so an observed agent
/// that never draws a box (a copy of `sleep` named `claude`) still says why nothing is typed; a
/// session the host does not hold, or another holder holds, is read and kept but not told, and
/// is told once the host claims it. NEGATIVE CONTROL (by hand, run): with the roster read telling
/// whatever the claim, the unheld reads are told.
#[cfg(unix)]
#[test]
fn the_roster_read_tells_the_act_of_a_session_the_host_holds() {
    let w = World::new();
    let places = w.places();
    let h = crate::session_store::test_handle(72);
    let s = h.ctx.self_id.as_str().to_string();
    let ours = crate::harness_host::holder();
    let whys = || -> Vec<String> {
        let timeline = h
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        timeline
            .slog()
            .log()
            .window()
            .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).expect("a record fits"))
            .filter(|l| l.contains(" meta ") && l.contains(" why="))
            .collect()
    };
    set(w.running(&w.sleep, 50));
    let read = || roster_read_with(&h.ctx, &s, Program::Claude, GROUP, &ours, &FAKE, &places);
    read();
    assert_eq!(whys(), Vec::<String>::new(), "unclaimed: read, not told");
    assert_eq!(cached(&s, GROUP), Some(Act::Observe), "but kept");
    let theirs = "jsmith-loop@1";
    let claim = |holder: &str| {
        crate::session_timeline::claim_supervisor(
            &h.ctx,
            holder,
            None,
            None,
            crate::metrics::now_us(),
        )
    };
    claim(theirs).expect("another holder's claim");
    read();
    assert_eq!(whys(), Vec::<String>::new(), "another's claim: not told");
    assert!(crate::session_timeline::release_supervisor_held_by(
        &h.ctx,
        theirs,
        crate::metrics::now_us(),
        aterm_slog::Actor::Harness,
    ));
    claim(&ours).expect("the host's claim");
    read();
    read();
    let told = whys();
    assert_eq!(told.len(), 1, "{told:#?}");
    assert!(told[0].contains("why=unconfirmed"), "{told:#?}");
    assert!(told[0].contains(&format!("value={ours}")), "{told:#?}");
    assert!(
        !told[0].contains(w.sleep.to_str().expect("utf-8")),
        "{told:#?}"
    );
}

/// The roster pass forgets the sessions gone, once more than its cap are kept.
#[test]
fn the_kept_verdicts_are_pruned_to_the_live_sessions() {
    let mut map: BTreeMap<String, u8> = (0..KEPT_MAX).map(|n| (format!("s-{n}"), 0)).collect();
    prune(&mut map, &["s-0"]);
    assert_eq!(map.len(), KEPT_MAX, "at the cap nothing is forgotten");
    map.insert("s-live".to_string(), 0);
    prune(&mut map, &["s-live", "s-1"]);
    assert_eq!(
        map.keys().map(String::as_str).collect::<Vec<_>>(),
        ["s-1", "s-live"]
    );
}

/// A second claim of the same session is told again, whether the process is new or the same: the
/// claim's own `field=supervisor` mark says no act, so the session log's latest supervisor mark
/// would otherwise carry no `why=`. NEGATIVE CONTROL (run): with `forget_told` a no-op, the second
/// and third claims are not told.
#[cfg(unix)]
#[test]
fn each_claim_of_a_session_is_told_its_act_again() {
    let w = World::new();
    let places = w.places();
    let h = crate::session_store::test_handle(73);
    let s = h.ctx.self_id.as_str().to_string();
    let ours = crate::harness_host::holder();
    let supervisor_marks = || -> Vec<String> {
        let timeline = h
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        timeline
            .slog()
            .log()
            .window()
            .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).expect("a record fits"))
            .filter(|l| l.contains(" meta ") && l.contains("field=supervisor"))
            .collect()
    };
    let read = || roster_read_with(&h.ctx, &s, Program::Claude, GROUP, &ours, &FAKE, &places);
    let claim = || {
        crate::session_timeline::claim_supervisor(
            &h.ctx,
            &ours,
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("the host's claim");
    };
    let release = || {
        assert!(crate::session_timeline::release_supervisor_held_by(
            &h.ctx,
            &ours,
            crate::metrics::now_us(),
            aterm_slog::Actor::Harness,
        ));
    };
    set(w.running(&w.sleep, 60));
    claim();
    read();
    release();
    set(w.running(&w.sleep, 61));
    claim();
    read();
    read();
    release();
    claim();
    read();
    let marks = supervisor_marks();
    let told = marks
        .iter()
        .filter(|l| l.contains("why=unconfirmed"))
        .count();
    assert_eq!(told, 3, "one tell per claim: {marks:#?}");
    assert!(
        marks.last().is_some_and(|l| l.contains("why=unconfirmed")),
        "the latest supervisor mark says why: {marks:#?}"
    );
}

/// A key the kernel gave no start or no image identity for stands for no later read: each read
/// reads the image again, so an exec or a new process can never hide behind it. NEGATIVE CONTROL
/// (run): with `Key::stands` ignored, the second read of each row is the kept verdict.
#[cfg(unix)]
#[test]
fn a_key_without_a_start_or_an_image_identity_is_read_afresh() {
    let w = World::new();
    let places = w.places();
    let gone = w.install.with_file_name("2.1.400");
    for (name, proc) in [
        (
            "no start",
            Proc {
                start_us: None,
                ..w.running(&w.install, 1)
            },
        ),
        ("no image identity", w.running(&gone, 1)),
    ] {
        let s = sid(&format!("afresh-{name}"));
        set(proc);
        VERDICTS.set(0);
        read_with(&s, Program::Claude, GROUP, &FAKE, &places);
        read_with(&s, Program::Claude, GROUP, &FAKE, &places);
        assert_eq!(VERDICTS.get(), 2, "{name}: read again");
    }
}

/// The team (a `codesign` run, live) is read only where no vendor layout confirms the image.
/// NEGATIVE CONTROL (run): reading the team before the layout, the install row reads it.
#[cfg(unix)]
#[test]
fn the_team_is_read_only_where_no_layout_confirms_the_image() {
    let w = World::new();
    let places = w.places();
    for (name, proc, reads) in [
        ("install", w.running(&w.install, 1), 0),
        ("copy", w.running(&w.sleep, 1), 1),
    ] {
        set(proc);
        TEAM_READS.set(0);
        read_with(
            &sid(&format!("team-read-{name}")),
            Program::Claude,
            GROUP,
            &FAKE,
            &places,
        );
        assert_eq!(TEAM_READS.get(), reads, "{name}");
    }
}

/// THE KERNEL'S READ OF A SIGNATURE, live: a copy of `sleep` signed ad hoc runs with a signature
/// the kernel holds valid, yet the kernel validates it as no Developer ID, so its team (none, or
/// one its code directory claims) vouches for nothing; the status flags alone could not say it
/// (the kernel reports no `CS_ADHOC` for it). NEGATIVE CONTROL (run): with `vouches` reading the
/// status flags (`CS_VALID` and no `CS_ADHOC`), the copy vouches.
#[cfg(target_os = "macos")]
#[test]
fn the_kernel_vouches_for_no_ad_hoc_signature() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let copy = dir.path().join("claude");
    std::fs::copy("/bin/sleep", &copy).expect("a copy of sleep");
    let signed = aterm_uds::spawnfd::command("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(&copy)
        .output()
        .expect("codesign runs");
    assert!(signed.status.success(), "{signed:?}");
    let mut child = aterm_uds::spawnfd::command(&copy)
        .arg("30")
        .spawn()
        .expect("the copy runs");
    let pid = child.id();
    let id = aterm_sysprobe::identity::identity(pid);
    let team = live_team(pid);
    let _ = child.kill();
    let _ = child.wait();
    let id = id.expect("the kernel reads the copy");
    let flags = id.cs_status.expect("its status");
    assert_ne!(flags & aterm_sysprobe::identity::CS_VALID, 0, "{flags:#x}");
    assert!(!vouches(&id), "{id:?}");
    assert_eq!(team, None);
}

/// A running process signed with a Developer ID names its team through its certificate, and the
/// certificate check names no other team (live: the first of this user's processes the kernel
/// validates as Developer ID, COULD NOT RUN when none runs). NEGATIVE CONTROL (run): with
/// `process_meets_team` answering whether `codesign` ran, another team's row is met.
#[cfg(target_os = "macos")]
#[test]
fn a_developer_id_process_names_its_team_through_its_certificate() {
    use aterm_sysprobe::identity::identity;
    let pids = aterm_uds::holders::uid_pids().expect("this user's processes");
    let judged = pids.into_iter().find_map(|pid| {
        let team = identity(pid).filter(vouches)?.team?;
        let named = live_team(pid);
        let other = aterm_containment::consent::process_meets_team(pid, "QZPTEAM001");
        // A process that ended or exec'd meanwhile is no row: the next one is.
        let still = identity(pid).filter(vouches).and_then(|id| id.team);
        (still.as_deref() == Some(team.as_str())).then_some((team, named, other))
    });
    let Some((team, named, other)) = judged else {
        aterm_spec::host::could_not_run("no process of this user is signed with a Developer ID");
    };
    assert_eq!(named.as_deref(), Some(team.as_str()));
    assert!(!other, "another team's requirement is not met");
}
