// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The accounts' upkeep's verdicts over scripted duties: what each pass runs, skips, keeps and
//! counts on the keeper's thread and off it. Every pass compares through scripted readings of
//! the wall and monotonic clocks, and every file read is judged settled as of a minute past the
//! clock ([`settled`]), so no test waits.

use super::*;
use crate::harness::accounts_upkeep_counts::{UpkeepCounts, counts_here};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The wall reading of the first pass: passes come a minute apart after it.
const NOW: i64 = 1_792_660_000;

/// The wall reading of the `nth` pass after the first, a minute apart.
fn at(nth: i64) -> Now {
    Now::at(NOW + 60 * nth)
}

/// The monotonic reading of the first pass, taken once: a test whose duties compare a
/// monotonic deadline reads later passes as offsets from it, and every other test reads it at
/// each pass.
fn mono() -> Instant {
    static FIRST: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    *FIRST
}

/// Run `f` with each read this thread records judged settled: as of a minute past the clock,
/// so a file the test just wrote has stood longer than `aterm_uds::trace::SETTLED`.
fn settled<R>(f: impl FnOnce() -> R) -> R {
    trace::read_as_if_later(Duration::from_secs(60), f)
}

/// What this thread's duty counts moved by over `f`, as `[runs, skips, kept, poisoned]`.
fn duties_moved(f: impl FnOnce()) -> [u64; 4] {
    let list = |c: UpkeepCounts| [c.duty_runs, c.duty_skips, c.duty_kept, c.duty_poisoned];
    let before = list(counts_here());
    f();
    let after = list(counts_here());
    std::array::from_fn(|i| after[i] - before[i])
}

/// A scratch folder with one file a clean duty reads, and how often each scripted duty ran.
struct Bed {
    _dir: aterm_tempfile::TempDir,
    file: PathBuf,
    poisoned: Cell<u32>,
    clean: Cell<u32>,
}

impl Bed {
    fn new(text: &str) -> Self {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let file = dir.path().join("roster.toml");
        std::fs::write(&file, text).expect("the file");
        Self {
            _dir: dir,
            file,
            poisoned: Cell::new(0),
            clean: Cell::new(0),
        }
    }

    /// How often each duty ran: `(poisoned, clean)`.
    fn ran(&self) -> (u32, u32) {
        (self.poisoned.get(), self.clean.get())
    }

    /// One pass at `now` of two duties: one that takes its clock reading raw, as every duty of
    /// the window's upkeep does before its clock is migrated, and one that reads the file
    /// through the traced reader and compares nothing. The clean duty's answer is the file's
    /// text.
    fn pass(&self, now: Now) -> String {
        pass(
            || "bed",
            now,
            mono(),
            || {
                run(Duty::Heal, (), || {
                    self.poisoned.set(self.poisoned.get() + 1);
                    let _ = now.raw();
                });
                self.read(Duty::SheetLog)
            },
        )
    }

    /// `duty`, made through its verdict: it reads the file ([`Self::read_text`]).
    fn read(&self, duty: Duty) -> String {
        run(duty, (), || self.read_text())
    }

    /// The file's text, read through the traced reader, counted as a run of the clean duty.
    fn read_text(&self) -> String {
        self.clean.set(self.clean.get() + 1);
        String::from_utf8(trace::fs::read(&self.file).expect("the file read")).expect("text")
    }

    fn path(&self) -> &Path {
        &self.file
    }
}

/// A POISONED DUTY RUNS AT EVERY PASS, AND A CLEAN ONE IS SKIPPED UNTIL A READ MOVES. On the
/// keeper's thread, over passes a minute apart of two duties (one that takes its clock reading
/// raw, one that only reads a file through the traced reader):
/// - the first pass runs both; the clean one's run is kept, the poisoned one's is not;
/// - the next three run the poisoned duty each time and skip the clean one, which answers the
///   text its run read;
/// - after the file is rewritten, the next pass runs the clean duty again and answers the new
///   text; the pass after it skips it again.
///
/// By `[runs, skips, kept, poisoned]` the six passes count `[8, 4, 2, 6]`. CONTROL: off the
/// keeper's thread, two passes run both duties each time and count nothing.
///
/// NEGATIVE CONTROLS (2026-11-15), each the one mutation of its mutant build that reached this
/// test:
/// - `run_replaying` keeping a poisoned run as its duty's verdict (its `None` arm as
///   `None | Some(Unkept::Poison(_))`): `[runs, skips, kept, poisoned]` failed, left
///   `[8, 4, 8, 0]`, right `[8, 4, 2, 6]` (the kept poisoned verdicts never stood, so the duty
///   still ran at each pass);
/// - `standing` answering a kept verdict without replaying its trace (its
///   `kept.trace.stands(..)` line removed): `a moved read runs the clean duty again` failed,
///   left `"one"`, right `"three"`.
///
/// Each restored, the file touched.
#[test]
fn a_poisoned_duty_runs_at_every_pass_and_a_clean_one_is_skipped_until_a_read_moves() {
    let bed = Bed::new("one");
    let moved = duties_moved(|| {
        trace::keeper(|| {
            settled(|| {
                assert_eq!(bed.pass(at(0)), "one");
                assert_eq!(bed.ran(), (1, 1), "the first pass runs both");
                for nth in 1..=3 {
                    assert_eq!(bed.pass(at(nth)), "one", "the kept answer");
                }
                assert_eq!(bed.ran(), (4, 1), "the next three passes");
                std::fs::write(bed.path(), "three").expect("the file rewritten");
                assert_eq!(
                    bed.pass(at(4)),
                    "three",
                    "a moved read runs the clean duty again"
                );
                assert_eq!(bed.ran(), (5, 2));
                assert_eq!(bed.pass(at(5)), "three");
                assert_eq!(bed.ran(), (6, 2), "the new read is kept and skipped");
            });
        });
    });
    assert_eq!(moved, [8, 4, 2, 6], "[runs, skips, kept, poisoned]");

    // CONTROL: off the keeper's thread every pass makes every duty, and nothing is counted.
    let off = Bed::new("one");
    let moved = duties_moved(|| {
        settled(|| {
            for nth in 0..2 {
                assert_eq!(off.pass(at(nth)), "one");
            }
        });
    });
    assert_eq!(
        off.ran(),
        (2, 2),
        "CONTROL: off the keeper, both run at each pass"
    );
    assert_eq!(
        moved,
        [0, 0, 0, 0],
        "CONTROL: off the keeper, nothing is counted"
    );
}

/// A DUTY GIVEN OTHER INPUTS, OR MADE IN A PASS OF ANOTHER CONTEXT, RUNS AGAIN. On the keeper's
/// thread, a clean duty given `false` is kept and skipped while it is given `false`; given
/// `true`, it runs, and its run is kept for `true` alone. A pass of another context (another
/// home) runs it again though nothing it read moved, and drops every verdict kept before.
///
/// NEGATIVE CONTROLS (2026-11-15), each the one mutation of its mutant build that reached this
/// test:
/// - `standing` answering a verdict kept for other inputs (its `.filter(|kept| *kept ==
///   given)` removed): `assertion failed: sync("home", true, at(2))`, the answer kept for
///   `false`;
/// - `pass` keeping the verdicts of another context (its `if !same { .. }` as `if false`):
///   `another context runs it again` failed, left `2`, right `3`.
///
/// Each restored, the file touched.
#[test]
fn a_duty_given_other_inputs_or_made_in_another_context_runs_again() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("pool.toml");
    std::fs::write(&file, "kimi").expect("the file");
    let ran = Cell::new(0);
    let sync = |context: &'static str, given: bool, now: Now| {
        pass(
            || context,
            now,
            mono(),
            || {
                run(Duty::PoolSync(Cli::Kimi), given, || {
                    ran.set(ran.get() + 1);
                    trace::fs::is_file(&file) && given
                })
            },
        )
    };
    trace::keeper(|| {
        settled(|| {
            assert!(!sync("home", false, at(0)));
            assert!(!sync("home", false, at(1)));
            assert_eq!(ran.get(), 1, "the same inputs skip it");
            assert!(sync("home", true, at(2)));
            assert_eq!(ran.get(), 2, "other inputs run it again");
            assert!(sync("home", true, at(3)));
            assert_eq!(ran.get(), 2, "kept for the new inputs");
            assert!(sync("another home", true, at(4)));
            assert_eq!(ran.get(), 3, "another context runs it again");
            assert!(sync("home", true, at(5)));
            assert_eq!(
                ran.get(),
                4,
                "back in the first context, its verdicts were dropped"
            );
        });
    });
}

/// A KEPT DUTY RUNS AGAIN ONCE THE WALL CROSSES A BOUND IT COMPARED, AND A SKIP RECORDS WHAT
/// THE RUN READ. On the keeper's thread, a duty that reads a file and asks whether a bound ten
/// minutes after the first pass has come is kept and skipped while the pass's reading stays
/// short of it, and runs again at the pass whose reading reaches it; that run, which compared
/// the same bound the other way, is kept and skipped after. A scope armed around a skipped pass
/// records the file the duty's kept run read, and the wall interval it compared over, as if
/// the pass had read and compared them again.
///
/// NEGATIVE CONTROL (2026-11-15): `standing` answering a kept verdict without replaying its
/// trace (its `kept.trace.stands(..)` line removed), the one mutation of its mutant build that
/// reached this test: `the bound crossed runs it again` failed, the answer kept short of the
/// bound; restored, the file touched.
#[test]
fn a_kept_duty_runs_again_once_the_wall_crosses_a_bound_it_compared() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("logins.json");
    std::fs::write(&file, "{}").expect("the file");
    let due = NOW + 600;
    let ran = Cell::new(0);
    let renew = |now: Now| {
        pass(
            || "home",
            now,
            mono(),
            || {
                run(Duty::ClaudeRenew, (), || {
                    ran.set(ran.get() + 1);
                    trace::fs::is_file(&file) && now.passed(due)
                })
            },
        )
    };
    trace::keeper(|| {
        settled(|| {
            assert!(!renew(at(0)));
            let (due_yet, skipped) = trace::scope(|| renew(at(9)));
            assert!(!due_yet);
            assert_eq!(ran.get(), 1, "short of the bound it is skipped");
            assert_eq!(
                skipped.files().map(|(path, _)| path).collect::<Vec<_>>(),
                [file.as_path()],
                "a skip records the file the kept run read"
            );
            assert_eq!(
                skipped.wall().hi,
                due - 1,
                "and the interval it compared over"
            );
            assert!(renew(at(10)), "the bound crossed runs it again");
            assert_eq!(ran.get(), 2);
            assert!(renew(at(11)));
            assert_eq!(ran.get(), 2, "past the bound it is kept again");
        });
    });
}

/// A KEPT DUTY RUNS AGAIN AT THE PASS WHOSE MONOTONIC READING REACHES A WAIT IT COMPARED. On the
/// keeper's thread, over passes whose monotonic readings come a scripted minute apart, a duty
/// that asks through `aterm_uds::trace::Since` whether a ten-minute wait from the first reading
/// is over is kept and skipped while the pass's reading stays short of the wait's end, and runs
/// again at the pass whose reading reaches it; that run, which found the wait over and so
/// compared nothing still to come, is kept and skipped after. The verdicts are judged at the
/// pass's own reading, not at the clock's, so the test never waits.
///
/// NEGATIVE CONTROL (2026-11-15): `standing` judging a kept verdict at `Instant::now()` in place
/// of the pass's reading, the one mutation of its mutant build that reached this test: `the
/// wait's end reached runs it again` failed, the answer kept short of the wait's end; restored,
/// the file touched.
#[test]
fn a_kept_duty_runs_again_at_the_pass_whose_monotonic_reading_reaches_a_wait_it_compared() {
    let first = mono();
    let wait = Duration::from_secs(600);
    let ran = Cell::new(0);
    let renew = |nth: u32| {
        let reading = first + Duration::from_secs(60) * nth;
        pass(
            || "home",
            at(i64::from(nth)),
            reading,
            || {
                run(Duty::ClaudeRenew, (), || {
                    ran.set(ran.get() + 1);
                    trace::Since::new(first).elapsed_at_least(wait, reading)
                })
            },
        )
    };
    trace::keeper(|| {
        assert!(!renew(0));
        assert!(!renew(9));
        assert_eq!(ran.get(), 1, "short of the wait's end it is skipped");
        assert!(renew(10), "the wait's end reached runs it again");
        assert_eq!(ran.get(), 2);
        assert!(renew(11));
        assert_eq!(ran.get(), 2, "past the wait it is kept again");
    });
}

/// A RUN THAT DID SOMETHING, DID NOT FINISH, OR READ A FILE THAT HAD NOT SETTLED IS NOT KEPT. On
/// the keeper's thread, a duty that records a write, one that records a retry and one whose
/// read is judged within `aterm_uds::trace::SETTLED` of its file's change each run at every
/// pass; none of them counts as poisoned, and nothing is kept.
///
/// NEGATIVE CONTROL (2026-11-15): `run_replaying` keeping a run not kept for an effect, a retry
/// or an unsettled read (its `None` arm as `None | Some(Unkept::Effect(_) | Unkept::Retry(_) |
/// Unkept::Unsettled { .. })`), the one mutation of its mutant build that reached this test:
/// `each runs at every pass` failed, left `[1, 1, 1]`, right `[3, 3, 3]`; restored, the file
/// touched.
#[test]
fn a_run_that_did_something_or_did_not_finish_or_read_an_unsettled_file_is_not_kept() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("state.json");
    std::fs::write(&file, "{}").expect("the file");
    let changed = std::fs::metadata(&file)
        .and_then(|meta| meta.modified())
        .expect("its change time");
    let ran = [Cell::new(0), Cell::new(0), Cell::new(0)];
    let one_pass = |now: Now| {
        pass(
            || "home",
            now,
            mono(),
            || {
                run(Duty::Prepare, (), || {
                    ran[0].set(ran[0].get() + 1);
                    trace::effect(trace::Effect::Write);
                });
                run(Duty::MailboxNeeds, (), || {
                    ran[1].set(ran[1].get() + 1);
                    trace::retry("the lock was busy");
                });
                // Judged a second after the file's change: short of settled.
                trace::read_as_if_at(changed + Duration::from_secs(1), || {
                    run(Duty::Observe(Cli::Claude), (), || {
                        ran[2].set(ran[2].get() + 1);
                        trace::fs::is_file(&file)
                    })
                });
            },
        )
    };
    let moved = duties_moved(|| {
        trace::keeper(|| {
            for nth in 0..3 {
                one_pass(at(nth));
            }
        });
    });
    assert_eq!(
        ran.each_ref().map(Cell::get),
        [3, 3, 3],
        "each runs at every pass"
    );
    assert_eq!(moved, [9, 0, 0, 0], "[runs, skips, kept, poisoned]");
}

/// A SKIPPED DUTY REPLAYS WHAT ITS RUN NOTED. On the keeper's thread, a clean duty made through
/// [`run_replaying`] answers what its run noted; at each pass that skips it the replay is called
/// with that answer, and at a pass that runs it, it is not.
///
/// NEGATIVE CONTROL (2026-11-15): `run_replaying` not calling `replayed` on a skip, the one
/// mutation of its mutant build that reached this test: `each skip replays the kept answer`
/// failed, left `[]`, right `[true, true]`; restored, the file touched.
#[test]
fn a_skipped_duty_replays_what_its_run_noted() {
    let bed = Bed::new("one");
    let replayed = RefCell::new(Vec::new());
    let note = |now: Now| {
        pass(
            || "home",
            now,
            mono(),
            || {
                run_replaying(
                    Duty::TokensHere,
                    (),
                    || bed.read_text() == "one",
                    |&confirmed| replayed.borrow_mut().push(confirmed),
                )
            },
        )
    };
    trace::keeper(|| {
        settled(|| {
            assert!(note(at(0)));
            assert_eq!(*replayed.borrow(), [], "a run replays nothing");
            assert!(note(at(1)));
            assert!(note(at(2)));
        });
    });
    assert_eq!(bed.ran(), (0, 1), "one run, two skips");
    assert_eq!(
        *replayed.borrow(),
        [true, true],
        "each skip replays the kept answer"
    );
}

/// A state of the `AccountUpkeepVerdict` model.
type State = std::collections::BTreeMap<&'static str, i64>;

/// The wait a duty of [`verdict_transition`] compares through `aterm_uds::trace::Since`.
const SPAN: Duration = Duration::from_secs(600);

/// The memo's decision at the `AccountUpkeepVerdict` model's state `state`, made for real on the
/// keeper's thread within reads judged settled ([`settled`]), over the pass context `context`
/// (each state's its own, so no verdict of another state's is left): a first pass makes the duty
/// once, where `held` says it ran, doing something where `dirty` says and reading a file that has
/// not settled where `unsettled` says; then the file it read is rewritten where `moved` says;
/// and a second pass, whose monotonic reading is at the end of the wait the duty compared where
/// `past` says, makes it again, doing something where `soil` says and reading a file that has not
/// settled where `stir` says. Whether the second pass ran the work, and whether the memo keeps a
/// verdict for the duty after it.
fn verdict_made(state: &State, context: u64) -> (bool, bool) {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let file = dir.path().join("roster.toml");
    std::fs::write(&file, "one").expect("the file");
    let first = mono();
    let ran = Cell::new(false);
    let make = |now: Now, reading: Instant, effect: bool, unsettled: bool| {
        let fresh = unsettled.then(|| {
            let fresh = dir.path().join(format!("fresh-{}.toml", now.raw()));
            std::fs::write(&fresh, "new").expect("a fresh file");
            let changed = std::fs::metadata(&fresh)
                .and_then(|meta| meta.modified())
                .expect("its change time");
            (fresh, changed)
        });
        pass(
            || context,
            now,
            reading,
            || {
                run(Duty::Heal, (), || {
                    ran.set(true);
                    let _ = trace::fs::read(&file);
                    let _ = trace::Since::new(first).elapsed_at_least(SPAN, reading);
                    if effect {
                        trace::effect(trace::Effect::Write);
                    }
                    if let Some((fresh, changed)) = &fresh {
                        // Judged a second after the file's change: short of settled.
                        trace::read_as_if_at(*changed + Duration::from_secs(1), || {
                            trace::fs::is_file(fresh)
                        });
                    }
                });
            },
        );
    };
    if state["held"] > 0 {
        make(at(0), first, state["dirty"] == 1, state["unsettled"] == 1);
    }
    if state["moved"] == 1 {
        std::fs::write(&file, "moved").expect("the file rewritten");
    }
    ran.set(false);
    let reading = if state["past"] == 1 {
        first + SPAN
    } else {
        first + Duration::from_secs(60)
    };
    make(at(1), reading, state["soil"] == 1, state["stir"] == 1);
    let kept = MEMO.with_borrow(|memo| memo.kept.iter().any(|kept| kept.duty == Duty::Heal));
    (ran.get(), kept)
}

/// What the memo did at the model's state `state` ([`verdict_made`]: whether the work ran, and
/// whether a verdict is kept after), as the model names it: `Hit` where the work did not run,
/// which leaves the state as it was with the hit noted; `Run` where it ran, leaving the run held
/// as kept where a verdict is kept and not kept otherwise, with what it did and read as the work
/// was made to, and every input and clock of the last run new.
fn verdict_transition(state: &State, (ran, kept): (bool, bool)) -> (&'static str, State) {
    let mut next = state.clone();
    if !ran {
        assert!(kept, "{state:?}: an answered verdict stays kept");
        next.insert("hit", 1);
        return ("Hit", next);
    }
    next.insert("held", if kept { 1 } else { 2 });
    next.insert("dirty", state["soil"]);
    next.insert("unsettled", state["stir"]);
    for var in ["moved", "past", "floor", "soil", "stir", "hit"] {
        next.insert(var, 0);
    }
    ("Run", next)
}

/// THE MEMO REFINES THE UPKEEP'S VERDICT (`AccountUpkeepVerdict`, L0 of accounts upkeep on a
/// cause). At every state the model reaches, the memo's decision made for real
/// ([`verdict_made`], [`verdict_transition`]) is an action the model enables there, and leaves a state that action
/// can reach: a kept verdict answers only while nothing it read moved and the wait it compared
/// has not ended (`HitImpliesReplayEqual`, `HitWithinHorizon`), and a run is kept only where it
/// did nothing (`KeptIsClean`) and read only what had settled (`UnsettledNotKept`). Both of the
/// memo's actions are taken somewhere.
///
/// NEGATIVE CONTROL (2026-11-16): `run_replaying` keeping a run whose read had not settled (its
/// `None` arm as `None | Some(Unkept::Unsettled { .. })`): the state after the run read an
/// unsettled file is held as kept (`held` 1, `unsettled` 1), which no `Run` of the model reaches,
/// and the test failed at that projection; restored, the file touched.
#[test]
fn the_memo_refines_the_upkeep_verdict_at_every_reachable_state() {
    let model = aterm_models_harness::account_upkeep_verdict_model();
    let mut taken = std::collections::BTreeSet::new();
    let mut made = std::collections::BTreeMap::new();
    trace::keeper(|| {
        settled(|| {
            for state in aterm_spec::interp::reachable_states(&model) {
                // The memo reads none of the keeper's variables: one making per memo state.
                let key: Vec<i64> = [
                    "held",
                    "dirty",
                    "unsettled",
                    "moved",
                    "past",
                    "soil",
                    "stir",
                ]
                .map(|var| state[var])
                .to_vec();
                let context = u64::try_from(made.len()).expect("a count");
                let did = *made
                    .entry(key)
                    .or_insert_with(|| verdict_made(&state, context));
                let (action, next) = verdict_transition(&state, did);
                assert!(
                    model.action_enabled(action, &state),
                    "{state:?}: the memo took {action}, which the model does not enable there"
                );
                assert!(
                    model.successors(action, &state).contains(&next),
                    "{state:?}: the memo's {action} left {next:?}, which the model does not reach"
                );
                taken.insert(action);
            }
        });
    });
    assert_eq!(
        taken.into_iter().collect::<Vec<_>>(),
        ["Hit", "Run"],
        "both of the memo's actions are taken"
    );
}
