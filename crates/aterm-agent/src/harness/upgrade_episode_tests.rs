// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the hold's episode ([`Episode`]) and a wall's first sighting ([`sighted`]): the clock
//! itself, the record's wait and own-work hold counted on it (m16's two tabs replayed, with
//! today's clock as the negative control), and a wall no transcript row dates aging from the
//! first look that saw it, across the record's save and a successor's adoption.

use std::path::PathBuf;

use super::super::{HandAt, Proc, Row, St, wall_age};
use super::*;
use crate::harness::upgrade::{self, Candidate, Facts, Phase, Request, Source, Step, Version};

const T0: u64 = 1_800_000_000;
const PID: u32 = 4_242;
const TARGET: &str = "2.1.325";

#[test]
fn an_episode_keeps_its_start_until_progress_or_a_proved_clear_gap() {
    let mut e = Episode::default();
    e.clear(T0);
    assert_eq!(e, Episode::default(), "a clear look begins nothing");
    e.hold(T0);
    e.hold(T0 + 60);
    assert_eq!(
        (e.since, e.age(T0 + 600)),
        (T0, 600),
        "held from its first look"
    );
    e.clear(T0 + 100);
    e.hold(T0 + 100 + CLEAR_GAP_S + 600);
    assert_eq!(
        e.since, T0,
        "one clear look proves no gap, however long before the next hold"
    );
    e.clear(T0 + 2_000);
    e.clear(T0 + 2_000 + CLEAR_GAP_S - 1);
    assert_eq!(e.since, T0, "a gap shorter than the clear gap");
    e.hold(T0 + 2_000 + CLEAR_GAP_S);
    e.clear(T0 + 3_000);
    assert_eq!(
        e.clear_at,
        T0 + 3_000,
        "a hold between clear looks starts the gap again"
    );
    e.clear(T0 + 3_000 + CLEAR_GAP_S);
    assert_eq!(e, Episode::default(), "a gap proved that long ends it");
    e.hold(T0 + 4_000);
    e.end();
    assert_eq!(e.age(T0 + 5_000), 0, "progress ends it");
    assert_eq!(sighted(0, true, T0), T0, "a wall first seen here");
    assert_eq!(sighted(T0, true, T0 + 60), T0, "kept while it stands");
    assert_eq!(sighted(T0, false, T0 + 60), 0, "gone with the wall");
}

/// A wait that changes its word keeps its episode's clock (ATERM-KI-1112-964): the record's word
/// clock starts again at each word, as the status page's rules for one word read it, while the
/// hold's episode keeps its first look through own work, a usage limit, a short next point and a
/// box, and `wait_for=` reads the episode. A move, a new target build and a person's key end it.
/// NEGATIVE CONTROL: the word's own clock, which `wait_for=` read before, says the hold began at
/// its last word.
#[test]
fn a_wait_that_changes_its_word_keeps_its_episode_clock() {
    let mut st = St {
        from: "2.1.323".to_string(),
        to: "2.1.324".to_string(),
        proc_start: "A".to_string(),
        ..St::default()
    };
    for (word, at) in [
        ("background", T0),
        ("not-idle:busy", T0 + 300),
        ("limited", T0 + 600),
        ("next-point", T0 + 900),
        ("box", T0 + 1_000),
        ("background", T0 + 1_200),
    ] {
        st.note_wait(word, at);
        assert_eq!(
            st.wait_episode.since, T0,
            "{word}: the episode keeps its start"
        );
        assert_eq!(st.wait_since, at, "{word}: the word's own clock");
    }
    let row = |st: &St| Row {
        wait: st.wait.clone(),
        wait_since: st.wait_since,
        episode_since: st.wait_episode.since,
        ..Row::default()
    };
    let now = T0 + 7 * 3_600;
    assert_eq!(
        row(&st).waited_for(now),
        Some(now - T0),
        "wait_for reads it"
    );
    assert!(
        row(&st).line(now).contains(" wait_for=7h "),
        "{}",
        row(&st).line(now)
    );
    assert_eq!(
        row(&st).to_json(now)["wait_for_s"].as_u64(),
        Some(now - T0),
        "and its JSON"
    );
    assert_eq!(
        Row {
            episode_since: 0,
            ..row(&st)
        }
        .waited_for(now),
        Some(now - T0 - 1_200),
        "the word's clock alone is the restart m16's tab showed (`wait_for=7m`)"
    );

    // A new target build is progress: a retarget in place ends the hold's episode; the agent's
    // own work keeps its episode, as its told deadline does.
    let running = Version::parse("2.1.323").expect("version");
    let newer = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse(TARGET).expect("version"),
        source: Source::Managed,
    };
    let who = Proc {
        start: "A",
        started_s: 0,
    };
    let own = st.own;
    let mut st = St::for_target(Some(st), &running, &newer, who, now);
    assert_eq!(st.to, TARGET, "retargeted in place");
    assert_eq!(
        st.wait_episode,
        Episode::default(),
        "a new target build is progress"
    );
    assert_eq!(st.own, own, "the own-work hold is the process's");
    st.note_wait("background", now + 60);
    assert_eq!(
        st.wait_episode.since,
        now + 60,
        "the next hold is an episode of its own"
    );

    // A move is progress.
    st.note_step("announced:1", "idle", now + 120);
    assert_eq!(st.wait_episode, Episode::default(), "a move ends it");
}

/// A record written before the hold's episode was kept carries no `episode_since`: it reads back
/// with its standing wait's episode begun at that word's own clock ([`read_back`]), so the first
/// look of the build that reads it keeps that start, and `wait_for=` still reads the hold that
/// stood across the update (m16's tabs, held about 11 hours) rather than one begun at that look.
/// A next point begins none. A record that keeps its episode reads back as written, an empty one
/// too (a person's key ended it while the word stood).
/// NEGATIVE CONTROL: that empty episode is the reading a record from before the episode got: its
/// next look begins the episode there, and `wait_for=` reads a minute of an 11-hour hold.
#[test]
fn a_record_from_before_the_episode_keeps_its_holds_start() {
    let record = |wait: &str, extra: &str| {
        format!(
            r#"{{"phase":"pending","from":"2.1.323","to":"2.1.324","source":"managed","tab":"s-1","salt":1,"pending_since":{T0},"wait":"{wait}","wait_since":{T0}{extra}}}"#
        )
    };
    let row = |st: &St| Row {
        wait: st.wait.clone(),
        wait_since: st.wait_since,
        episode_since: st.wait_episode.since,
        ..Row::default()
    };
    let look = T0 + 11 * 3_600;

    let mut old = St::from_json(&record("background", "")).expect("an older record reads back");
    assert_eq!(
        old.wait_episode,
        Episode::from(T0),
        "its hold counts from its word's own clock"
    );
    old.note_wait("background", look);
    assert_eq!(
        old.wait_episode.since, T0,
        "the first look keeps that start"
    );
    assert_eq!(
        row(&old).waited_for(look + 60),
        Some(look + 60 - T0),
        "wait_for reads the whole hold"
    );

    let clear = St::from_json(&record("next-point", "")).expect("an older record reads back");
    assert_eq!(
        clear.wait_episode,
        Episode::default(),
        "a next point holds nothing"
    );

    let kept = St::from_json(&record(
        "background",
        &format!(
            r#","episode_since":{},"episode_clear_at":{}"#,
            T0 + 60,
            T0 + 90
        ),
    ))
    .expect("the record reads back");
    assert_eq!(
        kept.wait_episode,
        Episode {
            since: T0 + 60,
            clear_at: T0 + 90
        },
        "a kept episode reads back as written"
    );

    let mut empty = St::from_json(&record(
        "background",
        r#","episode_since":0,"episode_clear_at":0"#,
    ))
    .expect("the record reads back");
    assert_eq!(empty.wait_episode, Episode::default(), "an empty one too");
    empty.note_wait("background", look);
    assert_eq!(
        row(&empty).waited_for(look + 60),
        Some(60),
        "its next look begins the episode: the reading an older record got without its seed"
    );
}

/// What a clock under test does at the replay's looks: the record noting a look's word (a live
/// step's, through [`St::note_step`], or the watch's dry look, through [`St::note_wait`]), a line's
/// first key, the line taken whole, and what a live look at a break reads from it.
trait Clock {
    fn look(&mut self, word: &str, now: u64);
    fn watch(&mut self, word: &str, now: u64);
    fn keyed(&mut self, now: u64);
    fn told(&mut self, now: u64);
    fn hold_s(&self, now: u64) -> u64;
    fn rests(&self, now: u64) -> bool;
    fn deadline_at(&self) -> u64;
    fn tells(&self) -> u32;
}

impl Clock for St {
    fn look(&mut self, word: &str, now: u64) {
        let step = if word == "background" && self.tells == 1 && self.told_at == now {
            "told:background".to_string()
        } else {
            format!("wait:{word}")
        };
        self.note_step(&step, "busy", now);
    }
    fn watch(&mut self, word: &str, now: u64) {
        self.note_wait(word, now);
    }
    fn keyed(&mut self, now: u64) {
        self.tell_keyed(now);
    }
    fn told(&mut self, now: u64) {
        St::told(self, PID, now);
    }
    fn hold_s(&self, now: u64) -> u64 {
        self.own_hold_s(now)
    }
    fn rests(&self, now: u64) -> bool {
        self.tell_rests(now)
    }
    fn deadline_at(&self) -> u64 {
        if self.deadline_pid == PID {
            self.deadline_at
        } else {
            0
        }
    }
    fn tells(&self) -> u32 {
        self.tells
    }
}

/// Today's clock before the episode, as `St::note_wait` and `tell` kept `St::own_since`: begun at
/// the first own-work word, zeroed at every person's or limit's word (aterm's own unsent line
/// among them), and started again at a line's first key, the line resting on that restart alone.
#[derive(Default)]
struct Today {
    own_since: u64,
    held_before: u64,
    tells: u32,
    deadline_at: u64,
}

impl Clock for Today {
    fn look(&mut self, word: &str, now: u64) {
        self.watch(word, now);
    }
    fn watch(&mut self, word: &str, now: u64) {
        let head = word.split_once(':').map_or(word, |(head, _)| head);
        match head {
            "background" | "not-idle" | "status-stale" if self.own_since == 0 => {
                self.own_since = now;
            }
            "box" | "draft" | "login" | "attended" | "held" | "terminal" | "left-typed"
            | "limited" => self.own_since = 0,
            _ => {}
        }
    }
    fn keyed(&mut self, now: u64) {
        self.held_before = self.own_since;
        self.own_since = now;
    }
    fn told(&mut self, now: u64) {
        self.own_since = self.held_before;
        self.tells = 1;
        self.deadline_at = now + upgrade::WORK_DEADLINE_S;
    }
    fn hold_s(&self, now: u64) -> u64 {
        if self.own_since == 0 {
            0
        } else {
            now - self.own_since
        }
    }
    fn rests(&self, _: u64) -> bool {
        false
    }
    fn deadline_at(&self) -> u64 {
        self.deadline_at
    }
    fn tells(&self) -> u32 {
        self.tells
    }
}

/// What a replay saw: when each line was typed and whether it was taken, the hold the taken line
/// named, the longest hold a look read before a line was taken, and when and why the round moved.
#[derive(Debug, Default, PartialEq, Eq)]
struct Trace {
    lines: Vec<(u64, bool)>,
    named_s: u64,
    untold_s: u64,
    moved: Option<(u64, Option<upgrade::Yield>)>,
}

/// A live look at a break of the agent's own background work, in a session its autopilot carries
/// at the land rung, read from `clock` at `now` (the window's facts at such a look).
fn at_break(clock: &dyn Clock, now: u64) -> Facts {
    let deadline_at = clock.deadline_at();
    Facts {
        status: "busy".to_string(),
        status_age_s: 3_600,
        composer_empty: true,
        background: vec!["zsh".to_string()],
        background_point: true,
        quiet_s: 3_600,
        rung: upgrade::Rung::Land,
        still_s: 3_600,
        quiet: true,
        may_tell: true,
        own_hold_s: clock.hold_s(now),
        tell_rests: clock.rests(now),
        deadline_set: deadline_at != 0,
        work_due: deadline_at != 0 && now >= deadline_at,
        tells: clock.tells(),
        ..Facts::default()
    }
}

/// m16's tab s-7844dfcf (2026-11-14) as its record and ledger read: a rolling set of zsh children
/// under the agent, each from under a minute to well over an hour, with short gaps between them;
/// the loop's breaks while one runs looked at every minute, the watch's dry look in each gap
/// reading the move held at the next point; the words changing between `background` and
/// `status-stale`, as a busy status read late does; and every line to the agent typed and not
/// taken whole (here cut on the busy screen, `cut-changed`), standing unsent in the composer at
/// the next look (`left-typed:unsent`) and taken back there, as the ledger's `left-typed-cleared`
/// rows a minute after each line read. Here the third line is taken whole. `clock` keeps the
/// hold.
fn replay(clock: &mut dyn Clock) -> Trace {
    // The children's spans, seconds after T0: gaps of 49, 60, 50 and 40 seconds.
    let children = [
        (0, 51),
        (100, 1_000),
        (1_060, 3_000),
        (3_050, 5_000),
        (5_040, 9_000),
    ];
    let runs = |t: u64| children.iter().any(|&(a, b)| (a..b).contains(&t));
    let mut trace = Trace::default();
    let mut left_standing = false;
    let mut t = 0;
    while t < 9_000 && trace.moved.is_none() {
        let now = T0 + t;
        if !runs(t) {
            // The watch's dry look in the gap: the move held at the next point.
            clock.watch(upgrade::NEXT_POINT, now + 10);
            t = children
                .iter()
                .map(|&(a, _)| a)
                .find(|&a| a > t)
                .unwrap_or(9_000);
            t = t.div_ceil(60) * 60;
            continue;
        }
        if left_standing {
            // aterm's own line stands unsent: settled first, and taken back.
            clock.look("left-typed:unsent", now);
            left_standing = false;
            t += 60;
            continue;
        }
        let f = at_break(clock, now);
        if !trace.lines.iter().any(|&(_, taken)| taken) {
            trace.untold_s = trace.untold_s.max(f.own_hold_s);
        }
        if f.deadline_set {
            assert!(
                !upgrade::deadline_voids(&f),
                "a deadline stands across rolling children: {f:?}"
            );
        }
        let step = upgrade::break_step(
            upgrade::requested_step(&Request::None, &Phase::Pending, &f, false, now, TARGET),
            &f,
        );
        match step {
            Step::Tell(_) => {
                clock.keyed(now);
                if trace.lines.len() < 2 {
                    trace.lines.push((now, false));
                    clock.look("cut-changed", now);
                    left_standing = true;
                } else {
                    trace.lines.push((now, true));
                    trace.named_s = f.own_hold_s;
                    clock.told(now);
                    clock.look("background", now);
                }
            }
            Step::Terminate => trace.moved = Some((now, upgrade::own_yields(&f))),
            Step::Wait(word) => {
                let word = if t % 600 == 300 { "status-stale" } else { word };
                clock.look(word, now);
            }
            other => panic!("a quiet pending round at a break: {other:?} at {f:?}"),
        }
        t += 60;
    }
    trace
}

/// m16's live residue (2026-11-13/14): two tabs pending 2.1.323 → 2.1.325 for 11 hours at
/// `wait=background`, `cause=own-work`, never told, one reading `wait_for=7m`. Counted as one
/// episode, the hold keeps its first look across rolling children, short gaps, changes of word,
/// lines cut on the busy screen and aterm's own unsent line: the first line goes `WORK_TELL_S`
/// into the hold, a cut line rests `WORK_TELL_S` and is typed again naming the whole hold, the
/// deadline it names stands across the children and gaps that follow, and the round moves over
/// the work at its first break past that deadline. The record's clocks survive its save and a
/// successor loop's read in the middle of it.
#[test]
fn a_rolling_background_hold_keeps_its_episode_and_is_told() {
    let mut st = St {
        to: TARGET.to_string(),
        ..St::default()
    };
    let trace = replay(&mut st);
    let tell = upgrade::WORK_TELL_S;
    assert_eq!(
        trace.lines,
        [
            (T0 + tell, false),
            (T0 + 2 * tell, false),
            (T0 + 3 * tell, true)
        ],
        "told at WORK_TELL_S into the hold, each untaken line resting WORK_TELL_S: {trace:?}"
    );
    assert_eq!(
        trace.named_s,
        3 * tell,
        "the taken line names the whole hold"
    );
    assert_eq!(
        trace.untold_s,
        3 * tell,
        "and no look before it read less of it"
    );
    assert_eq!(
        trace.moved,
        Some((
            T0 + 3 * tell + upgrade::WORK_DEADLINE_S,
            Some(upgrade::Yield::Deadline)
        )),
        "moved over the work at its told deadline"
    );
    assert_eq!(st.own.since, T0, "one episode from the first look");

    // The loop's handoff: the record saved and read by a successor loop keeps every clock.
    let read = St::from_json(&st.to_json()).expect("the record reads back");
    assert_eq!(
        (read.own, read.wait_episode, read.tell_tried_at),
        (st.own, st.wait_episode, st.tell_tried_at)
    );
    let mut resting = St {
        to: TARGET.to_string(),
        ..St::default()
    };
    resting.note_step("wait:background", "busy", T0);
    resting.tell_keyed(T0 + tell);
    resting.note_step("wait:cut-changed", "busy", T0 + tell);
    let read = St::from_json(&resting.to_json()).expect("the record reads back");
    assert_eq!(
        (read.own.since, read.tell_rests(T0 + tell + 60)),
        (T0, true),
        "a resting line rests in the successor too"
    );
}

/// NEGATIVE CONTROL for [`a_rolling_background_hold_keeps_its_episode_and_is_told`]: the same
/// replay on today's clock (`St::own_since` before the episode) starts the hold again at every
/// line's first key and zeroes it at aterm's own unsent line, so until a line is taken no look
/// reads more than `WORK_TELL_S` of a hold that ran from the replay's start, the taken line names
/// `WORK_TELL_S` alone, and every line after the first and the move come later than the episode
/// puts them. On m16 no line was ever taken, so no look read more.
#[test]
fn the_word_reset_clock_negative_control_fails() {
    let mut today = Today::default();
    let trace = replay(&mut today);
    let tell = upgrade::WORK_TELL_S;
    assert_eq!(
        trace.untold_s, tell,
        "the hold never reads past WORK_TELL_S before a line is taken: {trace:?}"
    );
    assert_eq!(
        trace.named_s, tell,
        "the taken line names WORK_TELL_S alone"
    );
    assert_ne!(
        trace.lines,
        [
            (T0 + tell, false),
            (T0 + 2 * tell, false),
            (T0 + 3 * tell, true)
        ],
        "its lines are not the episode's"
    );
    let moved = trace
        .moved
        .expect("a taken line's deadline still moves it")
        .0;
    assert!(
        moved > T0 + 3 * tell + upgrade::WORK_DEADLINE_S,
        "and it moves later: {trace:?}"
    );
}

/// A wall only the screen shows, or one whose transcript or keys cannot be read, ages from the
/// first look that saw it (ATERM-KI-1112-964): the watch's dry look that records the wall's word
/// stamps the sighting, later looks keep it, the record's save and a successor loop's read keep
/// it, a live look reads its age from it, and past `LOGIN_WALL_S` with nobody seen the round
/// moves there as at an idle point. A look that finds no wall clears it. The usage limit is dated
/// the same way. NEGATIVE CONTROL: with no sighting kept (the wall's age before the episode), the
/// wall never ages and the round waits on it for good.
#[test]
fn a_wall_only_the_screen_shows_ages_from_its_first_sighting() {
    let wall = Facts {
        status: "idle".to_string(),
        status_age_s: 3_600,
        composer_empty: true,
        quiet_s: 3_600,
        idle_looks: upgrade::IDLE_LOOKS,
        rung: upgrade::Rung::Land,
        still_s: 3_600,
        quiet: true,
        may_tell: true,
        login: true,
        ..Facts::default()
    };
    let mut st = St::default();
    st.note_wait("login", T0);
    st.note_wait("login", T0 + 60);
    assert_eq!(st.login_seen, T0, "the first look that saw it, kept");
    let st = St::from_json(&st.to_json()).expect("the record reads back");
    assert_eq!(
        st.login_seen, T0,
        "across the record's save and a successor's read"
    );
    let bound = T0 + upgrade::LOGIN_WALL_S;
    let step = |f: &Facts, now| {
        upgrade::requested_step(&Request::None, &Phase::Pending, f, false, now, TARGET)
    };
    for hand in [HandAt::Never, HandAt::Unknown] {
        let seen = sighted(st.login_seen, true, bound - 1);
        let young = Facts {
            login_s: wall_age(true, &wall, None, seen, hand, bound - 1),
            ..wall.clone()
        };
        assert_eq!(young.login_s, upgrade::LOGIN_WALL_S - 1, "{hand:?}");
        assert_eq!(step(&young, bound - 1), Step::Wait("login"), "{hand:?}");
        let aged = Facts {
            login_s: wall_age(true, &wall, None, seen, hand, bound),
            ..wall.clone()
        };
        assert!(upgrade::walls_outlasted(&aged), "{hand:?}: {aged:?}");
        assert_eq!(step(&aged, bound), Step::Terminate, "{hand:?}");
        // NEGATIVE CONTROL: no sighting kept, no row: the wall never ages.
        let undated = Facts {
            login_s: wall_age(true, &wall, None, 0, hand, bound + 86_400),
            ..wall.clone()
        };
        assert_eq!(undated.login_s, 0, "{hand:?}");
        assert_eq!(
            step(&undated, bound + 86_400),
            Step::Wait("login"),
            "{hand:?}"
        );
    }
    assert_eq!(
        sighted(st.login_seen, false, bound),
        0,
        "a look with no wall clears it"
    );

    let mut st = St::default();
    st.note_wait("limited", T0);
    st.note_wait("background", T0 + 60);
    st.note_wait("limited", T0 + 120);
    assert_eq!(
        st.limit_seen, T0,
        "the usage limit's first sighting, kept across another word"
    );
}

/// A successor's adoption keeps the first sighting: a newer build landing for the same process
/// adopts the record's history, the walls' sightings, the own-work episode and a resting line with
/// it; another process starts them all afresh.
#[test]
fn a_first_sighting_survives_a_successors_adoption() {
    let running = Version::parse("2.1.323").expect("version");
    let newer = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse(TARGET).expect("version"),
        source: Source::Managed,
    };
    let rested = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        from: "2.1.323".to_string(),
        to: "2.1.324".to_string(),
        pending_since: T0,
        proc_start: "A".to_string(),
        login_seen: T0 + 10,
        limit_seen: T0 + 20,
        own: Episode::from(T0 + 30),
        tell_tried_at: T0 + 40,
        ..St::default()
    };
    let adopted = St::for_target(
        Some(rested.clone()),
        &running,
        &newer,
        Proc {
            start: "A",
            started_s: 0,
        },
        T0 + 900,
    );
    assert_eq!(adopted.to, TARGET);
    assert_eq!(
        (
            adopted.login_seen,
            adopted.limit_seen,
            adopted.own,
            adopted.tell_tried_at
        ),
        (T0 + 10, T0 + 20, Episode::from(T0 + 30), T0 + 40),
        "the successor adopts them"
    );
    let other = St::for_target(
        Some(rested),
        &running,
        &newer,
        Proc {
            start: "B",
            started_s: 0,
        },
        T0 + 900,
    );
    assert_eq!(
        (
            other.login_seen,
            other.limit_seen,
            other.own,
            other.tell_tried_at
        ),
        (0, 0, Episode::default(), 0),
        "another process starts afresh"
    );
}
