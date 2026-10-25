// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Muse Code upgrade driver tests over scripted screens and kernel.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::super::{Job, parse_exit_status};
use super::*;
use crate::harness::relaunch::{AgentRun, ExitCause, ExitRecord, MuseRun, Snapshot, exit_record};
use crate::supervise::ScratchDir;

const TAB: &str = "s-0123456789abcdef4d55";
const TOKEN: &str = "0badf00d0badf00d";
/// The session the crashed Muse held; another a `/new` moved it to.
const S1: &str = "01a148f8-9301-7752-96db-fef456f88074";
const S2: &str = "01a14a17-aa54-7652-8d89-33ef32250ae5";
/// Pids no real process of this test has: the kernel is scripted.
const TUI: u32 = 4_100_001;
const SHELL: u32 = 4_100_002;
const NEW: u32 = 4_100_003;
const OTHER: u32 = 4_100_004;
/// The store build atpkg encodes 1.4.10-R6427.1 as.
const BUILD: &str = "2001004010006427002";
const VERSION: &str = "1.4.10-R6427.1";
/// The workspace the session's log names, and the shell's own directory.
const WORKSPACE: &str = "/w/proj";
const SHELL_CWD: &str = "/w";

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("msd", name)
}

/// One `text --json` body: `rows`, the cursor at `(row, col)`, `seq`.
fn screen_json(rows: &[String], cursor: (usize, usize), seq: u64) -> String {
    let rows = aterm_json::to_string(&aterm_json::Value::Array(
        rows.iter()
            .map(|r| aterm_json::Value::from(r.as_str()))
            .collect(),
    ))
    .expect("rows");
    format!(
        r#"{{"rows":{rows},"cursor":{{"row":{},"col":{}}},"seq":{seq},"gen":"1.{seq}","human_ms":null,"first":0}}"#,
        cursor.0, cursor.1
    )
}

/// A fixture's rows, its provenance line dropped.
fn fixture_rows(text: &str) -> Vec<String> {
    text.lines().skip(1).map(str::to_string).collect()
}

/// Relaunched Muse Code idle at the end of a turn.
fn muse_idle() -> String {
    let rows = fixture_rows(aterm_phase::muse::fixtures::END_OF_TURN_1_4_10);
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("a caret row");
    screen_json(&rows, (caret, 2), 20)
}

/// Shell screen after Muse exited.
fn shell_after() -> String {
    let rows: Vec<String> = ["% sh muse.sh", "zsh: killed     sh", "% "]
        .iter()
        .map(|r| (*r).to_string())
        .collect();
    screen_json(&rows, (2, 2), 900)
}

/// Last two shell-integration blocks for the Muse command and following prompt.
fn blocks(exit: i64, word: bool, later: bool) -> String {
    let entering = r#"{"id":8,"state":"entering","exit":null,"prompt":4,"cmd":4,"cmdcol":2,"out":null,"end":null,"cmdline":""}"#;
    if !word {
        return format!(r#"{{"blocks":[{entering}]}}"#);
    }
    if later {
        return r#"{"blocks":[{"id":9,"state":"complete","exit":2,"prompt":4,"cmd":4,"cmdcol":2,"out":5,"end":6,"cmdline":"make"},{"id":10,"state":"entering","exit":null,"prompt":6,"cmd":6,"cmdcol":2,"out":null,"end":null,"cmdline":""}]}"#.to_string();
    }
    format!(
        r#"{{"blocks":[{{"id":7,"state":"complete","exit":{exit},"prompt":2,"cmd":2,"cmdcol":2,"out":3,"end":4,"cmdline":"sh muse.sh"}},{entering}]}}"#
    )
}

/// Shell blocks after the relaunch line ran in block 8.
fn line_blocks(exit: Option<i64>) -> String {
    let line = r#""cmdline":" ( cd -- '/w/proj' && XDG_DATA_HOME=/h exec '/p/agents/muse' 'resume' 's1' )""#;
    match exit {
        None => format!(
            r#"{{"blocks":[{{"id":8,"state":"executing","exit":null,"prompt":4,"cmd":4,"cmdcol":2,"out":5,"end":null,{line}}}]}}"#
        ),
        Some(exit) => format!(
            r#"{{"blocks":[{{"id":8,"state":"complete","exit":{exit},"prompt":4,"cmd":4,"cmdcol":2,"out":5,"end":6,{line}}},{{"id":9,"state":"entering","exit":null,"prompt":6,"cmd":6,"cmdcol":2,"out":null,"end":null,"cmdline":""}}]}}"#
        ),
    }
}

/// Stand-in overrides served after the relaunch line is typed.
#[derive(Default)]
struct After {
    /// The relaunched Muse's screen.
    screen: Option<String>,
    /// The tab's `status`.
    status: Option<String>,
    /// The tab's command blocks.
    blocks: Option<String>,
}

/// What the stand-in serves, and what it saw.
struct World {
    blocks2: String,
    after_exit: String,
    after_relaunch: String,
    status: String,
    relaunched: Arc<AtomicBool>,
    after: Arc<Mutex<After>>,
}

/// Control socket at `<dir>/t.sock` standing in for [`TAB`] and its request log.
fn instance(dir: &Path, w: World) -> (String, crate::harness::FakeInstanceLog) {
    crate::harness::fake_instance(&dir.join("t.sock"), TOKEN, move |line| {
        let verb = line
            .split_whitespace()
            .find(|w| !w.starts_with('@'))
            .unwrap_or("");
        let relaunched = w.relaunched.load(Ordering::SeqCst);
        let after = w.after.lock().expect("after");
        let served = |what: &Option<String>, before: &String| match what {
            Some(now) if relaunched => now.clone(),
            _ => before.clone(),
        };
        match verb {
            "sessions" => format!("OK 1\nlocal {TAB} 0 idle muse"),
            "text" => format!(
                "OK {}",
                if relaunched {
                    served(&after.screen, &w.after_relaunch)
                } else {
                    w.after_exit.clone()
                }
            ),
            "cell" => "OK ❯ ccd3db 1e1e1e none".to_string(),
            "status" => served(&after.status, &w.status),
            "help" if line.contains("help key") => {
                "OK 2\nkey  key [if-gen=<epoch>.<seq>] [if=<re>] <name>\n     FENCES".to_string()
            }
            "help" => "OK 1\nturn [if-gen=<epoch>.<seq>] <text>".to_string(),
            "blocks" => format!("OK {}", served(&after.blocks, &w.blocks2)),
            "blocktext" => "OK 0".to_string(),
            "line" => "OK % ".to_string(),
            "lease" => "ERR unscripted".to_string(),
            "turn" => {
                if line.contains("'resume'") {
                    w.relaunched.store(true, Ordering::SeqCst);
                }
                "OK 0 turn submitted=1 status=settled seq=1 id=1".to_string()
            }
            _ => "ERR unscripted".to_string(),
        }
    })
}

/// Scripted kernel for [`TUI`], [`SHELL`], and [`NEW`].
struct Script {
    exited: Arc<AtomicBool>,
    relaunched: Arc<AtomicBool>,
    lock: PathBuf,
    holder: bool,
    exe: PathBuf,
    files: Vec<PathBuf>,
    knobs: Knobs,
    /// `holders_of` calls on the session lock so far.
    lock_reads: AtomicUsize,
}

/// Per-test relaunch variations.
#[derive(Clone, Debug, Default)]
struct Knobs {
    /// The host's roster never shows [`NEW`] leading the tab.
    out_of_tab: bool,
    /// [`NEW`] is listed under the shell and exits immediately.
    new_dies: bool,
    /// No `muse` ever comes up under the shell.
    noappear: bool,
    /// The argv [`NEW`] runs with in place of `resume` [`S1`].
    new_argv: Option<Vec<String>>,
    /// [`NEW`]'s parent is another process than the shell.
    new_orphan: bool,
    /// [`NEW`] never holds the session's lock.
    new_lockless: bool,
    /// Who holds the session's lock cannot be read.
    holders_unreadable: bool,
    /// [`OTHER`] holds the lock starting at this read index (1-based).
    holder_from_read: Option<usize>,
}

impl Kernel for Script {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        Some((Job::Foreground, SHELL))
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        match pid {
            NEW if self.knobs.new_orphan => Some(1),
            NEW | TUI => Some(SHELL),
            _ => None,
        }
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

impl MuseKernel for Script {
    fn alive(&self, pid: u32) -> bool {
        match pid {
            TUI => !self.exited.load(Ordering::SeqCst),
            NEW => {
                self.relaunched.load(Ordering::SeqCst)
                    && !self.knobs.new_dies
                    && !self.knobs.noappear
            }
            OTHER => self.holder || self.knobs.holder_from_read.is_some(),
            _ => pid == SHELL,
        }
    }

    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        (pid == SHELL).then(|| atpkg::caller_shell::ProcArgs {
            exec_path: "/bin/zsh".to_string(),
            argv: vec!["-zsh".to_string()],
            env: Vec::new(),
        })
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        (pid == TUI).then(|| self.exe.clone())
    }

    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>> {
        Some(if pid == TUI && !self.exited.load(Ordering::SeqCst) {
            self.files.clone()
        } else {
            Vec::new()
        })
    }

    fn holders_of(&self, file: &Path) -> Option<Vec<u32>> {
        if file != self.lock {
            return Some(Vec::new());
        }
        let read = self.lock_reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.knobs.holders_unreadable {
            return None;
        }
        let mut h = Vec::new();
        if self.relaunched.load(Ordering::SeqCst) {
            if MuseKernel::alive(self, NEW) && !self.knobs.new_lockless {
                h.push(NEW);
            }
        } else if !self.exited.load(Ordering::SeqCst) {
            h.push(TUI);
        }
        if self.holder || self.knobs.holder_from_read.is_some_and(|from| read >= from) {
            h.push(OTHER);
        }
        Some(h)
    }

    fn shell_has_terminal(&self, _: u32) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    fn muse_children(&self, _: u32) -> Vec<(u32, Vec<String>)> {
        if self.relaunched.load(Ordering::SeqCst) && !self.knobs.noappear {
            vec![(
                NEW,
                self.knobs.new_argv.clone().unwrap_or_else(|| {
                    vec![
                        self.exe.to_string_lossy().into_owned(),
                        "--base-url".to_string(),
                        "http://127.0.0.1:9".to_string(),
                        "resume".to_string(),
                        S1.to_string(),
                    ]
                }),
            )]
        } else {
            Vec::new()
        }
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        Some(if pid == SHELL { SHELL_CWD } else { WORKSPACE }.to_string())
    }

    fn in_tab(&self, _: &mut Client, pid: u32, _: &str) -> bool {
        !(pid == NEW && self.knobs.out_of_tab) && MuseKernel::alive(self, pid)
    }
}

/// Lifecycle state recorded in the session's log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Log {
    Clean,
    Crash,
    None,
}

fn log_text(log: Log) -> Option<String> {
    let start = format!(
        r#"{{"schema_version":1,"sequence":5,"record_type":"event","payload_type":"runtime.session_start.intake_v3","payload":{{"kind":"session_start_intake_v3","payload":{{"schema_version":3,"session_metadata":{{"workspace_root":"{WORKSPACE}"}}}}}}}}"#
    );
    let started = r#"{"schema_version":1,"sequence":6,"record_type":"event","payload_type":"session.started","payload":{"kind":"session_started"}}"#;
    let end = r#"{"schema_version":1,"sequence":85,"record_type":"event","payload_type":"session.end","payload":{"kind":"session_end","record":{"exit_reason":"clean","uptime_ms":195060}}}"#;
    match log {
        Log::None => None,
        Log::Crash => Some(format!("{start}\n{started}\n")),
        Log::Clean => Some(format!("{start}\n{started}\n{end}\n")),
    }
}

/// Scripted test rig with a session lock and log, store build, twin, and fake instance.
struct Rig {
    dir: ScratchDir,
    opts: Opts,
    kernel: Script,
    asked: crate::harness::FakeInstanceLog,
    data: PathBuf,
    lock: PathBuf,
    exe: PathBuf,
    twin: PathBuf,
    after: Arc<Mutex<After>>,
    /// Virtual wait guard for [`crate::harness::relaunch::virtual_waits`].
    _waits: crate::harness::relaunch::VirtualWaits,
}

impl Rig {
    /// Create a scripted test rig.
    fn new(name: &str, status: i64, word: bool, later: bool, log: Log, lock_names: bool) -> Rig {
        let dir = scratch(name);
        let base = std::fs::canonicalize(&*dir).expect("canonical scratch");
        let data = base.join("home/.local/share");
        let session = mz::sessions_root(&data).join("2026/10/17").join(S1);
        std::fs::create_dir_all(&session).expect("session dir");
        let lock = session.join(mz::LOCK);
        std::fs::write(
            &lock,
            if lock_names {
                format!("pid={TUI}\nhost=Mac.localdomain\n")
            } else {
                String::new()
            },
        )
        .expect("lock");
        if let Some(text) = log_text(log) {
            std::fs::write(session.join(mz::LOG), text).expect("log");
        }
        let store = base.join("prefix/store/muse");
        let exe = store.join(BUILD).join("bin/muse");
        std::fs::create_dir_all(exe.parent().expect("bin")).expect("store");
        std::fs::write(&exe, "#!/bin/sh\n").expect("build");
        std::fs::write(
            store.join(format!("{BUILD}.vendor")),
            format!("version = \"{VERSION}\"\n"),
        )
        .expect("vendor record");
        let twin = base.join("prefix/agents/muse");
        std::fs::create_dir_all(twin.parent().expect("agents")).expect("agents");
        std::fs::write(
            &twin,
            format!(
                "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\nexec '{}' \"$@\"\n",
                exe.display()
            ),
        )
        .expect("twin");
        let exited = Arc::new(AtomicBool::new(true));
        let relaunched = Arc::new(AtomicBool::new(false));
        let after = Arc::new(Mutex::new(After::default()));
        let w = World {
            blocks2: blocks(status, word, later),
            after_exit: shell_after(),
            after_relaunch: muse_idle(),
            status: "OK schema=1 hold=0 hand=- integration=on".to_string(),
            relaunched: Arc::clone(&relaunched),
            after: Arc::clone(&after),
        };
        let (sock, asked) = instance(&base, w);
        let opts = Opts {
            home: base.join("home"),
            state: base.join("state"),
            sock: Some(sock),
            token: None,
            only_sid: None,
            dry_run: false,
            human_grace_s: 120,
            hand_back: false,
            background: false,
            aterm_state: None,
            ask: true,
            several_logins: false,
            usage_credits: false,
            usage_tap: None,
            instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
            agents_off: Vec::new(),
            land: false,
            timings: crate::harness::Timings::FAST,
        };
        Rig {
            _waits: crate::harness::relaunch::virtual_waits(),
            kernel: Script {
                exited,
                relaunched,
                lock: lock.clone(),
                holder: false,
                exe: exe.clone(),
                files: vec![lock.clone(), lock.clone(), session.join("cron.db")],
                knobs: Knobs::default(),
                lock_reads: AtomicUsize::new(0),
            },
            asked,
            after,
            data,
            lock,
            exe,
            twin,
            opts,
            dir,
        }
    }

    /// Snapshot of the running Muse process.
    fn snap(&self, session: Option<&str>) -> Snapshot {
        Snapshot {
            stored_login: false,
            profile: None,
            tab: TAB.to_string(),
            pid: TUI,
            start: "Sat Oct 17 06:40:00 2026".to_string(),
            shell: SHELL,
            program: self.exe.clone(),
            argv: vec![
                self.exe.to_string_lossy().into_owned(),
                "--base-url".to_string(),
                "http://127.0.0.1:9".to_string(),
                "--yolo".to_string(),
                "resume".to_string(),
                "my-session-name".to_string(),
            ],
            session: session.map(str::to_string),
            cwd: SHELL_CWD.to_string(),
            version: Some(VERSION.to_string()),
            run: AgentRun::Muse(MuseRun {
                data: self.data.clone(),
                env: vec![(
                    "XDG_DATA_HOME".to_string(),
                    self.data.to_string_lossy().into_owned(),
                )],
                workspace: Some(WORKSPACE.to_string()),
                lock: session.is_some().then(|| self.lock.clone()),
                started_ms: 0,
                block: Some(7),
            }),
            dialect: Some(Dialect::Zsh),
            login: None,
            home: None,
            claude_dir: None,
        }
    }

    fn attempt(&self, snap: &Snapshot, left: &ExitRecord, cause: ExitCause) -> Report {
        after_exit_onto(
            &self.opts,
            snap,
            left,
            cause,
            Some(self.twin.clone()),
            &self.kernel,
        )
    }

    fn typed(&self) -> Vec<String> {
        self.asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
            .unwrap_or_default()
    }

    fn state(&self) -> St {
        load(&self.opts, &key(TAB)).expect("a record")
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&*self.dir);
    }
}

/// A crashed Muse (SIGKILL, 137) is relaunched on its session in its workspace using the managed twin.
#[test]
fn a_crashed_muse_is_relaunched_on_its_session_in_its_workspace() {
    let rig = Rig::new("crash", 137, true, false, Log::Crash, true);
    let snap = rig.snap(Some(S1));
    let left = exit_record(
        || Some(look_at_exit(&rig.opts, &snap, &rig.kernel)),
        |_| true,
    );
    assert_eq!(left, ExitRecord::Crashed);
    let r = rig.attempt(&snap, &left, ExitCause::Exit);
    assert_eq!(r.step, "continued", "{r:?}");
    assert_eq!(r.session, format!("muse-{TAB}"));
    let typed = rig.typed();
    assert_eq!(typed.len(), 2, "{typed:#?}");
    let line = &typed[0];
    let twin = rig.twin.to_string_lossy();
    assert!(line.contains(&format!("cd -- '{WORKSPACE}' && ")), "{line}");
    assert!(
        line.contains(&format!("XDG_DATA_HOME={} ", rig.data.display())),
        "{line}"
    );
    assert!(line.contains(&format!("'{twin}'")), "{line}");
    assert!(
        line.contains(&format!(
            "'--base-url' 'http://127.0.0.1:9' 'resume' '{S1}'"
        )),
        "{line}"
    );
    assert!(
        !line.contains("/store/muse/"),
        "never the store path: {line}"
    );
    assert!(
        !line.contains("--yolo") && !line.contains("my-session-name"),
        "{line}"
    );
    assert!(
        typed[1].contains("[aterm harness] Relaunched: Muse Code exited"),
        "{}",
        typed[1]
    );
    let st = rig.state();
    assert_eq!(
        (
            st.agent,
            st.thread.as_str(),
            st.cwd.as_str(),
            st.source.as_str()
        ),
        (Agent::Muse, S1, WORKSPACE, "managed")
    );
    assert_eq!(
        st.outcome,
        format!("muse on {VERSION} · the same conversation resumed")
    );
}

/// Exit status is read from the finished command block.
#[test]
fn the_muse_exit_is_read_off_its_command_block() {
    assert_eq!(
        parse_exit_status("OK", &blocks(0, true, false), Some(7)),
        Some(0)
    );
    assert_eq!(
        parse_exit_status("OK", &blocks(137, true, false), Some(7)),
        Some(137)
    );
    assert_eq!(
        parse_exit_status("OK", &blocks(137, true, false), None),
        Some(137)
    );
    assert_eq!(
        parse_exit_status("OK", &blocks(137, false, false), Some(7)),
        None
    );
    let elsewhere = blocks(0, true, false).replace(r#""end":4"#, r#""end":3"#);
    assert_eq!(parse_exit_status("OK", &elsewhere, Some(7)), None);
    assert_eq!(parse_exit_status("ERR no such session", "", Some(7)), None);
    assert_eq!(
        parse_exit_status("OK", &blocks(0, true, true), Some(7)),
        None
    );
    assert_eq!(
        parse_exit_status("OK", &blocks(0, true, true), None),
        Some(2),
        "control: the newest block's word, with no block named"
    );
}

/// Graceful exits, signals (`SIGINT`, `SIGTERM`), and refused starts (`1`, `2`) are never relaunched.
#[test]
fn an_own_exit_a_signal_and_a_refused_start_are_never_relaunched() {
    for (status, want) in [
        (0, "ended:graceful-exit"),
        (130, "ended:graceful-exit"),
        (143, "ended:graceful-exit"),
        (1, "ended:refused-start"),
        (2, "ended:refused-start"),
        (129, "continued"),
    ] {
        let rig = Rig::new("left", status, true, false, Log::Crash, status != 0);
        let snap = rig.snap(Some(S1));
        let left = exit_record(
            || Some(look_at_exit(&rig.opts, &snap, &rig.kernel)),
            |_| true,
        );
        let r = rig.attempt(&snap, &left, ExitCause::Exit);
        assert_eq!(r.step, want, "{status}: {left:?} {r:?}");
        if want.starts_with("ended:") {
            assert!(rig.typed().is_empty(), "{status}: {:#?}", rig.typed());
        }
        let signals_only_crash = !matches!(status, 0 | 129 | 130 | 143);
        assert_eq!(
            signals_only_crash != (mz::exit_kind(status) == ExitKind::Crashed),
            matches!(status, 1 | 2 | 129),
            "a signals-only table differs from Muse Code's on 1, 2 and 129: {status}"
        );
    }
    let rig = Rig::new("refused-launch", 1, true, false, Log::None, false);
    let snap = rig.snap(None);
    assert_eq!(
        crate::harness::relaunch::launch_ended(
            &snap,
            &exit_record(
                || Some(look_at_exit(&rig.opts, &snap, &rig.kernel)),
                |_| true
            )
        ),
        Some("refused-start")
    );
}

/// Without a shell exit status, the witnessed session's log decides whether to relaunch.
#[test]
fn with_no_shell_word_the_sessions_log_decides() {
    for (log, witnessed, want) in [
        (Log::Clean, true, "ended:graceful-exit"),
        (Log::Crash, true, "continued"),
        (Log::None, true, "ended:exit-unwitnessed"),
        (Log::Crash, false, "ended:exit-unwitnessed"),
    ] {
        let rig = Rig::new("log", 137, false, false, log, true);
        let snap = rig.snap(witnessed.then_some(S1));
        let left = exit_record(
            || Some(look_at_exit(&rig.opts, &snap, &rig.kernel)),
            |_| true,
        );
        assert_eq!(left, ExitRecord::Unread, "{log:?}");
        let r = rig.attempt(&snap, &left, ExitCause::Exit);
        assert_eq!(r.step, want, "{log:?} witnessed={witnessed}: {r:?}");
    }
}

/// A crash without a witnessed session or with a missing session directory is not relaunched.
#[test]
fn no_witness_no_relaunch() {
    let rig = Rig::new("unwitnessed", 137, true, false, Log::Crash, true);
    let r = rig.attempt(&rig.snap(None), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "ended:no-conversation");
    assert!(rig.typed().is_empty());
    let gone = Rig::new("gone", 137, true, false, Log::Crash, true);
    std::fs::remove_dir_all(gone.lock.parent().expect("session dir")).expect("gone");
    let r = gone.attempt(&gone.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "ended:no-session");
    let control = Rig::new("witnessed", 137, true, false, Log::Crash, true);
    let r = control.attempt(
        &control.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "continued");
}

/// A tab's own Muse Code ([`crate::harness::relaunch::NO_SHELL`]) is never relaunched.
#[test]
fn a_tabs_own_muse_is_relaunched_by_nothing() {
    use crate::harness::relaunch::NO_SHELL;
    let rig = Rig::new("tab-program", 137, true, false, Log::Crash, true);
    let snap = Snapshot {
        shell: NO_SHELL,
        dialect: None,
        ..rig.snap(Some(S1))
    };
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "refused:not-a-shell-job", "{r:?}");
    assert!(rig.typed().is_empty());
    let st = St {
        shell: NO_SHELL,
        ..St::default()
    };
    assert!(matches!(
        plan(&rig.opts, &st, &rig.kernel),
        Err(NoPlan::Refused { why, .. }) if why == "not-a-shell-job"
    ));
    let control = Rig::new("tab-program-control", 137, true, false, Log::Crash, true);
    let r = control.attempt(
        &control.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "continued");
}

/// A session held by another live process waits without typing.
#[test]
fn a_held_session_is_never_typed_at() {
    let mut rig = Rig::new("held", 137, true, false, Log::Crash, true);
    rig.kernel.holder = true;
    let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "wait:session-held");
    assert!(rig.typed().is_empty(), "{:#?}", rig.typed());
    rig.kernel.holder = false;
    let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "continued", "{r:?}");
}

/// Uncarryable launch flags and missing managed twins are refused without typing.
#[test]
fn an_uncarryable_launch_and_a_missing_twin_are_refused() {
    let rig = Rig::new("worktree", 137, true, false, Log::Crash, true);
    let mut snap = rig.snap(Some(S1));
    snap.argv.push("--worktree".to_string());
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert!(r.step.starts_with("failed:argv:"), "{r:?}");
    assert_eq!(rig.typed().len(), 0, "{:#?}", rig.typed());
    let rig = Rig::new("no-twin", 137, true, false, Log::Crash, true);
    let r = after_exit_onto(
        &rig.opts,
        &rig.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
        None,
        &rig.kernel,
    );
    assert_eq!(r.step, "refused:no-managed-muse");
    let rig = Rig::new("own-install", 137, true, false, Log::Crash, true);
    let other = rig.dir.join("opt/bin/muse");
    std::fs::create_dir_all(other.parent().expect("bin")).expect("bin");
    std::fs::write(&other, "#!/bin/sh\n").expect("muse");
    let mut snap = rig.snap(Some(S1));
    snap.program.clone_from(&other);
    let r = after_exit_onto(
        &rig.opts,
        &snap,
        &ExitRecord::Crashed,
        ExitCause::Exit,
        None,
        &rig.kernel,
    );
    assert_eq!(r.step, "continued", "{r:?}");
    assert!(
        rig.typed()[0].contains(&format!("'{}'", other.display())),
        "{:#?}",
        rig.typed()
    );
}

/// The witness is the open session-depth lock naming the process's pid.
#[test]
fn the_witness_is_the_held_lock_naming_the_pid() {
    let rig = Rig::new("witness", 0, true, false, Log::Crash, true);
    rig.kernel.exited.store(false, Ordering::SeqCst);
    let args = atpkg::caller_shell::ProcArgs {
        exec_path: rig.exe.to_string_lossy().into_owned(),
        argv: rig.snap(None).argv,
        env: vec![format!("XDG_DATA_HOME={}", rig.data.display())],
    };
    let snap = snapshot(
        &rig.opts,
        TAB,
        TUI,
        "tui-birth".to_string(),
        SHELL,
        &args,
        &rig.kernel,
    )
    .expect("a snapshot");
    assert_eq!(snap.session.as_deref(), Some(S1));
    assert_eq!(snap.version.as_deref(), Some(VERSION));
    let run = snap.muse().expect("a Muse run");
    assert_eq!(run.workspace.as_deref(), Some(WORKSPACE));
    assert_eq!(
        run.env,
        vec![(
            "XDG_DATA_HOME".to_string(),
            rig.data.to_string_lossy().into_owned()
        )]
    );
    let sub = rig
        .lock
        .parent()
        .expect("dir")
        .join(format!("subagent/{S2}"));
    std::fs::create_dir_all(&sub).expect("subagent");
    std::fs::write(sub.join(mz::LOCK), format!("pid={TUI}\n")).expect("sub lock");
    let k = Script {
        files: vec![rig.lock.clone(), sub.join(mz::LOCK)],
        ..rig_kernel(&rig)
    };
    k.exited.store(false, Ordering::SeqCst);
    let again = snapshot(&rig.opts, TAB, TUI, "b".into(), SHELL, &args, &k).expect("snap");
    assert_eq!(again.session.as_deref(), Some(S1), "the subagent's is not");
    std::fs::write(&rig.lock, "").expect("truncate");
    let none = snapshot(&rig.opts, TAB, TUI, "b".into(), SHELL, &args, &k).expect("snap");
    assert_eq!(none.session, None);
    std::fs::write(&rig.lock, "pid=7\n").expect("other");
    let other = snapshot(&rig.opts, TAB, TUI, "b".into(), SHELL, &args, &k).expect("snap");
    assert_eq!(other.session, None);
    std::fs::write(&rig.lock, format!("pid={TUI}\n")).expect("names it");
    let unheld = Script {
        files: vec![rig.lock.with_file_name("cron.db")],
        ..rig_kernel(&rig)
    };
    unheld.exited.store(false, Ordering::SeqCst);
    let stale = snapshot(&rig.opts, TAB, TUI, "b".into(), SHELL, &args, &unheld).expect("snap");
    assert_eq!(stale.session, None, "a lock its process does not hold");
    let held = snapshot(&rig.opts, TAB, TUI, "b".into(), SHELL, &args, &k).expect("snap");
    assert_eq!(held.session.as_deref(), Some(S1), "the control: held");
    let second = rig
        .lock
        .parent()
        .expect("dir")
        .with_file_name(S2)
        .join(mz::LOCK);
    std::fs::create_dir_all(second.parent().expect("dir")).expect("second session");
    std::fs::write(&second, format!("pid={TUI}\n")).expect("second lock");
    let both = Script {
        files: vec![rig.lock.clone(), second.clone()],
        ..rig_kernel(&rig)
    };
    both.exited.store(false, Ordering::SeqCst);
    let two = snapshot(&rig.opts, TAB, TUI, "b".into(), SHELL, &args, &both).expect("snap");
    assert_eq!(two.session, None, "two locks naming it");
    std::fs::write(&second, "").expect("truncated, as `/new` leaves it");
    let one = snapshot(&rig.opts, TAB, TUI, "b".into(), SHELL, &args, &both).expect("snap");
    assert_eq!(
        one.session.as_deref(),
        Some(S1),
        "the control: one names it"
    );
    let exec = atpkg::caller_shell::ProcArgs {
        argv: vec![
            rig.exe.to_string_lossy().into_owned(),
            "exec".into(),
            "hi".into(),
        ],
        ..args.clone()
    };
    assert_eq!(
        snapshot(&rig.opts, TAB, TUI, "b".into(), SHELL, &exec, &k).err(),
        Some("not-a-tui")
    );
}

/// The rig's kernel, again, for a test that varies it.
fn rig_kernel(rig: &Rig) -> Script {
    Script {
        exited: Arc::clone(&rig.kernel.exited),
        relaunched: Arc::clone(&rig.kernel.relaunched),
        lock: rig.lock.clone(),
        holder: false,
        exe: rig.exe.clone(),
        files: rig.kernel.files.clone(),
        knobs: Knobs::default(),
        lock_reads: AtomicUsize::new(0),
    }
}

/// `follow` tracks `/new` to the new session's lock and workspace.
#[test]
fn the_follow_moves_with_new() {
    let rig = Rig::new("follow", 0, true, false, Log::Crash, true);
    let mut snap = rig.snap(Some(S1));
    let started = now_s().saturating_sub(5) * 1000;
    if let AgentRun::Muse(run) = &mut snap.run {
        run.started_ms = started;
    }
    assert!(!follow(&mut snap), "unmoved");
    std::fs::write(&rig.lock, "").expect("truncated by /new");
    let fresh = mz::sessions_root(&rig.data).join("2026/10/17").join(S2);
    std::fs::create_dir_all(&fresh).expect("new session");
    std::fs::write(fresh.join(mz::LOCK), format!("pid={TUI}\nhost=h\n")).expect("lock");
    std::fs::write(fresh.join(mz::LOG), log_text(Log::Crash).expect("log")).expect("log");
    assert!(follow(&mut snap));
    assert_eq!(snap.session.as_deref(), Some(S2));
    assert_eq!(
        snap.muse().and_then(|r| r.lock.clone()),
        Some(fresh.join(mz::LOCK))
    );
    let mut early = rig.snap(Some(S1));
    if let AgentRun::Muse(run) = &mut early.run {
        run.started_ms = (now_s() + 3600) * 1000;
    }
    assert!(!follow(&mut early), "written before it started");
    assert_eq!(early.session.as_deref(), Some(S1));
}

/// Restored layouts (`HostEnded`) relaunch the session and note that aterm ended.
#[test]
fn aterm_ending_relaunches_the_restored_session() {
    let rig = Rig::new("host", 0, true, false, Log::Clean, false);
    let mut snap = rig.snap(Some(S1));
    if let AgentRun::Muse(run) = &mut snap.run {
        run.lock = None;
        run.block = None;
    }
    let r = rig.attempt(&snap, &ExitRecord::Unread, ExitCause::HostEnded);
    assert_eq!(r.step, "continued", "{r:?}");
    assert!(
        rig.typed()[1].contains("aterm ended while this session ran"),
        "{:#?}",
        rig.typed()
    );
    let own = Rig::new("host-control", 0, true, false, Log::Clean, false);
    let r = own.attempt(&own.snap(Some(S1)), &ExitRecord::Unread, ExitCause::Exit);
    assert_eq!(r.step, "ended:graceful-exit");
}

/// Records are keyed `muse-<tab>` and pruned once the tab is gone and aged.
#[test]
fn records_are_keyed_by_tab_and_pruned_once_gone() {
    assert_eq!(key(TAB), format!("muse-{TAB}"));
    assert_eq!(Agent::Muse.record_tab(&key(TAB)), Some(TAB));
    assert_eq!(Agent::keyed(&key(TAB)), Some(Agent::Muse));
    assert!(!upgrade::is_session_id(&key(TAB)));
    let rig = Rig::new("prune", 0, true, false, Log::Crash, true);
    let old = |tab: &str, phase: Phase| {
        let st = St {
            agent: Agent::Muse,
            tab: tab.to_string(),
            phase,
            ..St::default()
        };
        save(&rig.opts, &key(tab), &st);
        let path = state_dir(&rig.opts).join(format!("{}.json", key(tab)));
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(when))
            .expect("aged");
        path
    };
    let homed = St {
        agent: Agent::Muse,
        tab: TAB.to_string(),
        muse_data: "/h/.local/share".to_string(),
        muse_env: vec!["XDG_DATA_HOME=/h/.local/share".to_string()],
        phase: Phase::Done,
        ..St::default()
    };
    let text = homed.to_json();
    assert!(
        text.contains("\"muse_data\":") && text.contains("\"muse_env\":"),
        "{text}"
    );
    assert_eq!(
        St::from_json(&text).map(|st| (st.muse_data, st.muse_env)),
        Some((homed.muse_data.clone(), homed.muse_env.clone()))
    );
    let codex = St {
        agent: Agent::Codex,
        ..St::default()
    }
    .to_json();
    assert!(!codex.contains("muse_"), "{codex}");
    let gone = old("s-00000000000000000001", Phase::Done);
    let live = old("s-00000000000000000002", Phase::Done);
    let flying = old("s-00000000000000000003", Phase::Exiting { at_s: 1 });
    prune(&rig.opts, now_s(), |tab| {
        if tab == "s-00000000000000000002" {
            TabLife::Live
        } else {
            TabLife::Gone
        }
    });
    assert!(!gone.exists() && live.exists() && flying.exists());
}

/// Build a rig configured with short relaunch timeouts and `knobs`.
fn relaunch_rig(name: &str, hand_back: bool, knobs: Knobs) -> Rig {
    let mut rig = Rig::new(name, 137, true, false, Log::Crash, true);
    rig.opts.hand_back = hand_back;
    rig.opts.timings.muse_found = Duration::from_millis(400);
    rig.opts.timings.muse_adopt = Duration::from_millis(300);
    rig.opts.timings.carry_settle = Duration::from_millis(400);
    rig.opts.timings.muse_up = Duration::from_millis(400);
    rig.opts.timings.muse_lock = Duration::from_millis(300);
    if knobs.new_dies || knobs.noappear {
        rig.after.lock().expect("after").screen = Some(shell_after());
    }
    rig.kernel.knobs = knobs;
    rig
}

impl Rig {
    /// The shell's blocks once the relaunch line ran ([`line_blocks`]).
    fn line_ran(&self, exit: Option<i64>) {
        self.after.lock().expect("after").blocks = Some(line_blocks(exit));
    }

    /// The relaunched Muse's screen and the tab's `status` from now on.
    fn relaunched_shows(&self, screen: Option<String>, status: Option<&str>) {
        let mut after = self.after.lock().expect("after");
        after.screen = screen;
        after.status = status.map(str::to_string);
    }

    /// The relaunch lines typed (the turns naming `resume`).
    fn lines(&self) -> usize {
        self.typed()
            .iter()
            .filter(|l| l.contains("'resume'"))
            .count()
    }

    /// The continuations typed.
    fn continuations(&self) -> usize {
        self.typed()
            .iter()
            .filter(|l| l.contains("[aterm harness]"))
            .count()
    }
}

/// A relaunch that Muse refused is recorded as `refused:resume-refused` and never typed again.
#[test]
fn a_relaunch_muse_refused_is_never_typed_again() {
    for (exit, want, lines) in [
        (1, "refused:resume-refused", 1),
        (2, "refused:resume-refused", 1),
        (127, "refused:resume-refused", 1),
        (0, "ended:relaunch-ended", 1),
        (137, "wait:resume", 2),
    ] {
        let rig = relaunch_rig(
            "refused-relaunch",
            true,
            Knobs {
                new_dies: true,
                ..Knobs::default()
            },
        );
        let snap = rig.snap(Some(S1));
        rig.line_ran(None);
        let first = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(first.step, "wait:resume", "{exit}: {first:?}");
        assert_eq!(rig.lines(), 1);
        assert_eq!(rig.state().relaunched_pid, NEW, "found, on the record");
        rig.line_ran(Some(exit));
        let again = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(again.step, want, "{exit}: {again:?}");
        assert_eq!(rig.lines(), lines, "{exit}: {:#?}", rig.typed());
        if lines == 1 {
            let third = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
            assert_eq!(third.step, want, "{exit}: {third:?}");
            assert_eq!(rig.lines(), 1);
        }
    }
    assert!(matches!(
        crate::harness::relaunch::outcome("refused:resume-refused"),
        crate::harness::relaunch::Outcome::Cannot(_)
    ));
    assert!(matches!(
        crate::harness::relaunch::outcome("ended:relaunch-ended"),
        crate::harness::relaunch::Outcome::Left(_)
    ));
}

/// A relaunch that never comes up is decided by its command block's exit or marked `refused:relaunch-unwitnessed` once stale.
#[test]
fn a_relaunch_that_never_comes_up_is_never_typed_again_but_on_a_crash() {
    let knobs = Knobs {
        noappear: true,
        ..Knobs::default()
    };
    let rig = relaunch_rig("never-refused", true, knobs.clone());
    let snap = rig.snap(Some(S1));
    rig.line_ran(Some(1));
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "refused:resume-refused", "{r:?}");
    assert_eq!(
        rig.state().phase,
        Phase::Failed("resume-refused".to_string())
    );
    let again = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(again.step, "refused:resume-refused", "{again:?}");
    assert_eq!(rig.lines(), 1, "{:#?}", rig.typed());
    let rig = relaunch_rig("never-stale", true, knobs.clone());
    let snap = rig.snap(Some(S1));
    rig.line_ran(None);
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "wait:resume", "{r:?}");
    let mut st = rig.state();
    st.phase = Phase::Relaunched {
        at_s: now_s().saturating_sub(STALE_S + 1),
    };
    save(&rig.opts, &key(TAB), &st);
    let stale = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(stale.step, "refused:relaunch-unwitnessed", "{stale:?}");
    assert_eq!(rig.lines(), 1, "{:#?}", rig.typed());
    let rig = relaunch_rig("never-crashed", true, knobs);
    let snap = rig.snap(Some(S1));
    rig.line_ran(Some(137));
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "failed:exited-before-continuing", "{r:?}");
    assert_eq!(
        crate::harness::relaunch::outcome(&r.step),
        crate::harness::relaunch::Outcome::NotYet(r.step.clone())
    );
    let _ = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(rig.lines(), 2, "typed again: {:#?}", rig.typed());
}

/// `typing_fence` reports `box`, `busy`, or `draft` for each refused screen state.
#[test]
fn the_typing_fence_names_what_it_refuses() {
    use aterm_phase::muse::fixtures as fx;
    let at_caret = |text: &str| {
        let rows = fixture_rows(text);
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('❯'))
            .unwrap_or(rows.len().saturating_sub(1));
        screen_json(&rows, (caret, 2), 20)
    };
    let rig = relaunch_rig("fence", false, Knobs::default());
    rig.kernel.relaunched.store(true, Ordering::SeqCst);
    let fence = |screen: String| {
        rig.relaunched_shows(Some(screen), None);
        let mut c = connect(&rig.opts, TAB).expect("the instance");
        typing_fence(&mut c, TAB).map(|_| ())
    };
    for (name, text, want) in [
        ("box", fx::BOX_SHELL, "box"),
        ("busy", fx::BUSY_1_4_10, "busy"),
        ("stopping", fx::INTERRUPTING, "busy"),
        ("draft", fx::DRAFT_1_4_10, "draft"),
    ] {
        assert_eq!(fence(at_caret(text)), Err(want), "{name}");
    }
    assert_eq!(fence(muse_idle()), Ok(()), "the control: at rest");
}

/// No continuation is typed into a relaunched Muse that is not idle, empty, and unheld.
#[test]
fn no_continuation_into_a_busy_drafted_or_held_muse() {
    use aterm_phase::muse::fixtures as fx;
    let at_caret = |text: &str| {
        let rows = fixture_rows(text);
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('❯'))
            .unwrap_or(rows.len().saturating_sub(1));
        screen_json(&rows, (caret, 2), 20)
    };
    let unknown = {
        let mut rows = fixture_rows(fx::END_OF_TURN_1_4_10);
        let reply = rows
            .iter()
            .rposition(|r| r.starts_with("◆ "))
            .expect("the reply");
        rows[reply + 1] = "Something unmeasured".to_string();
        assert_eq!(
            aterm_phase::muse::phase(&rows),
            (aterm_phase::Phase::Idle, false),
            "the screen's own precondition"
        );
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('❯'))
            .expect("caret");
        screen_json(&rows, (caret, 2), 20)
    };
    let person = "OK schema=1 hold=0 hand=- integration=on human_ms=400";
    let cases: Vec<(&str, Option<String>, Option<&str>, Knobs)> = vec![
        (
            "busy",
            Some(at_caret(fx::BUSY_1_4_10)),
            None,
            Knobs::default(),
        ),
        (
            "draft",
            Some(at_caret(fx::DRAFT_1_4_10)),
            None,
            Knobs::default(),
        ),
        ("box", Some(at_caret(fx::BOX_SHELL)), None, Knobs::default()),
        (
            "stopping",
            Some(at_caret(fx::INTERRUPTING)),
            None,
            Knobs::default(),
        ),
        ("no-evidence", Some(unknown), None, Knobs::default()),
        ("person", None, Some(person), Knobs::default()),
        (
            "out-of-tab",
            None,
            None,
            Knobs {
                out_of_tab: true,
                ..Knobs::default()
            },
        ),
        (
            "lockless",
            None,
            None,
            Knobs {
                new_lockless: true,
                ..Knobs::default()
            },
        ),
    ];
    for (name, screen, status, knobs) in cases {
        let rig = relaunch_rig(name, false, knobs);
        rig.relaunched_shows(screen, status);
        let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(rig.lines(), 1, "{name}: {:#?}", rig.typed());
        assert_eq!(rig.continuations(), 0, "{name}: {r:?} {:#?}", rig.typed());
        assert_ne!(r.step, "continued", "{name}");
    }
    let control = relaunch_rig("plain", false, Knobs::default());
    let r = control.attempt(
        &control.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "continued", "{r:?}");
    assert_eq!(control.continuations(), 1);
}

/// Only a child `muse` of the shell resuming the expected session is adopted.
#[test]
fn only_the_lines_own_muse_is_adopted() {
    let other_session = Knobs {
        new_argv: Some(vec![
            "/p/agents/muse".to_string(),
            "resume".to_string(),
            S2.to_string(),
        ]),
        ..Knobs::default()
    };
    let orphan = Knobs {
        new_orphan: true,
        ..Knobs::default()
    };
    for (name, knobs) in [("other-session", other_session), ("orphan", orphan)] {
        for hand_back in [true, false] {
            let rig = relaunch_rig(name, hand_back, knobs.clone());
            let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
            assert_eq!(r.step, "wait:resume", "{name} hand_back={hand_back}: {r:?}");
            assert_eq!(rig.state().resumed_pid, 0, "{name}: nothing adopted");
            assert_eq!(rig.state().relaunched_pid, 0, "{name}: nothing found");
            assert_eq!(rig.continuations(), 0, "{name}");
        }
    }
    let control = relaunch_rig("ours", true, Knobs::default());
    let r = control.attempt(
        &control.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!((r.step.as_str(), r.pid), ("adopted", NEW), "{r:?}");
    assert_eq!(control.state().resumed_pid, NEW);
}

/// Session lock holders are re-checked immediately before typing the relaunch line.
#[test]
fn the_sessions_holders_are_read_again_right_before_the_line() {
    for (name, knobs, want) in [
        (
            "late-holder",
            Knobs {
                holder_from_read: Some(2),
                ..Knobs::default()
            },
            "wait:conversation-owner-ambiguous",
        ),
        (
            "unreadable",
            Knobs {
                holders_unreadable: true,
                ..Knobs::default()
            },
            "wait:session-held",
        ),
    ] {
        let rig = relaunch_rig(name, true, knobs);
        let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(rig.lines(), 0, "{name}: {:#?}", rig.typed());
    }
    let control = relaunch_rig("free", true, Knobs::default());
    let r = control.attempt(
        &control.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "adopted", "{r:?}");
    assert_eq!(control.lines(), 1);
}

/// Explicit restart is not supported for Muse Code.
#[test]
fn the_explicit_restart_is_not_carried_for_a_muse() {
    let rig = Rig::new("restart", 137, true, false, Log::Crash, true);
    let snap = rig.snap(Some(S1));
    assert!(!crate::harness::relaunch::carries_restart(
        &rig.opts.home,
        &snap
    ));
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "continued", "{r:?}");
}

/// Each agent binary routes to its own snapshot lane, or to none when it has no lane.
#[test]
fn each_image_goes_to_its_own_lanes_reader() {
    use crate::harness::relaunch::{SnapshotLane, snapshot_lane};
    for (image, lane) in [
        ("/p/store/agy/1/bin/agy", SnapshotLane::Agy),
        ("/opt/homebrew/bin/gemini", SnapshotLane::None),
        (
            "/p/store/muse/2001004010006427002/bin/muse",
            SnapshotLane::Muse,
        ),
        ("/p/agents/muse", SnapshotLane::Muse),
        ("/usr/local/bin/codex", SnapshotLane::None),
        ("/Users//u/.local/bin/claude", SnapshotLane::Claude),
    ] {
        assert_eq!(snapshot_lane(Some(Path::new(image))), lane, "{image}");
    }
    assert_eq!(snapshot_lane(None), SnapshotLane::Claude);
}

/// Conformance test of `look_at_exit`, `exit_record`, and `after_exit` against `HarnessMuseExitWitness`.
#[test]
fn tier1_the_real_muse_exit_read_conforms_to_the_model() {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let m = aterm_models_harness::harness_muse_exit_witness_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let draw = |name: String, exit: i64, word: bool, later: bool| {
        let status = match exit {
            1 => 137,
            2 => 0,
            3 => 143,
            _ => 1,
        };
        let log = if matches!(exit, 2 | 3) {
            Log::Clean
        } else {
            Log::Crash
        };
        Rig::new(&name, status, word, later, log, exit != 2)
    };
    let kept = |kept: i64| match kept {
        1 => ExitRecord::Crashed,
        2 => ExitRecord::Removed,
        4 => ExitRecord::Refused,
        _ => ExitRecord::Unread,
    };
    let mut seen = BTreeSet::new();
    let mut attempted = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let (mut looks, mut attempts) = (0, 0);
    let mut caught: BTreeMap<&str, usize> = BTreeMap::new();
    while let Some(st) = queue.pop_front() {
        if !seen.insert(st.clone()) {
            continue;
        }
        for action in &m.actions {
            let mut next = st.clone();
            if m.fire(action.name, &mut next) {
                queue.push_back(next);
            }
        }
        if st["exit"] == 0 || st["decided"] > 0 {
            continue;
        }
        let witness = (st["witness"] == 1).then_some(S1);
        if st["seen"] == 0 {
            looks += 1;
            let word = st["word"] == 1;
            let later = st["later"] == 1;
            let look = if word && !later { "Look" } else { "LookGone" };
            let mut after = st.clone();
            assert!(m.fire(look, &mut after), "{look} at {st:?}");
            let rig = draw(format!("look-{looks}"), st["exit"], word, later);
            let snap = rig.snap(witness);
            let left = exit_record(
                || Some(look_at_exit(&rig.opts, &snap, &rig.kernel)),
                |_| true,
            );
            assert_eq!(left, kept(after["kept"]), "the look at {st:?}");
            for mutant in ["LookAsClaude", "RefusalAsCrash"] {
                let mut wrong = st.clone();
                if buggy.fire(mutant, &mut wrong) && wrong["kept"] != after["kept"] {
                    assert_ne!(left, kept(wrong["kept"]), "{mutant} at {st:?}");
                    *caught.entry(mutant).or_default() += 1;
                }
            }
            continue;
        }
        let (word, later) = if st["kept"] == 3 {
            (st["word"] == 1, st["later"] == 1)
        } else {
            (true, false)
        };
        if !attempted.insert((st["exit"], st["witness"], st["kept"], word, later)) {
            continue;
        }
        attempts += 1;
        let mut after = st.clone();
        assert!(m.fire("Decide", &mut after), "Decide at {st:?}");
        let rig = draw(format!("attempt-{attempts}"), st["exit"], word, later);
        let r = rig.attempt(&rig.snap(witness), &kept(st["kept"]), ExitCause::Exit);
        let relaunched = r.step == "continued";
        assert_eq!(
            relaunched,
            after["decided"] == 1,
            "the attempt at {st:?}: {r:?}"
        );
        for mutant in ["DecideOnLook", "DecideFresh"] {
            let mut wrong = st.clone();
            if buggy.fire(mutant, &mut wrong) && wrong["decided"] != after["decided"] {
                assert_ne!(relaunched, wrong["decided"] == 1, "{mutant} at {st:?}");
                *caught.entry(mutant).or_default() += 1;
            }
        }
    }
    assert_eq!(looks, 24, "{seen:?}");
    assert!(attempts > 0, "no attempt replayed");
    for mutant in [
        "LookAsClaude",
        "RefusalAsCrash",
        "DecideOnLook",
        "DecideFresh",
    ] {
        assert!(
            caught.get(mutant).copied().unwrap_or(0) >= 1,
            "{mutant} is never caught: {caught:?} ({attempts} attempts)"
        );
    }
}

#[path = "upgrade_muse_live_drive_tests.rs"]
mod live_tests;

/// Muse Code's gate reads the composer's first glyph as the loop does
/// ([`crate::supervise::screen::composer_glyph_cell`]): where a caret is drawn inverse over it,
/// the glyph after it says whether the text is the hint (`676c74`, Muse Code's placeholder ink) or
/// a draft. NEGATIVE CONTROLS: the same caret over plain text is a draft, and with no caret drawn
/// the first glyph is read alone, as before.
#[test]
fn the_gate_reads_the_composer_past_a_drawn_caret_as_the_loop_does() {
    let rows = fixture_rows(aterm_phase::muse::fixtures::DRAFT_1_4_10);
    let (caret, _) = aterm_phase::muse::composer_draft(&rows).expect("a composer");
    let scr = super::super::Screen {
        rows,
        cursor: Some((caret, 2)),
        seq: 20,
        first: 0,
        generation: None,
        human: crate::supervise::screen::HumanInput::Never,
    };
    let caret_cell = "OK a 111318 d0d0d0 inverse";
    for (cells, empty) in [
        (vec![caret_cell, "OK %20 676c74 111318 none"], true),
        (vec![caret_cell, "OK %20 d0d0d0 111318 none"], false),
        (vec!["OK a 676c74 111318 none"], true),
        (vec!["OK a d0d0d0 111318 none"], false),
    ] {
        let asked: Vec<String> = (0..cells.len())
            .map(|k| format!("@{TAB} cell {caret} {}", 2 + k))
            .collect();
        let (mut c, server) = super::super::tests::fake_tab(cells.clone());
        assert_eq!(composer_empty(&mut c, TAB, &scr), empty, "{cells:?}");
        drop(c);
        assert_eq!(server.join().expect("server"), asked, "{cells:?}");
    }
}
