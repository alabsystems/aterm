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
