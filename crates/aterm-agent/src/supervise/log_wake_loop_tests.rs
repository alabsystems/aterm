// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The loop's waits on the session log (Phase 6 step 6H-5, ruling U27): a host that keeps a
//! log wakes the wait for a running turn's end or a box, and the screen read after it
//! decides every press ([`super::super::log_wake`]). Each run is set beside the same run on
//! a host with no log, whose waits are the screen's alone.

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

/// The turn's wait sleeps on the log once a read showed the turn running, from the cursor
/// the `status` before it printed, and the read after it decides: every act, every line and
/// every point the loop typed at are the screen-only run's. The first turn of the session
/// waits on the log too: its wait reads the `status` that learns the host keeps a log. A
/// host with no log answers that one `status`, and every later wait is the screen's with
/// no `status` added.
/// NEGATIVE CONTROLS: a wait that skips the log until a decision has read a `status` leaves
/// the first turn on the screen; a `status` whose log the loop does not note reads `status`
/// again at every wait on a host with no log.
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

/// THE SCREEN DECIDES EVERY PRESS (6H-5's negative control). The log closes the turn while
/// the screen still shows it running (a transcript that ends a turn before its last frame):
/// the read after the wake is read as running, no turn is typed until the screen rests, and
/// the next wait, finding no turn open on the log, waits on the screen.
/// NEGATIVE CONTROLS: a wake taken as the turn's rest reports the running screen as one; a
/// cursor kept past the log's answer, or a wait that ignores the log's `turn=-`, sleeps on
/// the log again where the screen's wait serves.
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

/// A host that refuses the wait (an older build answers the comma list `ERR usage`) is
/// asked once: the loop waits on the screen from the refusal on, as it does on a host with
/// no log, and decides the same.
/// NEGATIVE CONTROL: a refusal that does not latch is asked at every wait.
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

/// A wait the step ran out on leaves its cursor standing: the next wait, after the read,
/// starts from it with no `status` between, and answers what came past it.
/// NEGATIVE CONTROL: a cursor dropped at a timeout reads `status` again.
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

/// A box raised mid-turn wakes the log's wait (its `need`), and the read after it presses
/// the box as the screen-only run does. The next wait starts from the cursor a fresh
/// `status` prints, past the box's record.
/// NEGATIVE CONTROL: a wake taken as the turn's rest reads the box as one and never presses.
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

/// ONLY A WAIT AFTER A BUSY READ IS THE LOG'S. A press the box did not take is read again
/// at once (`await gone` answers while no busy row is drawn), never after a log wait,
/// though the host keeps a log and holds the turn open: no read showed the turn running
/// since the press. The one log wait is the first, after the read that showed the turn
/// running, and the box woke it.
/// NEGATIVE CONTROL: a wait that sleeps on the log with no busy read since the press.
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

/// A HEAD THAT ALREADY NAMES THE BOX IS NEVER SLEPT ON. The box was drawn and recorded
/// between the read that showed the turn running and the `status` that gives the cursor, so
/// the head prints its need and its record stands at the cursor: a wait from there would
/// find nothing past it and sleep its whole step. The screen's wait serves, the read after
/// it presses the box as the screen-only run does, and the turn's next wait is the log's.
/// NEGATIVE CONTROL: a head taken whenever it holds a turn open sleeps on the box's record.
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

/// A READER WORD THAT OWES NO RECORD IS WATCHED ON THE SCREEN. While the host's screen
/// reader reads the turn as neither `busy` nor `idle` (`unknown`, say), no move of its word
/// is owed and nothing would wake a log wait when the busy row goes: the screen's wait
/// serves every turn, and the loop decides as the screen-only run does.
/// NEGATIVE CONTROL: a head taken whatever the reader's word sleeps on the log.
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

/// A HOST THAT REFUSES `status` IS ASKED FOR THE LOG ONCE. The first busy wait's `status`
/// is refused; the loop reads the host as keeping no log and waits on the screen, and no
/// later busy wait asks `status` again for the log's head (each decision still reads its
/// own, as it did before 6H-5). It decides as the same host with no log does.
/// NEGATIVE CONTROL: a refusal that teaches nothing asks `status` at every busy wait.
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
