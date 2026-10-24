// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The loop's waits on the session log ([`super::super::log_wake`]): a host that keeps a log
//! wakes the wait for a turn's end or a box, and the screen read after it decides every press.
//! Each run is compared with the same run on a host with no log.

use super::*;

/// A turn that rests and is continued, then a second that rests on a stop.
fn worked_twice() -> Vec<Vec<String>> {
    vec![
        busy_screen(),
        busy_screen(),
        busy_screen(),
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
        busy_screen(),
        busy_screen(),
        busy_screen(),
        ended(STOP),
    ]
}

/// A turn that rests and is continued, then a second that raises a box mid-turn and rests
/// on a stop once it is pressed.
fn worked_then_boxed() -> Vec<Vec<String>> {
    vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
        busy_screen(),
        busy_screen(),
        busy_screen(),
        bash_box(&["⏺ Looking."], "git status"),
        busy_screen(),
        busy_screen(),
        ended(STOP),
    ]
}

/// The worked-twice run's hold: the first rest stands until the loop types.
fn held_twice(m: &mut Mock) {
    m.turn_releases = Some(4);
}

/// The boxed run's holds: the box takes the loop's press (`fenced`), and the first rest and
/// the box each stand until the loop acts on them.
fn held_boxed(m: &mut Mock) {
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(7);
    m.human = vec!["null"];
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named key\n"
        .to_string();
    m.turn_releases = Some(1);
    m.key_releases = Some(5);
}

fn run_on(
    screens: Vec<Vec<String>>,
    log: Option<LogWorld>,
    hold: fn(&mut Mock),
) -> (Vec<String>, Mock) {
    let mut m = Mock::new(screens);
    hold(&mut m);
    m.vanish_after = Some(2);
    m.log = log;
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    (lines, m)
}

fn log_of(m: &Mock) -> &LogWorld {
    m.log.as_ref().expect("a host with a log")
}

/// What the loop did to the session: the turns it typed, the keys it pressed, the marks it
/// set and the badges it raised. Reads and waits are left out.
fn acts(m: &Mock) -> Vec<&String> {
    m.requests
        .iter()
        .filter(|r| {
            matches!(verb_of(r), "turn" | "key" | "send" | "mark") || r.starts_with("meta set ")
        })
        .collect()
}

/// Where the `n`th `await log` request stands in the run's requests.
fn nth_log_wait(m: &Mock, n: usize) -> usize {
    m.requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.starts_with("await log "))
        .nth(n)
        .map(|(at, _)| at)
        .unwrap_or_else(|| panic!("await log #{n}: {:#?}", m.requests))
}

const READ: &str = "text --json tail=40";
const WAIT_40: &str = "log close=turn,need,kind=agent since=40 ep=1 timeout 20000";
const WAIT_41: &str = "log close=turn,need,kind=agent since=41 ep=1 timeout 20000";
const WAIT_42: &str = "log close=turn,need,kind=agent since=42 ep=1 timeout 20000";
const GONE: &str = "await gone esc.to.interrupt rows 2 7 timeout 20000";

/// How many `status` reads stand right before a screen wait for the busy row to go.
fn statuses_before_gone(m: &Mock) -> usize {
    m.requests
        .windows(2)
        .filter(|w| w[0] == "status" && w[1].starts_with("await gone "))
        .count()
}

/// Every turn's wait, the first included, sleeps on the log once a read shows the turn running,
/// from the cursor the `status` before it printed, and the read after it decides as the
/// screen-only run does. A host with no log answers one `status`, then is waited on the screen
/// alone.
#[test]
fn the_turn_wait_sleeps_on_the_log_and_the_screen_decides_the_press() {
    let (plain, pm) = run_on(worked_twice(), None, held_twice);
    let (lines, m) = run_on(worked_twice(), Some(LogWorld::new()), held_twice);
    assert_eq!(
        plain,
        [
            "EVENT idle seq=105 ⏺ Fixed the parser; the suite is green.",
            "CONTINUED seq=105 rule=continue@v1 keep going",
            "EVENT idle seq=109 ⏺ I need your decision on the schema before I go on.",
            "EXIT session gone (await seq 109 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(lines, plain, "{:#?}", m.requests);
    assert_eq!(acts(&m), acts(&pm));
    assert_eq!(m.turns_served, [5], "the loop typed at the first rest");
    assert_eq!(pm.turns_served, m.turns_served);
    assert!(
        !pm.requests.iter().any(|r| r.starts_with("await log")),
        "a host with no log is never asked: {:#?}",
        pm.requests
    );
    let log = log_of(&m);
    assert_eq!(log.asked, [WAIT_40, WAIT_41], "{:#?}", m.requests);
    for n in 0..2 {
        let at = nth_log_wait(&m, n);
        assert_eq!(
            m.requests[at - 2..=at + 1],
            [READ, "status", &format!("await {}", log.asked[n]), READ],
            "a busy read, the log's head, the wait, and the read that decides: {:#?}",
            m.requests
        );
    }
    assert_eq!(log.slept, 5, "the waits slept through both turns' frames");
    let typed = m
        .requests
        .iter()
        .position(|r| verb_of(r) == "turn")
        .expect("a turn typed");
    assert!(
        m.requests[..typed]
            .iter()
            .any(|r| r.starts_with("await log")),
        "the first turn, before any decision has read a status, waits on the log: {:#?}",
        m.requests
    );
    assert_eq!(
        pm.requests[2..=4],
        [READ, "status", GONE],
        "a host with no log answers the first wait's status: {:#?}",
        pm.requests
    );
    assert_eq!(
        statuses_before_gone(&pm),
        1,
        "and is waited on the screen from then on, with no status added: {:#?}",
        pm.requests
    );
}

/// A log close while the screen still shows the turn running types nothing until the screen
/// rests, and the next wait, with no turn open on the log, is the screen's.
#[test]
fn a_log_close_with_no_screen_idle_never_presses() {
    let (plain, pm) = run_on(worked_twice(), None, held_twice);
    let mut early = LogWorld::new();
    early.early = 1;
    let (lines, m) = run_on(worked_twice(), Some(early), held_twice);
    assert_eq!(lines, plain, "{:#?}", m.requests);
    assert_eq!(acts(&m), acts(&pm));
    assert_eq!(
        m.turns_served,
        [5],
        "nothing typed at the log's close: {:#?}",
        m.requests
    );
    let log = log_of(&m);
    assert_eq!(log.asked, [WAIT_40, WAIT_41], "{:#?}", m.requests);
    assert!(
        log.records[0].1.starts_with("close t1 turn"),
        "{:?}",
        log.records
    );
    let at = nth_log_wait(&m, 0);
    assert_eq!(
        m.requests[at + 1..at + 4],
        [READ, "status", GONE],
        "the read decides, and with no turn open the screen's wait serves: {:#?}",
        m.requests
    );
}

/// A host that refuses the wait (`ERR usage` to the comma list) is asked once, then watched on
/// the screen.
#[test]
fn a_host_that_refuses_await_log_is_watched_on_the_screen() {
    let (plain, pm) = run_on(worked_twice(), None, held_twice);
    let mut older = LogWorld::new();
    older.refused = true;
    let (lines, m) = run_on(worked_twice(), Some(older), held_twice);
    assert_eq!(lines, plain, "{:#?}", m.requests);
    let log = log_of(&m);
    assert_eq!(log.asked, [WAIT_40], "asked once: {:#?}", m.requests);
    let at = nth_log_wait(&m, 0);
    assert_eq!(m.requests[at - 1], "status");
    let mut screen_only = m.requests.clone();
    screen_only.remove(at);
    assert_eq!(
        screen_only, pm.requests,
        "past the refusal, the screen-only run's requests"
    );
}

/// A log wait that runs out keeps its cursor: the next starts from it with no `status` between.
#[test]
fn a_log_wait_that_runs_out_keeps_its_cursor() {
    let (plain, _) = run_on(worked_twice(), None, held_twice);
    let mut slow = LogWorld::new();
    slow.timeouts = 1;
    let (lines, m) = run_on(worked_twice(), Some(slow), held_twice);
    assert_eq!(lines, plain, "{:#?}", m.requests);
    let log = log_of(&m);
    assert_eq!(log.asked, [WAIT_40, WAIT_40, WAIT_41], "{:#?}", m.requests);
    let (first, second) = (nth_log_wait(&m, 0), nth_log_wait(&m, 1));
    assert_eq!(
        m.requests[first + 1..second],
        [READ],
        "one read between: {:#?}",
        m.requests
    );
}

/// A box raised mid-turn wakes the log's wait (its `need`), and the read after it presses it;
/// the next wait starts past the box's record.
#[test]
fn the_log_wakes_the_loop_for_a_box_and_the_screen_presses_it() {
    let (plain, pm) = run_on(worked_then_boxed(), None, held_boxed);
    let (lines, m) = run_on(worked_then_boxed(), Some(LogWorld::new()), held_boxed);
    assert!(
        plain
            .iter()
            .any(|l| l.starts_with("APPROVED ") && l.ends_with(" git status")),
        "{plain:#?}"
    );
    assert_eq!(lines, plain, "{:#?}", m.requests);
    assert_eq!(acts(&m), acts(&pm));
    assert_eq!(m.presses().len(), 1, "{:#?}", m.requests);
    let log = log_of(&m);
    assert_eq!(log.asked, [WAIT_40, WAIT_41, WAIT_42], "{:#?}", m.requests);
    assert!(
        log.records[1].1.starts_with("open p1 step approval"),
        "the second turn's wake is the box's: {:?}",
        log.records
    );
    let at = nth_log_wait(&m, 1);
    assert_eq!(m.requests[at + 1], READ);
    let next = m.requests[at + 2..]
        .iter()
        .find(|r| verb_of(r) == "key" || r.starts_with("await "))
        .expect("a press");
    assert_eq!(
        verb_of(next),
        "key",
        "the read after the wake pressed: {:#?}",
        m.requests
    );
}

/// A press the box did not take is read again at once, never after a log wait: only a wait
/// after a busy read is the log's.
#[test]
fn the_wait_after_a_press_that_did_not_land_stays_the_screens() {
    let mut m = Mock::new(vec![busy_screen(), bash_box(&["⏺ Looking."], "git status")]);
    held_boxed(&mut m);
    m.turn_releases = None;
    m.key_releases = None;
    m.stall_sleep = Some(Duration::from_millis(50));
    m.exit_when = Some(|m| m.presses().len() >= 3);
    m.log = Some(LogWorld::new());
    let _ = watch_lines(&mut m, &hosted(30));
    assert!(m.presses().len() >= 3, "tried again: {:#?}", m.requests);
    let log = log_of(&m);
    assert!(!log.heads.is_empty(), "the loop read the log's head");
    assert_eq!(log.asked, [WAIT_40], "{:#?}", m.requests);
    let pressed = m
        .requests
        .iter()
        .position(|r| verb_of(r) == "key")
        .expect("a press");
    assert!(nth_log_wait(&m, 0) < pressed, "{:#?}", m.requests);
}

/// A box drawn and recorded between the busy read and the loop's `status`, whose holds
/// stand until the loop presses it.
fn raced_box() -> (Vec<Vec<String>>, fn(&mut Mock)) {
    fn hold(m: &mut Mock) {
        held_boxed(m);
        m.turn_releases = None;
        m.key_releases = Some(1);
    }
    (
        vec![
            busy_screen(),
            bash_box(&["⏺ Looking."], "git status"),
            busy_screen(),
            busy_screen(),
            busy_screen(),
            ended(STOP),
        ],
        hold,
    )
}

/// A head that already names the box (drawn between the busy read and the `status`) is never
/// slept on: the screen's wait serves, and the turn's next wait is the log's.
#[test]
fn a_box_recorded_before_the_heads_status_is_never_slept_on() {
    let (screens, hold) = raced_box();
    let (plain, pm) = run_on(screens.clone(), None, hold);
    let mut racing = LogWorld::new();
    racing.races = 1;
    let (lines, m) = run_on(screens, Some(racing), hold);
    assert!(
        plain
            .iter()
            .any(|l| l.starts_with("APPROVED ") && l.ends_with(" git status")),
        "{plain:#?}"
    );
    assert_eq!(lines, plain, "{:#?}", m.requests);
    assert_eq!(acts(&m), acts(&pm));
    assert_eq!(m.presses().len(), 1, "{:#?}", m.requests);
    let log = log_of(&m);
    assert_eq!(log.races, 0, "the race was run: {:#?}", m.requests);
    assert_eq!(
        log.ran_out, 0,
        "no wait slept on the box: {:#?}",
        m.requests
    );
    assert_eq!(
        m.requests[2..=5],
        [READ, "status", GONE, READ],
        "the head named the box, and the screen's wait served: {:#?}",
        m.requests
    );
    assert_eq!(log.asked, [WAIT_41], "{:#?}", m.requests);
    let pressed = m
        .requests
        .iter()
        .position(|r| verb_of(r) == "key")
        .expect("a press");
    assert!(pressed < nth_log_wait(&m, 0), "{:#?}", m.requests);
}

/// While the screen reader's word is neither `busy` nor `idle`, nothing would wake a log wait,
/// so every wait is the screen's.
#[test]
fn a_reader_word_that_owes_no_record_is_watched_on_the_screen() {
    let (plain, pm) = run_on(worked_twice(), None, held_twice);
    let mut unread = LogWorld::new();
    unread.reader = Some("unknown");
    let (lines, m) = run_on(worked_twice(), Some(unread), held_twice);
    assert_eq!(lines, plain, "{:#?}", m.requests);
    assert_eq!(acts(&m), acts(&pm));
    assert_eq!(m.turns_served, pm.turns_served);
    let log = log_of(&m);
    assert!(!log.heads.is_empty(), "the loop read the log's head");
    assert_eq!(log.asked, Vec::<String>::new(), "{:#?}", m.requests);
}

/// A host that refuses `status` is read as keeping no log from the first busy wait on: no later
/// busy wait asks `status` for the log's head (each decision still reads its own).
#[test]
fn a_host_that_refuses_status_is_asked_for_the_log_once() {
    fn refusing(m: &mut Mock) {
        held_twice(m);
        m.verb_replies.insert(
            "status",
            std::iter::repeat_with(|| err("status is refused here"))
                .take(64)
                .collect(),
        );
    }
    let (plain, pm) = run_on(worked_twice(), None, refusing);
    let (lines, m) = run_on(worked_twice(), Some(LogWorld::new()), refusing);
    assert_eq!(lines, plain, "{:#?}", m.requests);
    assert_eq!(acts(&m), acts(&pm));
    let log = log_of(&m);
    assert_eq!(log.asked, Vec::<String>::new(), "{:#?}", m.requests);
    for run in [&m, &pm] {
        assert_eq!(
            run.requests[2..=4],
            [READ, "status", GONE],
            "the first busy wait asks: {:#?}",
            run.requests
        );
        assert_eq!(
            statuses_before_gone(run),
            1,
            "and no busy wait after it: {:#?}",
            run.requests
        );
    }
}
