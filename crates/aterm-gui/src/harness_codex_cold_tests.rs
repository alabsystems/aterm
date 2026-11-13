// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A codex-alab worker that a restart or shut down ends comes back after the reboot (RS2): the
//! window that ends on the signal that ended it keeps its snapshot, the line its copy validated
//! and its task ([`self_claimed_exit`]), and the layout's leaf of that snapshot is relaunched
//! cold on that line, as ended under an earlier host.

use super::*;
use aterm_agent::harness::relaunch::self_claimed_line;
use std::sync::atomic::AtomicUsize;

const THREAD: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";

fn line() -> Vec<String> {
    vec![
        "/u/.aterm/agents/codex".to_string(),
        "resume".to_string(),
        THREAD.to_string(),
    ]
}

/// The record codex-alab leaves as a `SIGTERM` ends it: the signal written into `exit` before
/// it gives its claim back, as a restart or shut down that signals every process ends it.
fn ended_on_sigterm() -> self_claimed::Seen {
    match standing_record() {
        self_claimed::Seen::Present(record) => self_claimed::Seen::Present(self_claimed::Record {
            exit: Some("signal:15".to_string()),
            ..record
        }),
        other => other,
    }
}

/// The resume line a published snapshot carries for the restore (`None`: none).
fn carried_line(snap: Option<&Snapshot>) -> Option<Vec<String>> {
    snap.and_then(|snap| self_claimed_line(snap).map(|resume| resume.argv.clone()))
}

/// A proven codex-alab self-claim on tab `sid` ends on `SIGTERM` while the window ends
/// (`window_ending`) or does not: what the window leaves for the restore, the snapshot its crash
/// image names (`None`: the tab comes back a bare shell), and whether its task was marked exited.
fn the_signal_ends_it(sid: &str, window_ending: bool) -> (Option<Snapshot>, bool) {
    let a = Arc::new(Acting::default());
    a.open.lock().unwrap().insert(sid.to_string());
    *a.session_now.lock().unwrap() = Some(Some(THREAD.to_string()));
    a.set(&[(sid, Program::Codex)]);
    let copy = self_copy(&a, sid, ended_on_sigterm(), Ok(line()));
    a.self_copies
        .lock()
        .unwrap()
        .insert(sid.to_string(), copy.clone());
    crate::self_claim::set_copy_for_test(sid, Some(copy));
    let asked = Arc::new(AtomicUsize::new(0));
    let mut h = a.hooks();
    let seen = Arc::clone(&asked);
    h.acts.window_ending = Arc::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        window_ending
    });
    let host = HostHandle::start(on(), false, false, h);
    a.attached(&host, sid);
    until("its snapshot published with its copy's line", || {
        carried_line(published_snapshot(sid).as_ref()).is_some()
    });
    a.claims
        .lock()
        .unwrap()
        .insert(sid.to_string(), format!("codex-alab@{}", gone_pid()));
    a.proven.lock().unwrap().insert(sid.to_string());
    a.set(&[]);
    until("its exit decided", || {
        asked.load(Ordering::SeqCst) > 0
            && (window_ending || !a.tasks_exited.lock().unwrap().is_empty())
    });
    host.shutdown_and_join();
    let kept = published_snapshot(sid);
    let exited = a.tasks_exited.lock().unwrap().iter().any(|s| s == sid);
    assert!(
        a.self_relaunched.lock().unwrap().is_empty() && a.relaunched.lock().unwrap().is_empty(),
        "nothing is typed as the window goes"
    );
    crate::self_claim::set_copy_for_test(sid, None);
    publish_snapshot_for_test(sid, None);
    (kept, exited)
}

/// What a cold restore's relaunch was handed: the pid and kernel start it names, its line, and
/// whether it ended under an earlier host.
type Relaunch = (u32, String, Option<Vec<String>>, bool);
type Handed = Arc<Mutex<Vec<Relaunch>>>;

/// The cold restore of the crash image's leaf of `kept` ([`crate::restore::AgentRestore::of`],
/// read back as [`crate::restore::AgentRestore::snapshot`] hands it): what its relaunch was
/// handed, and whether its task was kept (no [`HostNote::RestoredOver`]).
fn restored_cold(sid: &str, kept: Snapshot) -> (Vec<Relaunch>, bool) {
    let shell = kept.shell;
    let snap = crate::restore::AgentRestore::of(kept)
        .snapshot(sid.to_string(), shell)
        .expect("the leaf relaunches its agent");
    let world = Arc::new(World::default());
    let handed: Handed = Arc::default();
    let mut h = hooks(&world, parking_body(&world));
    h.pause = Arc::new(|_| Duration::from_millis(1));
    // The restored pane is an open tab: a closed one is forgotten on the host's next pass
    // (`forget_closed`), its typed line with it.
    let tab = sid.to_string();
    h.acts.open = Arc::new(move |s| s == tab);
    h.acts.codex_cold = Arc::new(|_, _| Ok(None));
    let seen = Arc::clone(&handed);
    h.acts.relaunch_restored = Arc::new(move |_, snap, _, _, launch| {
        seen.lock().unwrap().push((
            snap.pid,
            snap.start.clone(),
            carried_line(Some(snap)),
            launch.earlier,
        ));
        "adopted".to_string()
    });
    let host = HostHandle::start(on(), false, false, h);
    host.relaunch_restored(vec![RestoredAgent {
        sid: sid.to_string(),
        snap,
        place: "test".to_string(),
        cold: true,
        asked: None,
        was: None,
    }]);
    until("its restore is over", || {
        world
            .badges
            .lock()
            .unwrap()
            .iter()
            .any(|(s, word)| s == sid && word.is_none())
    });
    host.shutdown_and_join();
    let kept = !world
        .notes
        .lock()
        .unwrap()
        .iter()
        .any(|n| matches!(n, HostNote::RestoredOver { sid: s } if s == sid));
    let handed = handed.lock().unwrap().clone();
    (handed, kept)
}

/// The line the crash rule's record says was typed in tab `sid` ([`crate::self_claim::typed`]),
/// forgotten as it is read so no other test sees it.
fn typed_in(sid: &str) -> Option<Vec<String>> {
    let typed = crate::self_claim::typed_for(sid).map(|resume| resume.argv);
    crate::self_claim::forget(sid);
    typed
}

/// A CODEX-ALAB WORKER PANE RESTORED COLD RELAUNCHES WITH ITS RESUME LINE AND KEEPS ITS TASK: a
/// restart or shut down signals every process, so the `SIGTERM` that ends this window ends its
/// codex-alab worker too, whose record names the signal; that end is its host's, so its snapshot
/// keeps the line its copy validated and its task stays working, and after the reboot the leaf
/// the crash image wrote of it is relaunched cold on that line, by its pid and kernel start, as
/// ended under an earlier host. CONTROL (the old path): read as the agent's own orderly end, the
/// snapshot is forgotten as the window goes, the crash image names no agent, and the tab comes
/// back a bare shell whose task reads exited.
#[test]
fn a_codex_alab_worker_pane_restored_cold_relaunches_with_its_resume_line_and_keeps_its_task() {
    let sid = "s-codex-alab-cold";
    let (kept, exited) = the_signal_ends_it(sid, true);
    let kept = kept.expect("the crash image names its agent");
    assert_eq!(carried_line(Some(&kept)), Some(line()), "its own line");
    assert!(!exited, "its task is kept");
    let (pid, start) = (kept.pid, kept.start.clone());
    let (handed, task_kept) = restored_cold(sid, kept);
    assert_eq!(
        handed,
        [(pid, start, Some(line()), true)],
        "relaunched once, on its line, by its pid and kernel start, as ended under an earlier host"
    );
    assert!(task_kept, "its task is kept through the restore");
    assert_eq!(
        typed_in(sid),
        Some(line()),
        "typed as the crash rule's line: the claim its relaunched process makes inherits it"
    );

    let sid = "s-codex-alab-bare";
    let (kept, exited) = the_signal_ends_it(sid, false);
    assert!(
        kept.is_none(),
        "the old path: no agent in the crash image, a bare shell: {kept:?}"
    );
    assert!(exited, "the old path: its task reads exited");
}

/// A RESTART THAT NAMED AN ACCOUNT ASKS ITS RELAUNCH'S ADMISSION FOR IT ([`Acts::asked_account`],
/// read once as the asked exit is decided): every attempt of that relaunch is handed the account
/// its stamp names, and an admission that did not take it is said once, of the attempt whose
/// line landed, however many attempts the relaunch takes. An attempt that kept the old row and
/// did not land says nothing: a later one took the account here, and the tab is told nothing
/// false. CONTROLS: a plain restart's stamp asks for none and says nothing; a crash nobody asked
/// for asks for none, whatever a stamp reader would answer.
#[test]
fn a_restart_that_named_an_account_asks_its_relaunchs_admission_for_it() {
    use restart_stamp::{Cause, Stamp, Wanted};
    const WHY: &str = "account-unread";
    // `kept`: how many first attempts keep the old row; the third attempt's line lands.
    let run = |sid: &str, stamp: Option<Option<Wanted>>, kept: usize| {
        let a = Arc::new(Acting::default());
        *a.self_relaunch_steps.lock().unwrap() =
            std::iter::repeat_n("wait:shell-prompt".to_string(), 2).collect();
        a.open.lock().unwrap().insert(sid.to_string());
        a.set(&[(sid, Program::Codex)]);
        let copy = self_copy(&a, sid, standing_record(), Ok(line()));
        a.self_copies
            .lock()
            .unwrap()
            .insert(sid.to_string(), copy.clone());
        let handed = Arc::new(Mutex::new(Vec::<Option<Wanted>>::new()));
        let mut h = a.hooks();
        let seen = Arc::clone(&handed);
        h.acts.codex_cold = Arc::new(move |_, used| {
            let mut seen = seen.lock().unwrap();
            let keeps = seen.len() < kept;
            seen.push(used.wanted().cloned());
            used.note_kept(used.wanted().filter(|_| keeps).map(|_| ColdKept {
                why: WHY.to_string(),
                on: None,
            }));
            Ok(None)
        });
        if stamp.is_none() {
            h.acts.asked_account = Arc::new(|_, _, _| Some(Wanted::Pool));
        }
        let host = HostHandle::start(on(), false, false, h);
        a.attached(&host, sid);
        until("its snapshot published", || {
            published_snapshot(sid).is_some()
        });
        if let Some(wanted) = stamp {
            restart_stamp::write(
                a.stamp_dir(),
                sid,
                &Stamp {
                    pid: copy.pid,
                    start: copy.start.clone(),
                    cause: Cause::Asked,
                    at: aterm_time::unix_secs().unwrap_or(0),
                    wanted,
                    input: None,
                },
            )
            .expect("the stamp");
        }
        a.claims
            .lock()
            .unwrap()
            .insert(sid.to_string(), format!("codex-alab@{}", gone_pid()));
        a.proven.lock().unwrap().insert(sid.to_string());
        a.set(&[]);
        until("its relaunch attempted three times", || {
            handed.lock().unwrap().len() >= 3
        });
        host.shutdown_and_join();
        crate::self_claim::forget(sid);
        let said = a
            .notes
            .lock()
            .unwrap()
            .iter()
            .filter(|n| matches!(n, HostNote::Inform { text, .. } if text.contains(WHY)))
            .count();
        let mut handed = handed.lock().unwrap().clone();
        handed.dedup();
        (handed, said)
    };
    let work = Wanted::Row("work".to_string());
    assert_eq!(
        run("s-asked-account", Some(Some(work.clone())), usize::MAX),
        (vec![Some(work.clone())], 1),
        "every attempt asks for the stamp's account; an admission that kept its row says so once"
    );
    assert_eq!(
        run("s-asked-account-later", Some(Some(work.clone())), 1),
        (vec![Some(work)], 0),
        "the first attempt kept the old row and did not land; the one that landed took the account"
    );
    assert_eq!(
        run("s-asked-plain", Some(None), usize::MAX),
        (vec![None], 0),
        "CONTROL: a plain restart asks for no account"
    );
    assert_eq!(
        run("s-asked-crash", None, usize::MAX),
        (vec![None], 0),
        "CONTROL: a crash nobody asked for asks for no account"
    );
}

/// AN ASKED EXIT'S RELAUNCH OWED THROUGH A PARK ASKS FOR ITS STAMP'S ACCOUNT: an update's Commit
/// that stops the worker before its relaunch landed hands that relaunch to the restored queue
/// ([`RestoredAgent`]), which reads the stamp standing for that very process
/// ([`Acts::asked_account`]) as the worker would have, asks every attempt's admission for its
/// account, and tells the tab once, as the line lands, that the account was not taken. CONTROL: a
/// relaunch nobody asked for asks for none and never reads a stamp.
#[test]
fn a_relaunch_owed_through_a_park_asks_for_the_account_its_restart_named() {
    use restart_stamp::{Cause, Wanted};
    const WHY: &str = "account-unread";
    let run = |sid: &str, asked: Option<Cause>| {
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        h.pause = Arc::new(|_| Duration::from_millis(1));
        let tab = sid.to_string();
        h.acts.open = Arc::new(move |s| s == tab);
        let copy = self_claimed::Copy {
            lifetime: None,
            pid: 0x3fff_ff41,
            start: "parked".into(),
            home: std::path::PathBuf::from("/u/.codex"),
            seen: standing_record(),
            resume: Ok(self_claimed::Resume {
                argv: line(),
                session: THREAD.to_string(),
                codex_home: None,
                codex_auth_path: None,
                cwd: None,
            }),
        };
        let resume = copy.resume.clone().unwrap();
        let snap = aterm_agent::harness::relaunch::snapshot_of_copy(sid, &copy, &resume, 1, None);
        let read = Arc::new(Mutex::new(Vec::<(String, u32, String)>::new()));
        let reads = Arc::clone(&read);
        h.acts.asked_account = Arc::new(move |sid, pid, start| {
            reads
                .lock()
                .unwrap()
                .push((sid.to_string(), pid, start.to_string()));
            Some(Wanted::Row("work".to_string()))
        });
        let handed = Arc::new(Mutex::new(Vec::<Option<Wanted>>::new()));
        let seen = Arc::clone(&handed);
        h.acts.codex_cold = Arc::new(move |_, used| {
            seen.lock().unwrap().push(used.wanted().cloned());
            used.note_kept(used.wanted().map(|_| ColdKept {
                why: WHY.to_string(),
                on: None,
            }));
            Ok(None)
        });
        h.acts.relaunch = Arc::new(|_, _, _, _, _, _, _, _| "adopted".to_string());
        h.acts.relaunch_restored = Arc::new(|_, _, _, _, _| "adopted".to_string());
        let host = HostHandle::start(on(), false, false, h);
        host.relaunch_restored(vec![RestoredAgent {
            sid: sid.to_string(),
            snap,
            place: "test".to_string(),
            cold: false,
            asked,
            was: None,
        }]);
        until("its restore is over", || {
            world
                .badges
                .lock()
                .unwrap()
                .iter()
                .any(|(s, word)| s == sid && word.is_none())
        });
        host.shutdown_and_join();
        let said = world
            .notes
            .lock()
            .unwrap()
            .iter()
            .filter(|n| matches!(n, HostNote::Inform { text, .. } if text.contains(WHY)))
            .count();
        let mut handed = handed.lock().unwrap().clone();
        handed.dedup();
        let read = read.lock().unwrap().clone();
        (handed, said, read)
    };
    let (handed, said, read) = run("s-parked-asked", Some(Cause::Asked));
    assert_eq!(handed, [Some(Wanted::Row("work".to_string()))]);
    assert_eq!(said, 1, "told once, as its line landed");
    assert_eq!(
        read,
        [(
            "s-parked-asked".to_string(),
            0x3fff_ff41,
            "parked".to_string()
        )],
        "the stamp is read for the process the relaunch is owed for"
    );
    let (handed, said, read) = run("s-parked-plain", None);
    assert_eq!(
        (handed, said, read.len()),
        (vec![None], 0, 0),
        "CONTROL: nobody asked"
    );
}

/// THE PRODUCTION COLD CODEX ACT STARTS A RELAUNCH ON THE ROW ITS RESTART ASKED FOR
/// ([`codex_cold_at`]): a conversation on this machine's own login, which still serves, moves onto
/// `work` when its restart named that row and `work` answers the relaunch's one read; a retried
/// attempt reads no provider, so it takes `work` only while that reading is fresh, and after that
/// keeps the row it ran on and says why ([`ColdUse::kept`]). CONTROLS: with no account asked the
/// relaunch keeps its row; a row that answers no read, and a label the roster does not have, are
/// refused by their words and the relaunch keeps its row.
#[test]
fn the_cold_codex_act_starts_a_relaunch_on_the_account_its_restart_asked_for() {
    use aterm_agent::harness::accounts::{self, Cli, Sample};
    use aterm_agent::harness::accounts_pool::{MOVE_UNKNOWN, MOVE_UNREAD, USAGE_FRESH_S};
    use aterm_agent::harness::codex_app_login::Usage;
    use restart_stamp::Wanted;
    let world = |tag: &str| {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical").join(tag);
        let (cfg, state, home) = (root.join("cfg"), root.join("state"), root.join("home"));
        let work = root.join("logins/codex/work");
        for dir in [&cfg, &state, &home.join(".codex"), &work] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        std::fs::write(home.join(".codex/auth.json"), "{}").expect("default login");
        std::fs::write(work.join("auth.json"), "{}").expect("auth");
        std::fs::write(
            cfg.join("accounts-codex.toml"),
            format!(
                "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
                 [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {}\n",
                crate::test_host::toml(&work)
            ),
        )
        .expect("roster");
        (tmp, root, cfg, state, home, work)
    };
    let now = 1_792_700_000_i64;
    let auth_of = |moved: Option<Snapshot>| {
        moved
            .as_ref()
            .and_then(Snapshot::codex)
            .and_then(|run| run.self_claimed.as_ref())
            .and_then(|resume| resume.codex_auth_path.clone())
    };
    let kept = |why: &str| {
        Some(ColdKept {
            why: why.to_string(),
            on: None,
        })
    };
    // (the account asked for, whether a read is answered) -> each attempt's (login moved to, kept).
    let run = |tag: &str, wanted: Option<Wanted>, answers: bool, at: &[i64]| {
        let (_tmp, root, cfg, state, home, work) = world(tag);
        let seat_at = |now| aterm_agent::harness::accounts_pool::Seat {
            config_dir: &cfg,
            state: &state,
            home: &home,
            now,
        };
        seat_at(now)
            .store(Cli::Codex)
            .transact_within(&accounts::LOCK_LADDER, now, &mut None, |s| {
                s.active = Some("main".to_string());
                s.proven = true;
                Ok(Vec::new())
            })
            .expect("on `main`");
        let held = held_codex_relaunch(&root, &home);
        let reader = |_: Cli, _: &std::path::Path, _: Option<&str>, at: i64| -> Option<Usage> {
            answers.then(|| Usage {
                sample: Sample {
                    session_pct: Some(10),
                    at,
                    ..Sample::default()
                },
                walls: Vec::new(),
            })
        };
        let used = ColdUse::asking(wanted);
        let attempts: Vec<_> = at
            .iter()
            .map(|later| {
                let moved = codex_cold_attempt(&seat_at(now + later), &held, &used, &reader)
                    .expect("its own row serves");
                (auth_of(moved), used.kept())
            })
            .collect();
        (attempts, work.join("auth.json").display().to_string())
    };
    let work = || Some(Wanted::Row("work".to_string()));
    let (attempts, auth) = run("asked", work(), true, &[0, 10, USAGE_FRESH_S + 1]);
    assert_eq!(
        attempts,
        [
            (Some(auth.clone()), None),
            (Some(auth), None),
            (None, kept(MOVE_UNREAD)),
        ],
        "onto `work` while its answered read is fresh, then back on the row it ran on"
    );
    let (attempts, _) = run("plain", None, true, &[0]);
    assert_eq!(attempts, [(None, None)], "CONTROL: nothing asked, it keeps");
    let (attempts, _) = run("unanswered", work(), false, &[0]);
    assert_eq!(attempts, [(None, kept(MOVE_UNREAD))], "CONTROL: unread");
    let nope = Some(Wanted::Row("nope".to_string()));
    let (attempts, _) = run("unknown", nope, true, &[0]);
    assert_eq!(attempts, [(None, kept(MOVE_UNKNOWN))], "CONTROL: unknown");
}
