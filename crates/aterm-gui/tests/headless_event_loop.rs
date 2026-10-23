// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS display-free event loop (`--headless`), driven in-process on the main thread.

fn main() {
    let cli = match libtest::Cli::parse(std::env::args().skip(1)) {
        Ok(cli) => cli,
        Err(why) => {
            eprintln!("error: {why}");
            std::process::exit(libtest::ERROR_EXIT);
        }
    };
    let exists = cfg!(target_os = "macos");
    if cli.list {
        cli.print_list(exists);
        return;
    }
    let run = libtest::Run::start(&cli, exists);
    if run.runs_the_test() {
        run_the_test();
    }
    run.finish();
}

#[cfg(target_os = "macos")]
fn run_the_test() {
    macos::run();
}

#[cfg(not(target_os = "macos"))]
fn run_the_test() {
    unreachable!("the test exists only on macOS, so nothing selects it here");
}

/// libtest's command line and output, for the one test this binary holds —
/// only as much as a one-test binary needs, and an ERROR for the rest.
mod libtest {
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    /// The one test, by the name libtest would list it under.
    pub const NAME: &str = "headless_event_loop";
    /// libtest's exit code for a failed run and for a command line it refused.
    pub const ERROR_EXIT: i32 = 101;

    pub struct Cli {
        pub list: bool,
        terse: bool,
        exact: bool,
        ignored_only: bool,
        bench: bool,
        test: bool,
        filters: Vec<String>,
        skips: Vec<String>,
    }

    impl Cli {
        pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
            let mut cli = Self {
                list: false,
                terse: false,
                exact: false,
                ignored_only: false,
                bench: false,
                test: false,
                filters: Vec::new(),
                skips: Vec::new(),
            };
            let mut args = args.into_iter();
            while let Some(arg) = args.next() {
                let (flag, inline) = match arg.split_once('=') {
                    Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
                    _ => (arg.as_str(), None),
                };
                let mut value = || -> Result<String, String> {
                    match inline {
                        Some(v) => Ok(v.to_string()),
                        None => args
                            .next()
                            .ok_or_else(|| format!("option `{flag}` needs a value")),
                    }
                };
                match flag {
                    "--list" => cli.list = true,
                    "--exact" => cli.exact = true,
                    "--ignored" => cli.ignored_only = true,
                    "--bench" => cli.bench = true,
                    "--test" => cli.test = true,
                    "-q" | "--quiet" => cli.terse = true,
                    "--skip" => cli.skips.push(value()?),
                    "--format" => match value()?.as_str() {
                        "pretty" => {}
                        "terse" => cli.terse = true,
                        other => {
                            return Err(format!(
                                "`--format {other}` is not spoken by {NAME} (a harness = false \
                                 binary); pretty and terse are"
                            ));
                        }
                    },
                    "--include-ignored"
                    | "--nocapture"
                    | "--no-capture"
                    | "--show-output"
                    | "--report-time"
                    | "--ensure-time"
                    | "--shuffle"
                    | "--force-run-in-process"
                    | "--exclude-should-panic" => {}
                    "--test-threads" | "--color" | "--shuffle-seed" | "-Z" => {
                        value()?;
                    }
                    _ if flag.starts_with("-Z") => {}
                    _ if flag.starts_with('-') => {
                        return Err(format!(
                            "unrecognized option `{arg}`: {NAME} is a harness = false binary \
                             that speaks the libtest options a one-test binary needs, and \
                             refuses the rest rather than run or skip its test silently"
                        ));
                    }
                    _ => cli.filters.push(arg.clone()),
                }
            }
            Ok(cli)
        }

        fn matches(&self, pattern: &str) -> bool {
            if self.exact {
                NAME == pattern
            } else {
                NAME.contains(pattern)
            }
        }

        /// libtest's `filter_tests`, for the one test.
        fn selects(&self, exists: bool) -> bool {
            exists
                && (self.filters.is_empty() || self.filters.iter().any(|f| self.matches(f)))
                && !self.skips.iter().any(|s| self.matches(s))
                && !self.ignored_only
        }

        pub fn print_list(&self, exists: bool) {
            let n = usize::from(self.selects(exists));
            if n == 1 {
                println!("{NAME}: test");
            }
            if !self.terse {
                if n != 0 {
                    println!();
                }
                println!("{}, {}", plural(n, "test"), plural(0, "benchmark"));
            }
        }
    }

    fn plural(n: usize, what: &str) -> String {
        if n == 1 {
            format!("1 {what}")
        } else {
            format!("{n} {what}s")
        }
    }

    static STARTED: OnceLock<Instant> = OnceLock::new();
    /// Held by the one thread that prints the verdict, until the process ends.
    static VERDICT: Mutex<()> = Mutex::new(());

    fn seconds() -> f64 {
        STARTED.get().map_or(0.0, |t| t.elapsed().as_secs_f64())
    }

    pub struct Run {
        selected: bool,
        /// `--bench` without `--test`: libtest reports a plain test `ignored`.
        force_ignored: bool,
        filtered_out: usize,
    }

    impl Run {
        pub fn start(cli: &Cli, exists: bool) -> Self {
            let _ = STARTED.set(Instant::now());
            let selected = cli.selects(exists);
            let run = Self {
                selected,
                force_ignored: cli.bench && !cli.test,
                filtered_out: usize::from(exists && !selected),
            };
            println!();
            println!("running {}", plural(usize::from(selected), "test"));
            run
        }

        pub fn runs_the_test(&self) -> bool {
            self.selected && !self.force_ignored
        }

        pub fn finish(self) {
            let held = VERDICT.lock();
            let (passed, ignored) = match (self.selected, self.force_ignored) {
                (false, _) => (0, 0),
                (true, true) => {
                    println!("test {NAME} ... ignored");
                    (0, 1)
                }
                (true, false) => {
                    println!("test {NAME} ... ok");
                    (1, 0)
                }
            };
            println!();
            println!(
                "test result: ok. {passed} passed; 0 failed; {ignored} ignored; 0 measured; {} \
                 filtered out; finished in {:.2}s",
                self.filtered_out,
                seconds()
            );
            println!();
            std::mem::forget(held);
        }
    }

    /// The failed verdict, printed once.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn fail(why: &str) -> ! {
        let _held = VERDICT.lock();
        eprintln!("{NAME}: FAIL: {why}");
        println!("test {NAME} ... FAILED");
        println!();
        println!("failures:");
        println!();
        println!("---- {NAME} stdout ----");
        println!("{NAME}: FAIL: {why}");
        println!();
        println!("failures:");
        println!("    {NAME}");
        println!();
        println!(
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in {:.2}s",
            seconds()
        );
        println!();
        std::process::exit(ERROR_EXIT);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::time::{Duration, Instant};

    use aterm_window::application::ApplicationHandler;
    use aterm_window::event::{StartCause, WindowEvent};
    use aterm_window::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
    use aterm_window::platform::headless::EventLoopBuilderExtHeadless as _;
    use aterm_window::window::{Window, WindowId};

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        /// AppKit's `NSApp`: nil until `+[NSApplication sharedApplication]`
        /// runs. Reading it creates nothing.
        static NSApp: *mut std::ffi::c_void;
    }

    fn ns_app_exists() -> bool {
        // SAFETY: a plain read of AppKit's exported pointer-sized global; the volatile read keeps
        // it from being folded across the loop's run.
        unsafe { !std::ptr::read_volatile(&raw const NSApp).is_null() }
    }

    /// Clones of one proxy, each posting once. Eight is enough to have every
    /// clone past the first; the lost-wake class lost ALL of them.
    const CLONES: usize = 8;
    const SEND_GAP: Duration = Duration::from_millis(40);
    /// A hang detector for each thing that must happen (AGENTS.md: a minute,
    /// never a latency budget). A lost wake never arrives, so no bound on how
    /// LATE an event may be is needed to catch one.
    const HANG: Duration = Duration::from_secs(60);
    const DEADLINE: Duration = Duration::from_millis(120);
    /// The whole run's backstop — it also covers the main-queue round trip and
    /// the `WaitUntil` deadline, which no per-clone wait does. Twice [`HANG`], so
    /// a lost clone wake is reported by the clone that lost it, not by this.
    const WATCHDOG: Duration = Duration::from_secs(2 * HANG.as_secs());

    enum Ev {
        Clone(usize, Instant),
        MainQueueAnswered(u32),
    }

    fn fail(why: &str) -> ! {
        super::libtest::fail(why)
    }

    struct Probe {
        proxy: Option<EventLoopProxy<Ev>>,
        /// Each clone's event is acknowledged back to the posting thread, which
        /// sends the next only after the ack: no later wake can rescue an
        /// earlier lost one.
        ack: std::sync::mpsc::Sender<usize>,
        acks: Option<std::sync::mpsc::Receiver<usize>>,
        received: Vec<Option<Duration>>,
        main_queue: Option<u32>,
        deadline_armed: Option<Instant>,
        turns_while_waiting: u32,
        deadline_slept: Option<Duration>,
    }

    impl Probe {
        fn all_delivered(&self) -> bool {
            self.received.iter().all(Option::is_some) && self.main_queue.is_some()
        }
    }

    impl ApplicationHandler<Ev> for Probe {
        fn new_events(&mut self, el: &ActiveEventLoop, cause: StartCause) {
            let Some(armed) = self.deadline_armed else {
                return;
            };
            match cause {
                StartCause::ResumeTimeReached { .. } => {
                    self.deadline_slept = Some(armed.elapsed());
                    el.exit();
                }
                _ => self.turns_while_waiting += 1,
            }
        }

        fn resumed(&mut self, el: &ActiveEventLoop) {
            if ns_app_exists() {
                fail("NSApp exists on the first turn of a display-free loop");
            }
            if el.primary_monitor().is_some() || el.available_monitors().next().is_some() {
                fail("a display-free loop answered a monitor (a CoreGraphics display query)");
            }
            if el.system_theme().is_some() {
                fail("a display-free loop answered a system theme (NSApplication's appearance)");
            }
            if el.create_window(Window::default_attributes()).is_ok() {
                fail("a display-free loop created a window");
            }
            let proxy = self
                .proxy
                .take()
                .expect("the proxy is handed in before run");
            let acks = self
                .acks
                .take()
                .expect("the ack channel is handed in before run");
            let clones: Vec<_> = (0..CLONES).map(|_| proxy.clone()).collect();
            drop(proxy);
            std::thread::spawn(move || {
                for (i, clone) in clones.iter().enumerate() {
                    std::thread::sleep(SEND_GAP);
                    if clone.send_event(Ev::Clone(i, Instant::now())).is_err() {
                        fail("the loop closed while clones were still posting");
                    }
                    match acks.recv_timeout(HANG) {
                        Ok(got) if got == i => {}
                        Ok(got) => fail(&format!("clone {i} was answered by an ack for {got}")),
                        Err(_) => fail(&format!(
                            "clone {i}'s event was not delivered within {}s: its wake did not \
                             wake the loop (a proxy source the run loop does not hold?)",
                            HANG.as_secs()
                        )),
                    }
                }
                let answer = aterm_objc::run_on_main(|_mt| 7_u32);
                let _ = clones[0].send_event(Ev::MainQueueAnswered(answer));
            });
            el.set_control_flow(ControlFlow::Wait);
        }

        fn user_event(&mut self, el: &ActiveEventLoop, event: Ev) {
            match event {
                Ev::Clone(i, sent) => {
                    self.received[i] = Some(sent.elapsed());
                    let _ = self.ack.send(i);
                }
                Ev::MainQueueAnswered(answer) => self.main_queue = Some(answer),
            }
            if self.all_delivered() && self.deadline_armed.is_none() {
                let now = Instant::now();
                self.deadline_armed = Some(now);
                el.set_control_flow(ControlFlow::WaitUntil(now + DEADLINE));
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {
            fail("a display-free loop delivered a window event");
        }
    }

    pub(super) fn run() {
        if ns_app_exists() {
            fail("NSApp exists before any event loop was built");
        }
        std::thread::spawn(|| {
            std::thread::sleep(WATCHDOG);
            fail(&format!(
                "the loop did not finish within {}s: a proxy wake was lost, the main queue was \
                 not drained, or a WaitUntil deadline never fired",
                WATCHDOG.as_secs()
            ));
        });

        #[expect(
            clippy::disallowed_methods,
            reason = "test code: a headless loop, which takes no front"
        )]
        let mut builder = EventLoop::<Ev>::with_user_event();
        builder.with_headless();
        let event_loop = builder
            .build()
            .unwrap_or_else(|e| fail(&format!("the display-free loop did not build: {e}")));
        if ns_app_exists() {
            fail("building the display-free loop created NSApplication");
        }
        let (ack, acks) = std::sync::mpsc::channel();
        let mut probe = Probe {
            proxy: Some(event_loop.create_proxy()),
            ack,
            acks: Some(acks),
            received: vec![None; CLONES],
            main_queue: None,
            deadline_armed: None,
            turns_while_waiting: 0,
            deadline_slept: None,
        };
        event_loop
            .run_app(&mut probe)
            .unwrap_or_else(|e| fail(&format!("run_app: {e}")));

        if ns_app_exists() {
            fail("running the display-free loop created NSApplication");
        }
        if let Some(i) = probe.received.iter().position(Option::is_none) {
            fail(&format!("clone {i}'s event never arrived"));
        }
        if probe.main_queue != Some(7) {
            fail("the main-queue round trip did not answer");
        }
        let Some(slept) = probe.deadline_slept else {
            fail("the WaitUntil deadline never reported ResumeTimeReached")
        };
        if slept < DEADLINE {
            fail(&format!(
                "ResumeTimeReached after {slept:?}, before its {DEADLINE:?} deadline"
            ));
        }
        if probe.turns_while_waiting > 2 {
            fail(&format!(
                "{} turns ran while a {DEADLINE:?} deadline was pending: that is a spin",
                probe.turns_while_waiting
            ));
        }
        let worst = probe
            .received
            .iter()
            .flatten()
            .max()
            .copied()
            .unwrap_or_default();
        println!(
            "headless_event_loop: ok — {CLONES} proxy clones delivered (worst {worst:?}), the main \
             queue drained, a {DEADLINE:?} WaitUntil slept {slept:?}, and NSApp stayed nil"
        );
    }
}
