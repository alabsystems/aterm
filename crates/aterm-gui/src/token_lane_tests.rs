// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TOKEN LANE READS ONLY FOR ITS CAUSES: a [`Lane`] over a scripted view (two files whose
//! versions a test moves by hand, its reads counted with the second of each, the deadline each
//! read names scripted) and a scripted watch (what it was asked to watch, or a refusal), driven
//! turn by turn on a scripted wall clock as its thread drives it ([`Rig::idle_until`]: each turn
//! at the second [`Lane::next`] names). Nothing here sleeps, times or reads a file.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;

/// The scripted clock's start (2027-01-15).
const T0: i64 = 1_800_000_000;

/// The scripted read set: a table and the vault's state.
fn files() -> [PathBuf; 2] {
    [
        PathBuf::from("/scripted/accounts/tokens.json"),
        PathBuf::from("/scripted/secrets/state.toml"),
    ]
}

/// What the scripted view and watch share with the test.
#[derive(Default)]
struct Script {
    /// Each file's version, which a write moves.
    versions: BTreeMap<PathBuf, u32>,
    /// The second of each read.
    reads: Vec<i64>,
    /// The deadlines a read names: the first one after it.
    deadlines: Vec<i64>,
    /// What the watch was last asked to watch.
    placed: Vec<PathBuf>,
    /// Whether the watch refuses, as a kernel that gives no watcher does.
    refuse: bool,
}

#[derive(Clone, Default)]
struct Scripted(Rc<RefCell<Script>>);

impl View for Scripted {
    type Set = Vec<PathBuf>;
    type Stamp = Vec<u32>;

    fn set(&mut self, _now: i64) -> Vec<PathBuf> {
        self.0.borrow().versions.keys().cloned().collect()
    }

    fn paths(&self, set: &Vec<PathBuf>) -> Vec<PathBuf> {
        set.clone()
    }

    fn stamp(&mut self, set: &Vec<PathBuf>) -> Vec<u32> {
        let script = self.0.borrow();
        set.iter().map(|path| script.versions[path]).collect()
    }

    fn read(&mut self, now: i64) -> Option<i64> {
        let mut script = self.0.borrow_mut();
        script.reads.push(now);
        script.deadlines.iter().copied().find(|due| *due > now)
    }
}

impl Watch for Scripted {
    fn place(&mut self, paths: &[PathBuf]) -> bool {
        let mut script = self.0.borrow_mut();
        if script.refuse {
            return false;
        }
        script.placed = paths.to_vec();
        true
    }
}

/// A lane over the scripted view and watch, and the second its clock stands at.
struct Rig {
    lane: Lane<Scripted, Scripted>,
    script: Scripted,
    now: i64,
}

impl Rig {
    /// A lane before its first read, over the two files at version 0, whose reads name
    /// `deadlines`, its watch refusing where `refuse`.
    fn new(deadlines: &[i64], refuse: bool) -> Rig {
        let script = Scripted::default();
        {
            let mut held = script.0.borrow_mut();
            held.versions = files().into_iter().map(|path| (path, 0)).collect();
            held.deadlines = deadlines.to_vec();
            held.refuse = refuse;
        }
        Rig {
            lane: Lane::new(script.clone(), script.clone()),
            script,
            now: T0,
        }
    }

    /// The seconds of every read so far.
    fn reads(&self) -> Vec<i64> {
        self.script.0.borrow().reads.clone()
    }

    /// The lane's own turns until `until`, as its thread takes them: each at the second
    /// [`Lane::next`] names, every one of which reads.
    fn idle_until(&mut self, until: i64) {
        while let Some(at) = self.lane.next().filter(|at| *at <= until) {
            self.now = at.max(self.now);
            assert!(
                self.lane.turn(self.now),
                "the turn at {} it named reads",
                self.now
            );
        }
        self.now = until;
    }

    /// The lane hears `msg` at `at`, after its own turns until then.
    fn hear(&mut self, at: i64, msg: Msg) {
        self.idle_until(at);
        self.lane.hear(msg);
        self.lane.turn(at);
    }

    /// The file `index` of [`files`] written: its version moves.
    fn write(&self, index: usize) {
        *self
            .script
            .0
            .borrow_mut()
            .versions
            .get_mut(&files()[index])
            .expect("a scripted file") += 1;
    }
}

/// THE LANE READS ONLY FOR ITS CAUSES (the lane half of the footer cause design's C2 exit test,
/// whose footer half is `claude_footer::cause_tests::the_footer_never_reads_the_token_view`):
/// the first ask reads once, placing its watches before the read; 1800 more asks over an idle
/// hour, one every 2 s as a writing tab's footer makes them, read nothing; a file of its read set
/// written and told reads once, at once; and the second its read named reads once, at that
/// second, after which an idle hour reads nothing.
///
/// NEGATIVE CONTROL (run 2026-11-14): with [`Lane::turn`] ignoring the deadline, the test fails
/// at "the turn at 1800005000 it named reads" (`T0 + 5000`, the deadline).
#[test]
fn the_lane_reads_only_for_its_causes() {
    let mut rig = Rig::new(&[T0 + 5_000], false);
    rig.hear(T0, Msg::Want);
    assert_eq!(rig.reads(), [T0], "the first ask reads once");
    assert_eq!(
        rig.script.0.borrow().placed,
        files(),
        "its files are watched"
    );
    for at in (T0 + 2..=T0 + 3_600).step_by(2) {
        rig.hear(at, Msg::Want);
    }
    rig.idle_until(T0 + 3_600);
    assert_eq!(rig.reads(), [T0], "an idle hour of asks reads nothing");

    rig.write(0);
    rig.hear(T0 + 3_700, Msg::Told);
    assert_eq!(
        rig.reads(),
        [T0, T0 + 3_700],
        "a file written and told reads once"
    );

    rig.idle_until(T0 + 5_000);
    assert_eq!(
        rig.reads(),
        [T0, T0 + 3_700, T0 + 5_000],
        "the deadline its read named"
    );
    rig.idle_until(T0 + 5_000 + 3_600);
    assert_eq!(
        rig.reads().len(),
        3,
        "no deadline after it, an idle hour reads nothing"
    );
}

/// A TELL WITH NOTHING MOVED READS NOTHING (the cause design's L8 for the lane): the first look of
/// each new watch, a write the read already saw, and the vault's `state.toml` rewritten for when
/// it last heard this Mac (which its stamp leaves out) are tells whose stamp stands; none reads.
/// A tell of a file moved after the stamp reads.
///
/// NEGATIVE CONTROL (run 2026-11-14): with [`Lane::hear`] owing a read for every tell, the test
/// fails at "tells with nothing moved" (reads at `T0`, `T0 + 60` and `T0 + 600`).
#[test]
fn a_tell_with_nothing_moved_reads_nothing() {
    let mut rig = Rig::new(&[], false);
    rig.hear(T0, Msg::Want);
    for at in [T0 + 1, T0 + 2, T0 + 30, T0 + 600] {
        rig.hear(at, Msg::Told);
    }
    rig.idle_until(T0 + 3_600);
    assert_eq!(rig.reads(), [T0], "tells with nothing moved");
    rig.write(1);
    rig.hear(T0 + 3_601, Msg::Told);
    assert_eq!(
        rig.reads(),
        [T0, T0 + 3_601],
        "CONTROL: a moved file's tell reads"
    );
}

/// A BURST OF WRITES INSIDE THE FLOOR IS ONE READ AT ITS END (the cause design's L6 for the lane):
/// a write told 10 s after a read, and another 20 s after it, read once, at [`FLOOR_S`] after the
/// read that began the floor, and not before.
///
/// NEGATIVE CONTROL (run 2026-11-14): with [`Lane::turn`] reading every owed read at once, the
/// test fails at "owed at the floor's end" (`None`: the tells read at once, at `T0 + 10` and
/// `T0 + 20`), and the clocks arm's test at "a read a floor while asked" (a read every 2 s).
#[test]
fn a_burst_inside_the_floor_is_one_read_at_its_end() {
    let mut rig = Rig::new(&[], false);
    rig.hear(T0, Msg::Want);
    rig.write(0);
    rig.hear(T0 + 10, Msg::Told);
    rig.write(1);
    rig.hear(T0 + 20, Msg::Told);
    assert_eq!(
        rig.lane.next(),
        Some(T0 + FLOOR_S),
        "owed at the floor's end"
    );
    rig.idle_until(T0 + 3_600);
    assert_eq!(
        rig.reads(),
        [T0, T0 + FLOOR_S],
        "one read at the floor's end"
    );
}

/// A lane on its own thread's view and watch ([`a_wall_move_ends_the_wait_toward_a_deadline`]):
/// no file, every watch placed, and each read's second kept, its deadline `deadline` while that
/// is ahead.
#[cfg(unix)]
#[derive(Clone, Default)]
struct Threaded {
    reads: std::sync::Arc<std::sync::Mutex<Vec<i64>>>,
    deadline: i64,
}

#[cfg(unix)]
impl View for Threaded {
    type Set = ();
    type Stamp = ();

    fn set(&mut self, _now: i64) {}

    fn paths(&self, (): &()) -> Vec<PathBuf> {
        Vec::new()
    }

    fn stamp(&mut self, (): &()) {}

    fn read(&mut self, now: i64) -> Option<i64> {
        self.reads.lock().unwrap().push(now);
        Some(self.deadline).filter(|due| *due > now)
    }
}

#[cfg(unix)]
impl Watch for Threaded {
    fn place(&mut self, _paths: &[PathBuf]) -> bool {
        true
    }
}

/// A WALL MOVE ENDS THE WAIT TOWARD A DEADLINE (the lane's half of the footer cause design's J9: a
/// Mac that sleeps over a deadline): a lane started as its door starts it ([`start_lane`]), on
/// its own thread, with its own wall clock watch (one told by hand, as the power listener tells a
/// wake), reads at its first ask, at `T0`, and names a deadline two hours on; its loop reads the
/// wall a third time and waits toward that deadline, measured on the monotonic clock. Then the
/// Mac sleeps ten hours: the scripted wall reads `T0 + 36000` from its fourth reading on, and the
/// watch is told the wake. The lane reads at once, at the wall's second, though its wait had two
/// hours still to run. Where the wall's moves are not heard, a wait is held to [`WALL_LOOK_S`],
/// and where they are, it runs to the deadline. The lane's two threads are left parked on their
/// waits, as they are for the window's life.
///
/// NEGATIVE CONTROL (run 2026-11-16): with `start_lane` starting no wall listener (its
/// `listen_wall` call replaced by `Ok(())`, so the lane waits as one that hears the wall), the
/// test fails at "the woken lane reads at the wake" once its one-minute hang detector runs out:
/// the lane is still waiting out the two hours toward its deadline.
#[cfg(unix)]
#[test]
fn a_wall_move_ends_the_wait_toward_a_deadline() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const SLEPT: i64 = 36_000;
    assert_eq!(
        wait_toward(T0 + 7_200, T0, false),
        Duration::from_secs(WALL_LOOK_S),
        "unheard, a wait is held to a look"
    );
    assert_eq!(
        wait_toward(T0 + 7_200, T0, true),
        Duration::from_secs(7_200),
        "heard, a wait runs to its deadline"
    );
    assert_eq!(
        wait_toward(T0, T0 + 1, true),
        Duration::ZERO,
        "a past deadline"
    );

    let view = Threaded {
        deadline: T0 + 7_200,
        ..Threaded::default()
    };
    let wall = std::sync::Arc::new(WallWatch::by_hand().expect("a wall clock watch told by hand"));
    let looks = std::sync::Arc::new(AtomicUsize::new(0));
    let clock = {
        let looks = std::sync::Arc::clone(&looks);
        move || {
            if looks.fetch_add(1, Ordering::AcqRel) < 3 {
                T0
            } else {
                T0 + SLEPT
            }
        }
    };
    let (door, heard) = channel();
    assert!(
        start_lane(
            (Lane::new(view.clone(), view.clone()), clock),
            (door.clone(), heard),
            Some(std::sync::Arc::clone(&wall)),
        )
        .expect("the lane starts"),
        "a watch told each sleep: the waits run to their deadlines"
    );
    door.send(Msg::Want).expect("the lane hears");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the lane reads at its first ask and then waits toward the deadline it named",
        || (looks.load(Ordering::Acquire) >= 3).then_some(()),
    );
    assert_eq!(
        *view.reads.lock().unwrap(),
        [T0],
        "the first ask reads once"
    );
    wall.tell(aterm_uds::wall_watch::Moved {
        woke: true,
        slept: Some(Duration::from_secs(SLEPT.unsigned_abs())),
        ..aterm_uds::wall_watch::Moved::default()
    });
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the woken lane reads at the wake",
        || (view.reads.lock().unwrap().len() >= 2).then_some(()),
    );
    assert_eq!(
        *view.reads.lock().unwrap(),
        [T0, T0 + SLEPT],
        "the deadline slept past reads at the wall's second"
    );
}

/// A LANE WHOSE WATCH HEARS NO SLEEP HOLDS ITS WAITS TO A LOOK ([`start_lane`] by
/// [`WallWatch::hears_sleep`]): a lane started with no wall clock watch, or with one that hears the
/// system's step announcement alone (the watch [`WallWatch::start`] makes on macOS where its power
/// half was refused, which says nothing of a sleep the clock was not set across), holds its waits
/// to [`WALL_LOOK_S`]; one whose watch hears each sleep (told by hand, as the power listener tells
/// a wake) runs them to their deadlines. On Linux a resume is a step, so a step-only watch hears
/// each sleep. Each lane's threads are left parked on their waits, as the window leaves its own.
///
/// NEGATIVE CONTROL (run 2026-11-16): with `start_lane` taking a polled watch as hearing the wall
/// whatever it hears (its `hears_sleep` read as `true`, as the lane read every polled watch
/// before), the test fails at "a watch of steps alone".
#[cfg(unix)]
#[test]
fn a_lane_whose_watch_hears_no_sleep_holds_its_waits_to_a_look() {
    let started = |wall: Option<WallWatch>| {
        let view = Threaded::default();
        start_lane(
            (Lane::new(view.clone(), view), || T0),
            channel(),
            wall.map(std::sync::Arc::new),
        )
        .expect("the lane starts")
    };
    assert!(!started(None), "no watch: held to a look");
    assert!(
        started(Some(
            WallWatch::by_hand().expect("a wall clock watch told by hand")
        )),
        "a watch told each sleep runs its waits to their deadlines"
    );
    let steps = WallWatch::steps_only();
    assert!(
        steps.is_some() || !cfg!(any(target_os = "macos", target_os = "linux")),
        "macOS and Linux announce a step"
    );
    if let Some(steps) = steps {
        assert_eq!(
            started(Some(steps)),
            cfg!(target_os = "linux"),
            "a watch of steps alone runs its waits whole only where a resume is a step"
        );
    }
}

/// ON THE CLOCKS ARM A DEMAND READS ONCE A FLOOR (the lane where the kernel gives no watcher): asks
/// every 2 s for ten minutes read at the first ask and then once each [`FLOOR_S`], as the
/// footer's minute gate read the view before the lane; once the asks stop, the floor's owed read
/// is the last.
///
/// NEGATIVE CONTROL (run 2026-11-14): with [`Lane::hear`] taking a demand after the first read for
/// nothing on either arm, the test fails at "a read a floor while asked" (one read, at `T0`).
#[test]
fn on_the_clocks_arm_a_demand_reads_once_a_floor() {
    let mut rig = Rig::new(&[], true);
    for at in (T0..T0 + 600).step_by(2) {
        rig.hear(at, Msg::Want);
    }
    rig.idle_until(T0 + 3_600);
    assert_eq!(rig.lane.arm(), Arm::Clocks);
    assert_eq!(
        rig.reads(),
        (0..=10).map(|k| T0 + k * FLOOR_S).collect::<Vec<_>>(),
        "a read a floor while asked"
    );
}
