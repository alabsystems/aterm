// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::Mutex;
use std::sync::atomic::AtomicU64;

use super::super::tests::{HANG, install, layout};
use super::super::{AFTER_WAKE, CADENCE, FAILED_PASS_RETRY, Watcher, watcher};
use super::*;
use crate::flow::VendorGet;

const CLAUDE_HEAD: &str = "https://downloads.claude.ai/claude-code-releases/latest";

/// The two clocks a test moves by hand: every park advances both by its slice, and a
/// sleep the next park takes moves the wall clock alone.
#[derive(Clone, Default)]
struct Time {
    wall_ms: Arc<AtomicU64>,
    mono_ms: Arc<AtomicU64>,
    nap_ms: Arc<AtomicU64>,
    last_park_ms: Arc<AtomicU64>,
    parks: Arc<AtomicU64>,
}

impl Time {
    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
            + Duration::from_secs(1_790_000_000)
            + Duration::from_millis(self.wall_ms.load(Ordering::SeqCst))
    }

    fn advance(&self, by: Duration) {
        let ms = u64::try_from(by.as_millis()).unwrap();
        self.wall_ms.fetch_add(ms, Ordering::SeqCst);
        self.mono_ms.fetch_add(ms, Ordering::SeqCst);
    }

    fn sleep_in_next_park(&self, by: Duration) {
        let ms = u64::try_from(by.as_millis()).unwrap();
        self.nap_ms.store(ms, Ordering::SeqCst);
    }
}

struct FakeClock(Time);

impl Clock for FakeClock {
    fn wall(&self) -> SystemTime {
        self.0.wall()
    }

    fn mono(&self) -> Duration {
        Duration::from_millis(self.0.mono_ms.load(Ordering::SeqCst))
    }

    fn park(&mut self, watch: &mut HeadWatch, slice: Duration) {
        self.0
            .last_park_ms
            .store(u64::try_from(slice.as_millis()).unwrap(), Ordering::SeqCst);
        self.0.parks.fetch_add(1, Ordering::SeqCst);
        await_round(watch);
        self.0.advance(slice);
        let nap = self.0.nap_ms.swap(0, Ordering::SeqCst);
        self.0.wall_ms.fetch_add(nap, Ordering::SeqCst);
    }
}

fn await_round(watch: &mut HeadWatch) {
    let backstop = Instant::now() + HANG;
    while watch.has_pending()
        && watch.ready_notices.is_empty()
        && watch.deferred_moved.is_empty()
        && Instant::now() < backstop
    {
        watch.park_for_hint(backstop.saturating_duration_since(Instant::now()));
    }
}

/// Wait, without stepping, until no index probe is in flight unfinished.
/// A fake park charges a whole slice and does not wait for the probe's
/// worker, which runs on real time, so each step taken while it runs moves
/// the fake clock by time production would not spend: its park wakes on
/// the probe's hint and its completion.
fn settle_index_probe(runner: &Runner) {
    let deadline = Instant::now() + HANG;
    while runner
        .index
        .pending
        .as_ref()
        .is_some_and(|pending| !pending.done.load(Ordering::Acquire))
    {
        assert!(Instant::now() < deadline, "the index probe completed");
        std::thread::sleep(Duration::from_millis(1));
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

fn window_slice(
    window: &mut HeadWatch,
    l: &Layout,
    time: &Time,
    get: &Arc<ConcurrentVendorGetFn<'static>>,
) {
    if window.has_pending() {
        window.park_for_hint(HANG);
    }
    let moved = window.slice(l, time.wall(), Some(SLICE), SLICE, get, false);
    assert!(moved.is_none(), "the head in these scenes has not moved");
}

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
        window_slice(&mut window, &l, &time, &window_get);
        let session_before = vendors.count("session");
        for _ in 0..(5 * 60 / SLICE.as_secs()) {
            runner.step();
            await_round(&mut runner.watch);
            window_slice(&mut window, &l, &time, &window_get);
        }
        if !hosted {
            assert!(
                vendors.count("session") > session_before,
                "negative control: a watch no host runs goes on asking beside the window"
            );
            let _ = std::fs::remove_dir_all(&l.prefix);
            continue;
        }
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

        drop(window);
        let closed = time.wall();
        while vendors.count("session") == session_before {
            assert!(
                time.wall() <= closed + CADENCE + SLICE * 2,
                "the session took the watch back within a minute"
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
    window_slice(&mut window, &l, &time, &window_get);
    assert_eq!(
        window.pending_count(),
        0,
        "refused while the round holds the watch"
    );
    assert!(
        !window.due(time.wall()),
        "and not asked again at every slice"
    );
    assert!(window.due(time.wall() + super::super::BUSY_RETRY));
    vendors.release();
    runner.step();
    assert_eq!(runner.watch.pending_count(), 0, "the round was harvested");
    time.advance(super::super::BUSY_RETRY);
    window_slice(&mut window, &l, &time, &window_get);
    assert_eq!(watcher(&l), Watcher::Window);
    let asked = vendors.count("session");
    for _ in 0..(3 * 60 / SLICE.as_secs()) {
        window_slice(&mut window, &l, &time, &window_get);
        runner.step();
    }
    assert_eq!(vendors.count("session"), asked, "the session stands by");
    assert!(vendors.count("window") >= 2);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn one_session_watches_and_the_next_takes_the_seat_when_it_goes() {
    let l = layout("two-sessions");
    install(&l, "claude", "2.1.280");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut first = session(&l, &time, &vendors, "first", &passes, &on());
    let mut second = session(&l, &time, &vendors, "second", &passes, &on());
    first.step();
    second.step();
    assert_eq!(vendors.count("second"), 0, "the other stood by");
    let start = time.wall();
    while time.wall() < start + Duration::from_secs(5 * 60) {
        first.step();
    }
    assert!(
        (5..=6).contains(&vendors.count("first")),
        "the seat holder asked each minute: {} GETs over {:?}",
        vendors.count("first"),
        time.wall().duration_since(start).unwrap()
    );
    second.step();
    assert_eq!(vendors.count("second"), 0, "the other stood by");
    assert_eq!(second.admit_after - second.clock.mono(), CADENCE);
    assert!(
        second.watch.session_admission_attempts() <= 6,
        "a second tab tries the seat at most once a minute, not every slice: {}",
        second.watch.session_admission_attempts()
    );
    let ending = Instant::now();
    while first
        .watch
        .pending
        .iter()
        .flatten()
        .any(|head| !head.handle.is_finished())
    {
        assert!(
            ending.elapsed() < HANG,
            "the first session's GET never ended"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(first);
    let gone = time.wall();
    while vendors.count("second") == 0 {
        assert!(
            time.wall() <= gone + CADENCE + SLICE * 2,
            "the next session took the seat within a minute: seat {:?}, {} round(s) \
             pending, {} admission(s) tried, GETs {:?}",
            watcher(&l),
            second.watch.pending_count(),
            second.watch.session_admission_attempts(),
            vendors.gets.lock().unwrap()
        );
        std::thread::current().unpark();
        second.step();
    }
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_denied_idle_session_parks_to_admission_and_reads_the_gate_first() {
    let l = layout("denied-idle-park");
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
    second.step();
    assert_eq!(second.admit_after - second.clock.mono(), CADENCE);
    let reads_before = gate_reads.load(Ordering::SeqCst);
    let parks_before = time.parks.load(Ordering::SeqCst);
    gate.store(false, Ordering::SeqCst);
    second.step();
    assert_eq!(time.last_park_ms.load(Ordering::SeqCst), 60_000);
    assert_eq!(time.parks.load(Ordering::SeqCst), parks_before + 1);
    assert_eq!(gate_reads.load(Ordering::SeqCst), reads_before + 1);
    assert!(!second.on);
    assert_eq!(vendors.count("second"), 0);
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));

    gate.store(true, Ordering::SeqCst);
    second.step();
    assert_eq!(time.last_park_ms.load(Ordering::SeqCst), 5_000);
    assert!(second.on);
    assert_eq!(vendors.count("second"), 0);
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
    for _ in 0..(10 * 60 / SLICE.as_secs()) {
        runner.step();
    }
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
    for _ in 0..(2 * 3600 / SLICE.as_secs()) {
        runner.step();
    }
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
    time.sleep_in_next_park(Duration::from_secs(3600));
    runner.step();
    let woke = time.wall();
    assert_eq!(
        vendors.count("session"),
        asked,
        "not in the slice it woke in"
    );
    while vendors.count("session") == asked {
        runner.step();
    }
    assert!(time.wall() >= woke + AFTER_WAKE, "after the grace");
    assert!(time.wall() <= woke + AFTER_WAKE + SLICE * 2);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

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
    gate.store(false, Ordering::SeqCst);
    let asked = vendors.count("session");
    runner.step();
    assert!(
        time.last_park_ms.load(Ordering::SeqCst) <= 5_000,
        "a seated session still observes a live config edit within one slice"
    );
    for _ in 1..(5 * 60 / SLICE.as_secs()) {
        runner.step();
    }
    assert_eq!(vendors.count("session"), asked);
    assert_eq!(watcher(&l), Watcher::Nobody);
    gate.store(true, Ordering::SeqCst);
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
fn stop_unparks_a_denied_minute_wait() {
    struct NotifyingClock {
        origin: Instant,
        parked: std::sync::mpsc::Sender<Duration>,
    }

    impl Clock for NotifyingClock {
        fn wall(&self) -> SystemTime {
            SystemTime::now()
        }

        fn mono(&self) -> Duration {
            self.origin.elapsed()
        }

        fn park(&mut self, watch: &mut HeadWatch, wait: Duration) {
            if wait >= CADENCE - SLICE {
                self.parked.send(wait).unwrap();
                watch.park_for_hint(10 * HANG);
                return;
            }
            watch.park_for_hint(wait);
        }
    }

    let l = layout("stop-denied-park");
    let (parked_tx, parked_rx) = std::sync::mpsc::channel();
    let get: Arc<ConcurrentVendorGetFn<'static>> =
        Arc::new(|_, _, _, _| panic!("a denied session must not ask a vendor"));
    let mut runner = Runner::new(
        l.clone(),
        (
            HeadWatch::hosted(&[String::from("codex")], Host::Session),
            get,
        ),
        Box::new(NotifyingClock {
            origin: Instant::now(),
            parked: parked_tx,
        }),
        Box::new(|| Gate {
            on: true,
            exclude: vec![String::from("codex")],
        }),
        Box::new(Passes::default()),
        Box::new(|_: &str| {}),
    );
    runner.admit_after = CADENCE;
    let handle = runner.spawn().unwrap();
    assert!(
        parked_rx.recv_timeout(HANG).unwrap() >= CADENCE - SLICE,
        "the denied runner entered its long park"
    );
    let (stopped_tx, stopped_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        handle.stop();
        stopped_tx.send(()).unwrap();
    });
    stopped_rx
        .recv_timeout(HANG)
        .expect("the stop ended the long park");
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

        fn park(&mut self, watch: &mut HeadWatch, slice: Duration) {
            watch.park_for_hint(slice.min(Duration::from_millis(50)));
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
    let mut window = HostClaim::new(Host::Window, &l);
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
    let mut steps = 0;
    while passes.launched().is_empty() {
        runner.step();
        steps += 1;
        assert!(
            steps
                <= 2 + usize::try_from(crate::quiet::RECHECK.as_secs() / SLICE.as_secs()).unwrap()
                    + 2,
            "two looks a minute apart, then the pass: {runner:?}"
        );
    }
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
    window_slice(&mut window, &l, &time, &window_get);
    let mut runner =
        session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
    for _ in 0..(5 * 60 / SLICE.as_secs()) {
        runner.step();
        window_slice(&mut window, &l, &time, &window_get);
    }
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
    let deadline = Instant::now() + HANG;
    while !first
        .index
        .pending
        .as_ref()
        .expect("the far HEAD is still owned")
        .done
        .load(Ordering::Acquire)
    {
        assert!(Instant::now() < deadline, "the far HEAD completed");
        std::thread::sleep(Duration::from_millis(1));
    }
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
fn an_empty_index_source_becomes_probe_eligible_on_the_next_slice() {
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

    time.advance(SLICE / 2);
    assert!(!runner.check_index(time.wall()));
    assert_eq!(
        checks.load(Ordering::SeqCst),
        1,
        "local look is slice-paced"
    );
    time.advance(SLICE / 2);
    assert!(model.fire("Tick", &mut state));
    assert!(!runner.check_index(time.wall()));
    assert!(model.fire("EmptyLook", &mut state));
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
    assert_eq!(runner.index.last_completed, None);

    eligible.store(true, Ordering::SeqCst);
    assert!(model.fire("Enable", &mut state));
    time.advance(SLICE / 2);
    assert!(!runner.check_index(time.wall()));
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    assert!(runner.index.pending.is_none());
    time.advance(SLICE / 2);
    assert!(model.fire("Tick", &mut state));
    assert!(!runner.check_index(time.wall()));
    assert!(model.fire("FirstEligibleLook", &mut state));
    assert!(
        runner.index.pending.is_some(),
        "the HEAD worker starts at the next slice"
    );
    started_rx.recv_timeout(HANG).unwrap();
    assert_eq!(checks.load(Ordering::SeqCst), 3);
    assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
    assert_eq!(runner.index.last_ineligible, None);

    let deadline = Instant::now() + HANG;
    while runner.index.pending.is_some() {
        runner
            .index
            .harvest(Duration::from_millis(time.mono_ms.load(Ordering::SeqCst)));
        assert!(Instant::now() < deadline, "the injected HEAD completed");
        std::thread::sleep(Duration::from_millis(1));
    }
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
    time.advance(SLICE * 5);
    let now = || Duration::from_millis(time.mono_ms.load(Ordering::SeqCst));
    let probes = Arc::new(AtomicU64::new(0));
    let asked = Arc::clone(&probes);
    let mut index = IndexWatch::new();
    index.probe = Arc::new(move |_, _| {
        if asked.fetch_add(1, Ordering::SeqCst) == 0 {
            crate::index_probe::Probe::Suppressed(SLICE)
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
    index.start(&l, now()).unwrap();
    let deadline = Instant::now() + HANG;
    while index.pending.is_some() {
        index.harvest(now());
        assert!(Instant::now() < deadline, "the suppressed worker completed");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(model.fire("Handoff", &mut state));
    let completed = index.last_completed.expect("worker completion");
    assert_eq!(index.shared_retry_at, Some(completed + SLICE));
    assert!(!index.due(completed + SLICE - Duration::from_millis(1)));
    time.advance(SLICE);
    assert!(model.fire("TickAfterHandoff", &mut state));
    assert!(index.due(completed + SLICE));
    assert!(model.fire("AtExpiry", &mut state));
    index.start(&l, completed + SLICE).unwrap();
    while index.pending.is_some() {
        index.harvest(completed + SLICE);
        assert!(Instant::now() < deadline, "the due worker completed");
        std::thread::sleep(Duration::from_millis(1));
    }
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
    for _ in 0..3 {
        runner.step();
    }
    assert_eq!(probes.load(Ordering::SeqCst), 1);
    while runner.index.pending.is_none() {
        runner.step();
        assert!(
            time.mono_ms.load(Ordering::SeqCst)
                <= (completed + crate::index_probe::INTERVAL + SLICE * 2).as_millis() as u64,
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
    assert!(index.last_attempt.unwrap().holds(43, 5 * minute - SLICE));
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
    assert!(index.last_attempt.unwrap().holds(43, 20 * minute - SLICE));
    assert!(model.fire("Tick", &mut state));
    assert!(model.action_enabled("RetrySecond", &state));
    assert!(!index.last_attempt.unwrap().holds(43, 20 * minute));
    index.note_attempt(43, 20 * minute, true);
    assert!(model.fire("RetrySecond", &mut state));
    assert_eq!(
        state["passes"],
        i64::from(index.last_attempt.unwrap().passes)
    );
    assert!(index.last_attempt.unwrap().holds(43, 80 * minute - SLICE));
    assert!(!index.last_attempt.unwrap().holds(43, 80 * minute));
    assert!(!index.last_attempt.unwrap().holds(44, 20 * minute + SLICE));
    index.note_attempt(44, 20 * minute + SLICE, true);
    assert!(model.fire("NewerHint", &mut state));
    assert_eq!(
        state["passes"],
        i64::from(index.last_attempt.unwrap().passes)
    );
    assert_eq!(index.last_attempt.unwrap().passes, 1);
    index.note_attempt(44, 25 * minute, false);
    assert!(index.last_attempt.unwrap().holds(44, 25 * minute + SLICE));
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
    window_slice(&mut window, &l, &time, &window_get);
    assert_eq!(watcher(&l), Watcher::Window);
    let looked_before_window = looks.load(Ordering::SeqCst);
    for _ in 0..(3 * 60 / SLICE.as_secs()) {
        runner.step();
    }
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
    drop(window);
    runner.step();
    assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
    assert_eq!(
        looks.load(Ordering::SeqCst),
        looked_before_window + 1,
        "the session reads the record on its first slice back"
    );
    for _ in 0..(60 / SLICE.as_secs()) + 2 {
        runner.step();
        if !passes.launched().is_empty() {
            break;
        }
    }
    assert_eq!(passes.launched(), [WHOLE], "the seat resumed promptly");
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
    let looks = 2 + usize::try_from(crate::quiet::RECHECK.as_secs() / SLICE.as_secs()).unwrap() + 2;
    let l = layout("held-no-vendor");
    let time = Time::default();
    let vendors = Vendors::new("2.1.280");
    let passes = Passes::default();
    let mut runner =
        session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due.clone());
    assert_eq!(watcher(&l), Watcher::Nobody, "before the first look");
    for _ in 0..looks {
        runner.step();
    }
    assert_eq!(
        watcher(&l),
        Watcher::Session(Some(std::process::id())),
        "the session sits while the move is still busy"
    );
    assert!(passes.launched().is_empty(), "busy: nothing woken");
    quiet.store(true, Ordering::SeqCst);
    let mut steps = 0;
    while passes.launched().is_empty() {
        runner.step();
        steps += 1;
        assert!(
            steps <= 2 * looks,
            "the next look, a second a minute later, then the pass: {runner:?}"
        );
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
    for _ in 0..(5 * 60 / SLICE.as_secs()) {
        runner.step();
        assert!(window.sit(&l));
    }
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
