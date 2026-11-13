// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Muse Code upgrade driver tests over scripted screens and kernel.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::super::{Job, parse_exit_status};
use super::*;
use crate::harness::relaunch::{AgentRun, ExitCause, ExitRecord, MuseRun, Snapshot, exit_record};
use crate::harness::upgrade_drive::load;
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
    /// The shell's screen before the relaunch line is typed.
    before_screen: Option<String>,
    /// The relaunched Muse's screen.
    screen: Option<String>,
    /// The tab's `status` before the relaunch line is typed.
    before_status: Option<String>,
    /// The tab's `status`.
    status: Option<String>,
    /// The tab's command blocks.
    blocks: Option<String>,
    /// The relaunched screen from this read of it on (1 the first): what a person began as a look
    /// went on.
    screen_from: Option<(usize, String)>,
    /// Reads of the relaunched screen so far.
    screen_reads: usize,
    /// What a line of the harness's is answered, in place of its being settled.
    harness_answer: Option<String>,
    /// The session's log a line of the harness's reaches as it is typed.
    harness_reaches: Option<PathBuf>,
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
        let mut after = w.after.lock().expect("after");
        let served = |what: &Option<String>, before: &String| match what {
            Some(now) if relaunched => now.clone(),
            _ => before.clone(),
        };
        match verb {
            "sessions" => format!("OK 1\nlocal {TAB} 0 idle muse"),
            "text" if relaunched => {
                after.screen_reads += 1;
                match &after.screen_from {
                    Some((from, screen)) if after.screen_reads >= *from => {
                        format!("OK {screen}")
                    }
                    _ => format!("OK {}", served(&after.screen, &w.after_relaunch)),
                }
            }
            "text" => format!(
                "OK {}",
                after
                    .before_screen
                    .as_ref()
                    .unwrap_or(&w.after_exit)
                    .clone()
            ),
            "cell" => "OK ❯ ccd3db 1e1e1e none".to_string(),
            "status" => served(
                &after.status,
                after.before_status.as_ref().unwrap_or(&w.status),
            ),
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
                if line.contains(upgrade::HARNESS_MARK) {
                    if let Some(log) = &after.harness_reaches {
                        log_told(log, upgrade::HARNESS_MARK);
                    }
                    if let Some(answer) = &after.harness_answer {
                        return answer.clone();
                    }
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

    fn restart_background(&self, _: u32) -> Vec<String> {
        crate::harness::upgrade_drive::tests::unstated("background")
    }

    fn restart_held(&self, _: u32) -> Option<Vec<crate::harness::upgrade::Held>> {
        crate::harness::upgrade_drive::tests::unstated("held")
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
        match pid {
            SHELL => Some(atpkg::caller_shell::ProcArgs {
                exec_path: "/bin/zsh".to_string(),
                argv: vec!["-zsh".to_string()],
                env: Vec::new(),
            }),
            TUI if !self.knobs.noappear => Some(atpkg::caller_shell::ProcArgs {
                exec_path: self.exe.to_string_lossy().into_owned(),
                argv: self.knobs.new_argv.clone().unwrap_or_else(|| {
                    vec![
                        self.exe.to_string_lossy().into_owned(),
                        "resume".to_string(),
                        S1.to_string(),
                    ]
                }),
                env: vec![format!("ATERM_PARENT_SESSION_ID={TAB}")],
            }),
            _ => None,
        }
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
            account_roster: None,
            sock: Some(sock),
            token: None,
            only_sid: None,
            dry_run: false,
            human_grace_s: 120,
            hand_back: false,
            background: false,
            aterm_state: None,
            ask: true,
            usage_tap: None,
            instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
            agents_off: Vec::new(),
            land: false,
            tell: crate::harness::upgrade_drive::Tell::Off,
            press: None,
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
            lifetime: None,
            launch_env: None,
            auth_observed: true,
            route: crate::harness::accounts::AuthRoute::Outside,
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
            "'--base-url' 'http://127.0.0.1:9' '--yolo' '--contributor-mode' 'off' 'resume' '{S1}'"
        )),
        "{line}"
    );
    assert!(
        !line.contains("/store/muse/"),
        "never the store path: {line}"
    );
    assert!(!line.contains("my-session-name"), "{line}");
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

/// An exit an Autopilot carries ([`ExitCause::Carried`]) comes back however its shell read it,
/// a signal's (`SIGINT`, `SIGTERM`), an own exit's or an unwitnessed one; a start Muse Code
/// refused is a refusal the owner hears, never relaunched.
#[test]
fn an_exit_an_autopilot_carries_comes_back_unless_its_start_was_refused() {
    for (status, want) in [
        (0, "continued"),
        (130, "continued"),
        (143, "continued"),
        (137, "continued"),
        (1, "refused:refused-start"),
        (2, "refused:refused-start"),
    ] {
        let rig = Rig::new("carried", status, true, false, Log::Crash, true);
        let snap = rig.snap(Some(S1));
        let left = exit_record(
            || Some(look_at_exit(&rig.opts, &snap, &rig.kernel)),
            |_| true,
        );
        let r = rig.attempt(&snap, &left, ExitCause::Carried);
        assert_eq!(r.step, want, "{status}: {left:?} {r:?}");
        if want.starts_with("refused:") {
            assert!(rig.typed().is_empty(), "{status}: {:#?}", rig.typed());
        }
    }
    let rig = Rig::new("carried-unwitnessed", 137, false, false, Log::None, true);
    let snap = rig.snap(Some(S1));
    let left = exit_record(
        || Some(look_at_exit(&rig.opts, &snap, &rig.kernel)),
        |_| true,
    );
    assert_eq!(
        rig.attempt(&snap, &left, ExitCause::Exit).step,
        "ended:exit-unwitnessed",
        "the control"
    );
    assert_eq!(
        rig.attempt(&snap, &left, ExitCause::Carried).step,
        "continued"
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

    /// The session's log.
    fn log(&self) -> PathBuf {
        self.lock.parent().expect("the session").join(mz::LOG)
    }

    /// The stand-in's answers to the carry-on's own steps, each as a test sets it.
    fn on_harness_line(&self, answer: Option<&str>, reaches_log: bool) {
        let log = self.log();
        let mut after = self.after.lock().expect("after");
        after.harness_answer = answer.map(str::to_string);
        after.harness_reaches = reaches_log.then_some(log);
    }

    /// The relaunched screen turns to `screen` at the `from`th read from now.
    fn shows_from(&self, from: usize, screen: String) {
        let mut after = self.after.lock().expect("after");
        after.screen_reads = 0;
        after.screen_from = Some((from, screen));
    }

    /// The relaunch's record, its line `age` seconds ago.
    fn relaunch_aged(&self, age: u64) {
        let mut st = self.state();
        st.phase = Phase::Relaunched {
            at_s: now_s().saturating_sub(age),
        };
        st.relaunched_at = 0;
        save(&self.opts, &key(TAB), &st);
    }
}

/// A row of a session's log: a run started on `prompt`, dated past any test's relaunch.
fn log_told(log: &Path, prompt: &str) {
    use std::io::Write as _;
    let at_us = (now_s() + 60) * 1_000_000;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(log)
        .expect("the log");
    writeln!(
        file,
        r#"{{"schema_version":1,"sequence":90,"recorded_at":{at_us},"record_type":"event","payload_type":"runtime.session","payload":{{"kind":"run","run_id":"r9","event":{{"kind":"started","prompt":"{prompt} go"}}}}}}"#
    )
    .expect("a row");
}

/// The screen `text` draws, its caret on the composer row.
fn at_caret(text: &str) -> String {
    let rows = fixture_rows(text);
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .unwrap_or(rows.len().saturating_sub(1));
    screen_json(&rows, (caret, 2), 20)
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
        title: None,
        human_seq: None,
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

/// Muse Code's gate reads the cursor as the loop does ([`crate::supervise::screen::cursor_home`]): the
/// hint's ink on the first glyph is no draft only with the cursor home, on the caret's row or, on a
/// lone line, the row under it. Anywhere else, or with no cursor on the rows read, it says neither and
/// no cell is asked, as there the loop holds. A blank first row over text on a later row is a draft
/// with no cell asked, as to the loop and Claude Code's gate, whatever a blank's cell is drawn in. A
/// cell that does not say, refused or inside an unfinished frame, is a draft too.
/// NEGATIVE CONTROLS: the gate asking for the cursor exactly at the caret on a lone line, as on
/// `bd67622c8c`, and a suggestion the loop types over holds this gate on the row under it and on two
/// rows; `composer_holds_no_draft` asking a blank first row's cell, as on `98b9c4d903`, and a person's
/// two rows read as no draft where a blank is drawn in the hint's ink, so the upgrade's line is typed
/// into them; `composer_empty` taking a cell that does not say for grey, and the two replies below
/// read as no draft, so the upgrade's line is typed over a draft it could not read.
#[test]
fn the_gate_reads_the_cursor_as_the_loop_does() {
    let lone = fixture_rows(aterm_phase::muse::fixtures::DRAFT_1_4_10);
    let (caret, lines) = aterm_phase::muse::composer_draft(&lone).expect("a composer");
    assert_eq!(lines.len(), 1, "{lines:?}");
    let mut two = lone.clone();
    two.insert(caret + 1, "  and the release notes".to_string());
    assert_eq!(
        aterm_phase::muse::composer_draft(&two).map(|(_, l)| l.len()),
        Some(2)
    );
    let mut blank_first = two.clone();
    blank_first[caret] = "❯".to_string();
    assert_eq!(
        aterm_phase::muse::composer_draft(&blank_first).map(|(_, l)| l[0].is_empty()),
        Some(true)
    );
    let hint = "OK %20 676c74 111318 none";
    let screen_of = |rows: &[String], cursor| super::super::Screen {
        rows: rows.to_vec(),
        cursor,
        seq: 20,
        first: 0,
        generation: None,
        human: crate::supervise::screen::HumanInput::Never,
        title: None,
        human_seq: None,
    };
    for (rows, cursor, empty) in [
        (&lone, Some((caret, 2)), true),
        (&lone, Some((caret + 1, 2)), true),
        (&lone, Some((caret, 9)), false),
        (&lone, Some((0, 2)), false),
        (&lone, None, false),
        (&two, Some((caret, 2)), true),
        (&two, Some((caret + 1, 2)), false),
        (&blank_first, Some((caret, 2)), false),
        (&blank_first, Some((caret + 1, 2)), false),
    ] {
        let scr = screen_of(rows, cursor);
        let (mut c, server) = super::super::tests::fake_tab(vec![hint]);
        assert_eq!(composer_empty(&mut c, TAB, &scr), empty, "{cursor:?}");
        drop(c);
        let asked: Vec<String> = empty
            .then(|| format!("@{TAB} cell {caret} 2"))
            .into_iter()
            .collect();
        assert_eq!(server.join().expect("server"), asked, "{cursor:?}");
    }
    for reply in [
        "ERR no such verb: cell",
        "OK %20 676c74 111318 none frame=open",
    ] {
        let scr = screen_of(&lone, Some((caret, 2)));
        let (mut c, server) = super::super::tests::fake_tab(vec![reply]);
        assert!(!composer_empty(&mut c, TAB, &scr), "{reply}");
        drop(c);
        server.join().expect("server");
    }
    // The prompt-history search (measured on 1.4.14), over an old prompt and over an empty
    // composer, its cursor on its query under the composer's bottom rule, and in windows of 34, 26
    // and 22 columns, where its row wraps and drops its scope: no composer the upgrade's line could
    // go into, no cell asked.
    // NEGATIVE CONTROL: `screen::composer_holds_no_draft` without its cursor rule
    // (`cursor_under_composer`), and the empty one reads empty.
    for text in [
        aterm_phase::muse::fixtures::HISTORY_SEARCH,
        aterm_phase::muse::fixtures::HISTORY_SEARCH_EMPTY,
        aterm_phase::muse::fixtures::HISTORY_SEARCH_NO_MATCH_34COL,
        aterm_phase::muse::fixtures::HISTORY_SEARCH_26COL,
        aterm_phase::muse::fixtures::HISTORY_SEARCH_NO_MATCH_22COL,
    ] {
        let rows = fixture_rows(text);
        let scr = screen_of(&rows, aterm_phase::prompt::fixtures::cursor(text));
        let (mut c, server) = super::super::tests::fake_tab(vec![hint]);
        assert!(!composer_empty(&mut c, TAB, &scr), "{rows:#?}");
        drop(c);
        assert_eq!(server.join().expect("server"), Vec::<String>::new());
    }
}

#[test]
fn an_in_flight_muse_relaunch_waits_out_a_hand_and_counts_staleness_from_seen_exit() {
    let rig = Rig::new("hand-and-exit-clock", 137, true, false, Log::Crash, true);
    std::fs::create_dir_all(state_dir(&rig.opts)).expect("state");
    let snap = rig.snap(Some(S1));
    let now = now_s();
    let signal = now - STALE_S - 60;
    let base = St {
        agent: Agent::Muse,
        thread: S1.to_string(),
        muse_data: rig.data.to_string_lossy().into_owned(),
        argv: snap.argv.clone(),
        cwd: WORKSPACE.to_string(),
        twin: rig.twin.to_string_lossy().into_owned(),
        pid: TUI,
        shell: SHELL,
        proc_start: snap.start.clone(),
        tab: TAB.to_string(),
        from: VERSION.to_string(),
        to: VERSION.to_string(),
        source: super::super::SAME_PROGRAM.to_string(),
        phase: Phase::Exiting { at_s: signal },
        ..St::default()
    };

    // 1. A hand standing on the tab (`hold=1`) is waited out before `expired` is evaluated,
    // even when `at_s` is past `STALE_S`, and stamps both `hand_seen_s` and `exited_at`.
    save(&rig.opts, &key(TAB), &base);
    rig.after.lock().expect("after").before_status =
        Some("OK schema=1 hold=1 hand=- integration=on".to_string());
    let held = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(held.step, WAIT_HAND_LIFTS, "{held:?}");
    assert_eq!(rig.lines(), 0);
    let st = rig.state();
    assert_eq!(st.phase, Phase::Exiting { at_s: signal });
    assert!(st.hand_seen_s >= now && st.exited_at >= now, "{st:?}");

    // Once the hand lifts, `first_mark` requires proof no keys arrived while the hand stood;
    // once `unkeyed_ms` predates `signal`, the relaunch carries on from the lifted hand's clock.
    rig.after.lock().expect("after").before_status = None;
    let unconfirmed_keys = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(
        unconfirmed_keys.step, "wait:keyed-while-held",
        "{unconfirmed_keys:?}"
    );
    rig.after.lock().expect("after").before_screen = Some(shell_after().replace(
        r#""human_ms":null"#,
        r#""human_ms":null,"unkeyed_ms":86400000"#,
    ));
    let carried = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(carried.step, "continued", "{carried:?}");
    assert_eq!(rig.lines(), 1);

    // 2. Without a hand, an exit seen recently (`exited_at` within `STALE_S`) is not stale
    // even when `at_s` is older than `STALE_S`; one never seen gone (`exited_at == 0`) fails
    // `stale-exit` and stamps `exited_at`.
    rig.kernel.relaunched.store(false, Ordering::SeqCst);
    let fresh_exit = St {
        exited_at: now - 10,
        ..base.clone()
    };
    save(&rig.opts, &key(TAB), &fresh_exit);
    let r = carry_in_flight_with(
        &rig.opts,
        Report {
            rung: None,
            pid: TUI,
            tab: TAB.to_string(),
            session: key(TAB),
            from: VERSION.to_string(),
            to: VERSION.to_string(),
            step: String::new(),
        },
        fresh_exit,
        &key(TAB),
        &rig.kernel,
    );
    assert_eq!(r.step, "continued", "{r:?}");

    save(&rig.opts, &key(TAB), &base);
    let expired_r = carry_in_flight_with(
        &rig.opts,
        Report {
            rung: None,
            pid: TUI,
            tab: TAB.to_string(),
            session: key(TAB),
            from: VERSION.to_string(),
            to: VERSION.to_string(),
            step: String::new(),
        },
        base,
        &key(TAB),
        &rig.kernel,
    );
    assert_eq!(expired_r.step, "failed:stale-exit", "{expired_r:?}");
    let failed = rig.state();
    assert_eq!(failed.phase, Phase::Failed("stale-exit".to_string()));
    assert!(failed.exited_at >= now, "{failed:?}");
}

/// The relaunched Muse Code's settled screen with `said` as its last reply: the resumed session
/// it draws (its warning of the abnormal end above the idle composer), the reply swapped.
fn resumed_saying(said: &str) -> String {
    let mut rows = fixture_rows(aterm_phase::muse::fixtures::RESUMED_AFTER_CRASH);
    let last = rows
        .iter()
        .rposition(|r| r.starts_with("◆ Hi!"))
        .expect("the last reply");
    rows[last] = format!("◆ {said}");
    assert_eq!(
        aterm_phase::muse::said_tail(&rows).as_deref(),
        Some(said),
        "the screen's own precondition"
    );
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("a caret row");
    screen_json(&rows, (caret, 2), 20)
}

/// The carry-on a relaunch `rig` adopted (`after_exit` on the window's `hand_back`) owes at the
/// loop's idle point, with the record's cause `cause` and `land` as the host passes it
/// (`relaunch::resume_as`).
fn carried(rig: &Rig, cause: &str, land: bool) -> Report {
    let adopted = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(adopted.step, "adopted", "{adopted:?}");
    assert_eq!(rig.continuations(), 0, "the window's relaunch types none");
    carry_adopted(rig, cause, land)
}

/// Carry an already adopted process, allowing a test to change the later kernel and screen
/// observations without changing what the successful adoption itself saw.
fn carry_adopted(rig: &Rig, cause: &str, land: bool) -> Report {
    let mut st = rig.state();
    st.cause = cause.to_string();
    let opts = Opts {
        hand_back: false,
        land,
        ..rig.opts.clone()
    };
    carry_in_flight_with(
        &opts,
        Report {
            rung: None,
            pid: TUI,
            tab: TAB.to_string(),
            session: key(TAB),
            from: VERSION.to_string(),
            to: VERSION.to_string(),
            step: String::new(),
        },
        st,
        &key(TAB),
        &rig.kernel,
    )
}

/// AT THE LOOP'S IDLE POINT A MUSE CODE RELAUNCH NO STEP ADOPTED KEEPS ITS LANE'S OWN CARRY-ON:
/// the late adoption there ([`crate::harness::relaunch::at_point`]) is Claude Code's lane's
/// alone, so the Muse Code that comes up only after the relaunch's own step gave up waiting for it
/// (`wait:resume`) is carried on (`continued`, its continuation typed), not handed back.
/// NEGATIVE CONTROL: a point read as a task done under Land hands it back (`adopted`, nothing
/// typed). MUTANT: with `carried_here` out of `relaunch::unadopted`, the first case is adopted
/// and types nothing.
#[test]
fn a_late_muse_relaunch_at_the_loops_point_keeps_its_own_carry_on() {
    for done_under_land in [false, true] {
        let case = format!("done under Land {done_under_land}");
        let never = Knobs {
            noappear: true,
            ..Knobs::default()
        };
        let mut rig = relaunch_rig(&format!("point-late-{done_under_land}"), true, never);
        rig.line_ran(None);
        let gave_up = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(
            gave_up.step, "wait:resume",
            "PRECONDITION: {case}: {gave_up:?}"
        );
        let st = rig.state();
        assert!(
            matches!(st.phase, Phase::Relaunched { .. })
                && st.relaunched_pid == 0
                && st.resumed_pid == 0,
            "PRECONDITION: {case}: no step adopted its agent: {st:?}"
        );
        assert_eq!(rig.lines(), 1, "PRECONDITION: {case}: the line typed");
        // The Muse Code comes up now, after its relaunch's own step gave up.
        rig.kernel.knobs = Knobs::default();
        rig.relaunched_shows(None, None);
        let at = crate::harness::relaunch::at_point(
            &Opts {
                hand_back: false,
                ..rig.opts.clone()
            },
            &st,
            done_under_land,
        );
        let r = carry_in_flight_with(
            &at,
            Report {
                rung: None,
                pid: TUI,
                tab: TAB.to_string(),
                session: key(TAB),
                from: VERSION.to_string(),
                to: VERSION.to_string(),
                step: String::new(),
            },
            st,
            &key(TAB),
            &rig.kernel,
        );
        let (step, continuations) = if done_under_land {
            ("adopted", 0)
        } else {
            ("continued", 1)
        };
        assert_eq!((r.step.as_str(), r.pid), (step, NEW), "{case}: {r:?}");
        assert_eq!(
            rig.continuations(),
            continuations,
            "{case}: {:#?}",
            rig.typed()
        );
        assert_eq!(rig.lines(), 1, "{case}: one relaunch line");
    }
}

/// UNDER LAND A RELAUNCH IS CARRIED ON AS UNDER CRUISE (owner report 2026-11-01), whether the host
/// passes Land (`Opts::land`) or the tab's status names it: one continuation that says the agent
/// was restarted, and no done check of Cruise's ([`crate::harness::relaunch::carries_under_land`]).
#[test]
fn a_relaunch_under_land_is_carried_on_as_under_cruise() {
    use crate::harness::relaunch::{CAUSE_EXIT, CAUSE_HOST};
    let land_status = "OK schema=1 hold=0 hand=- integration=on autopilot=land";
    for cause in [CAUSE_EXIT, CAUSE_HOST] {
        for (land, by_status) in [(true, false), (false, true), (false, false)] {
            let case = format!("cause `{cause}`, land {land}, by status {by_status}");
            let rig = relaunch_rig(
                &format!("land-{land}-{by_status}-{}", cause.len()),
                true,
                Knobs::default(),
            );
            if by_status {
                rig.relaunched_shows(None, Some(land_status));
            }
            let r = carried(&rig, cause, land);
            let st = rig.state();
            assert_eq!(r.step, "continued", "{case}: {r:?}");
            assert_eq!(rig.continuations(), 1, "{case}: {:#?}", rig.typed());
            assert_eq!(st.phase, Phase::Done, "{case}");
            assert_eq!(rig.lines(), 1, "{case}: one relaunch line");
        }
    }
    // The relaunch the host carries at once (no window adoption) keeps the rule too.
    for land in [true, false] {
        let mut rig = relaunch_rig(&format!("land-direct-{land}"), false, Knobs::default());
        rig.opts.land = land;
        let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, "continued", "direct, land {land}: {r:?}");
        assert_eq!(rig.continuations(), 1, "direct, land {land}");
    }
    for (name, knobs, composer) in [
        (
            "lockless",
            Knobs {
                new_lockless: true,
                ..Knobs::default()
            },
            true,
        ),
        (
            "lock-unreadable",
            Knobs {
                holders_unreadable: true,
                ..Knobs::default()
            },
            true,
        ),
        (
            "other-foreground",
            Knobs {
                out_of_tab: true,
                ..Knobs::default()
            },
            true,
        ),
        ("no-composer-holds-lock", Knobs::default(), false),
        (
            "no-composer-no-lock",
            Knobs {
                new_lockless: true,
                ..Knobs::default()
            },
            false,
        ),
        (
            "no-composer-process-gone",
            Knobs {
                new_dies: true,
                ..Knobs::default()
            },
            false,
        ),
    ] {
        let mut rig = relaunch_rig(name, true, Knobs::default());
        let adopted = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(adopted.step, "adopted", "{name}: {adopted:?}");
        rig.kernel.knobs = knobs;
        if !composer {
            rig.relaunched_shows(
                Some(screen_json(&["Booting Muse Code".to_string()], (0, 0), 20)),
                None,
            );
        }
        let r = carry_adopted(&rig, CAUSE_EXIT, true);
        assert_ne!(r.step, "continued", "{name}: {r:?}");
        let st = rig.state();
        assert_ne!(st.phase, Phase::Done, "{name}: unproved resume is pending");
        assert!(
            st.resumed_on.is_empty(),
            "{name}: no proved resumed version"
        );
        assert_eq!(rig.continuations(), 0, "{name}: {:#?}", rig.typed());
        if name != "no-composer-process-gone" {
            rig.kernel.knobs = Knobs::default();
            rig.relaunched_shows(Some(muse_idle()), None);
            let r = carry_adopted(&rig, CAUSE_EXIT, true);
            assert_eq!(r.step, "continued", "{name}: later proof: {r:?}");
            assert_eq!(rig.state().phase, Phase::Done, "{name}: later proof");
            assert_eq!(rig.lines(), 1, "{name}: no duplicate relaunch");
            assert_eq!(rig.continuations(), 1, "{name}: the prompt, once");
        }
    }
}

/// A MUSE CODE RELAUNCH INTO A DONE REPORT ASKS THE DONE CHECK, as Claude Code's does
/// (`a_relaunch_into_a_done_claim_asks_the_done_check`): the resumed session's last
/// reply claims the work done, so the facts of the relaunch are typed with Cruise's done check
/// (`continued:check`, the reversible clause where the claim names an irreversible act) in place
/// of `upgrade::CARRY_ON`. NEGATIVE CONTROL: a reply of work going on still gets the carry-on.
/// MUTANT: with `relaunch::said_on_screen` answering no claim, the done reports get the carry-on.
#[test]
fn a_relaunch_into_a_done_claim_asks_the_done_check() {
    use crate::harness::relaunch::{CAUSE_EXIT, CAUSE_HOST};
    use crate::supervise::policy::turn_end::{DONE_CHECK, DONE_CHECK_REVERSIBLE, HostTurn};
    let done = "All tasks are complete and verified.\nNothing left: the parser and its tests are on origin/main.";
    let forced = "All tasks are complete and verified. Should I force-push the rewritten history to \
                  origin main?\nNothing left: the parser and its tests are on origin/main.";
    let going = "Committed the parser; the lexer is next.";
    for cause in [CAUSE_EXIT, CAUSE_HOST] {
        for (said, check, irreversible) in [
            (done, true, false),
            (forced, true, true),
            (going, false, false),
        ] {
            let case = format!("cause `{cause}`: {said}");
            let rig = relaunch_rig(
                &format!("said-{}-{}", said.len(), cause.len()),
                true,
                Knobs::default(),
            );
            rig.relaunched_shows(Some(resumed_saying(said)), None);
            let r = carried(&rig, cause, false);
            let typed: Vec<String> = rig
                .typed()
                .into_iter()
                .filter(|l| l.contains(upgrade::HARNESS_MARK))
                .collect();
            assert_eq!(typed.len(), 1, "{case}: {:#?}", rig.typed());
            assert!(typed[0].contains("Relaunched"), "{case}: the facts kept");
            if check {
                assert_eq!(r.step, "continued:check", "{case}: {r:?}");
                assert!(typed[0].contains(DONE_CHECK), "{case}: {typed:?}");
                assert!(!typed[0].contains(upgrade::CARRY_ON), "{case}: {typed:?}");
                assert_eq!(
                    typed[0].contains(DONE_CHECK_REVERSIBLE),
                    irreversible,
                    "{case}: {typed:?}"
                );
                assert_eq!(
                    super::super::host_turn(&r.step),
                    Some(HostTurn::Check),
                    "{case}"
                );
            } else {
                assert_eq!(r.step, "continued", "{case}: {r:?}");
                assert!(typed[0].contains(upgrade::CARRY_ON), "{case}: {typed:?}");
                assert_eq!(
                    super::super::host_turn(&r.step),
                    Some(HostTurn::CarryOn),
                    "{case}"
                );
            }
        }
    }
}

/// The word a Muse Code's exit left, read off a window that never answers its block, is no
/// word within the drive's bound, so the exit watch's look ends, and the exit is asked again
/// (`wait:no-socket`); answered, it is the crash (lanes:H8).
#[test]
fn an_exit_word_the_window_never_answers_ends_at_its_bound() {
    let rig = Rig::new("exit-unanswered", 137, true, false, Log::Crash, true);
    let snap = rig.snap(Some(S1));
    let read = |what: &str, opts: Opts, snap: Snapshot| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(shell_word(&opts, &snap));
        });
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap_or_else(|_| panic!("{what}: no word within the hang detector"))
    };
    assert!(
        read("the answered block", rig.opts.clone(), snap.clone()).is_ok_and(|w| w.is_some()),
        "an answered block is read"
    );
    let dir = scratch("exit-unanswered-mute");
    let (mute, _asked) = crate::harness::fake_instance(&dir.join("m.sock"), TOKEN, |_| {
        crate::harness::UNANSWERED.to_string()
    });
    let muted = |rig: &Rig| Opts {
        sock: Some(mute.clone()),
        timings: crate::harness::Timings {
            drive_reply: Duration::from_millis(200),
            ..rig.opts.timings
        },
        ..rig.opts.clone()
    };
    assert_eq!(
        read("the unanswered block", muted(&rig), snap),
        Err(NoSocket),
        "an unanswered block is no word"
    );
    let unlogged = Rig::new(
        "exit-unanswered-unlogged",
        137,
        true,
        false,
        Log::None,
        true,
    );
    let r = after_exit_onto(
        &muted(&unlogged),
        &unlogged.snap(Some(S1)),
        &ExitRecord::Unread,
        ExitCause::Exit,
        Some(unlogged.twin.clone()),
        &unlogged.kernel,
    );
    assert_eq!(r.step, "wait:no-socket", "asked again, never unwitnessed");
    assert!(unlogged.typed().is_empty(), "{:#?}", unlogged.typed());
}

/// The relaunch a step adopted, its carry-on still owed.
fn adopted(name: &str) -> Rig {
    let rig = relaunch_rig(name, true, Knobs::default());
    let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "adopted", "{name}: {r:?}");
    assert_eq!(
        rig.continuations(),
        0,
        "{name}: the window's relaunch types none"
    );
    rig
}

/// WHAT STANDS AT THE COMPOSER KEEPS THE CARRY-ON OWED, UNDER CRUISE AND LAND ALIKE: each of a
/// person's draft, a box, a busy agent, Off and a person's grace ends the look on its own word with
/// nothing typed and the record still `Relaunched`, and the same look with it gone types the
/// carry-on once. MUTANT: with the words of `relaunch::settled_for_carry` read as `false`, a draft
/// reads as `wait:unsettled`; with any arm of the look's holds removed, its case types.
#[test]
fn what_stands_at_the_composer_keeps_the_carry_on_owed() {
    use crate::harness::relaunch::CAUSE_EXIT;
    use aterm_phase::muse::fixtures as fx;
    let person = "OK schema=1 hold=0 hand=- integration=on human_ms=400";
    let off = "OK schema=1 hold=0 hand=- integration=on autopilot=off";
    let cases: [(&str, Option<String>, Option<&str>); 5] = [
        ("wait:draft", Some(at_caret(fx::DRAFT_1_4_10)), None),
        ("wait:box", Some(at_caret(fx::BOX_SHELL)), None),
        ("wait:unsettled", Some(at_caret(fx::BUSY_1_4_10)), None),
        ("wait:off", None, Some(off)),
        ("wait:held", None, Some(person)),
    ];
    for land in [false, true] {
        for (word, screen, status) in &cases {
            let case = format!("{word}, land {land}");
            let rig = adopted(&format!("stands-{}-{land}", word.len()));
            rig.relaunched_shows(screen.clone(), *status);
            let r = carry_adopted(&rig, CAUSE_EXIT, land);
            assert_eq!(r.step, *word, "{case}: {r:?}");
            assert!(
                matches!(rig.state().phase, Phase::Relaunched { .. }),
                "{case}: still owed"
            );
            assert_eq!(rig.continuations(), 0, "{case}: {:#?}", rig.typed());
            rig.relaunched_shows(None, None);
            let r = carry_adopted(&rig, CAUSE_EXIT, land);
            assert_eq!(r.step, "continued", "{case}: cleared: {r:?}");
            assert_eq!(rig.continuations(), 1, "{case}: {:#?}", rig.typed());
        }
    }
}

/// A CARRY-ON OWED OUTLIVES THE RELAUNCH'S STALE BOUND WHILE ITS AGENT IS UP: a draft standing past
/// five minutes is still `wait:draft`, and its clearing still carries on. NEGATIVE CONTROL: the
/// agent gone, the same age is `failed:no-resume`. MUTANT: without `relaunch_present` in `expired`,
/// the aged draft reads `failed:no-resume`.
#[test]
fn a_carry_on_owed_outlives_the_stale_bound_while_its_agent_is_up() {
    use crate::harness::relaunch::CAUSE_EXIT;
    use aterm_phase::muse::fixtures as fx;
    for land in [false, true] {
        let rig = adopted(&format!("aged-{land}"));
        rig.relaunch_aged(STALE_S + 1);
        rig.relaunched_shows(Some(at_caret(fx::DRAFT_1_4_10)), None);
        let r = carry_adopted(&rig, CAUSE_EXIT, land);
        assert_eq!(r.step, "wait:draft", "land {land}: {r:?}");
        assert!(matches!(rig.state().phase, Phase::Relaunched { .. }));
        rig.relaunched_shows(None, None);
        let r = carry_adopted(&rig, CAUSE_EXIT, land);
        assert_eq!(r.step, "continued", "land {land}: {r:?}");
        assert_eq!(rig.continuations(), 1, "land {land}: {:#?}", rig.typed());
    }
    let mut gone = adopted("aged-gone");
    gone.relaunch_aged(STALE_S + 1);
    gone.kernel.knobs.new_dies = true;
    gone.kernel.knobs.noappear = true;
    gone.relaunched_shows(Some(shell_after()), None);
    let r = carry_adopted(&gone, CAUSE_EXIT, false);
    assert_eq!(r.step, "failed:no-resume", "{r:?}");
    assert_eq!(gone.continuations(), 0);
}

/// A DRAFT THAT APPEARS AS THE LOOK GOES ON IS STILL A DRAFT: past the settling reads, at each read
/// the look takes, the word is `wait:draft` and the carry-on stays owed, never given up as
/// `done:no-continue`; and three fences that moved before the line was typed are `wait:unsettled`.
/// NEGATIVE CONTROL: no draft types once. MUTANT: a fence's `Err` read as the end of the tries gives
/// `done:no-continue` at the last settling read's successors.
#[test]
fn a_draft_begun_as_the_look_goes_on_keeps_the_carry_on_owed() {
    use crate::harness::relaunch::CAUSE_EXIT;
    use aterm_phase::muse::fixtures as fx;
    for from in 1..=8 {
        let rig = adopted(&format!("late-draft-{from}"));
        rig.shows_from(from, at_caret(fx::DRAFT_1_4_10));
        let r = carry_adopted(&rig, CAUSE_EXIT, false);
        let typed = rig.continuations();
        if typed == 0 {
            assert_eq!(r.step, "wait:draft", "read {from}: {r:?}");
            assert!(matches!(rig.state().phase, Phase::Relaunched { .. }));
        } else {
            assert_eq!(
                r.step, "continued",
                "read {from}: past the look's reads: {r:?}"
            );
        }
    }
    let late = adopted("late-draft-none");
    let r = carry_adopted(&late, CAUSE_EXIT, false);
    assert_eq!(r.step, "continued", "{r:?}");
    let moved = adopted("late-moved");
    moved.on_harness_line(Some("OK 0 turn skipped reason=changed"), false);
    let r = carry_adopted(&moved, CAUSE_EXIT, false);
    assert_eq!(r.step, "wait:unsettled", "{r:?}");
    assert!(matches!(moved.state().phase, Phase::Relaunched { .. }));
}

/// A CARRY-ON LEFT UNANSWERED IS READ FOR WHAT IT DID, NOT TYPED AGAIN: the session's log showing
/// the line within the look is `continued`; not showing it is `wait:unanswered`, and at the next
/// look, showing it now, `done:carried`. Each is one line typed. MUTANT: with `Unanswered` read as
/// nothing typed, the first case ends `wait:no-socket` and each second look types it again.
#[test]
fn a_carry_on_left_unanswered_is_read_for_what_it_did() {
    use crate::harness::relaunch::CAUSE_EXIT;
    let muted = |rig: &mut Rig| {
        rig.opts.timings.drive_reply = Duration::from_millis(200);
    };
    // Only the carry-on's own line is waited out; every answered exchange waits the hang detector.
    let _answered = crate::AnsweredBut::set(|l| l.contains(upgrade::HARNESS_MARK));
    let mut reached = adopted("unanswered-reached");
    muted(&mut reached);
    reached.on_harness_line(Some(crate::harness::UNANSWERED), true);
    let r = carry_adopted(&reached, CAUSE_EXIT, false);
    assert_eq!(r.step, "continued", "{r:?}");
    assert_eq!(reached.continuations(), 1, "{:#?}", reached.typed());
    let mut later = adopted("unanswered-later");
    muted(&mut later);
    later.on_harness_line(Some(crate::harness::UNANSWERED), false);
    let r = carry_adopted(&later, CAUSE_EXIT, false);
    assert_eq!(r.step, "wait:unanswered", "{r:?}");
    assert_eq!(later.continuations(), 1, "{:#?}", later.typed());
    assert!(matches!(later.state().phase, Phase::Relaunched { .. }));
    later.on_harness_line(None, false);
    log_told(&later.log(), upgrade::HARNESS_MARK);
    let r = carry_adopted(&later, CAUSE_EXIT, false);
    assert_eq!(r.step, "done:carried", "{r:?}");
    assert_eq!(later.continuations(), 1, "{:#?}", later.typed());
}

/// WHAT REACHED THE SESSION FIRST IS READ OFF ITS LOG, NOT OFF A KEY OR A GESTURE: a person's
/// gesture since the relaunch, past their grace, leaves the carry-on owed (`continued`); a message of theirs since
/// the relaunch is `done:left`, naming them; a line of the harness's is `done:carried`. MUTANT: the
/// gesture alone read as a message (`human_ms=` as `person_keyed_since` read it) is `done:left` in
/// the first case.
#[test]
fn what_reached_the_session_first_is_read_off_its_log() {
    use crate::harness::relaunch::{CAUSE_EXIT, PERSON_FIRST};
    let gestured = "OK schema=1 hold=0 hand=- integration=on human_ms=130000 human_typed_ms=-";
    for land in [false, true] {
        let rig = adopted(&format!("gesture-{land}"));
        rig.relaunch_aged(200);
        rig.relaunched_shows(None, Some(gestured));
        let r = carry_adopted(&rig, CAUSE_EXIT, land);
        assert_eq!(r.step, "continued", "land {land}: {r:?}");
        assert_eq!(rig.continuations(), 1, "land {land}");
        let told = adopted(&format!("told-{land}"));
        log_told(&told.log(), "fix the tests");
        let r = carry_adopted(&told, CAUSE_EXIT, land);
        assert_eq!(r.step, "done:left", "land {land}: {r:?}");
        assert!(told.state().outcome.contains(PERSON_FIRST), "land {land}");
        assert_eq!(told.continuations(), 0, "land {land}");
        let ours = adopted(&format!("ours-{land}"));
        log_told(&ours.log(), upgrade::HARNESS_MARK);
        let r = carry_adopted(&ours, CAUSE_EXIT, land);
        assert_eq!(r.step, "done:carried", "land {land}: {r:?}");
        assert_eq!(ours.continuations(), 0, "land {land}");
    }
}

/// A RELAUNCH UNDER LAND WHOSE LAST REPLY DEFENDS THE WORK TYPES NOTHING: the loop's land check
/// reads the defense (`done:landing`, whose step moves nothing). NEGATIVE CONTROLS: the same reply
/// under Cruise gets its done check; a reply of work going on under Land gets the carry-on. MUTANT:
/// without `defended_on_screen`'s arm, the defense under Land is typed the restart's facts, which
/// the land check then reads as a new turn.
#[test]
fn a_relaunch_under_land_into_a_defense_leaves_it_to_the_land_check() {
    use crate::harness::relaunch::CAUSE_EXIT;
    let defense = "Nothing left: all of it is merged into the remote main branch.";
    let rig = adopted("defended-land");
    rig.relaunched_shows(Some(resumed_saying(defense)), None);
    let r = carry_adopted(&rig, CAUSE_EXIT, true);
    assert_eq!(r.step, crate::harness::relaunch::LANDING, "{r:?}");
    assert_eq!(rig.continuations(), 0, "{:#?}", rig.typed());
    assert_eq!(rig.state().phase, Phase::Done);
    assert!(
        !super::super::moved(&r.step),
        "the loop's check runs at this point"
    );
    let cruise = adopted("defended-cruise");
    cruise.relaunched_shows(Some(resumed_saying(defense)), None);
    let r = carry_adopted(&cruise, CAUSE_EXIT, false);
    assert_eq!(r.step, "continued:check", "{r:?}");
    assert_eq!(cruise.continuations(), 1);
    let going = adopted("going-land");
    going.relaunched_shows(
        Some(resumed_saying("Committed the parser; the lexer is next.")),
        None,
    );
    let r = carry_adopted(&going, CAUSE_EXIT, true);
    assert_eq!(r.step, "continued", "{r:?}");
    assert_eq!(going.continuations(), 1);
}

#[test]
fn holders_in_reports_live_tui_and_ignores_non_tui_commands() {
    let mut rig = Rig::new("holders-tui", 0, true, false, Log::Clean, true);
    let session = key(TAB);
    let sessions = BTreeSet::from([session.clone()]);
    let pids = [TUI];
    let live_tabs = vec![LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(TUI)),
    }];

    let found = holders_in(&sessions, Some(&live_tabs), &pids, &rig.kernel);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].pid, TUI);
    assert_eq!(found[0].session, session);
    assert_eq!(found[0].version, VERSION);
    assert_eq!(found[0].tab.as_deref(), Some(TAB));

    let found_env = holders_in(&sessions, None, &pids, &rig.kernel);
    assert_eq!(found_env.len(), 1);
    assert_eq!(found_env[0].pid, TUI);
    assert_eq!(found_env[0].version, VERSION);

    for argv in [
        vec!["muse", "serve", "--no-session-log", "--provider", "meta"],
        vec!["muse", "--version"],
        vec!["muse", "login"],
        vec!["muse", "exec", "hi"],
    ] {
        rig.kernel.knobs.new_argv = Some(argv.iter().map(|s| (*s).to_string()).collect());
        assert!(
            holders_in(&sessions, Some(&live_tabs), &pids, &rig.kernel).is_empty(),
            "non-TUI argv {argv:?} must not count as a live TUI holder"
        );
        assert!(
            holders_in(&sessions, None, &pids, &rig.kernel).is_empty(),
            "non-TUI argv {argv:?} must not count as a live TUI holder without live_tabs"
        );
    }
}

/// A Muse Code session folder reached by a symbolic link (a year kept on another volume) is read
/// through it, as `Path::is_dir` reads one: the session's lock is found there.
/// NEGATIVE CONTROL: the entry's own type alone (`DirEntry::file_type().is_dir()` in
/// `child_dirs`, 9445f7df77) passes the linked year over: `left: None`.
#[cfg(unix)]
#[test]
fn a_session_lock_under_a_linked_year_is_found() {
    let dir = scratch("linked-year");
    let root = dir.join("sessions");
    let real = dir.join("volume/2026");
    let session = real.join("10/17").join(S1);
    std::fs::create_dir_all(&session).expect("session");
    std::fs::write(session.join(mz::LOCK), "{}").expect("lock");
    std::fs::create_dir_all(&root).expect("root");
    std::os::unix::fs::symlink(&real, root.join("2026")).expect("linked year");
    assert_eq!(
        find_lock(&root, S1),
        Some(root.join("2026/10/17").join(S1).join(mz::LOCK))
    );
    let _ = std::fs::remove_dir_all(&dir);
}
