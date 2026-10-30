// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the Muse Code live upgrade's quiet flow over a stand-in instance and scripted kernel.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::super::live::{self, Move, claim_record, upgrade_visit};
use super::*;
use crate::harness::relaunch::ended_by_restart;
use crate::harness::upgrade::Request;
use crate::harness::upgrade_drive::build_start::{NamedProbe, PROBE, Probe};
use crate::harness::upgrade_drive::{Ask, ask_for};
use crate::harness::upgrade_drive::{Due, Job, Kernel, LiveTab};
use crate::harness::upgrade_muse_live::{self as pure, Look, Open};

/// The newer build atpkg lands: 1.4.11-R6500.1 at its build id.
const NEW_BUILD: &str = "2001004011006500002";
const NEW_VERSION: &str = "1.4.11-R6500.1";
/// An older revision of the running build's `x.y.z`.
const OLD_BUILD: &str = "2001004010006400002";
const OLD_VERSION: &str = "1.4.10-R6400.1";
/// When the Muse started (unix ms) and its log's route record after it.
const START_MS: u64 = 1_792_284_625_000;
const ROUTE_US: u64 = START_MS * 1000 + 15_816;
/// A child of the Muse (a backgrounded shell's `sh`).
const CHILD: u32 = 4_100_009;

/// Which build the twin execs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Twin {
    Newer,
    Same,
    Older,
}

/// What a test varies, each default as an idle Muse behind a newer build.
#[derive(Clone, Debug, Default)]
struct Knobs {
    /// The screen before the signal: `None` idle at a turn's end.
    screen: Option<&'static str>,
    /// The tab is held (`hold=1`).
    held: bool,
    /// Held from the Nth `status` read on (the last look's).
    held_from: Option<usize>,
    /// The session log's records after the route record (default: one
    /// finished turn).
    log: Option<String>,
    /// No route record of the Muse in its log.
    unrouted: bool,
    /// A subagent of the session: its log's text, or `Err(())` for one that
    /// cannot be read (a directory where its log should be).
    subagent: Option<Result<String, ()>>,
    /// A child process under the Muse.
    child: bool,
    /// The process table cannot be read.
    table_unread: bool,
    /// The build's start probe's answer (default: it starts).
    probe: Option<Probe>,
    /// The twin's build (default newer).
    twin: Option<Twin>,
    /// The signal is refused by the server.
    signal_refused: bool,
    /// The launch's argv beyond the program (default `--base-url <stub>
    /// --trust-workspace`).
    argv: Option<Vec<String>>,
    /// The shell takes its terminal back only once this is set.
    shell_slow: Option<Arc<AtomicBool>>,
    /// The relaunched Muse never names itself in the session's lock.
    lockless: bool,
    /// The step is a background break's.
    background: bool,
    /// The host's step (hand the relaunch back to the loop).
    hand_back: bool,
    /// The session's lock names another process from the hand's take on
    /// (between the first look and the last).
    lock_moves: bool,
}

/// The scripted kernel: [`TUI`] runs under [`SHELL`] until the signal; its
/// relaunch [`NEW`] appears when the line is typed.
struct Kernel2 {
    exited: Arc<AtomicBool>,
    relaunched: Arc<AtomicBool>,
    lock: PathBuf,
    data: PathBuf,
    exe: PathBuf,
    new_exe: PathBuf,
    argv: Vec<String>,
    knobs: Knobs,
}

impl Kernel for Kernel2 {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        Some((Job::Foreground, SHELL))
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        matches!(pid, NEW | TUI).then_some(SHELL)
    }

    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        Some((1, "launchd".to_string()))
    }

    fn process_start(&self, pid: u32) -> Option<String> {
        match pid {
            TUI => Some("tui-birth".to_string()),
            NEW => Some("new-birth".to_string()),
            _ => None,
        }
    }
}

impl MuseKernel for Kernel2 {
    fn alive(&self, pid: u32) -> bool {
        match pid {
            TUI => !self.exited.load(Ordering::SeqCst),
            NEW => self.relaunched.load(Ordering::SeqCst),
            _ => pid == SHELL,
        }
    }

    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        match pid {
            SHELL => Some(atpkg::caller_shell::ProcArgs {
                exec_path: "/bin/zsh".to_string(),
                argv: vec!["-zsh".to_string()],
                env: Vec::new(),
            }),
            TUI => Some(atpkg::caller_shell::ProcArgs {
                exec_path: self.exe.to_string_lossy().into_owned(),
                argv: self.argv.clone(),
                env: vec![format!("XDG_DATA_HOME={}", self.data.display())],
            }),
            _ => None,
        }
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        match pid {
            TUI => Some(self.exe.clone()),
            NEW => Some(self.new_exe.clone()),
            _ => None,
        }
    }

    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>> {
        Some(if pid == TUI && !self.exited.load(Ordering::SeqCst) {
            vec![self.lock.clone()]
        } else {
            Vec::new()
        })
    }

    fn holders_of(&self, file: &Path) -> Option<Vec<u32>> {
        if file != self.lock {
            return Some(Vec::new());
        }
        Some(if self.relaunched.load(Ordering::SeqCst) {
            vec![NEW]
        } else if self.exited.load(Ordering::SeqCst) {
            Vec::new()
        } else {
            vec![TUI]
        })
    }

    fn shell_has_terminal(&self, _: u32) -> bool {
        self.exited.load(Ordering::SeqCst)
            && self
                .knobs
                .shell_slow
                .as_ref()
                .is_none_or(|ready| ready.load(Ordering::SeqCst))
    }

    fn muse_children(&self, _: u32) -> Vec<(u32, Vec<String>)> {
        if self.relaunched.load(Ordering::SeqCst) {
            vec![(
                NEW,
                vec![
                    self.new_exe.to_string_lossy().into_owned(),
                    "--base-url".to_string(),
                    "http://127.0.0.1:9".to_string(),
                    "--trust-workspace".to_string(),
                    "resume".to_string(),
                    S1.to_string(),
                ],
            )]
        } else {
            Vec::new()
        }
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        Some(if pid == SHELL { SHELL_CWD } else { WORKSPACE }.to_string())
    }

    fn in_tab(&self, _: &mut Client, pid: u32, _: &str) -> bool {
        MuseKernel::alive(self, pid)
    }

    fn descendants(&self, pid: u32) -> Option<Vec<(u32, String)>> {
        if self.knobs.table_unread {
            return None;
        }
        let table = if self.knobs.child && pid == TUI && MuseKernel::alive(self, TUI) {
            vec![(TUI, SHELL, "muse".into()), (CHILD, TUI, "sleep".into())]
        } else {
            vec![(TUI, SHELL, "muse".into())]
        };
        descendants_in(&table, pid)
    }

    fn started_ms(&self, pid: u32) -> Option<u64> {
        (pid == TUI).then_some(START_MS)
    }
}

/// One record line as Muse writes it.
fn rec(payload_type: &str, payload: &str, at_us: u64) -> String {
    format!(
        r#"{{"schema_version":1,"stream":{{"kind":"session","id":"s"}},"sequence":1,"recorded_at":{at_us},"record_type":"event","durability":"durable","payload_type":"{payload_type}","payload_schema_version":1,"payload":{payload}}}"#
    )
}

fn route_line(pid: u32, at_us: u64) -> String {
    rec(
        "runtime.session.route_facts",
        &format!(r#"{{"kind":"route_facts","record":{{"cwd":"{WORKSPACE}","pid":{pid}}}}}"#),
        at_us,
    )
}

fn run_line(id: &str, kind: &str, at_us: u64) -> String {
    rec(
        "runtime.session",
        &format!(r#"{{"kind":"run","run_id":"{id}","event":{{"kind":"{kind}"}}}}"#),
        at_us,
    )
}

fn task_line(id: &str, event: &str, at_us: u64) -> String {
    rec(
        "runtime.session",
        &format!(r#"{{"kind":"task","run_id":"r","task_id":"{id}","event":{event}}}"#),
        at_us,
    )
}

/// A finished turn of this process.
fn finished_turn(at_us: u64) -> String {
    format!(
        "{}\n{}\n",
        run_line("r1", "started", at_us),
        run_line("r1", "terminal", at_us + 1)
    )
}

/// A backgrounded shell still running (its task open, its last word
/// `background_running`).
fn bg_running(at_us: u64) -> String {
    [
        task_line("t1", r#"{"kind":"accepted"}"#, at_us),
        task_line("t1", r#"{"kind":"started"}"#, at_us + 1),
        task_line(
            "t1",
            r#"{"kind":"status","message":"terminal process running"}"#,
            at_us + 2,
        ),
        task_line(
            "t1",
            r#"{"kind":"output","chunk":"{\"execution_state\":\"background_running\"}"}"#,
            at_us + 3,
        ),
    ]
    .join("\n")
        + "\n"
}

/// Its end queued and not drained.
fn bg_queued(at_us: u64) -> String {
    format!(
        "{}\n{}\n",
        rec(
            "runtime.session",
            r#"{"kind":"run","run_id":"r1","event":{"kind":"inbox_item_queued","source":{"source":"background_task_terminal","task_id":"t1"}}}"#,
            at_us,
        ),
        task_line("t1", r#"{"kind":"completed"}"#, at_us + 1)
    )
}

/// The session log's head: the workspace the session names, its start.
fn log_head() -> String {
    format!(
        "{}\n{}\n",
        rec(
            "runtime.session_start.intake_v3",
            &format!(
                r#"{{"kind":"session_start_intake_v3","payload":{{"session_metadata":{{"workspace_root":"{WORKSPACE}"}}}}}}"#
            ),
            START_MS * 1000 + 1_000,
        ),
        rec(
            "session.started",
            r#"{"kind":"session_started","background_tasks":"keep"}"#,
            START_MS * 1000 + 2_000,
        )
    )
}

/// One idle Muse behind a build: its data home and session, the store's two
/// builds and the twin, the stand-in instance, the kernel.
struct LiveRig {
    dir: ScratchDir,
    opts: Opts,
    kernel: Kernel2,
    asked: crate::harness::FakeInstanceLog,
    lock: PathBuf,
    twin: PathBuf,
    /// The tab's record as it stood on disk when the `signal` came.
    at_signal: Arc<Mutex<Option<String>>>,
}

/// Lays a store build `build` of `version` under `store`: its program and
/// its record.
fn lay(store: &Path, build: &str, version: &str) -> PathBuf {
    let exe = store.join(build).join("bin/muse");
    std::fs::create_dir_all(exe.parent().expect("bin")).expect("store");
    std::fs::write(&exe, "#!/bin/sh\n").expect("build");
    std::fs::write(
        store.join(format!("{build}.vendor")),
        format!("schema = 1\nprogram = \"muse\"\nversion = \"{version}\"\nvendor = \"Meta\"\n"),
    )
    .expect("vendor record");
    exe
}

impl LiveRig {
    #[allow(clippy::too_many_lines)]
    fn new(name: &str, knobs: Knobs) -> LiveRig {
        let dir = scratch(name);
        let base = std::fs::canonicalize(&*dir).expect("canonical scratch");
        let data = base.join("home/.local/share");
        let session = mz::sessions_root(&data).join("2026/10/17").join(S1);
        std::fs::create_dir_all(&session).expect("session dir");
        let lock = session.join(mz::LOCK);
        std::fs::write(&lock, format!("pid={TUI}\nhost=Mac.localdomain\n")).expect("lock");
        let route = if knobs.unrouted {
            route_line(OTHER, ROUTE_US)
        } else {
            route_line(TUI, ROUTE_US)
        };
        let records = knobs
            .log
            .clone()
            .unwrap_or_else(|| finished_turn(ROUTE_US + 100));
        std::fs::write(
            session.join(mz::LOG),
            format!("{}{route}\n{records}", log_head()),
        )
        .expect("log");
        if let Some(sub) = &knobs.subagent {
            let sub_dir = session.join("subagent").join(S2);
            std::fs::create_dir_all(&sub_dir).expect("subagent dir");
            match sub {
                Ok(text) => std::fs::write(sub_dir.join(mz::LOG), text).expect("subagent log"),
                Err(()) => std::fs::create_dir_all(sub_dir.join(mz::LOG)).expect("unreadable"),
            }
        }
        let store = base.join("prefix/store/muse");
        let exe = lay(&store, BUILD, VERSION);
        let new_exe = lay(&store, NEW_BUILD, NEW_VERSION);
        let old_exe = lay(&store, OLD_BUILD, OLD_VERSION);
        let twin = base.join("prefix/agents/muse");
        std::fs::create_dir_all(twin.parent().expect("agents")).expect("agents");
        let target = match knobs.twin.unwrap_or(Twin::Newer) {
            Twin::Newer => &new_exe,
            Twin::Same => &exe,
            Twin::Older => &old_exe,
        };
        std::fs::write(
            &twin,
            format!(
                "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\nexec '{}' \"$@\"\n",
                target.display()
            ),
        )
        .expect("twin");
        let exited = Arc::new(AtomicBool::new(false));
        let relaunched = Arc::new(AtomicBool::new(false));
        let at_signal = Arc::new(Mutex::new(None));
        let state = base.join("state");
        let before = knobs.screen.map_or_else(muse_idle, |fixture| {
            let rows = fixture_rows(fixture);
            let caret = rows.iter().rposition(|r| r.starts_with('❯')).unwrap_or(0);
            let col = rows.get(caret).map_or(2, |r| r.chars().count());
            screen_json(&rows, (caret, col), 10)
        });
        let statuses = std::sync::atomic::AtomicUsize::new(0);
        let (sock, asked) = {
            let (exited, relaunched, at_signal) = (
                Arc::clone(&exited),
                Arc::clone(&relaunched),
                Arc::clone(&at_signal),
            );
            let lock = lock.clone();
            let record = state.join("upgrade").join(format!("muse-{TAB}.json"));
            let knobs = knobs.clone();
            crate::harness::fake_instance(&base.join("t.sock"), TOKEN, move |line| {
                let verb = line
                    .split_whitespace()
                    .find(|w| !w.starts_with('@'))
                    .unwrap_or("");
                let gone = exited.load(Ordering::SeqCst);
                let back = relaunched.load(Ordering::SeqCst);
                match verb {
                    "sessions" => format!("OK 1\nlocal {TAB} 0 idle muse"),
                    "text" => format!(
                        "OK {}",
                        if back {
                            muse_idle()
                        } else if gone {
                            shell_after()
                        } else {
                            before.clone()
                        }
                    ),
                    "cell" => "OK ❯ ccd3db 1e1e1e none".to_string(),
                    "status" => {
                        let n = statuses.fetch_add(1, Ordering::SeqCst) + 1;
                        let hold = knobs.held || knobs.held_from.is_some_and(|from| n >= from);
                        format!(
                            "OK schema=1 hold={} hand=- integration=on",
                            u8::from(hold && !gone)
                        )
                    }
                    "signal" => {
                        if knobs.signal_refused {
                            return "ERR halted reason=hold".to_string();
                        }
                        *at_signal.lock().expect("at signal") =
                            std::fs::read_to_string(&record).ok();
                        exited.store(true, Ordering::SeqCst);
                        format!("OK signalled pid {TUI}")
                    }
                    "help" if line.contains("help key") => {
                        "OK 2\nkey  key [if-gen=<epoch>.<seq>] [if=<re>] <name>\n     FENCES"
                            .to_string()
                    }
                    "help" => "OK 1\nturn [if-gen=<epoch>.<seq>] <text>".to_string(),
                    "blocks" => format!(
                        "OK {}",
                        if back {
                            line_blocks(None)
                        } else {
                            blocks(143, gone, false)
                        }
                    ),
                    "blocktext" => "OK 0".to_string(),
                    "line" => "OK % ".to_string(),
                    "lease" => {
                        if knobs.lock_moves {
                            let _ = std::fs::write(&lock, format!("pid={OTHER}\nhost=Mac\n"));
                        }
                        "ERR unscripted".to_string()
                    }
                    "turn" => {
                        if line.contains("'resume'") {
                            relaunched.store(true, Ordering::SeqCst);
                            if !knobs.lockless {
                                let _ = std::fs::write(&lock, format!("pid={NEW}\nhost=Mac\n"));
                            }
                        }
                        "OK 0 turn submitted=1 status=settled seq=1 id=1".to_string()
                    }
                    _ => "ERR unscripted".to_string(),
                }
            })
        };
        let mut argv = vec![exe.to_string_lossy().into_owned()];
        argv.extend(knobs.argv.clone().unwrap_or_else(|| {
            vec![
                "--base-url".to_string(),
                "http://127.0.0.1:9".to_string(),
                "--trust-workspace".to_string(),
            ]
        }));
        let opts = Opts {
            home: base.join("home"),
            state,
            sock: Some(sock),
            token: None,
            only_sid: Some(TAB.to_string()),
            dry_run: false,
            human_grace_s: 120,
            hand_back: knobs.hand_back,
            background: knobs.background,
            aterm_state: None,
            ask: true,
            several_logins: false,
            usage_credits: false,
            usage_tap: None,
            instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
            agents_off: Vec::new(),
            land: false,
            tell: crate::harness::upgrade_drive::Tell::Off,
            press: None,
            timings: crate::harness::Timings::FAST,
        };
        LiveRig {
            kernel: Kernel2 {
                exited,
                relaunched,
                lock: lock.clone(),
                data,
                exe,
                new_exe,
                argv,
                knobs,
            },
            asked,
            lock,
            twin,
            at_signal,
            opts,
            dir,
        }
    }

    fn tabs() -> Vec<LiveTab> {
        vec![LiveTab {
            sid: TAB.to_string(),
            fgpgid: Some(i64::from(TUI)),
        }]
    }

    /// One quiet-flow step, the build's probe answering as the knobs say.
    fn visit(&self) -> Report {
        let answer = self.kernel.knobs.probe.unwrap_or(Probe::Starts);
        let named: NamedProbe = std::rc::Rc::new(move |_| answer);
        PROBE.with(|p| *p.borrow_mut() = Some(named));
        let r = upgrade_visit(
            &self.opts,
            &Self::tabs(),
            Some(self.twin.clone()),
            &self.kernel,
        )
        .expect("a Muse Code leads the tab");
        PROBE.with(|p| *p.borrow_mut() = None);
        r
    }

    /// The requests of verb `verb` to the tab.
    fn asked(&self, verb: &str) -> Vec<String> {
        let head = format!("@{TAB} {verb} ");
        self.asked
            .lock()
            .map(|a| a.iter().filter(|l| l.starts_with(&head)).cloned().collect())
            .unwrap_or_default()
    }

    /// The lane's own carry of the tab's record in flight (what the host's
    /// exit watch and its idle point reach, `muse_carry_in_flight`), over
    /// this rig's kernel, as a loop's carry takes it (`hand_back` off).
    fn carry(&self) -> Report {
        let st = self.record().expect("a record in flight");
        let r = Report {
            pid: st.pid,
            tab: TAB.to_string(),
            session: key(TAB),
            from: st.from.clone(),
            to: st.to.clone(),
            step: String::new(),
        };
        let opts = Opts {
            hand_back: false,
            ..self.opts.clone()
        };
        carry_in_flight_with(&opts, r, st, &key(TAB), &self.kernel)
    }

    fn record(&self) -> Option<St> {
        load(&self.opts, &key(TAB))
    }

    /// The actual claim/failure producers' record of an earlier refused signal: the same
    /// conversation still runs the old build. No real process is signalled to prepare it.
    fn failed_claim(&self) -> St {
        let snap = self.snap_of(S1);
        let mv = live::behind(&self.kernel.exe, Some(self.twin.clone()))
            .expect("validated records")
            .expect("newer build");
        let mut st = claim_record(&snap, &mv, self.opts.human_grace_s, now_s()).expect("claim");
        st.fail("signal-refused", now_s());
        st
    }

    fn store(&self, st: &St) {
        crate::harness::upgrade_drive::save_checked(&self.opts, &key(TAB), st)
            .expect("persisted record");
    }
}

impl Drop for LiveRig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&*self.dir);
    }
}

#[test]
fn a_persisted_muse_revision_skip_or_defer_holds_the_real_quiet_worker() {
    for (ask, step) in [
        (Ask::Skip, "wait:skipped"),
        (Ask::Defer(3600), "wait:deferred"),
    ] {
        let rig = LiveRig::new("owner-word", Knobs::default());
        rig.store(&rig.failed_claim());
        let row = ask_for(&rig.opts, TAB, ask, NEW_VERSION, &key(TAB)).expect("the public request");
        assert!(row.word_in_force(now_s()));
        let stored = rig.record().expect("the request reloaded");
        assert_eq!(
            stored.request, row.request,
            "the revision word survives JSON decoding"
        );
        assert_eq!(stored.request_tab, TAB);
        let report = rig.visit();
        assert_eq!(report.step, step, "{report:?}");
        assert!(rig.asked("signal").is_empty() && rig.asked("turn").is_empty());
        assert_eq!(
            rig.record(),
            Some(stored),
            "no fresh claim overwrote the word"
        );
    }
    let rig = LiveRig::new("legacy-word", Knobs::default());
    let mut st = rig.failed_claim();
    st.request = Request::Skip(format!("v{NEW_VERSION}"));
    assert!(st.request_tab.is_empty(), "legacy tab scope");
    rig.store(&st);
    assert_eq!(rig.visit().step, "wait:skipped");
    assert!(rig.asked("signal").is_empty());
}

#[test]
fn stale_foreign_or_invalid_muse_words_cannot_hold_another_move() {
    type Change = fn(&mut St);
    let cases: [(&str, Change); 11] = [
        ("other-agent", |s| s.agent = Agent::Agy),
        ("other-tab", |s| s.tab = "s-foreign".into()),
        ("other-request-tab", |s| s.request_tab = "s-foreign".into()),
        ("other-thread", |s| s.thread = S2.into()),
        ("other-source", |s| s.source = "native".into()),
        ("other-running", |s| s.from = OLD_VERSION.into()),
        ("other-target", |s| {
            s.to = OLD_VERSION.into();
            s.request = Request::Skip(OLD_VERSION.into());
        }),
        ("other-skip", |s| {
            s.request = Request::Skip(OLD_VERSION.into())
        }),
        ("invalid-equal", |s| {
            s.to = "latest".into();
            s.request = Request::Skip("latest".into());
        }),
        ("expired-defer", |s| s.request = Request::DeferUntil(1)),
        ("stale-defer", |s| {
            s.to = OLD_VERSION.into();
            s.request = Request::DeferUntil(u64::MAX);
        }),
    ];
    for (name, change) in cases {
        let rig = LiveRig::new(name, Knobs::default());
        rig.store(&rig.failed_claim());
        ask_for(&rig.opts, TAB, Ask::Skip, NEW_VERSION, &key(TAB)).expect("request producer");
        let mut st = rig.record().expect("stored request");
        change(&mut st);
        rig.store(&st);
        let report = rig.visit();
        assert_eq!(report.step, "done:resumed", "{name}: {report:?}");
        assert_eq!(rig.asked("signal").len(), 1, "{name}");
        assert_eq!(rig.asked("turn").len(), 1, "{name}");
    }
}

#[test]
fn a_saved_word_does_not_strand_a_muse_restart_already_in_flight() {
    let ready = Arc::new(AtomicBool::new(false));
    let rig = LiveRig::new(
        "inflight-word",
        Knobs {
            shell_slow: Some(Arc::clone(&ready)),
            ..Knobs::default()
        },
    );
    assert_eq!(rig.visit().step, "wait:shell-prompt");
    let mut st = rig.record().expect("actual in-flight claim");
    st.request = Request::DeferUntil(u64::MAX);
    st.request_tab = TAB.into();
    rig.store(&st);
    ready.store(true, Ordering::SeqCst);
    assert_eq!(rig.visit().step, "done:resumed");
    assert_eq!(rig.asked("signal").len(), 1);
    assert_eq!(rig.asked("turn").len(), 1);
}

/// An idle Muse behind a newer build is restarted onto it with its session kept and nothing typed after.
#[test]
fn an_idle_muse_behind_is_restarted_onto_the_new_build_and_nothing_is_typed() {
    let rig = LiveRig::new("idle", Knobs::default());
    let r = rig.visit();
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert_eq!(
        (r.from.as_str(), r.to.as_str()),
        (VERSION, "1.4.11-R6500.1(managed)")
    );
    let signals = rig.asked("signal");
    assert_eq!(
        signals,
        vec![format!("@{TAB} signal term pid={TUI} quiet=120")]
    );
    let claim = rig
        .at_signal
        .lock()
        .expect("at signal")
        .clone()
        .and_then(|text| St::from_json(&text))
        .expect("the claim was on disk when the signal came");
    assert_eq!(
        (
            claim.agent,
            claim.pid,
            claim.tab.as_str(),
            claim.thread.as_str()
        ),
        (Agent::Muse, TUI, TAB, S1)
    );
    assert!(
        matches!(claim.phase, Phase::Exiting { .. }),
        "{:?}",
        claim.phase
    );
    assert!(claim.cause.is_empty() && claim.quiet && claim.in_flight());
    assert_eq!(claim.twin, rig.twin.to_string_lossy());
    assert!(
        ended_by_restart(&claim, Agent::Muse, TAB, TUI, now_s()),
        "its exit is the upgrade's own restart"
    );
    let crash = St {
        cause: crate::harness::relaunch::CAUSE_EXIT.to_string(),
        ..claim.clone()
    };
    assert!(
        !ended_by_restart(&crash, Agent::Muse, TAB, TUI, now_s()),
        "a crash relaunch's record claims nothing"
    );
    let typed = rig.asked("turn");
    assert_eq!(typed.len(), 1, "the relaunch line alone: {typed:#?}");
    let line = &typed[0];
    assert!(
        line.contains(&format!("'{}'", rig.twin.display())),
        "{line}"
    );
    assert!(
        line.contains(&format!(
            "'--base-url' 'http://127.0.0.1:9' '--trust-workspace' 'resume' '{S1}'"
        )),
        "{line}"
    );
    assert!(!line.contains("/store/muse/"), "never a store path: {line}");
    let st = rig.record().expect("the record");
    assert_eq!(st.phase, Phase::Done);
    assert_eq!((st.resumed_pid, st.resumed_on.as_str()), (NEW, NEW_VERSION));
    assert_eq!(
        st.outcome,
        format!(
            "muse upgraded to {NEW_VERSION} (from {VERSION}) · the same session resumed, nothing typed"
        )
    );

    let blocked = LiveRig::new("claim-write-refused", Knobs::default());
    let record =
        crate::harness::upgrade_drive::state_dir(&blocked.opts).join(format!("{}.json", key(TAB)));
    std::fs::create_dir_all(&record).expect("a directory at the claim path");
    let kept = record.join("kept");
    std::fs::write(&kept, b"keep the existing destination").expect("owned marker");
    let refused = blocked.visit();
    assert_eq!(
        std::fs::read(&kept).expect("destination retained"),
        b"keep the existing destination"
    );
    assert!(
        refused.step == "wait:claim-unwritten"
            && blocked.asked("signal").is_empty()
            && blocked.asked("turn").is_empty(),
        "MUSE_CLAIM_MUST_BE_WRITTEN_BEFORE_SIGNAL: {refused:?}"
    );
}

/// The host's step hands the relaunched Muse back to its loop, and its carry types nothing.
#[test]
fn the_hosts_step_adopts_and_its_carry_types_nothing() {
    let rig = LiveRig::new(
        "host",
        Knobs {
            hand_back: true,
            ..Knobs::default()
        },
    );
    let r = rig.visit();
    assert_eq!(r.step, "adopted", "{r:?}");
    assert_eq!(r.pid, NEW);
    assert!(
        crate::harness::relaunch::restarted(&rig.opts, Agent::Muse, Some(TUI)),
        "the old process's exit is the restart's"
    );
    assert!(crate::harness::relaunch::owed_of(
        &rig.opts,
        Some(Agent::Muse)
    ));
    assert!(
        !crate::harness::relaunch::owed_of(&rig.opts, Some(Agent::Claude)),
        "never Claude Code's to carry"
    );
    let carried = rig.carry();
    assert_eq!(carried.step, "done:resumed", "{carried:?}");
    assert_eq!(rig.asked("turn").len(), 1, "the relaunch line alone");
    assert_eq!(rig.record().expect("record").phase, Phase::Done);
}

/// THE EXIT CARRIED BY THE HOST'S EXIT WATCH: a shell slow to take its
/// terminal back leaves the step `wait:shell-prompt` with the claim in
/// flight.
#[test]
fn a_restart_left_in_flight_is_carried_as_the_harnesss_own() {
    let ready = Arc::new(AtomicBool::new(false));
    let rig = LiveRig::new(
        "carried",
        Knobs {
            shell_slow: Some(Arc::clone(&ready)),
            ..Knobs::default()
        },
    );
    let r = rig.visit();
    assert_eq!(r.step, "wait:shell-prompt", "{r:?}");
    assert!(rig.asked("turn").is_empty(), "no line yet");
    assert!(crate::harness::relaunch::restarted(
        &rig.opts,
        Agent::Muse,
        Some(TUI)
    ));
    assert!(
        !crate::harness::relaunch::restarted(&rig.opts, Agent::Muse, Some(NEW)),
        "another process's exit is no restart's"
    );
    ready.store(true, Ordering::SeqCst);
    let carried = rig.carry();
    assert_eq!(carried.step, "done:resumed", "{carried:?}");
    assert_eq!(rig.asked("turn").len(), 1);
}

/// THE QUIET ADOPTION NEEDS THE LOCK: a relaunched Muse that never names
/// itself in the session's `.session.lock` is not proved the session's —
/// `done:unconfirmed`, nothing typed either way.
#[test]
fn a_relaunch_the_lock_never_names_is_unconfirmed() {
    let rig = LiveRig::new(
        "lockless",
        Knobs {
            lockless: true,
            ..Knobs::default()
        },
    );
    let r = rig.visit();
    assert_eq!(r.step, "done:unconfirmed", "{r:?}");
    assert_eq!(rig.asked("turn").len(), 1);
}

/// One knob, the word the step waits on, and nothing signalled or typed:
/// no record is left where none stood.
fn holds(name: &str, knobs: Knobs, want: &str) {
    let rig = LiveRig::new(name, knobs);
    let r = rig.visit();
    assert_eq!(r.step, want, "{name}: {r:?}");
    assert!(rig.asked("signal").is_empty(), "{name}: nothing signalled");
    assert!(rig.asked("turn").is_empty(), "{name}: nothing typed");
    assert!(rig.record().is_none(), "{name}: no claim left");
}

/// A turn, backgrounded shell, queued end, subagent task, unreadable log, or unproved log holds the restart.
#[test]
fn the_sessions_log_holds_running_work() {
    let open_turn = format!(
        "{}{}\n",
        finished_turn(ROUTE_US + 100),
        run_line("r2", "started", ROUTE_US + 500)
    );
    holds(
        "turn",
        Knobs {
            log: Some(open_turn),
            ..Knobs::default()
        },
        "wait:turn-open",
    );
    holds(
        "bg",
        Knobs {
            log: Some(finished_turn(ROUTE_US + 100) + &bg_running(ROUTE_US + 200)),
            ..Knobs::default()
        },
        "wait:background-work",
    );
    holds(
        "queued",
        Knobs {
            log: Some(
                finished_turn(ROUTE_US + 100)
                    + &bg_running(ROUTE_US + 200)
                    + &bg_queued(ROUTE_US + 300),
            ),
            ..Knobs::default()
        },
        "wait:queued-work",
    );
    holds(
        "subagent",
        Knobs {
            subagent: Some(Ok(bg_running(ROUTE_US + 400))),
            ..Knobs::default()
        },
        "wait:background-work",
    );
    holds(
        "subagent-unread",
        Knobs {
            subagent: Some(Err(())),
            ..Knobs::default()
        },
        "wait:log-unreadable",
    );
    holds(
        "unrouted",
        Knobs {
            unrouted: true,
            ..Knobs::default()
        },
        "wait:log-unproved",
    );
}

/// Ended and drained background or subagent work holds nothing.
#[test]
fn work_that_ended_and_was_drained_holds_nothing() {
    let drained = finished_turn(ROUTE_US + 100)
        + &bg_running(ROUTE_US + 200)
        + &bg_queued(ROUTE_US + 300)
        + &format!(
            "{}\n{}\n{}\n",
            run_line("r3", "started", ROUTE_US + 400),
            rec(
                "runtime.session",
                r#"{"kind":"run","run_id":"r3","event":{"kind":"inbox_item_drained","delivery_snapshot":{"source":{"source":"background_task_terminal","task_id":"t1"}}}}"#,
                ROUTE_US + 401,
            ),
            run_line("r3", "terminal", ROUTE_US + 402)
        );
    let rig = LiveRig::new(
        "drained",
        Knobs {
            log: Some(drained),
            subagent: Some(Ok(format!(
                "{}{}\n",
                bg_running(ROUTE_US + 10),
                task_line("t1", r#"{"kind":"failed"}"#, ROUTE_US + 20)
            ))),
            ..Knobs::default()
        },
    );
    assert_eq!(rig.visit().step, "done:resumed");
}

/// A busy screen, background panel, draft, hold, child process, or unreadable process table holds the restart.
#[test]
fn the_screen_the_person_and_any_child_hold_the_restart() {
    for (name, knobs, want) in [
        (
            "busy",
            Knobs {
                screen: Some(aterm_phase::muse::fixtures::BUSY_1_4_10),
                ..Knobs::default()
            },
            "wait:screen-not-idle",
        ),
        (
            "panel",
            Knobs {
                screen: Some(aterm_phase::muse::fixtures::END_OF_TURN_BG_RUNNING),
                ..Knobs::default()
            },
            "wait:screen-not-idle",
        ),
        (
            "draft",
            Knobs {
                screen: Some(aterm_phase::muse::fixtures::DRAFT_1_4_10),
                ..Knobs::default()
            },
            "wait:draft",
        ),
        (
            "held",
            Knobs {
                held: true,
                ..Knobs::default()
            },
            "wait:held",
        ),
        (
            "child",
            Knobs {
                child: true,
                ..Knobs::default()
            },
            "wait:child-process",
        ),
        (
            "table",
            Knobs {
                table_unread: true,
                ..Knobs::default()
            },
            "wait:process-table",
        ),
    ] {
        holds(name, knobs, want);
    }
}

/// A failed probe, a same or older twin, an unknown flag, or a background break holds the restart.
#[test]
fn the_build_the_order_and_the_launch_hold_the_restart() {
    for (name, probe) in [("timeout", Probe::TimedOut), ("exits", Probe::Exited)] {
        holds(
            name,
            Knobs {
                probe: Some(probe),
                ..Knobs::default()
            },
            "wait:build-fails",
        );
    }
    for (name, twin) in [("same", Twin::Same), ("older", Twin::Older)] {
        holds(
            name,
            Knobs {
                twin: Some(twin),
                ..Knobs::default()
            },
            "current",
        );
    }
    holds(
        "unknown-flag",
        Knobs {
            argv: Some(vec!["--brand-new-flag".to_string()]),
            ..Knobs::default()
        },
        "skip:argv:unknown-flag --brand-new-flag",
    );
    holds(
        "break",
        Knobs {
            background: true,
            ..Knobs::default()
        },
        "wait:break",
    );
}

/// A hold before the signal or a refused signal takes the claim back and types nothing.
#[test]
fn a_change_before_the_signal_or_its_refusal_takes_the_claim_back() {
    holds(
        "changed",
        Knobs {
            held_from: Some(2),
            ..Knobs::default()
        },
        "wait:changed-before-signal",
    );
    let rig = LiveRig::new(
        "refused",
        Knobs {
            signal_refused: true,
            ..Knobs::default()
        },
    );
    let r = rig.visit();
    assert_eq!(r.step, "wait:signal-held", "{r:?}");
    assert!(rig.record().is_none(), "the claim is taken back");
    assert!(rig.asked("turn").is_empty());
}

/// A fault or a wall on an idle screen holds the restart ([`live::screen_idle`]).
#[test]
fn a_fault_or_a_wall_on_an_idle_screen_holds_the_restart() {
    use aterm_phase::Phase;
    use aterm_phase::muse::fixtures as fx;
    let read = |text: &str| {
        let rows = fixture_rows(text);
        (
            aterm_phase::muse::phase(&rows),
            aterm_phase::muse::wall(&rows).is_some(),
            aterm_phase::muse::composer_draft(&rows)
                .map(|(_, lines)| lines.iter().all(|l| l.trim().is_empty())),
            live::screen_idle(&rows),
        )
    };
    let fault: &'static str = Box::leak(
        fx::END_OF_TURN_1_4_10
            .replace(
                "◆ Hi! I am the local lab stub, not a model. (reply 1)",
                "◆ A configuration problem stopped this turn.",
            )
            .into_boxed_str(),
    );
    assert_ne!(
        fault,
        fx::END_OF_TURN_1_4_10,
        "the fault drawn in its place"
    );
    for (name, screen, reading) in [
        ("fault", fault, ((Phase::Idle, false), false, Some(true))),
        (
            "exited",
            fx::EXITED_1_4_10,
            ((Phase::Idle, false), false, None),
        ),
        (
            "500-final",
            fx::SERVER_ERROR_FINAL,
            ((Phase::Idle, true), true, Some(true)),
        ),
        (
            "401",
            fx::KEY_REJECTED,
            ((Phase::Idle, true), true, Some(true)),
        ),
        ("sign-in", fx::SIGNED_OUT, ((Phase::Idle, true), true, None)),
    ] {
        let (phase, wall, composer) = reading;
        assert_eq!(
            read(screen),
            (phase, wall, composer, false),
            "{name}: read as measured, and no idle point"
        );
        holds(
            name,
            Knobs {
                screen: Some(screen),
                ..Knobs::default()
            },
            "wait:screen-not-idle",
        );
    }
    assert_eq!(
        read(fx::END_OF_TURN_1_4_10),
        ((Phase::Idle, true), false, Some(true), true),
        "the measured turn end is the point"
    );
    let rig = LiveRig::new(
        "end-of-turn",
        Knobs {
            screen: Some(fx::END_OF_TURN_1_4_10),
            ..Knobs::default()
        },
    );
    assert_eq!(rig.visit().step, "done:resumed");
}

/// A lock naming another process at the last look puts the record back as it stood.
#[test]
fn a_lock_naming_another_process_at_the_last_look_takes_the_claim_back() {
    let rig = LiveRig::new(
        "lock-moved",
        Knobs {
            lock_moves: true,
            ..Knobs::default()
        },
    );
    let prior = St {
        agent: Agent::Muse,
        tab: TAB.to_string(),
        thread: S1.to_string(),
        pid: OTHER,
        from: OLD_VERSION.to_string(),
        to: VERSION.to_string(),
        phase: Phase::Done,
        quiet: true,
        ..St::default()
    };
    save(&rig.opts, &key(TAB), &prior);
    let r = rig.visit();
    assert_eq!(r.step, "wait:changed-before-signal", "{r:?}");
    assert!(rig.asked("signal").is_empty(), "nothing signalled");
    assert!(rig.asked("turn").is_empty(), "nothing typed");
    let back = rig.record().expect("the record put back");
    assert_eq!(
        (back.pid, back.phase.clone(), back.from.as_str()),
        (OTHER, Phase::Done, OLD_VERSION),
        "as it stood"
    );
    let kept = LiveRig::new("lock-kept", Knobs::default());
    save(&kept.opts, &key(TAB), &prior);
    assert_eq!(kept.visit().step, "done:resumed");
}

/// A restart in flight is carried by the lane's own rules while the shell leads the tab.
#[test]
fn a_restart_in_flight_is_carried_while_the_shell_leads_the_tab() {
    let ready = Arc::new(AtomicBool::new(false));
    let rig = LiveRig::new(
        "handoff",
        Knobs {
            shell_slow: Some(Arc::clone(&ready)),
            ..Knobs::default()
        },
    );
    assert_eq!(rig.visit().step, "wait:shell-prompt");
    assert!(
        matches!(
            rig.record().expect("in flight").phase,
            Phase::Exiting { .. }
        ),
        "the claim is in flight, its agent gone"
    );
    let shell_leads = vec![LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(SHELL)),
    }];
    ready.store(true, Ordering::SeqCst);
    let r = upgrade_visit(&rig.opts, &shell_leads, Some(rig.twin.clone()), &rig.kernel)
        .expect("its restart in flight is this lane's whoever leads the tab");
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert_eq!(rig.asked("turn").len(), 1, "the relaunch line alone");
    assert_eq!(rig.record().expect("record").phase, Phase::Done);
    assert!(
        upgrade_visit(&rig.opts, &shell_leads, Some(rig.twin.clone()), &rig.kernel).is_none(),
        "nothing in flight and no Muse leading: not this lane's"
    );
}

/// A Muse restart finished by the step before the host's carry of its exit reports `landed`.
#[test]
fn a_muse_restart_finished_before_its_carry_has_landed() {
    use crate::harness::relaunch::{CAUSE_EXIT, STOPPED_OWN_S, carry_restart};
    let rig = LiveRig::new("landed", Knobs::default());
    let now = now_s();
    let done = St {
        agent: Agent::Muse,
        tab: TAB.to_string(),
        thread: S1.to_string(),
        pid: TUI,
        from: VERSION.to_string(),
        to: NEW_VERSION.to_string(),
        source: "managed".to_string(),
        phase: Phase::Done,
        quiet: true,
        exited_at: now - 5,
        resumed_pid: std::process::id(),
        ..St::default()
    };
    let carry = |st: &St| {
        save(&rig.opts, &key(TAB), st);
        carry_restart(&rig.opts, Agent::Muse, Some(TUI), false)
    };
    let landed = carry(&done);
    assert_eq!(landed.step, "landed", "{landed:?}");
    assert_eq!(
        (landed.from.as_str(), landed.to.as_str()),
        (VERSION, "1.4.11-R6500.1(managed)")
    );
    for (why, st) in [
        (
            "its resumed process gone",
            St {
                resumed_pid: NEW,
                ..done.clone()
            },
        ),
        (
            "another process's record",
            St {
                pid: OTHER,
                ..done.clone()
            },
        ),
        (
            "an exit older than the bound",
            St {
                exited_at: now - STOPPED_OWN_S - 5,
                ..done.clone()
            },
        ),
        (
            "a crash relaunch's record",
            St {
                cause: CAUSE_EXIT.to_string(),
                ..done.clone()
            },
        ),
    ] {
        assert_eq!(carry(&st).step, "refused:no-restart-in-flight", "{why}");
    }
}

/// [`live::upgrade_due`]: due while behind, not due when current, never
/// another lane's tab (a leader that is no `muse`).
#[test]
fn the_tab_is_due_while_its_build_is_behind() {
    let behind = LiveRig::new("due", Knobs::default());
    let tabs = LiveRig::tabs();
    assert_eq!(
        live::upgrade_due_with(
            &behind.opts,
            &tabs,
            Some(behind.twin.clone()),
            &behind.kernel
        ),
        Some(Due::Yes)
    );
    let current = LiveRig::new(
        "due-current",
        Knobs {
            twin: Some(Twin::Same),
            ..Knobs::default()
        },
    );
    assert_eq!(
        live::upgrade_due_with(
            &current.opts,
            &tabs,
            Some(current.twin.clone()),
            &current.kernel
        ),
        Some(Due::No)
    );
    let other = vec![LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(SHELL)),
    }];
    assert_eq!(
        live::upgrade_due_with(
            &behind.opts,
            &other,
            Some(behind.twin.clone()),
            &behind.kernel
        ),
        None,
        "a shell leading the tab is not the Muse lane's"
    );
}

type State = BTreeMap<&'static str, i64>;

/// The running build's version, and the twin's for each `landed`.
fn versions(landed: i64) -> (&'static str, &'static str) {
    match landed {
        1 => (VERSION, NEW_VERSION),
        2 => (VERSION, OLD_VERSION),
        _ => (VERSION, VERSION),
    }
}

/// The session's log a state's `turn`, `task`, `queued` and `unread` stand
/// for, as 1.4.10 writes it: this process's route record (another's where
/// `unread`), a finished turn, a backgrounded shell running (`task`) or
/// ended with its item queued (`queued`), and a turn still running (`turn`).
fn state_log(s: &State) -> String {
    let route = if s["unread"] == 1 {
        route_line(OTHER, ROUTE_US)
    } else {
        route_line(TUI, ROUTE_US)
    };
    let mut log = format!("{route}\n{}", finished_turn(ROUTE_US + 100));
    if s["task"] == 1 || s["queued"] == 1 {
        log.push_str(&bg_running(ROUTE_US + 200));
    }
    if s["queued"] == 1 && s["task"] == 0 {
        log.push_str(&bg_queued(ROUTE_US + 300));
    }
    if s["turn"] == 1 {
        log.push_str(&run_line("r9", "started", ROUTE_US + 400));
        log.push('\n');
    }
    log
}

/// The screen a state's `shown` and `draft` stand for: Muse Code 1.4.10 at a
/// turn's end, thinking, or with a draft.
fn state_rows(s: &State) -> Vec<String> {
    let fixture = if s["draft"] == 1 {
        aterm_phase::muse::fixtures::DRAFT_1_4_10
    } else if s["shown"] == 1 {
        aterm_phase::muse::fixtures::BUSY_1_4_10
    } else {
        aterm_phase::muse::fixtures::END_OF_TURN_1_4_10
    };
    fixture_rows(fixture)
}

/// Whether the real quiet flow restarts at state `s` ([`pure::moves_onto`] and [`pure::gate`]).
fn real_restarts(s: &State) -> bool {
    let (running, target) = versions(s["landed"]);
    let rows = state_rows(s);
    let status = format!("OK schema=1 hold={} hand=- integration=on", s["held"]);
    let composer_empty = || {
        aterm_phase::muse::composer_draft(&rows)
            .is_some_and(|(_, lines)| lines.iter().all(|l| l.trim().is_empty()))
    };
    let table = if s["child"] == 1 {
        vec![(TUI, SHELL, "muse".into()), (CHILD, TUI, "sleep".into())]
    } else {
        vec![(TUI, SHELL, "muse".into())]
    };
    let look: Look = live::look_of(
        crate::harness::upgrade_drive::held_by_status(&status, 120),
        Some(&rows),
        composer_empty,
        pure::quiet_log(&state_log(s), TUI, START_MS * 1000).map(|_| ()),
        descendants_in(&table, TUI).map(|d| d.len()),
    );
    pure::moves_onto(running, target) && pure::gate(&look).is_ok()
}

/// Whether the record written before the signal is read by `ended_by_restart` as a harness restart.
fn real_claim_relaunches() -> bool {
    let rig = LiveRig::new("tier1-claim", Knobs::default());
    let mut snap = rig.snap_of(S1);
    snap.argv = rig.kernel.argv.clone();
    let mv = Move {
        running: VERSION.to_string(),
        target: NEW_VERSION.to_string(),
        twin: rig.twin.clone(),
    };
    let now = now_s();
    let st = claim_record(&snap, &mv, rig.opts.human_grace_s, now).expect("a claim");
    assert_eq!(
        st.proc_start, snap.start,
        "the claim preserves its snapshot's run identity"
    );
    ended_by_restart(&st, Agent::Muse, TAB, TUI, now)
}

/// 2026-10-26 review: the Muse Code quiet upgrade's SIGTERM waits out the whole grace, so its claim
/// names that grace, and a key typed at the prompt after it holds the relaunch line it owes exactly
/// as long as it would have held the SIGTERM. The control: a claim naming the land rung's keys gap
/// (what the line read a Muse claim that named none as) lets go a key the SIGTERM waited on.
#[test]
fn a_muse_claim_holds_its_line_on_the_grace_its_sigterm_waited_out() {
    use crate::harness::relaunch::{PromptLook, line_grace, line_held_by_status};
    let rig = LiveRig::new("claim-grace", Knobs::default());
    let mut snap = rig.snap_of(S1);
    snap.argv = rig.kernel.argv.clone();
    let mv = Move {
        running: VERSION.to_string(),
        target: NEW_VERSION.to_string(),
        twin: rig.twin.clone(),
    };
    let grace_s = rig.opts.human_grace_s;
    let key_status =
        |age_s: u64| format!("OK schema=1 hold=0 hand=- input=clear human_ms={age_s}000");
    let owed_line_held = |claim: &St, grace_s: u32, age_s: u64| {
        let mut claim = claim.clone();
        let look = PromptLook::of(&mut claim);
        let grace = line_grace(grace_s, look.ended_at_s, look.line_grace_s);
        line_held_by_status(Some(&key_status(age_s)), grace, "aterm-harness@1").is_some()
    };
    let claim = claim_record(&snap, &mv, grace_s, now_s()).expect("a claim");
    assert_eq!(
        claim.line_grace_s,
        Some(grace_s),
        "the claim names the grace its SIGTERM waited out"
    );
    let sigterm_held =
        |age_s: u64| crate::harness::upgrade_drive::held_by_status(&key_status(age_s), grace_s);
    for age_s in [0, 1, 19, 20, 21, 30, 60, 119, 120, 121, 300] {
        assert_eq!(
            owed_line_held(&claim, grace_s, age_s),
            sigterm_held(age_s),
            "a key {age_s} s old"
        );
    }
    let gap = St {
        line_grace_s: Some(crate::harness::upgrade::KEYS_GAP_S),
        ..claim.clone()
    };
    assert!(
        (0..=300).any(|age_s| sigterm_held(age_s) && !owed_line_held(&gap, grace_s, age_s)),
        "control: the keys gap lets go a key the SIGTERM waited on"
    );
}

impl LiveRig {
    /// The snapshot of the live Muse, read through the lane's own
    /// `snapshot` over this rig's kernel.
    fn snap_of(&self, session: &str) -> crate::harness::relaunch::Snapshot {
        let args = self.kernel.args(TUI).expect("argv");
        let snap = snapshot(
            &self.opts,
            TAB,
            TUI,
            "tui-birth".to_string(),
            SHELL,
            &args,
            &self.kernel,
        )
        .expect("a snapshot");
        assert_eq!(snap.session.as_deref(), Some(session));
        snap
    }
}

/// Tier-1 conformance test for `HarnessMuseQuietUpgrade`.
#[test]
fn tier1_the_real_muse_quiet_upgrade_conforms_to_the_model() {
    let model = aterm_models_harness::harness_muse_quiet_upgrade_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    const MUTANTS: [&str; 6] = [
        "RestartWithOpenWork",
        "RestartMidTurn",
        "RestartWithChild",
        "RestartUnclaimed",
        "RevisionReadAsOlder",
        "RevisionReadAsNewer",
    ];
    let fired = aterm_spec::interp::fired_actions(&model);
    let dead: BTreeSet<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    assert_eq!(dead, BTreeSet::from(MUTANTS), "the model's whole Buggy set");
    let claim_relaunches = real_claim_relaunches();
    assert!(
        claim_relaunches,
        "the real claim makes the exit the restart's"
    );
    assert_eq!(
        mz::exit_kind(143),
        mz::ExitKind::Clean,
        "an unclaimed SIGTERM is read as someone's end"
    );
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([model.init_state()]);
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    let mut real_cache: BTreeMap<Vec<i64>, bool> = BTreeMap::new();
    let (mut restarts, mut waits, mut exits) = (0, 0, 0);
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &model.actions {
            for next in model.successors(a.name, &s) {
                queue.push_back(next);
            }
        }
        if s["exited"] == 1 && s["decided"] == 0 {
            exits += 1;
            let mut after = s.clone();
            assert!(model.fire("ReadExit", &mut after));
            assert_eq!(after["decided"] == 1, claim_relaunches, "ReadExit at {s:?}");
        }
        if s["moved"] == 1 {
            continue;
        }
        let key: Vec<i64> = [
            "landed", "turn", "task", "queued", "child", "shown", "draft", "held", "unread",
        ]
        .iter()
        .map(|k| s[k])
        .collect();
        let real = *real_cache.entry(key).or_insert_with(|| real_restarts(&s));
        assert_eq!(
            model.action_enabled("Restart", &s),
            real,
            "Restart at {s:?}"
        );
        if real {
            restarts += 1;
        } else {
            waits += 1;
        }
        for mutant in MUTANTS {
            if !buggy.action_enabled(mutant, &s) {
                continue;
            }
            let caught_here = match mutant {
                "RevisionReadAsOlder" => real,
                "RestartUnclaimed" => {
                    let mut wrong = s.clone();
                    assert!(buggy.fire(mutant, &mut wrong));
                    assert!(buggy.fire("Exit", &mut wrong));
                    assert!(buggy.fire("ReadExit", &mut wrong));
                    wrong["decided"] == 2 && claim_relaunches
                }
                _ => !real,
            };
            if caught_here {
                caught.insert(mutant);
            }
        }
    }
    assert!(
        restarts > 0 && waits > 0 && exits > 0,
        "{restarts} {waits} {exits}"
    );
    assert_eq!(caught, BTreeSet::from(MUTANTS), "every mutant caught alone");
}

/// The process rule's walk: every descendant, a grandchild too; none for a
/// process with none; `None` for an empty table (unread, never "no child").
#[test]
fn the_process_rule_finds_every_descendant() {
    let table = vec![
        (TUI, SHELL, "muse".into()),
        (CHILD, TUI, "sh".into()),
        (CHILD + 1, CHILD, "sleep".into()),
        (OTHER, SHELL, "vim".into()),
    ];
    let found = descendants_in(&table, TUI).expect("read");
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(descendants_in(&table, OTHER), Some(Vec::new()));
    assert_eq!(descendants_in(&[], TUI), None);
}

/// The log look is a session's whole: an open log reads, a session whose log
/// is gone fails closed, and a process with no kernel start proves no part.
#[test]
fn the_log_look_fails_closed() {
    let rig = LiveRig::new("loglook", Knobs::default());
    assert_eq!(live::log_look(&rig.lock, TUI, START_MS), Ok(()));
    assert_eq!(live::log_look(&rig.lock, TUI, 0), Err(Open::Unproved));
    std::fs::remove_file(rig.lock.with_file_name(mz::LOG)).expect("rm log");
    assert_eq!(
        live::log_look(&rig.lock, TUI, START_MS),
        Err(Open::Unreadable)
    );
}
