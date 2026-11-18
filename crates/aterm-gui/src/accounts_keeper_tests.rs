// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keeper's turns over scripted duties, readings and scratch files: a part runs when what it
//! read moved, at the rerun a run not kept owes, and at a forced or asked turn, and at no other.

use super::*;
use aterm_uds::trace::{Effect, fs};
use std::path::Path;

/// The wall reading of the first turn, in Unix seconds.
const T: i64 = 1_800_000_000;

/// The readings `secs` seconds after the first turn's, over the monotonic `base`.
fn at(base: Instant, secs: u64) -> At {
    At {
        wall_ns: (i128::from(T) + i128::from(secs)) * SEC_NS,
        mono: base + Duration::from_secs(secs),
    }
}

/// Duties that read a roster in both parts and compare the wall reading against a day after
/// [`T`], counting each part's runs; the upkeep writes while `writes` is set.
struct Scripted {
    roster: PathBuf,
    upkeeps: bool,
    writes: bool,
    passes: u32,
    told: u32,
    again: u32,
    upkept: u32,
}

impl Scripted {
    fn new(roster: &Path) -> Self {
        Self {
            roster: roster.to_path_buf(),
            upkeeps: true,
            writes: false,
            passes: 0,
            told: 0,
            again: 0,
            upkept: 0,
        }
    }

    /// `[passes, told, again]`.
    fn ran(&self) -> [u32; 3] {
        [self.passes, self.told, self.again]
    }
}

impl Duties for Scripted {
    fn upkeeps(&self) -> bool {
        self.upkeeps
    }

    fn upkeep(&mut self, now: Now) {
        self.passes += 1;
        let _ = fs::read_to_string(&self.roster);
        let _ = now.before(T + 86_400);
        if self.writes {
            trace::effect(Effect::Write);
        }
    }

    fn upkept(&mut self, _now: Now) {
        self.upkept += 1;
    }

    fn told(&mut self, _now: Now, again: bool) {
        self.told += 1;
        self.again += u32::from(again);
        let _ = fs::read_to_string(&self.roster);
    }
}

/// A scratch roster.
fn roster() -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("accounts.toml");
    std::fs::write(&path, "[[account]]\nlabel = \"a\"\n").expect("the fixture's roster");
    (dir, path)
}

/// L1: ten wakes of every cause with nothing moved run no pass and tell nothing; a touched
/// roster runs one pass and one look after it, and then nothing again.
///
/// NEGATIVE CONTROL (2026-11-16): `Part::due` answering `true` for a kept run failed at the
/// first quiet wake, left `Turned { upkept: true, told: true }`, right `Turned { upkept: false,
/// told: false }`. Restored, the file touched.
#[test]
fn ten_wakes_with_nothing_moved_run_no_pass_and_a_touched_roster_runs_one() {
    let (_dir, path) = roster();
    let base = Instant::now();
    let mut duties = Scripted::new(&path);
    let mut keep = Keep::default();
    trace::read_as_if_later(Duration::from_secs(60), || {
        let first = keep.turn(Woke::Start, false, at(base, 0), &mut duties);
        assert_eq!(
            first,
            Turned {
                upkept: true,
                told: true
            }
        );
        assert_eq!(duties.ran(), [1, 1, 0]);
        assert_eq!(
            keep.paths(),
            BTreeSet::from([(path.clone(), false)]),
            "the paths both parts read"
        );
        let causes = [
            Woke::File,
            Woke::Bell,
            Woke::Deadline,
            Woke::Wall,
            Woke::Unwatched,
        ];
        for nth in 0..10 {
            let woke = causes[nth % causes.len()];
            let turned = keep.turn(woke, false, at(base, 60 * (nth as u64 + 1)), &mut duties);
            assert_eq!(turned, Turned::default(), "{woke:?}");
        }
        assert_eq!(duties.ran(), [1, 1, 0], "nothing moved");
        assert_eq!(
            keep.next(at(base, 600), true),
            Next {
                mono: None,
                wall_ns: Some(i128::from(T + 86_400) * SEC_NS)
            },
            "the comparison's bound is the one deadline"
        );

        std::fs::write(&path, "[[account]]\nlabel = \"b\", touched\n").expect("a touched roster");
        let touched = keep.turn(Woke::File, false, at(base, 660), &mut duties);
        assert_eq!(
            touched,
            Turned {
                upkept: true,
                told: true
            }
        );
        assert_eq!(duties.ran(), [2, 2, 0]);
        assert_eq!(duties.upkept, 2);
        let quiet = keep.turn(Woke::Bell, false, at(base, 720), &mut duties);
        assert_eq!(quiet, Turned::default());

        let bound = keep.turn(Woke::Wall, false, at(base, 86_400), &mut duties);
        assert_eq!(
            bound,
            Turned {
                upkept: true,
                told: true
            },
            "the bound's second"
        );
    });
}

/// An upkeep not kept (it wrote) runs again at its rerun with nothing moved, and a moved input
/// brings it due no sooner than [`RERUN_FIRST`] after its run; five runs not kept in a row owe
/// 10, 20, 40, 60 and 60 s (`aterm_uds::trace::backoff`); once a run is kept, nothing more. The
/// told part, by its side, runs at once when what it read moved.
///
/// NEGATIVE CONTROL (2026-11-16): `Keep::turn` judging the upkeep with a floor of
/// `Duration::ZERO` failed at "a moved input within the first rerun", left `Turned { upkept:
/// true, told: true }`, right `Turned { upkept: false, told: true }`. Restored, the file touched.
#[test]
fn an_upkeep_not_kept_runs_at_its_rerun_and_a_moved_input_no_sooner_than_the_first() {
    let (_dir, path) = roster();
    let base = Instant::now();
    let mut duties = Scripted::new(&path);
    duties.writes = true;
    let mut keep = Keep::default();
    trace::read_as_if_later(Duration::from_secs(60), || {
        keep.turn(Woke::Start, false, at(base, 0), &mut duties);
        assert_eq!(duties.ran(), [1, 1, 0]);
        assert_eq!(
            keep.next(at(base, 0), true).mono,
            Some(base + RERUN_FIRST),
            "the rerun the write owes"
        );
        assert_eq!(
            keep.turn(Woke::Bell, false, at(base, 5), &mut duties),
            Turned::default()
        );

        std::fs::write(&path, "[[account]]\nlabel = \"b\", moved\n").expect("a moved roster");
        assert_eq!(
            keep.turn(Woke::File, false, at(base, 6), &mut duties),
            Turned {
                upkept: false,
                told: true
            },
            "a moved input within the first rerun"
        );
        assert_eq!(duties.ran(), [1, 2, 0]);

        assert_eq!(
            keep.turn(Woke::Deadline, false, at(base, 10), &mut duties),
            Turned {
                upkept: true,
                told: true
            },
            "the rerun owed"
        );
        assert_eq!(
            keep.next(at(base, 10), true).mono,
            Some(base + Duration::from_secs(30)),
            "the second run not kept in a row owes the first backoff's double"
        );
        for (run, owed) in [(30, 40), (70, 60), (130, 60)] {
            assert_eq!(
                keep.turn(Woke::Deadline, false, at(base, run), &mut duties),
                Turned {
                    upkept: true,
                    told: true
                },
                "the rerun owed at {run}s"
            );
            assert_eq!(
                keep.next(at(base, run), true).mono,
                Some(base + Duration::from_secs(run + owed)),
                "the run not kept at {run}s owes {owed}s: the backoff doubles to its cap"
            );
        }
        duties.writes = false;
        assert_eq!(
            keep.turn(Woke::Deadline, false, at(base, 190), &mut duties),
            Turned {
                upkept: true,
                told: true
            }
        );
        assert_eq!(
            keep.turn(Woke::Deadline, false, at(base, 250), &mut duties),
            Turned::default(),
            "a kept run owes nothing"
        );
        assert_eq!(duties.ran(), [6, 7, 0]);
    });
}

/// A forced turn makes both parts and tells each standing word again; an asked re-tell makes the
/// told part alone and tells again; an inert upkeep's turns make the told part alone.
///
/// NEGATIVE CONTROL (2026-11-16): `Keep::turn` passing its `again` through unchanged, so a forced
/// turn told nothing again, failed at "forced", left `[2, 2, 0]`, right `[2, 2, 1]`. Restored,
/// the file touched.
#[test]
fn a_forced_turn_makes_both_parts_and_an_asked_one_tells_again() {
    let (_dir, path) = roster();
    let base = Instant::now();
    let mut duties = Scripted::new(&path);
    let mut keep = Keep::default();
    trace::read_as_if_later(Duration::from_secs(60), || {
        keep.turn(Woke::Start, false, at(base, 0), &mut duties);
        keep.turn(Woke::Forced, false, at(base, 1), &mut duties);
        assert_eq!(duties.ran(), [2, 2, 1], "forced");
        keep.turn(Woke::Asked, true, at(base, 2), &mut duties);
        assert_eq!(duties.ran(), [2, 3, 2], "asked");

        let mut inert = Scripted::new(&path);
        inert.upkeeps = false;
        let mut keep = Keep::default();
        keep.turn(Woke::Start, false, at(base, 0), &mut inert);
        keep.turn(Woke::Forced, false, at(base, 1), &mut inert);
        assert_eq!(inert.ran(), [0, 2, 1]);
        assert_eq!(inert.upkept, 0);
        assert_eq!(
            keep.next(at(base, 1), false).wall_ns,
            None,
            "the told part compared no time"
        );
    });
}

/// The door answers what was asked before any deadline, a forced turn first, and a deadline
/// that has come at once; the stop ends every wait.
///
/// NEGATIVE CONTROL (2026-11-16): `Door::wait` taking the file flag before the forced one failed
/// at "a forced turn first", left `Some((File, false))`, right `Some((Forced, false))`. Restored,
/// the file touched.
#[test]
fn the_door_answers_what_was_asked_first_and_a_deadline_come_at_once() {
    let door = Door::default();
    let none = Next::default();
    door.ask(|asks| {
        asks.file = true;
        asks.bell = true;
        asks.forced = true;
    });
    assert_eq!(
        door.wait(none, true, true, None),
        Some((Woke::Forced, false)),
        "a forced turn first"
    );
    door.ask(|asks| {
        asks.bell = true;
        asks.again_at.insert(1);
        asks.again_at.insert(u64::MAX);
    });
    assert_eq!(door.wait(none, true, true, None), Some((Woke::Asked, true)));
    assert_eq!(door.wait(none, true, true, None), Some((Woke::Bell, false)));
    door.ask(|asks| asks.wall = true);
    assert_eq!(
        door.wait(none, true, true, None),
        Some((Woke::Wall, false)),
        "a move of the wall clock heard"
    );
    let come = Next {
        mono: Some(Instant::now()),
        wall_ns: None,
    };
    assert_eq!(
        door.wait(come, true, true, None),
        Some((Woke::Deadline, false))
    );
    door.ask(|asks| asks.stop = true);
    assert_eq!(door.wait(come, true, true, None), None);
}

/// A WALL-CLOCK DEADLINE IS WAITED FOR WHOLE WHERE A STEP OF THE CLOCK IS HEARD, and at most
/// [`LOOK_EVERY`] at a time where none is: a kept trace a day from its bound makes no turn a
/// minute while the wall's moves are heard. A monotonic deadline is waited for whole either way;
/// with no file watch, no wait is longer than [`LOOK_EVERY`].
///
/// NEGATIVE CONTROL (2026-11-16), the one mutation of its mutant build that reached this test:
/// `span` capping a wall-clock deadline whatever is heard (its `&& !heard` dropped) failed
/// `heard: the day waited for whole`, left `Some((Some(60s), Some(Wall)))`, right
/// `Some((Some(86400s), None))`. Restored, the file touched.
#[test]
fn a_wall_deadline_is_waited_for_whole_where_a_step_is_heard_and_a_minute_at_a_time_where_not() {
    let base = Instant::now();
    let now = at(base, 0);
    let day = Duration::from_secs(86_400);
    let wall = Next {
        mono: None,
        wall_ns: Some(i128::from(T + 86_400) * SEC_NS),
    };
    assert_eq!(
        span(now, wall, None, true, true, None),
        Some((Some(day), None)),
        "heard: the day waited for whole"
    );
    assert_eq!(
        span(now, wall, None, true, false, None),
        Some((Some(LOOK_EVERY), Some(Woke::Wall))),
        "unheard: a minute at a time"
    );
    assert_eq!(
        span(now, wall, None, false, true, None),
        Some((Some(LOOK_EVERY), Some(Woke::Unwatched))),
        "no file watch: a minute at a time"
    );
    let mono = Next {
        mono: Some(base + day),
        wall_ns: None,
    };
    assert_eq!(
        span(now, mono, None, true, false, None),
        Some((Some(day), None))
    );
    let asked = Some((i128::from(T) + 5) * SEC_NS);
    assert_eq!(
        span(now, wall, asked, true, true, None),
        Some((Some(Duration::from_secs(5)), None)),
        "an asked re-tell sooner"
    );
    assert_eq!(
        span(now, Next::default(), None, true, true, None),
        Some((None, None))
    );
    let come = Next {
        mono: Some(base),
        wall_ns: None,
    };
    assert_eq!(
        span(now, come, None, true, true, None),
        None,
        "a deadline come"
    );
}

/// A WALL-CLOCK DEADLINE IS WAITED FOR WHOLE ONLY WHERE THE WATCH HEARS A SLEEP AS WELL AS A STEP
/// OF THE CLOCK: a watch whose power half was refused (`WallWatch::steps_only`) hears a step
/// alone on macOS, where a sleep the clock was not set across moves the wall while the monotonic
/// clock stands still, so the keeper waits such a watch's deadlines a minute at a time. A watch
/// that hears both, with no hearing running, is not heard either.
///
/// NEGATIVE CONTROL (2026-11-16, at the merge of main's `WallWatch::hears_sleep`):
/// `hears_wall` without its `&& watch.hears_sleep()` failed `steps alone on macOS: a minute at a
/// time`, left `true`, right `false`. Restored, the file touched.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_wall_deadline_is_waited_for_whole_only_where_the_watch_hears_a_sleep() {
    let both = WallWatch::by_hand().expect("a watch told by hand");
    assert_eq!(
        hears_wall(&both, true),
        aterm_uds::wall_watch::STEPS_ANNOUNCED,
        "a watch that hears a step and a sleep"
    );
    assert!(!hears_wall(&both, false), "no hearing running");
    let steps = WallWatch::steps_only().expect("macOS and Linux announce a step");
    assert_eq!(
        hears_wall(&steps, true),
        cfg!(target_os = "linux"),
        "steps alone on macOS: a minute at a time"
    );
}

/// A MOVE OF THE WALL CLOCK THE SYSTEM ANNOUNCES IS HEARD ON THE HEARING'S THREAD AND TELLS THE
/// KEEPER'S DOOR, which wakes its turn as [`Woke::Wall`]; the hearing's thread ends as it drops.
///
/// NEGATIVE CONTROL (2026-11-16), the one mutation of its mutant build that reached this test:
/// the hearing's thread telling the door nothing at a move (its `door.ask(|asks| asks.wall =
/// true)` dropped) failed `the door wakes as the wall moved` at its one-minute hang detector,
/// left `Some((Deadline, false))`, right `Some((Wall, false))`. Restored, the file touched.
#[cfg(unix)]
#[test]
fn a_move_of_the_wall_clock_announced_wakes_the_keepers_door() {
    let door = Arc::new(Door::default());
    let watch = Arc::new(WallWatch::by_hand().expect("a watch told by hand"));
    let hearing = WallHearing::start("aterm-test-keeper-wall", Arc::clone(&watch), &door)
        .expect("the hearing starts");
    watch.tell(aterm_uds::wall_watch::Moved {
        stepped: true,
        ..Default::default()
    });
    let far = Next {
        mono: Some(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        wall_ns: None,
    };
    assert_eq!(
        door.wait(far, true, true, None),
        Some((Woke::Wall, false)),
        "the door wakes as the wall moved"
    );
    drop(hearing);
}

/// THE KEEPER'S STOP DOES WHAT ITS DUTIES DO AT A STOP AT ONCE, ON THE THREAD THAT STOPS IT, ONCE,
/// while the keeper's thread is still in the turn in hand (a held pass), which the process may
/// never see end.
///
/// NEGATIVE CONTROL (2026-11-16), the one mutation of its mutant build that reached this test:
/// `Keeper::stop` dropping what the duties do at a stop unrun (its `on_stop()` call dropped)
/// failed `done at once, on the stopping thread, the pass still held`, left
/// `Err(Disconnected)`. Restored, the file touched.
#[test]
fn the_keepers_stop_does_its_duties_stop_at_once_on_its_own_thread_while_a_pass_is_held() {
    struct Held {
        began: std::sync::mpsc::Sender<()>,
        free: std::sync::mpsc::Receiver<()>,
        stopped: Option<std::sync::mpsc::Sender<std::thread::ThreadId>>,
    }
    impl Duties for Held {
        fn upkeeps(&self) -> bool {
            true
        }
        fn upkeep(&mut self, _now: Now) {
            let _ = self.began.send(());
            let _ = self.free.recv();
        }
        fn upkept(&mut self, _now: Now) {}
        fn told(&mut self, _now: Now, _again: bool) {}
        fn on_stop(&mut self) -> Option<Box<dyn FnOnce() + Send>> {
            let stopped = self.stopped.take()?;
            Some(Box::new(move || {
                let _ = stopped.send(std::thread::current().id());
            }))
        }
    }
    let (began_tx, began) = std::sync::mpsc::channel();
    let (free, freed) = std::sync::mpsc::channel();
    let (stopped_tx, stopped) = std::sync::mpsc::channel();
    let keeper = Keeper::spawn_for_test(
        "aterm-test-keeper-stop",
        Box::new(Held {
            began: began_tx,
            free: freed,
            stopped: Some(stopped_tx),
        }),
        None,
    )
    .expect("the keeper starts");
    began
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the first turn's pass, held");
    keeper.stop();
    assert_eq!(
        stopped.try_recv(),
        Ok(std::thread::current().id()),
        "done at once, on the stopping thread, the pass still held"
    );
    keeper.stop();
    assert!(stopped.try_recv().is_err(), "once");
    drop(free);
}

/// Each counted wake counts under its cause, and a turn that ran nothing counts as unmoved;
/// the first turn is no wake.
///
/// NEGATIVE CONTROL (2026-11-16): `count` taking a turn that told nothing as unmoved failed with
/// left `[1, 1, 1, 1, 1, 1, 1, 9]`, right `[1, 1, 1, 1, 1, 1, 1, 1]`. Restored, the file touched.
#[test]
fn each_wake_counts_under_its_cause_and_a_turn_that_ran_nothing_as_unmoved() {
    let list = |c: KeeperCounts| {
        [
            c.file,
            c.bell,
            c.deadline,
            c.wall,
            c.asked,
            c.forced,
            c.unwatched,
            c.unmoved,
        ]
    };
    let before = list(counts_here());
    let ran = Turned {
        upkept: true,
        told: false,
    };
    count_for_test(Woke::Start, ran);
    for woke in Woke::COUNTED {
        count_for_test(woke, ran);
    }
    count_for_test(Woke::Start, Turned::default());
    let after = list(counts_here());
    let moved: [u64; 8] = std::array::from_fn(|i| after[i] - before[i]);
    assert_eq!(moved, [1, 1, 1, 1, 1, 1, 1, 1]);
    assert!(
        list(counts())
            .iter()
            .zip(&after)
            .all(|(all, here)| all >= here)
    );
}

/// A state of the `AccountUpkeepVerdict` model.
type State = BTreeMap<&'static str, i64>;

/// The wait a kept run of [`part_at`] compares through `aterm_uds::trace::Since`.
const SPAN: Duration = Duration::from_secs(600);

/// The keeper's upkeep part as the `AccountUpkeepVerdict` model's state `state` leaves it, over
/// a scratch file in `dir` and the monotonic `base`, and the readings of a look at it. No run for
/// `held` 0. For `held` 1, a kept run that read the file and compared a wait of [`SPAN`] from
/// `base`, looked at short of the wait's end, or past it where `past` says. For `held` 2, a run
/// not kept: where `dirty` says, the second in a row that did something (its rerun owed 20 s
/// after it, its least wait [`RERUN_FIRST`]), looked at 5 s after it, 15 s after it where
/// `floor` says, or 25 s after it where `past` says; otherwise one that read a file that had not
/// settled, looked at short of its settling or past it where `past` says, 5 s or, where `floor`
/// says, 15 s after it. The file the run read is rewritten where `moved` says.
fn part_at(state: &State, dir: &Path, base: Instant) -> (Part, At) {
    let file = dir.join("roster.toml");
    std::fs::write(&file, "one").expect("the file");
    let read = || {
        trace::read_as_if_later(Duration::from_secs(60), || fs::read(&file))
            .expect("the file read");
    };
    let mut part = Part::default();
    let at = match state["held"] {
        0 => at(base, 0),
        1 => {
            let ((), ran) = trace::scope(|| {
                read();
                let _ = trace::Since::new(base).elapsed_at_least(SPAN, base);
            });
            assert!(ran.unkept().is_none(), "the run is kept");
            part.ran(ran, at(base, 0));
            at(base, if state["past"] == 1 { 700 } else { 30 })
        }
        _ if state["dirty"] == 1 => {
            let ((), ran) = trace::scope(|| {
                read();
                trace::effect(Effect::Write);
            });
            part.ran(ran.clone(), at(base, 0));
            part.ran(ran, at(base, 0));
            let secs = match (state["past"], state["floor"]) {
                (1, _) => 25,
                (_, 1) => 15,
                _ => 5,
            };
            at(base, secs)
        }
        _ => {
            let fresh = dir.join("fresh.toml");
            std::fs::write(&fresh, "new").expect("a fresh file");
            let changed = std::fs::metadata(&fresh)
                .and_then(|meta| meta.modified())
                .expect("its change time");
            let ((), ran) = trace::scope(|| {
                read();
                // Judged a second after the file's change: short of settled.
                trace::read_as_if_at(changed + Duration::from_secs(1), || fs::is_file(&fresh));
            });
            let Some(trace::Unkept::Unsettled { until_ns }) = ran.unkept() else {
                panic!(
                    "the run read a file that had not settled: {:?}",
                    ran.unkept()
                );
            };
            part.ran(ran, at(base, 0));
            At {
                wall_ns: if state["past"] == 1 {
                    until_ns + SEC_NS
                } else {
                    until_ns - SEC_NS
                },
                mono: base + Duration::from_secs(if state["floor"] == 1 { 15 } else { 5 }),
            }
        }
    };
    if state["moved"] == 1 {
        std::fs::write(&file, "moved").expect("the file rewritten");
    }
    (part, at)
}

/// The keeper's look at the model's state `state` ([`part_at`]), as the model names it: `due`
/// is what [`Part::due`] answers there, with [`RERUN_FIRST`] its floor as the upkeep's; `cause`
/// is whether one stood, which the realized part was made to stand for: no run, or an input of
/// the last run moved, or the deadline it owes (a kept run's wait, a run not kept's rerun) came.
fn look_transition(state: &State, due: bool) -> State {
    let mut next = state.clone();
    let cause = state["held"] == 0 || state["moved"] == 1 || state["past"] == 1;
    next.insert("due", i64::from(due));
    next.insert("cause", i64::from(cause));
    next
}

/// THE KEEPER'S LOOK REFINES THE UPKEEP'S VERDICT (`AccountUpkeepVerdict`'s `Look`, L0 of
/// accounts upkeep on a cause). At every state the model reaches, the upkeep part made as it
/// says ([`part_at`]) is due exactly where the model's look finds it due: never without a cause
/// (`DueOnlyForCause`), and for a run not kept whose input moved only once its least wait
/// passed. Both answers are given somewhere.
///
/// NEGATIVE CONTROL (2026-11-16): `Part::due` dropping the least wait of a run not kept (its last
/// line as `owed || ran.trace.moved(now, at.mono).is_err()`): the look at a run not kept whose
/// input moved before its least wait passed answered due, which no `Look` of the model reaches,
/// and the test failed at that projection; restored, the file touched.
#[test]
fn the_keepers_look_refines_the_upkeep_verdict_at_every_reachable_state() {
    let model = aterm_models_harness::account_upkeep_verdict_model();
    let base = Instant::now();
    let mut seen = BTreeSet::new();
    let mut looked = BTreeMap::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        // The look reads none of the memo's variables: one look per keeper state.
        let key = ["held", "dirty", "unsettled", "moved", "past", "floor"].map(|var| state[var]);
        let due = *looked.entry(key).or_insert_with(|| {
            let dir = aterm_tempfile::tempdir().expect("scratch");
            let (part, at) = part_at(&state, dir.path(), base);
            part.due(at, RERUN_FIRST)
        });
        let next = look_transition(&state, due);
        assert!(
            model.successors("Look", &state).contains(&next),
            "{state:?}: the keeper's look left {next:?}, which the model does not reach"
        );
        seen.insert(due);
    }
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        [false, true],
        "both answers are given"
    );
}

/// The keeper's file watch watches at most its cap of the paths the traces name, those it watches
/// already first and then the rest in order, and says how many it leaves unwatched; the cap is a
/// thirty-second of the soft descriptor limit, 8 to 128.
///
/// NEGATIVE CONTROL (2026-11-16): `Watching::set` watching every path whatever its cap failed at
/// the first set past the cap, left `0`, right `2` ("two past the cap"). Restored with cp then
/// touch, cmp-identical.
#[test]
fn the_keepers_watch_holds_at_most_its_cap_of_paths_and_counts_the_rest() {
    assert_eq!(watch_cap(None), 8);
    assert_eq!(watch_cap(Some(64)), 8);
    assert_eq!(watch_cap(Some(256)), 8);
    assert_eq!(watch_cap(Some(1_024)), 32);
    assert_eq!(watch_cap(Some(4_096)), 128);
    assert_eq!(watch_cap(Some(1 << 20)), 128);

    let dir = aterm_tempfile::tempdir().expect("scratch");
    let paths = |ns: &[u32]| {
        ns.iter()
            .map(|n| (dir.path().join(n.to_string()), n % 2 == 0))
            .collect::<BTreeSet<_>>()
    };
    let watched = |watching: &Watching| watching.keys.keys().cloned().collect::<BTreeSet<_>>();
    let watch = FileWatch::spawn(
        "keeper-cap-test",
        Box::new(aterm_uds::file_watch::InertEvents::default()),
        Box::new(|_| {}),
    )
    .expect("an inert watch");
    let mut watching = Watching::default();

    assert_eq!(watching.set(&watch, paths(&[5, 6, 7]), 3), 0);
    assert_eq!(watched(&watching), paths(&[5, 6, 7]));
    assert_eq!(
        watching.set(&watch, paths(&[0, 1, 5, 6, 7]), 3),
        2,
        "two past the cap"
    );
    assert_eq!(
        watched(&watching),
        paths(&[5, 6, 7]),
        "the paths watched already keep their places"
    );
    assert_eq!(
        watching.set(&watch, paths(&[0, 1, 5, 6, 7]), 2),
        3,
        "a lower cap"
    );
    assert_eq!(watched(&watching), paths(&[5, 6]));
    assert_eq!(watching.set(&watch, paths(&[0, 1, 6]), 3), 0);
    assert_eq!(watched(&watching), paths(&[0, 1, 6]));
}
