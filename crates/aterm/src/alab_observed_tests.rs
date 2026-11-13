// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::{
    ai::connector::Cli,
    harness::journal::{LoginState, ObservedSrc, Tab},
};

fn login(email: &str, tab: Option<&str>, state: LoginState, at: u64) -> ObservedLogin {
    ObservedLogin {
        cli: Cli::Claude.into(),
        realm: "claude".into(),
        email: email.into(),
        state,
        tab: tab.map(|sid| Tab::of("m65-test", sid)),
        at,
        src: ObservedSrc::Profile,
    }
}

/// Sightings of one login coalesce whatever the spelling, counting each tab once and keeping
/// the newest state; the planner reads milliseconds; a taken queue is empty, and a second
/// pass waits out the minute from the last.
/// NEGATIVE CONTROL: with `gather` keyed by the email as spelled, two rows go to the
/// planner: `` left: 2 right: 1 ``.
#[test]
fn sightings_of_one_login_coalesce_and_passes_stay_a_minute_apart() {
    let mut queue = Queue::default();
    assert!(!queue.start(), "nothing gathered, no pass");
    queue.gather(&login(
        "unicorn@proton.example",
        Some("s1"),
        LoginState::Running,
        10,
    ));
    queue.gather(&login(
        "Unicorn@Proton.example ",
        Some("s2"),
        LoginState::Running,
        12,
    ));
    queue.gather(&login(
        "unicorn@proton.example",
        Some("s1"),
        LoginState::Running,
        11,
    ));
    assert!(queue.start());
    assert!(!queue.start(), "one thread at a time");
    let taken = queue.take(12);
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].tabs, 2);
    assert_eq!(taken[0].at_ms, 12_000);
    assert_eq!(taken[0].state, "running");
    assert_eq!(taken[0].src, "profile");
    assert!(queue.by.is_empty());
    let now = Instant::now();
    assert_eq!(queue.wait(now), Duration::ZERO);
    queue.last = Some(now);
    assert_eq!(queue.wait(now), EVERY);
}

/// The count is of the tabs running a login, not of the sightings one pass gathered: three
/// tabs whose sightings fall due in three passes count three on the third; a tab that ends
/// or moves to another login leaves the count, and one that stays silent past
/// [`TAB_LIVE_S`] falls out with the next pass, its login passed again as idle.
/// NEGATIVE CONTROL: with the tab map emptied by `take`, the third pass counts one: `` left:
/// 1 right: 3 ``.
#[test]
fn tabs_count_every_tab_running_the_login_across_passes() {
    const A: &str = "a@proton.example";
    const B: &str = "b@proton.example";
    let mut queue = Queue::default();
    for (n, tab) in ["s1", "s2", "s3"].into_iter().enumerate() {
        let at = 100 + 60 * n as u64;
        queue.gather(&login(A, Some(tab), LoginState::Running, at));
        let taken = queue.take(at);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].tabs, n as u64 + 1, "pass {n}");
    }
    queue.gather(&login(A, Some("s1"), LoginState::Idle, 400));
    let taken = queue.take(400);
    assert_eq!((taken[0].tabs, taken[0].state.as_str()), (2, "running"));
    queue.gather(&login(B, Some("s2"), LoginState::Running, 460));
    let mut taken = queue.take(460);
    taken.sort_by(|x, y| x.email.cmp(&y.email));
    assert_eq!(
        taken
            .iter()
            .map(|s| (s.email.as_str(), s.tabs))
            .collect::<Vec<_>>(),
        [(A, 1), (B, 1)],
        "s2 moved from a to b"
    );
    let late = 220 + TAB_LIVE_S + 1;
    queue.gather(&login(B, Some("s2"), LoginState::Running, late));
    let mut taken = queue.take(late);
    taken.sort_by(|x, y| x.email.cmp(&y.email));
    assert_eq!(
        taken
            .iter()
            .map(|s| (s.email.as_str(), s.tabs, s.state.as_str()))
            .collect::<Vec<_>>(),
        [(A, 0, "idle"), (B, 1, "running")],
        "s3 went silent past the live span"
    );
    assert!(queue.take(late).is_empty(), "nothing moved since");
}

/// A pass's thread takes what gathered, and ends once nothing more waits so the next
/// sighting starts another. A test never installs the queue, so `take_up` is inert here.
/// NEGATIVE CONTROL: with `run` leaving `running` set, `` assertion failed: queue.start() ``.
#[test]
fn a_pass_ends_with_its_queue_and_take_up_is_inert_uninstalled() {
    fn quiet(sightings: &[Sighting]) -> Result<Observed, String> {
        assert_eq!(sightings.len(), 1);
        Ok(Observed {
            quiet: Some("not-held"),
            born: 0,
            marked: 0,
            findings: Vec::new(),
            records: 0,
        })
    }
    let queue = Mutex::new(Queue::default());
    queue.lock().unwrap().gather(&login(
        "a@proton.example",
        Some("s1"),
        LoginState::Running,
        5,
    ));
    assert!(queue.lock().unwrap().start());
    run(&queue, quiet);
    let mut held = queue.lock().unwrap();
    assert!(!held.running);
    assert!(held.last.is_some());
    held.gather(&login("a@proton.example", None, LoginState::Idle, 6));
    assert!(held.start());
    drop(held);
    take_up(&login("b@proton.example", None, LoginState::Running, 7));
    assert!(QUEUE.get().is_none());
}

/// A pass that panics still ends its thread's claim: the next sighting starts a pass.
/// NEGATIVE CONTROL: without `run`'s drop guard, `` assertion failed: held.start() ``.
#[test]
fn a_pass_that_panics_frees_the_queue() {
    fn broken(_: &[Sighting]) -> Result<Observed, String> {
        panic!("a pass that panics");
    }
    let queue = Mutex::new(Queue::default());
    let seen = login("a@proton.example", Some("s1"), LoginState::Running, 5);
    queue.lock().unwrap().gather(&seen);
    assert!(queue.lock().unwrap().start());
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&queue, broken)));
    assert!(ended.is_err());
    let mut held = queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(!held.running);
    held.gather(&seen);
    assert!(held.start());
}

/// A swept login reaches the planner as idle with no tab, and as running where a tab of
/// this process runs it: the sweep says a login is here, never that no tab runs it.
#[test]
fn a_swept_login_is_idle_unless_a_tab_runs_it() {
    const A: &str = "a@proton.example";
    let mut queue = Queue::default();
    queue.gather(&login(A, None, LoginState::Idle, 100));
    let taken = queue.take(100);
    assert_eq!((taken[0].tabs, taken[0].state.as_str()), (0, "idle"));
    queue.gather(&login(A, Some("s1"), LoginState::Running, 160));
    queue.gather(&login(A, None, LoginState::Idle, 170));
    let taken = queue.take(170);
    assert_eq!(taken.len(), 1);
    assert_eq!((taken[0].tabs, taken[0].state.as_str()), (1, "running"));
}

/// The sweep hands a login at every look, and the queue passes it once a day: a swept
/// login is gathered again until a pass whose plan ran took it, then not within the day, in
/// whatever spelling; a pass that failed or read no registry took nothing, so the next look
/// is gathered and passed again. A tab's sighting is gathered whatever was passed.
/// NEGATIVE CONTROL: with `run` keeping every pass as taken, the look after the quiet pass
/// is not gathered: `` not taken yet: gathered again ``.
#[test]
fn a_swept_login_is_passed_again_until_a_pass_takes_it() {
    const A: &str = "a@proton.example";
    fn incomplete(_: &[Sighting]) -> Result<Observed, String> {
        Ok(Observed {
            quiet: Some("incomplete"),
            born: 0,
            marked: 0,
            findings: Vec::new(),
            records: 0,
        })
    }
    fn failed(_: &[Sighting]) -> Result<Observed, String> {
        Err("no vault state on this Mac".to_owned())
    }
    fn planned(sightings: &[Sighting]) -> Result<Observed, String> {
        Ok(Observed {
            quiet: None,
            born: 0,
            marked: sightings.len(),
            findings: Vec::new(),
            records: 1,
        })
    }
    let day = IDLE_REFRESH_S.unsigned_abs();
    let now = aterm_time::unix_millis().unwrap() / 1_000;
    let queue = Mutex::new(Queue::default());
    for pass in [incomplete, failed, planned] {
        let mut held = queue.lock().unwrap();
        held.last = None;
        held.gather(&login(A, None, LoginState::Idle, now));
        assert!(held.start(), "not taken yet: gathered again");
        drop(held);
        run(&queue, pass);
    }
    let mut held = queue.lock().unwrap();
    held.gather(&login(
        " A@Proton.example",
        None,
        LoginState::Idle,
        now + 900,
    ));
    assert!(!held.start(), "taken: not within the day");
    held.gather(&login(A, Some("s1"), LoginState::Running, now + 900));
    assert_eq!(held.take(now + 900).len(), 1, "a tab's sighting always");
    held.gather(&login(A, None, LoginState::Idle, now + day + 60));
    assert_eq!(held.take(now + day + 60).len(), 1, "the day is over");
}

/// The sweep reads the login places of the home and the rosters it is given and keeps its
/// day under the state it is given; with no journal sink in a test nothing is keyed, so it
/// notes nothing and writes no file, and a clock before the epoch sweeps nothing.
/// `start_sweep` is inert where the queue is not installed.
#[test]
fn a_sweep_reads_its_own_home_and_start_is_inert_uninstalled() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-lab-sweep-")
        .tempdir()
        .unwrap();
    let state = scratch.path().join("state");
    std::fs::write(
        scratch.path().join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"a@proton.example"}}"#,
    )
    .unwrap();
    assert_eq!(sweep_at(&state, scratch.path(), None, 0), 0);
    assert_eq!(sweep_at(&state, scratch.path(), None, 1_790_000_000), 0);
    assert!(!state.exists());
    start_sweep();
    assert!(QUEUE.get().is_none());
}
