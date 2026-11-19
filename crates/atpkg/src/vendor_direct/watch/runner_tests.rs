// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::Mutex;
use std::sync::atomic::AtomicU64;

use super::super::tests::{HANG, install, layout};
use super::super::{AFTER_WAKE, CADENCE, FAILED_PASS_RETRY, Watcher, watcher};
use super::*;
use crate::flow::VendorGet;

const CLAUDE_HEAD: &str = "https://downloads.claude.ai/claude-code-releases/latest";

/// The two clocks a test moves by hand, in whole milliseconds. A timed wait no ring ends advances
/// both by its length rounded up (so a wait always reaches its deadline), and a sleep the next
/// wait takes moves the wall clock alone. With the timers withheld only a
/// real ring ends a wait (a one-minute hang detector), so a test can show a state waits for its
/// event rather than for a deadline.
#[derive(Clone, Default)]
struct Time {
    wall_ms: Arc<AtomicU64>,
    mono_ms: Arc<AtomicU64>,
    nap_ms: Arc<AtomicU64>,
    /// The last wait's length in ms; `u64::MAX` for a wait with no deadline.
    last_wait_ms: Arc<AtomicU64>,
    waits: Arc<AtomicU64>,
    /// Timed waits that ran to their deadline.
    timers: Arc<AtomicU64>,
    withheld: Arc<AtomicBool>,
    /// Waits in a row that passed no time, being zero or rung: a runner that spins fails at
    /// [`SPIN`] of them rather than hanging its test.
    still: Arc<AtomicU64>,
}

/// More waits in a row than any state needs to act without time passing.
const SPIN: u64 = 1000;

impl Time {
    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
            + Duration::from_secs(1_790_000_000)
            + Duration::from_millis(self.wall_ms.load(Ordering::SeqCst))
    }

    fn mono(&self) -> Duration {
        Duration::from_millis(self.mono_ms.load(Ordering::SeqCst))
    }

    fn advance(&self, by: Duration) {
        let ms = u64::try_from(by.as_nanos().div_ceil(1_000_000)).unwrap();
        self.wall_ms.fetch_add(ms, Ordering::SeqCst);
        self.mono_ms.fetch_add(ms, Ordering::SeqCst);
    }

    fn sleep_in_next_wait(&self, by: Duration) {
        let ms = u64::try_from(by.as_millis()).unwrap();
        self.nap_ms.store(ms, Ordering::SeqCst);
    }

    fn withhold_timers(&self) {
        self.withheld.store(true, Ordering::SeqCst);
    }

    fn release_timers(&self) {
        self.withheld.store(false, Ordering::SeqCst);
    }

    fn last_wait(&self) -> Option<Duration> {
        let ms = self.last_wait_ms.load(Ordering::SeqCst);
        (ms != u64::MAX).then(|| Duration::from_millis(ms))
    }
}

struct FakeClock(Time);

impl Clock for FakeClock {
    fn wall(&self) -> SystemTime {
        self.0.wall()
    }

    fn mono(&self) -> Duration {
        self.0.mono()
    }

    fn wait(&mut self, rest: &Rest, watch: &HeadWatch, within: Option<Duration>) -> Option<Told> {
        let ms = within.map_or(u64::MAX, |w| {
            u64::try_from(w.as_nanos().div_ceil(1_000_000)).unwrap()
        });
        self.0.last_wait_ms.store(ms, Ordering::SeqCst);
        self.0.waits.fetch_add(1, Ordering::SeqCst);
        settle_round(watch);
        let nap = self.0.nap_ms.swap(0, Ordering::SeqCst);
        self.0.wall_ms.fetch_add(nap, Ordering::SeqCst);
        let still = || {
            let n = self.0.still.fetch_add(1, Ordering::SeqCst) + 1;
            assert!(
                n < SPIN,
                "the runner spins: {n} waits in a row passed no time"
            );
        };
        if let Some(told) = rest.take() {
            still();
            return Some(told);
        }
        match within {
            Some(wait) if wait.is_zero() => {
                still();
                None
            }
            Some(wait) if !self.0.withheld.load(Ordering::SeqCst) => {
                self.0.still.store(0, Ordering::SeqCst);
                self.0.timers.fetch_add(1, Ordering::SeqCst);
                self.0.advance(wait);
                None
            }
            _ => {
                self.0.still.store(0, Ordering::SeqCst);
                let told = rest.wait(Some(HANG));
                assert!(
                    told.is_some(),
                    "a wait no timer may end was rung within a minute (a hang detector): \
                     {within:?}"
                );
                told
            }
        }
    }
}

/// Wait, as real time would, until the head round in flight has rung its news: each of its
/// workers is finished, so its drop guard has rung.
fn settle_round(watch: &HeadWatch) {
    aterm_spec::wait::until_within(HANG, "the head round in flight rang", || {
        watch
            .pending
            .iter()
            .flatten()
            .all(|head| head.handle.is_finished())
            .then_some(())
    });
}

/// Wait, without stepping, until no index probe is in flight unfinished: a fake clock passes
/// no time for the probe's worker, which runs on real time.
fn settle_index_probe(runner: &Runner) {
    aterm_spec::wait::until_within(HANG, "the index probe completed", || {
        runner
            .index
            .pending
            .as_ref()
            .is_none_or(|pending| pending.done.load(Ordering::Acquire))
            .then_some(())
    });
}

/// Harvest `index`'s probe at `now` once its worker is done.
fn harvest_when_done(index: &mut IndexWatch, now: Duration) {
    aterm_spec::wait::until_within(HANG, "the index probe completed", || {
        index
            .pending
            .as_ref()
            .is_none_or(|pending| pending.done.load(Ordering::Acquire))
            .then_some(())
    });
    index.harvest(now);
    assert!(index.pending.is_none(), "the finished probe was harvested");
}

/// Step `runner` until `span` of virtual time has passed.
fn run_for(runner: &mut Runner, time: &Time, span: Duration) {
    let until = time.wall() + span;
    while time.wall() < until {
        runner.step();
    }
}

/// The vendors every host in a test reads, and who asked them.
#[derive(Default)]
struct Vendors {
    claude: Mutex<String>,
    /// Every GET, by the asking host's label.
    gets: Mutex<Vec<&'static str>>,
    /// A GET waits while this is set, until it is cleared.
    hold: Mutex<bool>,
    released: std::sync::Condvar,
}

impl Vendors {
    fn new(claude: &str) -> Arc<Self> {
        let vendors = Self::default();
        *vendors.claude.lock().unwrap() = claude.to_string();
        Arc::new(vendors)
    }

    fn count(&self, host: &str) -> usize {
        self.gets
            .lock()
            .unwrap()
            .iter()
            .filter(|h| **h == host)
            .count()
    }

    fn release(&self) {
        *self.hold.lock().unwrap() = false;
        self.released.notify_all();
    }

    fn get(self: &Arc<Self>, host: &'static str) -> Arc<ConcurrentVendorGetFn<'static>> {
        let vendors = Arc::clone(self);
        Arc::new(move |program: &str, url: &str, _cap, _etag: Option<&str>| {
            assert_eq!((program, url), ("claude", CLAUDE_HEAD));
            vendors.gets.lock().unwrap().push(host);
            let held = vendors.hold.lock().unwrap();
            let (held, _) = vendors
                .released
                .wait_timeout_while(held, 2 * HANG, |held| *held)
                .unwrap();
            assert!(!*held, "the test released the held GET");
            drop(held);
            Ok(VendorGet::Body {
                bytes: vendors.claude.lock().unwrap().clone().into_bytes(),
                etag: None,
                effective_url: url.to_string(),
            })
        })
    }
}

/// Passes a test ends by hand.
#[derive(Clone, Default)]
struct Passes {
    launched: Arc<Mutex<Vec<&'static str>>>,
    /// The pass in flight ends when this is set.
    end: Arc<AtomicBool>,
}

struct FakePass(Arc<AtomicBool>);

impl RunningPass for FakePass {
    fn ended(&mut self) -> Option<bool> {
        self.0.swap(false, Ordering::SeqCst).then_some(true)
    }
}

impl PassLauncher for Passes {
    fn launch(&mut self, program: &'static str) -> io::Result<Box<dyn RunningPass>> {
        self.launched.lock().unwrap().push(program);
        Ok(Box::new(FakePass(Arc::clone(&self.end))))
    }

    fn launch_whole(&mut self) -> io::Result<Box<dyn RunningPass>> {
        self.launched.lock().unwrap().push(WHOLE);
        Ok(Box::new(FakePass(Arc::clone(&self.end))))
    }
}

const WHOLE: &str = "<the whole pass>";

impl Passes {
    fn launched(&self) -> Vec<&'static str> {
        self.launched.lock().unwrap().clone()
    }
}

fn session(
    l: &Layout,
    time: &Time,
    vendors: &Arc<Vendors>,
    host: &'static str,
    passes: &Passes,
    on: &Arc<AtomicBool>,
) -> Runner {
    let on = Arc::clone(on);
    Runner::new(
        l.clone(),
        (
            HeadWatch::hosted(&[String::from("codex")], Host::Session),
            vendors.get(host),
        ),
        Box::new(FakeClock(time.clone())),
        Box::new(move || Gate {
            on: on.load(Ordering::SeqCst),
            exclude: vec![String::from("codex")],
        }),
        Box::new(passes.clone()),
        Box::new(|_: &str| {}),
    )
    .with_rest(Rest::by_hand())
    .without_index_probe()
}

fn on() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(true))
}

#[test]
fn a_new_exclusion_retires_a_queued_pass_without_cancelling_the_running_pass() {
    let l = layout("queued-exclusion");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
    let gate = Arc::new(Mutex::new(Gate {
        on: true,
        exclude: Vec::new(),
    }));
    let current_gate = Arc::clone(&gate);
    runner.settings = Box::new(move || current_gate.lock().unwrap().clone());
    runner.queue.extend(["claude", "codex", "agy"]);
    runner.step();
    assert_eq!(passes.launched(), ["claude"]);

    gate.lock().unwrap().exclude = vec!["claude".into(), "codex".into()];
    runner.step();
    assert_eq!(passes.launched(), ["claude"]);
    assert_eq!(runner.following.as_ref().unwrap().program, Some("claude"));

    passes.end.store(true, Ordering::SeqCst);
    runner.step();
    assert_eq!(
        passes.launched(),
        ["claude", "agy"],
        "dispatch must honor the current gate without delaying another allowed pass"
    );
}

/// One round of a window's head watch at the test's wall clock, its round in flight settled
/// first.
fn window_round(
    window: &mut HeadWatch,
    l: &Layout,
    time: &Time,
    get: &Arc<ConcurrentVendorGetFn<'static>>,
) {
    settle_round(window);
    let moved = window.round(l, time.wall(), get, false);
    assert!(moved.is_none(), "the head in these scenes has not moved");
}

/// Step `runner` until its claim waits on a waiter's ring, within `limit` of virtual time.
fn step_until_waiting(runner: &mut Runner, time: &Time, limit: Duration) {
    let start = time.wall();
    while !runner.watch.admission_waits() {
        assert!(
            time.wall() <= start + limit,
            "the session armed its waiter within {limit:?}: {runner:?}"
        );
        runner.step();
    }
}

/// Move a window's head watch on through `span` of virtual time, a round every five seconds.
fn window_runs(
    window: &mut HeadWatch,
    l: &Layout,
    time: &Time,
    get: &Arc<ConcurrentVendorGetFn<'static>>,
    span: Duration,
) {
    let until = time.wall() + span;
    while time.wall() < until {
        time.advance(Duration::from_secs(5));
        window_round(window, l, time, get);
    }
}

/// A SESSION WATCHES ONLY WHILE NO WINDOW DOES, AND TAKES THE WATCH BACK THE MOMENT THE WINDOW
/// CLOSES: behind the window it waits on its hosts waiter with no deadline (it is not stepped at
/// all while the window asks once a minute), and the window's close ends that wait with no timer.
/// NEGATIVE CONTROL: a watch no host runs goes on asking beside the window. MUTANT: a refused
/// session answering `Later` (no waiter) waits on its minute and fails the timer-free take-back.
#[test]
fn a_session_watches_only_while_no_window_does() {
    for hosted in [true, false] {
        let l = layout(&format!("session-vs-window-{hosted}"));
        install(&l, "claude", "2.1.280");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
        if !hosted {
            runner.watch = HeadWatch::shared(&[String::from("codex")]);
            runner.watch.bind_ring(runner.rest.ringer());
        }
        for _ in 0..3 {
            runner.step();
        }
        assert_eq!(vendors.count("session"), 1, "the session asked at once");
        if hosted {
            assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        }

        let window_get = vendors.get("window");
        let mut window = HeadWatch::hosted(&[String::from("codex")], Host::Window);
        window_round(&mut window, &l, &time, &window_get);
        let session_before = vendors.count("session");
        if !hosted {
            let until = time.wall() + Duration::from_secs(5 * 60);
            while time.wall() < until {
                runner.step();
                window_round(&mut window, &l, &time, &window_get);
            }
            assert!(
                vendors.count("session") > session_before,
                "negative control: a watch no host runs goes on asking beside the window"
            );
            let _ = std::fs::remove_dir_all(&l.prefix);
            continue;
        }
        step_until_waiting(&mut runner, &time, CADENCE);
        assert!(runner.watch.session_behind_window());
        assert_eq!(runner.next_wait(time.wall(), time.mono()), None);
        window_runs(
            &mut window,
            &l,
            &time,
            &window_get,
            Duration::from_secs(5 * 60),
        );
        assert_eq!(watcher(&l), Watcher::Window);
        assert_eq!(
            vendors.count("session"),
            session_before,
            "the session asked nothing while the window watched"
        );
        assert!(
            (4..=6).contains(&vendors.count("window")),
            "the window asked once a minute: {}",
            vendors.count("window")
        );

        time.withhold_timers();
        settle_round(&window);
        drop(window);
        let closed = time.wall();
        for _ in 0..3 {
            if !runner.watch.admission_waits() && !runner.watch.session_behind_window() {
                break;
            }
            runner.step();
        }
        assert!(
            !runner.watch.admission_waits() && !runner.watch.session_behind_window(),
            "the window's close handed the session the watch: {runner:?}"
        );
        assert_eq!(time.wall(), closed, "with no timer");
        time.release_timers();
        while vendors.count("session") == session_before {
            assert!(
                time.wall() <= closed + CADENCE,
                "the session asked within the store's minute"
            );
            runner.step();
        }
        assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}

#[test]
fn a_window_opening_mid_round_waits_one_round_then_watches() {
    let l = layout("mid-round");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
    *vendors.hold.lock().unwrap() = true;
    runner.step();
    assert_eq!(
        runner.watch.pending_count(),
        1,
        "the session's round is in flight"
    );
    let window_get = vendors.get("window");
    let mut window = HeadWatch::hosted(&[String::from("codex")], Host::Window);
    window_round(&mut window, &l, &time, &window_get);
    assert_eq!(
        window.pending_count(),
        0,
        "refused while the round holds the watch"
    );
    assert!(!window.due(time.wall()), "and not asked again at once");
    assert!(window.due(time.wall() + super::super::BUSY_RETRY));
    vendors.release();
    runner.step();
    assert_eq!(runner.watch.pending_count(), 0, "the round was harvested");
    time.advance(super::super::BUSY_RETRY);
    window_round(&mut window, &l, &time, &window_get);
    assert_eq!(watcher(&l), Watcher::Window);
    let asked = vendors.count("session");
    step_until_waiting(&mut runner, &time, CADENCE);
    window_runs(
        &mut window,
        &l,
        &time,
        &window_get,
        Duration::from_secs(3 * 60),
    );
    assert_eq!(vendors.count("session"), asked, "the session stands by");
    assert!(runner.watch.admission_waits(), "on its waiter");
    assert!(vendors.count("window") >= 2);
    let _ = std::fs::remove_dir_all(&l.prefix);
}
#[test]
fn a_moved_head_runs_one_pass_and_its_end_is_reported() {
    let l = layout("moved");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.281");
    let passes = Passes::default();
    let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
    while passes.launched().is_empty() {
        assert!(
            time.wall() < Time::default().wall() + CADENCE,
            "launched at once"
        );
        runner.step();
    }
    assert_eq!(passes.launched(), ["claude"]);
    let asked = vendors.count("session");
    run_for(&mut runner, &time, Duration::from_secs(10 * 60));
    assert_eq!(
        vendors.count("session"),
        asked,
        "no head asked while the pass runs"
    );
    assert_eq!(passes.launched(), ["claude"], "and no second pass");

    passes.end.store(true, Ordering::SeqCst);
    let ended = time.wall();
    runner.step();
    assert!(runner.following.is_none(), "the end was read");
    while passes.launched().len() < 2 {
        assert!(
            time.wall() <= ended + FAILED_PASS_RETRY + CADENCE,
            "the short retry came"
        );
        runner.step();
    }
    assert!(
        time.wall() >= ended + FAILED_PASS_RETRY,
        "not before its five minutes"
    );

    install(&l, "claude", "2.1.281");
    passes.end.store(true, Ordering::SeqCst);
    run_for(&mut runner, &time, Duration::from_secs(2 * 3600));
    assert_eq!(passes.launched(), ["claude", "claude"]);
    assert!(
        vendors.count("session") > asked + 100,
        "the watch went on each minute"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_unread_owned_pass_stays_followed_until_an_observed_exit() {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt as _;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt as _;

    struct ObservedPass(VecDeque<io::Result<Option<std::process::ExitStatus>>>);
    impl RunningPass for ObservedPass {
        fn ended(&mut self) -> Option<bool> {
            owned_pass_end(self.0.pop_front().expect("one status read per look"))
        }
    }

    let l = layout("unread-owned-pass");
    let time = Time::default();
    let vendors = Vendors::new("2.1.281");
    let passes = Passes::default();
    let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
    let notes = Arc::new(Mutex::new(Vec::new()));
    let captured = notes.clone();
    runner.log = Box::new(move |line| captured.lock().unwrap().push(line.to_owned()));
    for (program, index_build, subject) in [
        (Some("claude"), None, "claude"),
        (None, None, "Trust"),
        (None, Some(43), "index 43"),
    ] {
        runner.following = Some(Following {
            program,
            index_build,
            pass: Box::new(ObservedPass(VecDeque::from([
                Err(io::Error::other("status unavailable")),
                Ok(None),
                Ok(Some(std::process::ExitStatus::from_raw(0))),
            ]))),
            since: Duration::from_millis(time.mono_ms.load(Ordering::SeqCst)),
        });
        for _ in 0..2 {
            assert!(!runner.follow(time.wall()), "no observed exit yet");
            assert!(
                runner.following.is_some(),
                "retain the pass and its custody"
            );
        }
        assert!(
            runner.follow(time.wall()),
            "an observed exit releases the follow"
        );
        assert!(runner.following.is_none());

        runner.following = Some(Following {
            program,
            index_build,
            pass: Box::new(ObservedPass(VecDeque::from([
                Err(io::Error::other("still unread before the follow limit")),
                Err(io::Error::other("still unread at the follow limit")),
            ]))),
            since: Duration::from_millis(time.mono_ms.load(Ordering::SeqCst)),
        });
        notes.lock().unwrap().clear();
        time.advance(PASS_FOLLOW_LIMIT - Duration::from_millis(1));
        assert!(!runner.follow(time.wall()));
        assert!(notes.lock().unwrap().is_empty());
        time.advance(Duration::from_millis(1));
        assert!(
            runner.follow(time.wall()),
            "unknown does not extend the follow limit"
        );
        assert!(runner.following.is_none());
        assert!(runner.follow(time.wall()));
        let notes = notes.lock().unwrap();
        assert_eq!(notes.len(), 1, "journal every expired pass exactly once");
        assert!(notes[0].contains(subject), "identify the expired pass");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A WAKE ASKS THE HEADS AFTER THE GRACE, AND AT ITS DEADLINE: the wait that spanned an hour's
/// sleep is judged a wake, and the next GET is at the wake plus [`AFTER_WAKE`] exactly. MUTANT:
/// `woke` not called on a wake asks at the old minute deadline, before the grace.
#[test]
fn a_wake_asks_the_heads_after_the_grace() {
    let l = layout("runner-wake");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
    for _ in 0..3 {
        runner.step();
    }
    let asked = vendors.count("session");
    assert_eq!(asked, 1);
    time.sleep_in_next_wait(Duration::from_secs(3600));
    runner.step();
    let woke = time.wall();
    assert_eq!(
        vendors.count("session"),
        asked,
        "not in the wait it woke in"
    );
    while vendors.count("session") == asked {
        assert!(time.wall() <= woke + AFTER_WAKE, "asked by the grace's end");
        runner.step();
    }
    assert_eq!(time.wall(), woke + AFTER_WAKE, "at the grace's end");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// THE SWITCH STANDS THE SESSION DOWN AND FREES THE SEAT, HEARD AT ONCE: `aterm.toml`'s ring ends
/// a seated session's wait with no time passed, the session lets the seat go and then waits with
/// no deadline; turned back on it asks the head at once. MUTANT: an off runner that keeps a
/// deadline (its old five-second look) fails the no-deadline wait.
#[test]
fn the_switch_stands_the_session_down_and_frees_the_seat() {
    let l = layout("switch");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let gate = on();
    let mut runner = session(&l, &time, &vendors, "session", &passes, &gate);
    for _ in 0..3 {
        runner.step();
    }
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    time.withhold_timers();
    gate.store(false, Ordering::SeqCst);
    runner.rest.tell(Told {
        inputs: true,
        ..Told::default()
    });
    let asked = vendors.count("session");
    let at = time.wall();
    runner.step();
    assert_eq!(
        time.wall(),
        at,
        "a seated session hears a live config edit at once"
    );
    assert_eq!(watcher(&l), Watcher::Nobody);
    assert_eq!(
        runner.next_wait(time.wall(), time.mono()),
        None,
        "off, it waits for the next edit with no timer"
    );
    time.advance(Duration::from_secs(5 * 60));
    assert_eq!(vendors.count("session"), asked);
    gate.store(true, Ordering::SeqCst);
    runner.rest.tell(Told {
        inputs: true,
        ..Told::default()
    });
    runner.step();
    runner.step();
    assert_eq!(vendors.count("session"), asked + 1);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_stopped_host_frees_the_watch() {
    let l = layout("stop");
    install(&l, "claude", "2.1.280");
    let (asked_tx, asked_rx) = std::sync::mpsc::channel();
    let asked_tx = Mutex::new(asked_tx);
    let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |_, url, _, _| {
        let _ = asked_tx.lock().unwrap().send(());
        Ok(VendorGet::Body {
            bytes: b"2.1.280".to_vec(),
            etag: None,
            effective_url: url.to_string(),
        })
    });
    let runner = Runner::new(
        l.clone(),
        (
            HeadWatch::hosted(&[String::from("codex")], Host::Session),
            get,
        ),
        Box::new(SystemClock::new()),
        Box::new(|| Gate {
            on: true,
            exclude: vec![String::from("codex")],
        }),
        Box::new(Passes::default()),
        Box::new(|_: &str| {}),
    );
    let handle = runner.spawn().unwrap();
    asked_rx.recv_timeout(HANG).unwrap();
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    handle.stop();
    assert_eq!(watcher(&l), Watcher::Nobody);
    let _ = std::fs::remove_dir_all(&l.prefix);
}
#[test]
fn a_pass_started_mid_round_still_harvests_the_round() {
    use super::super::BUSY_RETRY;
    use super::super::host::{Admit, HostClaim};
    let l = layout("pass-mid-round");
    install(&l, "claude", "2.1.280");
    install(&l, "codex", "0.156.0");
    let codex = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let (answered_tx, answered_rx) = std::sync::mpsc::channel();
    let answered_tx = Mutex::new(answered_tx);
    let get: Arc<ConcurrentVendorGetFn<'static>> = {
        let codex = Arc::clone(&codex);
        Arc::new(move |program: &str, url: &str, _cap, _etag: Option<&str>| {
            if program == "claude" {
                return Ok(VendorGet::Body {
                    bytes: b"2.1.281".to_vec(),
                    etag: None,
                    effective_url: url.to_string(),
                });
            }
            let (go, released) = &*codex;
            let go = go.lock().unwrap();
            let (go, _) = released
                .wait_timeout_while(go, Duration::from_secs(60), |go| !*go)
                .unwrap();
            assert!(*go, "the test released codex's GET");
            let _ = answered_tx.lock().unwrap().send(());
            Ok(VendorGet::Body {
                bytes: br#"{"tag_name":"rust-v0.157.0","assets":[]}"#.to_vec(),
                etag: None,
                effective_url: url.to_string(),
            })
        })
    };
    struct Launched(
        Mutex<std::sync::mpsc::Sender<&'static str>>,
        Arc<AtomicBool>,
    );
    impl PassLauncher for Launched {
        fn launch(&mut self, program: &'static str) -> io::Result<Box<dyn RunningPass>> {
            let _ = self.0.lock().unwrap().send(program);
            Ok(Box::new(FakePass(Arc::clone(&self.1))))
        }
    }
    struct QuickClock(SystemClock);
    impl Clock for QuickClock {
        fn wall(&self) -> SystemTime {
            self.0.wall()
        }

        fn mono(&self) -> Duration {
            self.0.mono()
        }

        fn wait(
            &mut self,
            rest: &Rest,
            watch: &HeadWatch,
            within: Option<Duration>,
        ) -> Option<Told> {
            let quick = Duration::from_millis(50);
            self.0
                .wait(rest, watch, Some(within.map_or(quick, |w| w.min(quick))))
        }
    }
    let (launched_tx, launched_rx) = std::sync::mpsc::channel();
    let end = Arc::new(AtomicBool::new(false));
    let runner = Runner::new(
        l.clone(),
        (HeadWatch::hosted(&[], Host::Session), get),
        Box::new(QuickClock(SystemClock::new())),
        Box::new(|| Gate {
            on: true,
            exclude: Vec::new(),
        }),
        Box::new(Launched(Mutex::new(launched_tx), Arc::clone(&end))),
        Box::new(|_: &str| {}),
    );
    let handle = runner.spawn().unwrap();
    assert_eq!(
        launched_rx.recv_timeout(HANG),
        Ok("claude"),
        "claude's pass started while codex's GET was in flight"
    );
    let mut window = HostClaim::new(Host::Window, &l, None);
    assert_eq!(window.admit().0, Admit::Later(BUSY_RETRY));

    {
        let (go, released) = &*codex;
        *go.lock().unwrap() = true;
        released.notify_all();
    }
    answered_rx.recv_timeout(HANG).unwrap();
    let deadline = Instant::now() + HANG;
    let admitted = loop {
        if window.admit().0 == Admit::Yes {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    end.store(true, Ordering::SeqCst);
    let next = launched_rx.recv_timeout(HANG);
    handle.stop();
    assert!(
        admitted,
        "the round was never harvested while the pass ran: the session kept the \
         rendezvous and the window was refused"
    );
    assert_eq!(
        next,
        Ok("codex"),
        "the peer's moved head ran after the pass"
    );
    drop(window);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn live_settings_hear_an_edit_and_keep_the_gate_through_an_unreadable_file() {
    let l = layout("live-settings");
    let path = l.prefix.join("aterm.toml");
    let mut live = LiveSettings::at(Some(path.clone()), Gate::default());
    assert_eq!(
        live.gate(),
        Gate {
            on: true,
            exclude: Vec::new()
        },
        "no file: the defaults"
    );
    std::fs::write(
        &path,
        "[packages]\nenabled = false\nexclude = [\"codex\"]\n",
    )
    .unwrap();
    assert_eq!(
        live.gate(),
        Gate {
            on: false,
            exclude: vec![String::from("codex")]
        }
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        !live.gate().on,
        "a file that cannot be read keeps the last gate"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn the_seated_session_wakes_a_held_toolchain_move_and_a_standing_one_does_not() {
    use crate::quiet::{Due, DueFlip};
    let due = |_: &Layout, _: i64| {
        Some(DueFlip {
            why: Due::Quiet,
            checked: 1,
        })
    };
    let l = layout("held-seated");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut runner =
        session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
    let start = time.wall();
    while passes.launched().is_empty() {
        assert!(
            time.wall() <= start + crate::quiet::RECHECK,
            "two looks a minute apart, then the pass: {runner:?}"
        );
        runner.step();
    }
    assert_eq!(
        time.wall(),
        start + crate::quiet::RECHECK,
        "the second look is the wait's deadline, a minute after the first"
    );
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    assert!(
        !runner.watch.seated_session(),
        "the session released the exclusive claim after launching"
    );
    assert_eq!(passes.launched(), [WHOLE], "the whole pass, once");
    let asked = vendors.count("session");
    for _ in 0..20 {
        runner.step();
    }
    assert_eq!(passes.launched(), [WHOLE]);
    assert_eq!(
        vendors.count("session"),
        asked,
        "no head asked while it runs"
    );
    passes.end.store(true, Ordering::SeqCst);
    for _ in 0..30 {
        runner.step();
    }
    assert_eq!(
        passes.launched(),
        [WHOLE],
        "the same look of the record wakes nothing again"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);

    let l = layout("held-window");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let window_get = vendors.get("window");
    let mut window = HeadWatch::hosted(&[String::from("codex")], Host::Window);
    window_round(&mut window, &l, &time, &window_get);
    let mut runner =
        session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
    step_until_waiting(&mut runner, &time, BLIND_LOOK);
    window_runs(
        &mut window,
        &l,
        &time,
        &window_get,
        Duration::from_secs(5 * 60),
    );
    assert!(!runner.watch.seated_session());
    assert!(
        passes.launched().is_empty(),
        "a session standing by never wakes it: {:?}",
        passes.launched()
    );
    drop(window);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_seated_session_offers_a_near_index_once_after_window_handoff() {
    use aterm_models_atpkg::atpkg_session_index_handoff_model;
    use std::sync::mpsc;

    let l = layout("session-index-window-handoff");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let requests = Arc::new(AtomicU64::new(0));
    let (near_tx, near_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let probe_requests = Arc::clone(&requests);
    let probe_release = Arc::clone(&release);
    let mut first = session(&l, &time, &vendors, "first", &passes, &on()).with_index_probe(
        move |_, on_near| {
            probe_requests.fetch_add(1, Ordering::SeqCst);
            on_near(43);
            near_tx.send(()).unwrap();
            let (go, wake) = &*probe_release;
            let held = go.lock().unwrap();
            let (held, _) = wake.wait_timeout_while(held, 2 * HANG, |go| !*go).unwrap();
            assert!(*held, "the test released the far HEAD");
            crate::index_probe::Probe::Published(43)
        },
    );
    let second_requests = Arc::clone(&requests);
    let mut second =
        session(&l, &time, &vendors, "second", &passes, &on()).with_index_probe(move |_, _| {
            second_requests.fetch_add(1, Ordering::SeqCst);
            crate::index_probe::Probe::Published(43)
        });
    let model = atpkg_session_index_handoff_model();
    let mut state = model.init_state();

    first.step();
    assert!(model.fire("Start", &mut state));
    near_rx.recv_timeout(HANG).unwrap();
    assert!(model.fire("NearPublished", &mut state));
    second.step();
    assert!(model.fire("OtherSessionStarts", &mut state));
    assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
    assert_eq!(
        second.watch.session_admission_attempts(),
        1,
        "one refused seat look"
    );
    assert_eq!(first.index.pending.is_some(), state["probe"] == 1);

    let mut window = HeadWatch::hosted(&[], Host::Window);
    assert!(window.sit(&l), "the session released the HEAD launch claim");
    assert!(model.fire("WindowOpens", &mut state));
    first.step();
    assert!(model.fire("TryLaunch", &mut state));
    assert_eq!(passes.launched().len(), state["launches"] as usize);
    assert!(passes.launched().is_empty(), "the window owns this hint");
    drop(window);
    assert!(model.fire("WindowCloses", &mut state));
    first.step();
    assert!(model.fire("TryLaunch", &mut state));
    assert_eq!(passes.launched(), [WHOLE]);
    assert_eq!(passes.launched().len(), state["launches"] as usize);

    let (go, wake) = &*release;
    *go.lock().unwrap() = true;
    wake.notify_all();
    assert!(first.index.pending.is_some(), "the far HEAD is still owned");
    settle_index_probe(&first);
    passes.end.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        first.step();
    }
    assert!(model.fire("Complete", &mut state));
    assert!(model.fire("FarReplay", &mut state));
    assert_eq!(passes.launched().len(), state["launches"] as usize);
    assert_eq!(
        passes.launched(),
        [WHOLE],
        "the far answer is not another pass"
    );
    drop(second);
    drop(first);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_empty_index_source_becomes_probe_eligible_at_the_next_look() {
    use aterm_models_atpkg::atpkg_session_index_eligibility_model;
    use std::sync::mpsc;

    let l = layout("session-index-eligibility");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let eligible = Arc::new(AtomicBool::new(false));
    let checks = Arc::new(AtomicU64::new(0));
    let requests = Arc::new(AtomicU64::new(0));
    let (started_tx, started_rx) = mpsc::channel();
    let probe_requests = Arc::clone(&requests);
    let mut runner =
        session(&l, &time, &vendors, "session", &passes, &on()).with_index_probe(move |_, _| {
            probe_requests.fetch_add(1, Ordering::SeqCst);
            started_tx.send(()).unwrap();
            crate::index_probe::Probe::Missing
        });
    let eligible_for_runner = Arc::clone(&eligible);
    let checks_for_runner = Arc::clone(&checks);
    runner.index.eligible = Arc::new(move |_| {
        checks_for_runner.fetch_add(1, Ordering::SeqCst);
        eligible_for_runner.load(Ordering::SeqCst)
    });
    let model = atpkg_session_index_eligibility_model();
    let mut state = model.init_state();

    assert!(!runner.check_index(time.wall()));
    assert!(model.fire("EmptyLook", &mut state));
    assert_eq!(checks.load(Ordering::SeqCst), 1);
    assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
    assert_eq!(runner.index.last_completed, None);
    assert_eq!(runner.index.last_ineligible, Some(Duration::ZERO));

    time.advance(BLIND_LOOK / 2);
    assert!(!runner.check_index(time.wall()));
    assert_eq!(
        checks.load(Ordering::SeqCst),
        1,
        "the local look is paced by the blind look"
    );
    assert_eq!(
        runner.index.next_due(),
        Some(BLIND_LOOK),
        "the next local look is the runner's deadline"
    );
    time.advance(BLIND_LOOK / 2);
    assert!(model.fire("Tick", &mut state));
    assert!(!runner.check_index(time.wall()));
    assert!(model.fire("EmptyLook", &mut state));
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
    assert_eq!(runner.index.last_completed, None);

    eligible.store(true, Ordering::SeqCst);
    assert!(model.fire("Enable", &mut state));
    time.advance(BLIND_LOOK / 2);
    assert!(!runner.check_index(time.wall()));
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    assert!(runner.index.pending.is_none());
    time.advance(BLIND_LOOK / 2);
    assert!(model.fire("Tick", &mut state));
    assert!(!runner.check_index(time.wall()));
    assert!(model.fire("FirstEligibleLook", &mut state));
    assert!(
        runner.index.pending.is_some(),
        "the HEAD worker starts at the next look"
    );
    started_rx.recv_timeout(HANG).unwrap();
    assert_eq!(checks.load(Ordering::SeqCst), 3);
    assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
    assert_eq!(runner.index.last_ineligible, None);

    harvest_when_done(&mut runner.index, time.mono());
    let completed = runner
        .index
        .last_completed
        .expect("network completion stamp");
    assert!(
        !runner
            .index
            .due(completed + crate::index_probe::INTERVAL - Duration::from_millis(1))
    );
    assert!(runner.index.due(completed + crate::index_probe::INTERVAL));
    assert!(passes.launched().is_empty());
    drop(runner);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_suppressed_session_index_probe_uses_the_shared_expiry() {
    use aterm_models_atpkg::atpkg_index_shared_handoff_model;

    let l = layout("session-index-shared-handoff");
    let time = Time::default();
    time.advance(BLIND_LOOK * 5);
    let now = || time.mono();
    let probes = Arc::new(AtomicU64::new(0));
    let asked = Arc::clone(&probes);
    let mut index = IndexWatch::new();
    index.probe = Arc::new(move |_, _| {
        if asked.fetch_add(1, Ordering::SeqCst) == 0 {
            crate::index_probe::Probe::Suppressed(BLIND_LOOK)
        } else {
            crate::index_probe::Probe::Missing
        }
    });
    let model = atpkg_index_shared_handoff_model();
    let mut state = model.init_state();
    assert!(model.fire("Stamp", &mut state));
    for _ in 0..5 {
        assert!(model.fire("TickBeforeHandoff", &mut state));
    }
    assert!(index.due(now()));
    let rest = Rest::inert();
    index.start(&l, now(), rest.ringer()).unwrap();
    harvest_when_done(&mut index, now());
    assert!(
        rest.take().is_some_and(|told| told.rung),
        "the probe's end rang"
    );
    assert!(model.fire("Handoff", &mut state));
    let completed = index.last_completed.expect("worker completion");
    assert_eq!(index.shared_retry_at, Some(completed + BLIND_LOOK));
    assert_eq!(
        index.next_due(),
        Some(completed + BLIND_LOOK),
        "the shared expiry is the runner's deadline"
    );
    assert!(!index.due(completed + BLIND_LOOK - Duration::from_millis(1)));
    time.advance(BLIND_LOOK);
    assert!(model.fire("TickAfterHandoff", &mut state));
    assert!(index.due(completed + BLIND_LOOK));
    assert!(model.fire("AtExpiry", &mut state));
    index
        .start(&l, completed + BLIND_LOOK, rest.ringer())
        .unwrap();
    harvest_when_done(&mut index, completed + BLIND_LOOK);
    assert_eq!(probes.load(Ordering::SeqCst) - 1, state["requests"] as u64);
    drop(index);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn verified_index_near_hint_starts_no_pass_and_probe_is_completion_paced() {
    use std::sync::mpsc;

    let l = layout("session-index-floor");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let probes = Arc::new(AtomicU64::new(0));
    let (started_tx, started_rx) = mpsc::channel();
    let probe_count = Arc::clone(&probes);
    let mut runner = session(&l, &time, &vendors, "session", &passes, &on()).with_index_probe(
        move |_, on_near| {
            probe_count.fetch_add(1, Ordering::SeqCst);
            on_near(43);
            started_tx.send(()).unwrap();
            crate::index_probe::Probe::Published(43)
        },
    );
    runner.step();
    started_rx.recv_timeout(HANG).unwrap();
    crate::sig::Floor::new(l.floor())
        .check_and_record(43)
        .unwrap();
    settle_index_probe(&runner);
    runner.step();
    assert!(
        runner.index.pending.is_none(),
        "one step harvests the finished probe"
    );
    assert!(passes.launched().is_empty(), "verified floor covers 43");
    let completed = runner.index.last_completed.expect("the probe ended");
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    while runner.index.pending.is_none() {
        runner.step();
        assert!(
            time.mono() <= completed + crate::index_probe::INTERVAL + Duration::from_millis(1),
            "the next probe is due at completion plus thirty seconds"
        );
    }
    assert!(
        time.mono_ms.load(Ordering::SeqCst)
            >= (completed + crate::index_probe::INTERVAL).as_millis() as u64,
        "a second probe cannot start before its predecessor's completion stamp"
    );
    started_rx.recv_timeout(HANG).unwrap();
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    assert!(passes.launched().is_empty());
    drop(runner);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_higher_index_wakes_promptly_after_an_older_index_pass() {
    let l = layout("session-index-higher");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let probes = Arc::new(AtomicU64::new(0));
    let count = Arc::clone(&probes);
    let mut runner = session(&l, &time, &vendors, "session", &passes, &on()).with_index_probe(
        move |_, on_near| {
            let build = 43 + count.fetch_add(1, Ordering::SeqCst);
            on_near(build);
            crate::index_probe::Probe::Published(build)
        },
    );
    let deadline = Instant::now() + HANG;
    while passes.launched().is_empty() {
        settle_index_probe(&runner);
        runner.step();
        assert!(Instant::now() < deadline, "the near 43 launched a pass");
    }
    assert_eq!(passes.launched(), [WHOLE]);
    passes.end.store(true, Ordering::SeqCst);
    let started = time.mono_ms.load(Ordering::SeqCst);
    while passes.launched().len() == 1 {
        settle_index_probe(&runner);
        runner.step();
        assert!(Instant::now() < deadline, "the higher 44 launched a pass");
    }
    assert_eq!(passes.launched(), [WHOLE, WHOLE]);
    assert!(
        time.mono_ms.load(Ordering::SeqCst) - started
            < aterm_update_core::pkg_check::PASS_SPACING_SECS * 1_000,
        "the higher build did not wait out the old build's spacing"
    );
    drop(runner);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_same_index_uses_the_bounded_retry_ladder() {
    let model = aterm_models_atpkg::atpkg_session_index_retry_model();
    let mut state = model.init_state();
    let mut index = IndexWatch::new();
    let minute = Duration::from_secs(60);
    index.note_attempt(43, Duration::ZERO, true);
    assert!(model.fire("First", &mut state));
    assert_eq!(
        state["passes"],
        i64::from(index.last_attempt.unwrap().passes)
    );
    assert!(model.action_enabled("EarlySame", &state));
    assert!(
        index
            .last_attempt
            .unwrap()
            .holds(43, 5 * minute - BLIND_LOOK)
    );
    assert!(model.fire("Tick", &mut state));
    assert!(model.action_enabled("RetryFirst", &state));
    assert!(!index.last_attempt.unwrap().holds(43, 5 * minute));
    index.note_attempt(43, 5 * minute, true);
    assert!(model.fire("RetryFirst", &mut state));
    assert_eq!(
        state["passes"],
        i64::from(index.last_attempt.unwrap().passes)
    );
    for _ in 0..2 {
        assert!(model.fire("Tick", &mut state));
        assert!(model.action_enabled("EarlySame", &state));
    }
    assert!(
        index
            .last_attempt
            .unwrap()
            .holds(43, 20 * minute - BLIND_LOOK)
    );
    assert!(model.fire("Tick", &mut state));
    assert!(model.action_enabled("RetrySecond", &state));
    assert!(!index.last_attempt.unwrap().holds(43, 20 * minute));
    index.note_attempt(43, 20 * minute, true);
    assert!(model.fire("RetrySecond", &mut state));
    assert_eq!(
        state["passes"],
        i64::from(index.last_attempt.unwrap().passes)
    );
    assert!(
        index
            .last_attempt
            .unwrap()
            .holds(43, 80 * minute - BLIND_LOOK)
    );
    assert!(!index.last_attempt.unwrap().holds(43, 80 * minute));
    assert!(
        !index
            .last_attempt
            .unwrap()
            .holds(44, 20 * minute + BLIND_LOOK)
    );
    index.note_attempt(44, 20 * minute + BLIND_LOOK, true);
    assert!(model.fire("NewerHint", &mut state));
    assert_eq!(
        state["passes"],
        i64::from(index.last_attempt.unwrap().passes)
    );
    assert_eq!(index.last_attempt.unwrap().passes, 1);
    index.note_attempt(44, 25 * minute, false);
    assert!(
        index
            .last_attempt
            .unwrap()
            .holds(44, 25 * minute + BLIND_LOOK)
    );
    assert!(
        !index
            .last_attempt
            .unwrap()
            .holds(44, 25 * minute + crate::index_probe::INTERVAL)
    );
}

#[test]
fn a_window_arriving_after_a_session_round_owns_the_held_move_look() {
    use crate::quiet::{Due, DueFlip};
    let l = layout("held-handoff");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let looks = Arc::new(AtomicU64::new(0));
    let due_looks = Arc::clone(&looks);
    let due = move |_: &Layout, _: i64| {
        due_looks.fetch_add(1, Ordering::SeqCst);
        Some(DueFlip {
            why: Due::Quiet,
            checked: 1,
        })
    };
    let mut runner =
        session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
    for _ in 0..3 {
        runner.step();
    }
    assert_eq!(vendors.count("session"), 1, "the first round completed");
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    assert!(
        !runner.watch.seated_session(),
        "the exclusive round ended but the seat remains"
    );

    let window_get = vendors.get("window");
    let mut window = HeadWatch::hosted(&[String::from("codex")], Host::Window);
    window_round(&mut window, &l, &time, &window_get);
    assert_eq!(watcher(&l), Watcher::Window);
    let looked_before_window = looks.load(Ordering::SeqCst);
    step_until_waiting(&mut runner, &time, crate::quiet::RECHECK);
    window_runs(
        &mut window,
        &l,
        &time,
        &window_get,
        Duration::from_secs(3 * 60),
    );
    assert_eq!(
        looks.load(Ordering::SeqCst),
        looked_before_window,
        "the standing session never scans the flip record or process table"
    );
    assert!(
        passes.launched().is_empty(),
        "the session must not wake the window's held move: {:?}",
        passes.launched()
    );
    time.withhold_timers();
    settle_round(&window);
    drop(window);
    let closed = time.wall();
    runner.step();
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    assert_eq!(
        looks.load(Ordering::SeqCst),
        looked_before_window + 1,
        "the session reads the record on its first step back"
    );
    assert_eq!(passes.launched(), [WHOLE], "the seat resumed at once");
    assert_eq!(time.wall(), closed, "with no timer");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_held_move_takes_the_seat_on_a_store_with_no_vendor_program() {
    use crate::quiet::{Due, DueFlip};
    let quiet = Arc::new(AtomicBool::new(false));
    let now_quiet = Arc::clone(&quiet);
    let due = move |_: &Layout, _: i64| {
        now_quiet.load(Ordering::SeqCst).then_some(DueFlip {
            why: Due::Quiet,
            checked: 1,
        })
    };
    let l = layout("held-no-vendor");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut runner =
        session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due.clone());
    assert_eq!(watcher(&l), Watcher::Nobody, "before the first look");
    run_for(
        &mut runner,
        &time,
        crate::quiet::RECHECK + crate::quiet::RECHECK / 2,
    );
    assert_eq!(
        watcher(&l),
        Watcher::Session(Some(std::process::id())),
        "the session sits while the move is still busy"
    );
    assert!(passes.launched().is_empty(), "busy: nothing woken");
    quiet.store(true, Ordering::SeqCst);
    let quiet_at = time.wall();
    while passes.launched().is_empty() {
        assert!(
            time.wall() <= quiet_at + 2 * crate::quiet::RECHECK,
            "the next look, a second a minute later, then the pass: {runner:?}"
        );
        runner.step();
    }
    assert!(
        !runner.watch.seated_session(),
        "the exclusive look ended after launching"
    );
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    assert_eq!(passes.launched(), [WHOLE]);
    assert_eq!(vendors.count("session"), 0, "no head was asked");
    drop(runner);
    let _ = std::fs::remove_dir_all(&l.prefix);

    let l = layout("held-no-vendor-window");
    let mut window = HeadWatch::hosted(&[], Host::Window);
    assert!(window.sit(&l), "the window takes the shared claim");
    assert_eq!(watcher(&l), Watcher::Window);
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut runner =
        session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
    step_until_waiting(&mut runner, &time, BLIND_LOOK);
    time.advance(Duration::from_secs(5 * 60));
    assert!(window.sit(&l));
    assert_eq!(watcher(&l), Watcher::Window);
    assert!(!runner.watch.seated_session());
    assert!(
        passes.launched().is_empty(),
        "a session beside a window never wakes it: {:?}",
        passes.launched()
    );
    drop(window);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// Ends a fake pass and removes its prefix on every way out of a test that unwinds, a
/// failed assertion included: the pass is detached and loops until `go` stands or its
/// directory is gone.
#[cfg(unix)]
struct Release {
    go: PathBuf,
    prefix: PathBuf,
}

#[cfg(unix)]
impl Drop for Release {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.go, b"");
        let _ = std::fs::remove_dir_all(&self.prefix);
    }
}

/// The most turns a fake pass's wait takes, 50 ms apart: ten minutes, far past any
/// test's [`HANG`].
#[cfg(unix)]
const FAKE_PASS_TURNS: u32 = 12_000;

/// A fake `aterm` pass for the detached launch: `warm` exits at once (its first exec is
/// paid unbounded, [`warm`]); anything else writes `<pid> <spawner> <argv>` to `said`
/// beside it and waits for `go` there.
#[cfg(unix)]
fn fake_pass(owner: u32) -> String {
    format!(
        "#!/bin/sh\n[ \"$1\" = warm ] && exit 0\nd=$(dirname \"$0\")\nprintf '%s %s %s\\n' \
         \"$$\" \"$ATPKG_SPAWNER_PID\" \"$*\" >\"$d/said.tmp\"\nmv \"$d/said.tmp\" \
         \"$d/said\"\nn=0\nwhile [ -d \"$d\" ] && [ ! -e \"$d/go\" ] && kill -0 {owner} \
         2>/dev/null && [ \"$n\" -lt {FAKE_PASS_TURNS} ]; do n=$((n + 1)); sleep 0.05; \
         done\n"
    )
}

/// Write [`fake_pass`] for `owner` as `dir/aterm`, and pay its first exec here,
/// unbounded: the first exec of a new file waits, at 0% CPU, until macOS `syspolicyd`
/// has assessed it (40 s measured for this script); a later exec of the same file costs
/// nothing (the manual.rs `run_once` rule).
#[cfg(unix)]
fn warm(dir: &Path, owner: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    let exe = dir.join("aterm");
    std::fs::write(&exe, fake_pass(owner)).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::process::Command::new(&exe)
        .arg("warm")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    exe
}

/// An exit is complete before a parent reaps the pid: signal-zero alone still finds it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_unreaped_exit_ends_the_followed_pass() {
    use aterm_uds::process::{ProcessPresence, process_presence};
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut pass = Detached { pid };
    assert_eq!(pass.ended(), None, "the live pass is still followed");
    drop(child.stdin.take());
    let deadline = Instant::now() + HANG;
    while process_presence(pid as i32) != ProcessPresence::Exited {
        assert!(Instant::now() < deadline, "the child exited before reap");
        std::thread::sleep(Duration::from_millis(5));
    }
    let still_has_pid = crate::progress::pid_alive(pid);
    let ended = pass.ended();
    child.wait().unwrap();
    assert!(
        still_has_pid,
        "negative control: signal-zero finds the zombie"
    );
    assert_eq!(ended, Some(true), "an unreaped exit completed the pass");
    assert_eq!(pass.ended(), Some(true), "reaping keeps the pass complete");
}

/// A detached pass is not this process's child (a session reaping its shell never takes
/// the pass's exit for the shell's, and the pass outlives the session), names no
/// spawner, and is followed to its end by pid.
#[cfg(unix)]
#[test]
fn a_detached_pass_is_nobodys_child_and_is_followed_to_its_end() {
    let l = layout("detached");
    let dir = l.prefix.join("bin");
    let _release = Release {
        go: dir.join("go"),
        prefix: l.prefix.clone(),
    };
    let exe = warm(&dir, std::process::id());
    let mut passes = DetachedPasses::new(exe, vec!["pkg".into()], &l);
    let mut pass = passes.launch("claude").unwrap();
    let said = dir.join("said");
    let deadline = Instant::now() + HANG;
    while !said.exists() {
        assert!(Instant::now() < deadline, "the pass started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let said = std::fs::read_to_string(&said).unwrap();
    let (pid, rest) = said.split_once(' ').unwrap();
    assert!(
        rest.starts_with("detached pkg update claude --head-watch --wait-lock"),
        "{said}"
    );
    assert_eq!(pass.ended(), None, "running");
    let pid: libc::pid_t = pid.parse().unwrap();
    // SAFETY: `waitpid` with WNOHANG on one pid only asks the kernel about that pid.
    let reaped = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    assert_eq!(
        (reaped, std::io::Error::last_os_error().raw_os_error()),
        (-1, Some(libc::ECHILD)),
        "the running pass is not this process's child"
    );
    std::fs::write(dir.join("go"), b"").unwrap();
    let deadline = Instant::now() + HANG;
    let ended = loop {
        if let Some(ok) = pass.ended() {
            break ok;
        }
        assert!(Instant::now() < deadline, "the pass's end was seen");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(ended, "a detached pass's end is the store's to judge");
}

/// The pid a runner follows is the pass's own behind the utility clamp: the launcher
/// execs the pass in place, so the detaching shell's `$!` is the pass's `$$`, in the
/// group the shell made and still marked detached.
#[cfg(unix)]
#[test]
fn the_followed_pid_is_the_clamped_passs_own() {
    struct Kill(libc::pid_t);
    impl Drop for Kill {
        fn drop(&mut self) {
            // SAFETY: a signal to one pid this test started; touches no memory.
            unsafe { libc::kill(self.0, libc::SIGKILL) };
        }
    }
    let dir = crate::testdir::dir("atpkg-detached-pid-");
    let said = dir.join("said");
    let script = format!(
        "printf '%s %s %s\\n' \"$$\" \"${}\" \"$(/bin/ps -o pri= -p $$)\" > '{1}.tmp' \
         && mv '{1}.tmp' '{1}' && exec /bin/sleep 60",
        crate::cli::SPAWNER_PID_ENV,
        said.display()
    );
    let pid = detach(Path::new("/bin/sh"), &["-c".into(), script.into()]).unwrap();
    let pid = libc::pid_t::try_from(pid).unwrap();
    let _kill = Kill(pid);
    let deadline = Instant::now() + HANG;
    let said = loop {
        if let Ok(text) = std::fs::read_to_string(&said) {
            break text;
        }
        assert!(Instant::now() < deadline, "the pass started");
        std::thread::sleep(Duration::from_millis(10));
    };
    let fields: Vec<&str> = said.split_whitespace().collect();
    let [own, spawner, ref pri @ ..] = fields[..] else {
        panic!("{said:?}");
    };
    assert_eq!(own, pid.to_string(), "the followed pid is the pass's own");
    assert_eq!(spawner, crate::cli::SPAWNER_DETACHED);
    // SAFETY: `getpgid` only asks the kernel about one pid, or this process.
    let (group, ours) = unsafe { (libc::getpgid(pid), libc::getpgid(0)) };
    assert_ne!(group, ours, "the pass shares this process's group");
    // SAFETY: `waitpid` with WNOHANG on one pid only asks the kernel about that pid.
    let reaped = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    assert_eq!(
        (reaped, std::io::Error::last_os_error().raw_os_error()),
        (-1, Some(libc::ECHILD)),
        "the pass is not this process's child"
    );
    if cfg!(target_os = "macos") {
        let pri: i32 = pri.concat().parse().unwrap();
        assert!(pri <= 20, "the pass ran at pri {pri}, not utility (20)");
    }
}

/// ONE SESSION WATCHES; THE NEXT TAKES THE SEAT THE MOMENT THE FIRST STOPS: the standing session
/// tries the seat once and waits on its seat waiter, with no deadline and no further try while the
/// seated one asks each minute. When the seated one goes the waiter takes the seat and holds it
/// for its session between its ring and the session's own look (a probe of the seat is refused),
/// and the session is seated with no timer. MUTANT: a waiter that lets the seat go before it rings
/// fails the probe; a refused seat answering `Later` (no waiter) tries once a minute and waits on
/// its timer.
#[test]
fn the_next_session_takes_the_seat_when_the_seated_one_stops() {
    let l = layout("two-sessions");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut first = session(&l, &time, &vendors, "first", &passes, &on());
    let mut second = session(&l, &time, &vendors, "second", &passes, &on());
    first.step();
    step_until_waiting(&mut second, &time, BLIND_LOOK);
    assert_eq!(vendors.count("second"), 0, "the other stood by");
    assert_eq!(second.next_wait(time.wall(), time.mono()), None);
    let start = time.wall();
    run_for(&mut first, &time, Duration::from_secs(5 * 60));
    assert!(
        (5..=6).contains(&vendors.count("first")),
        "the seat holder asked each minute: {} GETs over {:?}",
        vendors.count("first"),
        time.wall().duration_since(start).unwrap()
    );
    assert_eq!(vendors.count("second"), 0, "the other stood by");
    assert_eq!(
        second.watch.session_admission_attempts(),
        1,
        "a second tab tries the seat once and waits, not once a minute"
    );
    settle_round(&first.watch);
    time.withhold_timers();
    drop(first);
    let gone = time.wall();
    assert!(
        second.rest.heard(HANG, |told| told.rung),
        "the seat waiter rang when the seat freed"
    );
    let probe = crate::index_probe::open_private(&l.prefix.join("vendor-head.session"), true)
        .expect("the seat file");
    assert!(
        crate::lock::RecordLock::try_lock(probe, 64).is_err(),
        "the waiter holds the seat for its session until it looks"
    );
    second.step();
    assert!(second.watch.holds_seat(), "seated: {second:?}");
    assert!(!second.watch.admission_waits());
    assert_eq!(time.wall(), gone, "with no timer");
    time.release_timers();
    while vendors.count("second") == 0 {
        assert!(
            time.wall() <= gone + CADENCE,
            "the next session asked within the store's minute: seat {:?}, GETs {:?}",
            watcher(&l),
            vendors.gets.lock().unwrap()
        );
        second.step();
    }
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A SESSION WAITING ON THE SEAT HEARS A GATE EDIT AT ONCE AND READS THE GATE FIRST: its wait
/// has no deadline, the rest's ring for `aterm.toml` ends it with no time passed, the gate is read
/// once, and turning off stands the session down to a wait with no deadline again. Turned back on
/// it waits on the seat again, asking nothing. MUTANT: a refused seat answered by the old minute
/// (`Later`, no waiter) waits on a deadline, not with none.
#[test]
fn a_waiting_session_hears_a_gate_edit_at_once_and_reads_the_gate_first() {
    let l = layout("waiting-gate");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut first = session(&l, &time, &vendors, "first", &passes, &on());
    for _ in 0..3 {
        first.step();
    }
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));

    let gate = on();
    let gate_reads = Arc::new(AtomicU64::new(0));
    let mut second = session(&l, &time, &vendors, "second", &passes, &gate);
    let watched_gate = Arc::clone(&gate);
    let watched_reads = Arc::clone(&gate_reads);
    second.settings = Box::new(move || {
        watched_reads.fetch_add(1, Ordering::SeqCst);
        Gate {
            on: watched_gate.load(Ordering::SeqCst),
            exclude: vec![String::from("codex")],
        }
    });
    step_until_waiting(&mut second, &time, BLIND_LOOK);
    time.withhold_timers();
    let at = time.wall();
    let reads_before = gate_reads.load(Ordering::SeqCst);
    gate.store(false, Ordering::SeqCst);
    second.rest.tell(Told {
        inputs: true,
        ..Told::default()
    });
    second.step();
    assert_eq!(
        time.last_wait(),
        None,
        "the waiting session had no deadline"
    );
    assert_eq!(time.wall(), at, "the edit ended the wait at once");
    assert_eq!(gate_reads.load(Ordering::SeqCst), reads_before + 1);
    assert!(!second.on);
    assert_eq!(second.next_wait(time.wall(), time.mono()), None);
    assert_eq!(vendors.count("second"), 0);
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));

    gate.store(true, Ordering::SeqCst);
    second.rest.tell(Told {
        inputs: true,
        ..Told::default()
    });
    second.step();
    assert_eq!(time.last_wait(), None, "standing down had no deadline");
    assert_eq!(time.wall(), at);
    assert!(second.on);
    assert!(second.watch.admission_waits(), "back on the seat waiter");
    assert_eq!(vendors.count("second"), 0);
    drop(second);
    drop(first);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// STOP ENDS A RUNNER WAITING WITH NO DEADLINE: a session behind a window waits on its hosts
/// waiter with no deadline on the real clock, and [`HostHandle::stop`] ends that wait and the
/// thread. MUTANT: a stop that sets its flag without ringing the rest hangs the join (the hang
/// detector fails the test).
#[test]
fn stop_ends_a_runner_waiting_with_no_deadline() {
    struct NotifyingClock {
        inner: SystemClock,
        waited: std::sync::mpsc::Sender<Option<Duration>>,
    }

    impl Clock for NotifyingClock {
        fn wall(&self) -> SystemTime {
            self.inner.wall()
        }

        fn mono(&self) -> Duration {
            self.inner.mono()
        }

        fn wait(
            &mut self,
            rest: &Rest,
            watch: &HeadWatch,
            within: Option<Duration>,
        ) -> Option<Told> {
            let _ = self.waited.send(within);
            self.inner.wait(rest, watch, within)
        }
    }

    let l = layout("stop-waiting");
    let mut window = HeadWatch::hosted(&[], Host::Window);
    assert!(window.sit(&l), "the window holds the rendezvous");
    let (waited_tx, waited_rx) = std::sync::mpsc::channel();
    let get: Arc<ConcurrentVendorGetFn<'static>> =
        Arc::new(|_, _, _, _| panic!("a session behind a window must not ask a vendor"));
    let runner = Runner::new(
        l.clone(),
        (
            HeadWatch::hosted(&[String::from("codex")], Host::Session),
            get,
        ),
        Box::new(NotifyingClock {
            inner: SystemClock::new(),
            waited: waited_tx,
        }),
        Box::new(|| Gate {
            on: true,
            exclude: vec![String::from("codex")],
        }),
        Box::new(Passes::default()),
        Box::new(|_: &str| {}),
    )
    .with_rest(Rest::by_hand())
    .without_index_probe();
    let handle = runner.spawn().unwrap();
    let started = std::time::Instant::now();
    while waited_rx
        .recv_timeout(HANG)
        .expect("the runner waits (a hang detector)")
        .is_some()
    {
        assert!(
            started.elapsed() < HANG,
            "the runner comes to wait with no deadline (a hang detector)"
        );
    }
    let (stopped_tx, stopped_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        handle.stop();
        stopped_tx.send(()).unwrap();
    });
    stopped_rx
        .recv_timeout(HANG)
        .expect("the stop ended the wait with no deadline");
    drop(window);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// Step `runner` `steps` times, failing on two zero waits in a row: a state whose wait is zero
/// must act or name a later deadline in that step (design §1.6).
fn step_watching_zeros(runner: &mut Runner, time: &Time, steps: usize) {
    let mut zero_before = false;
    for _ in 0..steps {
        let zero = runner.next_wait(time.wall(), time.mono()) == Some(Duration::ZERO);
        assert!(
            !(zero && zero_before),
            "two zero waits in a row: {runner:?}"
        );
        zero_before = zero;
        runner.step();
    }
}

/// A session whose index probe is eligible and due and whose held move's look is due: every look
/// it has needs admission.
fn blocked_session(l: &Layout, time: &Time, vendors: &Arc<Vendors>, passes: &Passes) -> Runner {
    session(l, time, vendors, "blocked", passes, &on())
        .with_index_probe(|_, _| crate::index_probe::Probe::Missing)
        .with_held_move_due(|_, _| None)
}

/// `runner`'s claim waits on its waiter while the index probe, a published index and the held
/// move's look are all due: its wait has no deadline, and a stray ring leaves it so.
fn assert_blocked_on_its_waiter(runner: &mut Runner, time: &Time) {
    runner.index.ready = 43;
    assert!(runner.watch.admission_waits(), "{runner:?}");
    assert!(runner.index.due(time.mono()), "the index probe is due");
    assert!(
        runner.held.look_due(runner.held_origin + time.mono()),
        "the held move's look is due"
    );
    assert_eq!(
        runner.index_ready_due(time.wall(), time.mono()),
        Some(time.mono()),
        "the published index is due"
    );
    assert_eq!(
        runner.next_wait(time.wall(), time.mono()),
        None,
        "due but blocked: it waits on its waiter's ring, with no deadline"
    );
    let at = time.wall();
    runner.rest.tell(Told {
        rung: true,
        ..Told::default()
    });
    runner.step();
    assert_eq!(time.wall(), at);
    assert!(runner.watch.admission_waits());
    assert_eq!(
        runner.next_wait(time.wall(), time.mono()),
        None,
        "a stray ring leaves it waiting, not spinning"
    );
    runner.index.ready = 0;
}

/// NO BLOCKED STATE WAITS ZERO TWICE, AND A DUE-BUT-BLOCKED STATE NEVER WAITS ON A DEADLINE THAT
/// HIDES ITS WORK (design §1.6). A session whose seat is taken and a seated session behind a
/// window, each with the index probe due, a published index ready and a held move's look due,
/// wait with no deadline on their waiters, and a stray ring leaves them so; without a ring to
/// wait on, each keeps the bounded cadence it has on main (a minute for the seat, a blind look
/// behind a window), floored at its admission retry. A seated session then passes through a
/// deferred moved head, a refused helper thread, a queued pass and its follow, a published index
/// on its retry ladder and within the machine's pass spacing, an uninstalled head and an
/// ineligible index source, and never waits zero twice in a row. A waiter that hands over the seat
/// after the store's folder is gone, or while the admission clock stands, is taken in the one
/// step its zero wait buys: the next wait is the minute the missing folder gets on main, and with
/// the clock standing admission is asked at once. MUTANT: dropping the waiter row of `next_wait`
/// waits zero on the due index while the claim waits; dropping the `admit_after` floor waits zero
/// on it while admission is refused; taking a hand-over only through `begin_update_look` waits
/// zero again and again while the folder is gone; leaving the admission clock standing after a
/// hand-over asks for admission only when it runs out.
#[test]
fn no_blocked_state_waits_zero_twice() {
    let codex = || vec![String::from("codex")];
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();

    let l = layout("blocked-seat");
    install(&l, "claude", "2.1.280");
    let mut first = session(&l, &time, &vendors, "first", &passes, &on());
    first.step();
    let mut seat_taken = blocked_session(&l, &time, &vendors, &passes);
    step_until_waiting(&mut seat_taken, &time, BLIND_LOOK);
    assert!(!seat_taken.watch.session_behind_window());
    assert_blocked_on_its_waiter(&mut seat_taken, &time);
    let mut ringless = blocked_session(&l, &time, &vendors, &passes);
    ringless.watch = HeadWatch::hosted(&codex(), Host::Session);
    ringless.step();
    assert_eq!(
        ringless.next_wait(time.wall(), time.mono()),
        Some(CADENCE),
        "with no waiter the seat is tried once a minute, as on main"
    );
    ringless.index.ready = 43;
    assert_eq!(
        ringless.next_wait(time.wall(), time.mono()),
        Some(CADENCE),
        "floored at the admission retry, whatever is due"
    );
    step_watching_zeros(&mut ringless, &time, 4);
    drop((first, seat_taken, ringless));

    let l = layout("blocked-window");
    install(&l, "claude", "2.1.280");
    let mut window = HeadWatch::hosted(&[], Host::Window);
    assert!(window.sit(&l));
    let mut behind = blocked_session(&l, &time, &vendors, &passes);
    step_until_waiting(&mut behind, &time, BLIND_LOOK);
    assert!(behind.watch.session_behind_window());
    assert_blocked_on_its_waiter(&mut behind, &time);
    drop(behind);
    let mut ringless = blocked_session(&l, &time, &vendors, &passes);
    ringless.watch = HeadWatch::hosted(&codex(), Host::Session);
    ringless.step();
    assert_eq!(
        ringless.next_wait(time.wall(), time.mono()),
        Some(BLIND_LOOK),
        "with no waiter a session behind a window looks every blind look, as on main"
    );
    ringless.index.ready = 43;
    step_watching_zeros(&mut ringless, &time, 4);
    drop((ringless, window));

    let l = layout("blocked-gone");
    install(&l, "claude", "2.1.280");
    let mut first = session(&l, &time, &vendors, "first", &passes, &on());
    first.step();
    let mut gone = blocked_session(&l, &time, &vendors, &passes);
    step_until_waiting(&mut gone, &time, BLIND_LOOK);
    settle_round(&first.watch);
    std::fs::remove_dir_all(&l.prefix).unwrap();
    drop(first);
    aterm_spec::wait::until_within(HANG, "the waiter handed over the unlinked seat", || {
        gone.watch.admission_handed().then_some(())
    });
    step_watching_zeros(&mut gone, &time, 4);
    assert!(gone.watch.holds_seat(), "the hand-over was taken: {gone:?}");
    assert_eq!(
        gone.next_wait(time.wall(), time.mono()),
        Some(CADENCE),
        "with the store's folder gone it looks once a minute, as on main"
    );
    drop(gone);

    let l = layout("blocked-clock");
    install(&l, "claude", "2.1.280");
    let mut first = session(&l, &time, &vendors, "first", &passes, &on());
    first.step();
    let mut clocked = blocked_session(&l, &time, &vendors, &passes);
    step_until_waiting(&mut clocked, &time, BLIND_LOOK);
    // As a waiter that hands over between its admission and the runner's read of it leaves it.
    clocked.admit_after = time.mono() + CADENCE;
    settle_round(&first.watch);
    drop(first);
    aterm_spec::wait::until_within(HANG, "the waiter handed over the seat", || {
        clocked.watch.admission_handed().then_some(())
    });
    let asked = clocked.watch.session_admission_attempts();
    let at = time.wall();
    clocked.step();
    assert_eq!(time.wall(), at, "the hand-over's wait is zero");
    assert!(clocked.watch.holds_seat(), "{clocked:?}");
    assert!(
        clocked.watch.session_admission_attempts() > asked,
        "admission is asked at once, not when the clock runs out"
    );
    step_watching_zeros(&mut clocked, &time, 4);
    drop(clocked);
    let _ = std::fs::remove_dir_all(&l.prefix);

    let l = layout("blocked-free");
    install(&l, "claude", "2.1.280");
    let mut runner = session(&l, &time, &vendors, "free", &passes, &on());
    step_watching_zeros(&mut runner, &time, 3);
    runner.watch.defer_moved(&["claude"]);
    assert_eq!(
        runner.next_wait(time.wall(), time.mono()),
        Some(Duration::ZERO),
        "a deferred moved head is due now"
    );
    step_watching_zeros(&mut runner, &time, 3);
    time.advance(CADENCE);
    runner.watch.fail_next_pending_spawn = true;
    step_watching_zeros(&mut runner, &time, 3);
    runner.queue.push_back("claude");
    step_watching_zeros(&mut runner, &time, 3);
    assert_eq!(
        runner.next_wait(time.wall(), time.mono()),
        Some(BLIND_LOOK),
        "a followed pass is looked at every blind look"
    );
    passes.end.store(true, Ordering::SeqCst);
    step_watching_zeros(&mut runner, &time, 2);
    runner.index.ready = 43;
    runner.index.note_attempt(43, time.mono(), true);
    step_watching_zeros(&mut runner, &time, 3);
    runner.index.last_attempt = None;
    let unix = unix_seconds(time.wall());
    crate::status::stamp_pass_end(
        &l,
        &aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs()),
        aterm_update_core::pkg_check::PassOutcome::Ok,
    )
    .unwrap();
    assert!(
        runner
            .index_ready_due(time.wall(), time.mono())
            .is_some_and(|due| due > time.mono()),
        "within the pass spacing, a later deadline"
    );
    step_watching_zeros(&mut runner, &time, 3);
    runner.index.ready = 0;
    step_watching_zeros(&mut runner, &time, 6);
    drop(runner);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A RUNNER THAT HEARS SLEEPS WAKES ONLY AT ITS HEAD TICKS. A seated session whose rest hears
/// its files and its sleeps, with one installed head and an index probe whose shared stamp is
/// fresh past the hour, waits an idle hour on deadlines alone: sixty timers, each ending at a
/// head's minute, the head asked at exactly t0, t0+60 s, …, t0+3600 s, and the index probed
/// once. Its other wakes are rings (each round's end and the probe's), which pass no time.
/// MUTANT: a wait capped at the old five-second slice takes 720 timers; an index deadline that
/// ignores the shared stamp probes every thirty seconds.
#[test]
fn a_runner_that_hears_sleeps_wakes_only_at_its_head_ticks() {
    let l = layout("idle-hour");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let passes = Passes::default();
    let asked_at = Arc::new(Mutex::new(Vec::new()));
    let get: Arc<ConcurrentVendorGetFn<'static>> = {
        let asked = Arc::clone(&asked_at);
        let clock = time.clone();
        Arc::new(move |program: &str, url: &str, _cap, _etag: Option<&str>| {
            assert_eq!(program, "claude");
            asked.lock().unwrap().push(clock.wall());
            Ok(VendorGet::Body {
                bytes: b"2.1.280".to_vec(),
                etag: None,
                effective_url: url.to_string(),
            })
        })
    };
    let probes = Arc::new(AtomicU64::new(0));
    let probed = Arc::clone(&probes);
    let mut runner = Runner::new(
        l.clone(),
        (
            HeadWatch::hosted(&[String::from("codex")], Host::Session),
            get,
        ),
        Box::new(FakeClock(time.clone())),
        Box::new(|| Gate {
            on: true,
            exclude: vec![String::from("codex")],
        }),
        Box::new(passes.clone()),
        Box::new(|_: &str| {}),
    )
    .with_rest(Rest::by_hand())
    .with_index_probe(move |_, _| {
        probed.fetch_add(1, Ordering::SeqCst);
        crate::index_probe::Probe::Suppressed(Duration::from_secs(2 * 3600))
    });
    let t0 = time.wall();
    run_for(&mut runner, &time, Duration::from_secs(3600));
    settle_round(&runner.watch);
    let want: Vec<SystemTime> = (0..=60_u32).map(|k| t0 + CADENCE * k).collect();
    assert_eq!(*asked_at.lock().unwrap(), want, "the head at each minute");
    assert_eq!(time.timers.load(Ordering::SeqCst), 60, "one timer a minute");
    assert_eq!(probes.load(Ordering::SeqCst), 1, "the index probed once");
    assert!(passes.launched().is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// WITHOUT A WAKE SOURCE, A WATCHING RUNNER WAITS AT MOST A BLIND LOOK; BEHIND A WINDOW IT WAITS
/// FOR THE WINDOW. A terminal session's rest watches its files but hears no sleep, so while it
/// watches the heads every wait is at most [`BLIND_LOOK`] (a sleep is noticed as soon as on main),
/// even with its only deadline a minute off. Behind a window it waits on its hosts waiter with no
/// deadline at all. A rest whose file watch did not start looks at its files every blind look,
/// even with Automatic updates off. MUTANT: dropping the sleep cap waits a minute; dropping the
/// unwatched cap waits forever while off.
#[test]
fn without_a_wake_source_a_watching_runner_waits_at_most_a_blind_look() {
    let l = layout("no-wake-source");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let rest = Rest::start("atpkg-no-wake-test", [l.prefix.join("aterm.toml")]);
    assert!(rest.watching(), "the platform's file watch started");
    assert!(!rest.hears_sleep());
    let mut runner = session(&l, &time, &vendors, "session", &passes, &on())
        .with_rest(rest)
        .with_index_probe(|_, _| {
            crate::index_probe::Probe::Suppressed(Duration::from_secs(2 * 3600))
        });
    let start = time.wall();
    while time.wall() < start + Duration::from_secs(10 * 60) {
        runner.step();
        let wait = time.last_wait();
        assert!(
            wait.is_some_and(|w| w <= BLIND_LOOK),
            "every wait at most a blind look: {wait:?}"
        );
    }
    assert!(
        time.timers.load(Ordering::SeqCst) >= 10 * 60 / BLIND_LOOK.as_secs() - 1,
        "the blind look ran"
    );

    let mut window = HeadWatch::hosted(&[], Host::Window);
    while !window.sit(&l) {
        runner.step();
    }
    step_until_waiting(&mut runner, &time, CADENCE);
    assert_eq!(
        runner.next_wait(time.wall(), time.mono()),
        None,
        "behind a window a session waits for the window, with no deadline"
    );
    drop(window);

    runner.rest = Rest::inert();
    runner.on = false;
    assert_eq!(
        runner.next_wait(time.wall(), time.mono()),
        Some(BLIND_LOOK),
        "with no file watch the gate is read every blind look"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A GATE EDIT STANDS THE RUNNER DOWN WITH NO TIMER, ON THE REAL FILE WATCH: a seated session
/// whose rest watches `aterm.toml` (timers withheld, so only a ring can end a wait) stands down
/// when the file turns Automatic updates off, frees the seat, then waits with no deadline, and the
/// edit that turns them back on ends that wait. NEGATIVE CONTROL: a rest that does not watch the
/// file never hears the edit, and the hang detector fails the test.
#[test]
fn a_gate_edit_stands_the_runner_down_with_no_timer() {
    let l = layout("gate-file");
    install(&l, "claude", "2.1.280");
    let config = l.prefix.join("aterm.toml");
    std::fs::write(&config, "[packages]\nenabled = true\n").unwrap();
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut runner = session(&l, &time, &vendors, "session", &passes, &on())
        .with_rest(Rest::start("atpkg-gate-test", [config.clone()]).hearing_sleeps());
    runner.settings = Box::new(LiveSettings::at(Some(config.clone()), Gate::default()));
    assert!(runner.rest.watching(), "the platform's file watch started");
    for _ in 0..3 {
        runner.step();
    }
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    time.withhold_timers();
    let at = time.wall();
    std::fs::write(&config, "[packages]\nenabled = false\n").unwrap();
    for _ in 0..4 {
        if !runner.on {
            break;
        }
        runner.step();
    }
    assert!(!runner.on, "the edit stood the runner down");
    assert_eq!(time.wall(), at, "with no timer");
    assert_eq!(watcher(&l), Watcher::Nobody);
    assert_eq!(runner.next_wait(time.wall(), time.mono()), None);
    std::fs::write(&config, "[packages]\nenabled = true\n").unwrap();
    for _ in 0..4 {
        if runner.on {
            break;
        }
        runner.step();
    }
    assert!(runner.on, "the next edit ended the wait with no deadline");
    assert_eq!(time.wall(), at);
    let _ = std::fs::remove_dir_all(&l.prefix);
}
