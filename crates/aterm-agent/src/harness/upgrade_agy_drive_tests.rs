// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the agy upgrade driver over scripted screens and a scripted kernel.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::super::{Job, parse_exit_status};
use super::*;
use crate::harness::relaunch::{AgentRun, AgyRun, ExitCause, ExitRecord, Snapshot, exit_record};
use crate::supervise::ScratchDir;

const TAB: &str = "s-0123456789abcdef4a67";
const TOKEN: &str = "0badf00d0badf00d";
const C1: &str = "eaeec4fb-4e43-47da-b46f-8a10c0fee186";
const C2: &str = "5f2b9c3e-7d41-4a8e-9b2c-1e3d5f7a9b0c";
const CRASH_UUID: &str = "9c1f0d6e-2b7a-4c55-9a3e-1f2e3d4c5b6a";
const TUI: u32 = 4_200_001;
const SHELL: u32 = 4_200_002;
const NEW: u32 = 4_200_003;
const OTHER: u32 = 4_200_004;
const BUILD: &str = "1000001000004000000";
const VERSION: &str = "1.4.0";
const START: &str = "Sat Oct 17 06:40:00 2026";
const WORKSPACE: &str = "/w/proj";
const SHELL_CWD: &str = "/w";

fn ctrl_z() -> i64 {
    128 + i64::from(libc::SIGTSTP)
}

fn sigstop() -> i64 {
    128 + i64::from(libc::SIGSTOP)
}

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("agd", name)
}

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

fn fixture_rows(text: &str) -> Vec<String> {
    text.lines().skip(1).map(str::to_string).collect()
}

fn at_caret(text: &str) -> String {
    let rows = fixture_rows(text);
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('>'))
        .unwrap_or(rows.len().saturating_sub(1));
    screen_json(&rows, (caret, 2), 20)
}

fn agy_idle() -> String {
    at_caret(aterm_phase::agy::fixtures::END_OF_TURN)
}

fn shell_after() -> String {
    let rows: Vec<String> = ["% sh agy.sh", "zsh: killed     sh agy.sh", "% "]
        .iter()
        .map(|r| (*r).to_string())
        .collect();
    screen_json(&rows, (2, 2), 900)
}

fn blocks(exit: i64, word: bool, later: bool) -> String {
    let entering = r#"{"id":8,"state":"entering","exit":null,"prompt":4,"cmd":4,"cmdcol":2,"out":null,"end":null,"cmdline":""}"#;
    if !word {
        return format!(r#"{{"blocks":[{entering}]}}"#);
    }
    if later {
        return r#"{"blocks":[{"id":9,"state":"complete","exit":2,"prompt":4,"cmd":4,"cmdcol":2,"out":5,"end":6,"cmdline":"make"},{"id":10,"state":"entering","exit":null,"prompt":6,"cmd":6,"cmdcol":2,"out":null,"end":null,"cmdline":""}]}"#.to_string();
    }
    format!(
        r#"{{"blocks":[{{"id":7,"state":"complete","exit":{exit},"prompt":2,"cmd":2,"cmdcol":2,"out":3,"end":4,"cmdline":"sh agy.sh"}},{entering}]}}"#
    )
}

fn line_blocks(exit: Option<i64>) -> String {
    let line = r#""cmdline":" ( cd -- '/w/proj' && exec '/p/agents/agy' '--conversation=c1' )""#;
    match exit {
        None => format!(
            r#"{{"blocks":[{{"id":8,"state":"executing","exit":null,"prompt":4,"cmd":4,"cmdcol":2,"out":5,"end":null,{line}}}]}}"#
        ),
        Some(exit) => format!(
            r#"{{"blocks":[{{"id":8,"state":"complete","exit":{exit},"prompt":4,"cmd":4,"cmdcol":2,"out":5,"end":6,{line}}},{{"id":9,"state":"entering","exit":null,"prompt":6,"cmd":6,"cmdcol":2,"out":null,"end":null,"cmdline":""}}]}}"#
        ),
    }
}

/// What the stand-in serves once the relaunch line is typed, which a test
/// may change between attempts: `None` serves as before the line.
#[derive(Default)]
struct After {
    before_screen: Option<String>,
    screen: Option<String>,
    before_status: Option<String>,
    status: Option<String>,
    blocks: Option<String>,
    /// The shell's blocks BEFORE the line, in place of the exit's
    /// ([`blocks`]): the agy still running in its block ([`running`]).
    running: Option<String>,
}

fn running() -> String {
    r#"{"blocks":[{"id":7,"state":"executing","exit":null,"prompt":2,"cmd":2,"cmdcol":2,"out":3,"end":null,"cmdline":"sh agy.sh"}]}"#.to_string()
}

struct World {
    /// The shell's blocks after the exit ([`blocks`]), which a reset rig
    /// ([`Rig::reset`]) serves anew.
    blocks2: Arc<Mutex<String>>,
    after_exit: String,
    after_relaunch: String,
    status: String,
    relaunched: Arc<AtomicBool>,
    after: Arc<Mutex<After>>,
}

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
            "sessions" => format!("OK 1\nlocal {TAB} 0 idle agy"),
            "text" => format!(
                "OK {}",
                if relaunched {
                    served(&after.screen, &w.after_relaunch)
                } else {
                    after
                        .before_screen
                        .as_ref()
                        .unwrap_or(&w.after_exit)
                        .clone()
                }
            ),
            "cell" => "OK > ccd3db 1e1e1e none".to_string(),
            "status" => served(
                &after.status,
                after.before_status.as_ref().unwrap_or(&w.status),
            ),
            "help" if line.contains("help key") => {
                "OK 2\nkey  key [if-gen=<epoch>.<seq>] [if=<re>] <name>\n     FENCES".to_string()
            }
            "help" => "OK 1\nturn [if-gen=<epoch>.<seq>] <text>".to_string(),
            "blocks" => format!(
                "OK {}",
                match &after.running {
                    Some(now) if !relaunched => now.clone(),
                    _ => served(&after.blocks, &w.blocks2.lock().expect("blocks")),
                }
            ),
            "blocktext" => "OK 0".to_string(),
            "line" => "OK % ".to_string(),
            "lease" => "ERR unscripted".to_string(),
            "turn" => {
                if line.contains("'--conversation=") {
                    w.relaunched.store(true, Ordering::SeqCst);
                }
                "OK 0 turn submitted=1 status=settled seq=1 id=1".to_string()
            }
            _ => "ERR unscripted".to_string(),
        }
    })
}

/// What a test varies about the relaunch, each `false`/`None` as the plain
/// relaunch goes.
#[derive(Clone, Debug, Default)]
struct Knobs {
    /// The host's roster never shows [`NEW`] leading the tab.
    out_of_tab: bool,
    /// [`NEW`] is listed under the shell but gone at once.
    new_dies: bool,
    /// No `agy` ever comes up under the shell.
    noappear: bool,
    /// The argv [`NEW`] runs with in place of `--conversation=`[`C1`].
    new_argv: Option<Vec<String>>,
    /// [`NEW`]'s parent is another process than the shell.
    new_orphan: bool,
    /// [`NEW`] never holds the conversation's lock: it started fresh.
    new_lockless: bool,
    /// Who holds the lock cannot be read.
    holders_unreadable: bool,
    /// Another live process ([`OTHER`]) holds the lock from this read of it
    /// on (1 the first).
    holder_from_read: Option<usize>,
    /// Another live agy ([`OTHER`]) names the conversation in its argv,
    /// holding no lock (opened on it while another held it).
    argv_holder: bool,
    /// The live agy's argv cannot be read.
    argv_unreadable: bool,
    /// Another live agy ([`OTHER`]) holds the conversation's database and
    /// no presence lock: opened on it with `-c`/`--continue` or an in-TUI
    /// `/resume` while another held it, its argv naming nothing.
    db_opener: bool,
    /// Who holds the conversation's database, or what such a holder holds,
    /// cannot be read.
    openers_unreadable: bool,
    /// Another live agy ([`OTHER`]) holds the conversation's database and no
    /// presence lock from this read of the database's holders on (1 the
    /// first): it opened the conversation after the relaunch's first look.
    db_opener_from_read: Option<usize>,
    /// [`NEW`]'s number was taken by another process: alive, of another
    /// kernel start, no agy under the shell.
    new_reborn: bool,
    /// The tab's shell is fish.
    fish: bool,
    /// The tab's shell is in the launch's directory ([`WORKSPACE`]), not its
    /// own ([`SHELL_CWD`]).
    shell_in_workspace: bool,
    /// The launch's directory the kernel reads, in place of [`WORKSPACE`].
    workspace: Option<String>,
    /// Pid `0` ([`crate::harness::relaunch::NO_SHELL`]) reads as a zsh, as
    /// no live process table answers (pid 0 has no argv on macOS): only the
    /// lane's own guard keeps a tab program's dialect unread.
    zero_reads_as_zsh: bool,
}

struct Script {
    exited: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    relaunched: Arc<AtomicBool>,
    lock: PathBuf,
    holder: bool,
    exe: PathBuf,
    files: Vec<PathBuf>,
    knobs: Knobs,
    lock_reads: AtomicUsize,
    opener_reads: AtomicUsize,
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
            TUI => Some(START.to_string()),
            NEW if self.knobs.new_reborn => Some("another-birth".to_string()),
            NEW => Some("new-birth".to_string()),
            _ => None,
        }
    }
}

impl AgyKernel for Script {
    fn alive(&self, pid: u32) -> bool {
        match pid {
            0 => true,
            TUI => !self.exited.load(Ordering::SeqCst) || self.stopped.load(Ordering::SeqCst),
            NEW => {
                self.knobs.new_reborn
                    || (self.relaunched.load(Ordering::SeqCst)
                        && !self.knobs.new_dies
                        && !self.knobs.noappear)
            }
            OTHER => {
                self.holder
                    || self.knobs.holder_from_read.is_some()
                    || self.knobs.argv_holder
                    || self.knobs.db_opener
                    || self.knobs.db_opener_from_read.is_some()
            }
            _ => pid == SHELL,
        }
    }

    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        let shell = if self.knobs.fish { "fish" } else { "zsh" };
        (pid == SHELL || (pid == 0 && self.knobs.zero_reads_as_zsh)).then(|| {
            atpkg::caller_shell::ProcArgs {
                exec_path: format!("/bin/{shell}"),
                argv: vec![format!("-{shell}")],
                env: Vec::new(),
            }
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
            if AgyKernel::alive(self, NEW) && !self.knobs.new_lockless && !self.knobs.new_reborn {
                h.push(NEW);
            }
        } else if AgyKernel::alive(self, TUI) {
            h.push(TUI);
        }
        if self.holder || self.knobs.holder_from_read.is_some_and(|from| read >= from) {
            h.push(OTHER);
        }
        Some(h)
    }

    fn agy_resuming(&self, conversation: &str) -> Option<Vec<u32>> {
        if self.knobs.argv_unreadable {
            return None;
        }
        Some(if self.knobs.argv_holder && conversation == C1 {
            vec![OTHER]
        } else {
            Vec::new()
        })
    }

    fn lockless_openers(&self, config: &Path, conversation: &str) -> Option<Vec<u32>> {
        let read = self.opener_reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.knobs.openers_unreadable {
            return None;
        }
        let ours = self.lock.parent().and_then(Path::parent) == Some(config);
        let opener = self.knobs.db_opener
            || self
                .knobs
                .db_opener_from_read
                .is_some_and(|from| read >= from);
        Some(if ours && opener && conversation == C1 {
            vec![OTHER]
        } else {
            Vec::new()
        })
    }

    fn shell_has_terminal(&self, _: u32) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    fn agy_children(&self, _: u32) -> Vec<(u32, Vec<String>)> {
        if self.relaunched.load(Ordering::SeqCst) && !self.knobs.noappear && !self.knobs.new_reborn
        {
            vec![(
                NEW,
                self.knobs.new_argv.clone().unwrap_or_else(|| {
                    vec![
                        self.exe.to_string_lossy().into_owned(),
                        "--effort".to_string(),
                        "high".to_string(),
                        format!("--conversation={C1}"),
                    ]
                }),
            )]
        } else {
            Vec::new()
        }
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        Some(if pid == SHELL && !self.knobs.shell_in_workspace {
            SHELL_CWD.to_string()
        } else {
            self.knobs
                .workspace
                .clone()
                .unwrap_or_else(|| WORKSPACE.to_string())
        })
    }

    fn in_tab(&self, _: &mut Client, pid: u32, _: &str) -> bool {
        !(pid == NEW && self.knobs.out_of_tab) && AgyKernel::alive(self, pid)
    }
}

struct Rig {
    dir: ScratchDir,
    opts: Opts,
    kernel: Script,
    asked: crate::harness::FakeInstanceLog,
    config: PathBuf,
    lock: PathBuf,
    crash: PathBuf,
    exe: PathBuf,
    twin: PathBuf,
    blocks2: Arc<Mutex<String>>,
    after: Arc<Mutex<After>>,
    /// The harness's waits on the test's thread run on their own steps
    /// ([`crate::harness::relaunch::virtual_waits`]): every stand-in here answers at once.
    _waits: crate::harness::relaunch::VirtualWaits,
}

impl Rig {
    fn new(name: &str, status: i64, word: bool, later: bool, crash_left: bool) -> Rig {
        let dir = scratch(name);
        let base = std::fs::canonicalize(&*dir).expect("canonical scratch");
        let home = base.join("home");
        let config = home.join(".gemini/antigravity-cli");
        std::fs::create_dir_all(config.join(ag::PRESENCE)).expect("presence");
        std::fs::create_dir_all(config.join(ag::CRASHES)).expect("crashes");
        let lock = ag::presence_lock(&config, C1);
        std::fs::write(&lock, "").expect("lock");
        let crash = config
            .join(ag::CRASHES)
            .join(format!("crash_{TUI}_{CRASH_UUID}.log"));
        if crash_left {
            std::fs::write(&crash, "").expect("crash file");
        }
        let store = base.join("prefix/store/agy");
        let exe = store.join(BUILD).join("bin/agy");
        std::fs::create_dir_all(exe.parent().expect("bin")).expect("store");
        std::fs::write(&exe, "#!/bin/sh\n").expect("build");
        std::fs::write(
            store.join(format!("{BUILD}.vendor")),
            format!("version = \"{VERSION}\"\n"),
        )
        .expect("vendor record");
        let twin = base.join("prefix/agents/agy");
        std::fs::create_dir_all(twin.parent().expect("agents")).expect("agents");
        std::fs::write(
            &twin,
            format!(
                "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\nexport AGY_CLI_DISABLE_AUTO_UPDATE='true'\nexec '{}' \"$@\"\n",
                exe.display()
            ),
        )
        .expect("twin");
        let exited = Arc::new(AtomicBool::new(true));
        let relaunched = Arc::new(AtomicBool::new(false));
        let after = Arc::new(Mutex::new(After::default()));
        let blocks2 = Arc::new(Mutex::new(blocks(status, word, later)));
        let w = World {
            blocks2: Arc::clone(&blocks2),
            after_exit: shell_after(),
            after_relaunch: agy_idle(),
            status: "OK schema=1 hold=0 hand=- integration=on".to_string(),
            relaunched: Arc::clone(&relaunched),
            after: Arc::clone(&after),
        };
        let (sock, asked) = instance(&base, w);
        let opts = Opts {
            home: base.join("user"),
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
            press: None,
            timings: crate::harness::Timings::FAST,
        };
        Rig {
            _waits: crate::harness::relaunch::virtual_waits(),
            kernel: Script {
                exited,
                stopped: Arc::new(AtomicBool::new(false)),
                relaunched,
                lock: lock.clone(),
                holder: false,
                exe: exe.clone(),
                files: vec![lock.clone(), crash.clone(), config.join("log/cli.log")],
                knobs: Knobs::default(),
                lock_reads: AtomicUsize::new(0),
                opener_reads: AtomicUsize::new(0),
            },
            asked,
            after,
            config,
            lock,
            crash,
            exe,
            twin,
            blocks2,
            opts,
            dir,
        }
    }

    fn reset(&mut self, status: i64, word: bool, later: bool, crash_left: bool) {
        for dir in [&self.opts.state, &self.opts.home, &self.config] {
            match std::fs::remove_dir_all(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => panic!("{}: {e}", dir.display()),
            }
        }
        std::fs::create_dir_all(self.config.join(ag::PRESENCE)).expect("presence");
        std::fs::create_dir_all(self.config.join(ag::CRASHES)).expect("crashes");
        std::fs::write(&self.lock, "").expect("lock");
        if crash_left {
            std::fs::write(&self.crash, "").expect("crash file");
        }
        *self.blocks2.lock().expect("blocks") = blocks(status, word, later);
        *self.after.lock().expect("after") = After::default();
        self.asked.lock().expect("log").clear();
        let k = &mut self.kernel;
        k.exited.store(true, Ordering::SeqCst);
        k.stopped.store(false, Ordering::SeqCst);
        k.relaunched.store(false, Ordering::SeqCst);
        k.holder = false;
        k.knobs = Knobs::default();
        k.lock_reads.store(0, Ordering::SeqCst);
        k.opener_reads.store(0, Ordering::SeqCst);
    }

    fn snap(&self, conversation: Option<&str>) -> Snapshot {
        Snapshot {
            stored_login: false,
            profile: None,
            tab: TAB.to_string(),
            pid: TUI,
            start: START.to_string(),
            shell: SHELL,
            program: self.exe.clone(),
            argv: vec![
                self.exe.to_string_lossy().into_owned(),
                "--effort".to_string(),
                "high".to_string(),
                "--agent".to_string(),
                "labagent".to_string(),
                format!("-conversation={C2}"),
            ],
            session: conversation.map(str::to_string),
            cwd: WORKSPACE.to_string(),
            version: Some(VERSION.to_string()),
            run: AgentRun::Agy(AgyRun {
                config: self.config.clone(),
                env: vec![(
                    "HOME".to_string(),
                    self.config
                        .parent()
                        .and_then(Path::parent)
                        .expect("home")
                        .to_string_lossy()
                        .into_owned(),
                )],
                lock: conversation.is_some().then(|| self.lock.clone()),
                crash: Some(self.crash.clone()),
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

    fn look(&self, snap: &Snapshot) -> ExitRecord {
        let (mut first, mut looks) = (None, 0);
        exit_record(
            || {
                looks += 1;
                if looks <= 2 {
                    let now = look_at_exit(&self.opts, snap, &self.kernel);
                    assert_eq!(
                        *first.get_or_insert_with(|| now.clone()),
                        now,
                        "a look moved"
                    );
                }
                first.clone()
            },
            |_| true,
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

    fn line_ran(&self, exit: Option<i64>) {
        self.after.lock().expect("after").blocks = Some(line_blocks(exit));
    }

    fn relaunched_shows(&self, screen: Option<String>, status: Option<&str>) {
        let mut after = self.after.lock().expect("after");
        after.screen = screen;
        after.status = status.map(str::to_string);
    }

    fn lines(&self) -> usize {
        self.typed()
            .iter()
            .filter(|l| l.contains("'--conversation="))
            .count()
    }

    fn continuations(&self) -> usize {
        self.typed()
            .iter()
            .filter(|l| l.contains("[aterm harness]"))
            .count()
    }

    fn new_crashed(&self) {
        std::fs::write(
            self.config
                .join(ag::CRASHES)
                .join(format!("crash_{NEW}_{CRASH_UUID}.log")),
            "",
        )
        .expect("crash file");
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&*self.dir);
    }
}

#[test]
fn a_crashed_agy_is_relaunched_on_its_conversation_in_its_directory() {
    let rig = Rig::new("crash", 137, true, false, true);
    let snap = rig.snap(Some(C1));
    let left = rig.look(&snap);
    assert_eq!(left, ExitRecord::Crashed);
    let r = rig.attempt(&snap, &left, ExitCause::Exit);
    assert_eq!(r.step, "continued", "{r:?}");
    assert_eq!(r.session, format!("agy-{TAB}"));
    let typed = rig.typed();
    assert_eq!(typed.len(), 2, "{typed:#?}");
    let line = &typed[0];
    let twin = rig.twin.to_string_lossy();
    assert!(line.contains(&format!("cd -- '{WORKSPACE}' && ")), "{line}");
    assert!(line.contains("HOME="), "{line}");
    assert!(line.contains(&format!("'{twin}'")), "{line}");
    assert!(
        line.contains(&format!("'--effort' 'high' '--conversation={C1}'")),
        "{line}"
    );
    assert!(
        !line.contains("/store/agy/"),
        "never the store path: {line}"
    );
    assert!(
        !line.contains("--agent") && !line.contains(C2),
        "never the dropped flags nor the launch's own conversation: {line}"
    );
    assert!(
        typed[1].contains("[aterm harness] Relaunched: Antigravity CLI exited"),
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
        (Agent::Agy, C1, WORKSPACE, "managed")
    );
    assert_eq!(
        st.outcome,
        format!("agy on {VERSION} · the same conversation resumed")
    );
}

#[test]
fn the_agy_exit_is_read_off_its_command_block() {
    assert_eq!(
        parse_exit_status("OK", &blocks(137, true, false), Some(7)),
        Some(137)
    );
    assert_eq!(
        parse_exit_status("OK", &blocks(ctrl_z(), true, false), Some(7)),
        Some(ctrl_z())
    );
    assert_eq!(
        parse_exit_status("OK", &blocks(0, false, false), Some(7)),
        None
    );
    assert_eq!(
        parse_exit_status("OK", &blocks(0, true, true), Some(7)),
        None
    );
}

#[test]
fn a_graceful_exit_a_refused_start_and_an_unwitnessed_end_are_never_relaunched() {
    let mut status_alone_differs = 0;
    for (status, crash_left, want) in [
        (0, false, "ended:graceful-exit"),
        (2, false, "ended:refused-start"),
        (137, false, "ended:exit-unwitnessed"),
        (1, false, "ended:exit-unwitnessed"),
        (2, true, "continued"),
        (137, true, "continued"),
    ] {
        let rig = Rig::new("left", status, true, false, crash_left);
        let snap = rig.snap(Some(C1));
        let left = rig.look(&snap);
        let r = rig.attempt(&snap, &left, ExitCause::Exit);
        assert_eq!(r.step, want, "{status} {crash_left}: {left:?} {r:?}");
        if want.starts_with("ended:") {
            assert!(rig.typed().is_empty(), "{status}: {:#?}", rig.typed());
        }
        let table_crash = ag::exit_kind(status, crash_left) == ExitKind::Crashed;
        assert_eq!(table_crash, want == "continued", "{status} {crash_left}");
        if status != 0 && !table_crash {
            status_alone_differs += 1;
        }
    }
    assert_eq!(
        status_alone_differs, 3,
        "the status alone would relaunch the refusal and both unwitnessed ends"
    );
    let rig = Rig::new("refused-launch", 2, true, false, false);
    let snap = rig.snap(None);
    assert_eq!(
        crate::harness::relaunch::launch_ended(&snap, &rig.look(&snap)),
        Some("refused-start")
    );
}

#[test]
fn a_stopped_agy_is_never_relaunched() {
    let rig = Rig::new("stopped", ctrl_z(), true, false, true);
    rig.kernel.stopped.store(true, Ordering::SeqCst);
    let snap = rig.snap(Some(C1));
    let left = rig.look(&snap);
    assert_eq!(left, ExitRecord::Unread, "alive: no exit read");
    let r = rig.attempt(&snap, &left, ExitCause::Exit);
    assert_eq!(r.step, "wait:not-exited", "{r:?}");
    assert!(rig.typed().is_empty(), "{:#?}", rig.typed());
    assert!(
        rig.crash.is_file(),
        "the control: its crash file is still there"
    );
    rig.kernel.stopped.store(false, Ordering::SeqCst);
    let left = rig.look(&snap);
    assert_eq!(
        left,
        ExitRecord::Unread,
        "Ctrl-Z decides nothing at the look"
    );
    let r = rig.attempt(&snap, &left, ExitCause::Exit);
    assert_eq!(r.step, "ended:stopped", "{r:?}");
    assert!(rig.typed().is_empty(), "{:#?}", rig.typed());
}

#[test]
fn no_witness_no_relaunch() {
    let rig = Rig::new("unwitnessed", 137, true, false, true);
    let r = rig.attempt(&rig.snap(None), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "ended:no-conversation");
    assert!(rig.typed().is_empty());
    let gone = Rig::new("gone", 137, true, false, true);
    std::fs::remove_file(&gone.lock).expect("gone");
    let r = gone.attempt(&gone.snap(Some(C1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "ended:conversation-gone");
    let control = Rig::new("witnessed", 137, true, false, true);
    let r = control.attempt(
        &control.snap(Some(C1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "continued");
}

#[test]
fn a_tabs_own_agy_is_relaunched_by_nothing() {
    use crate::harness::relaunch::{NO_SHELL, carries_restart};
    let rig = Rig::new("tab-program", 137, true, false, true);
    let snap = Snapshot {
        shell: NO_SHELL,
        dialect: None,
        ..rig.snap(Some(C1))
    };
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "refused:not-a-shell-job", "{r:?}");
    assert!(rig.typed().is_empty());
    let st = St {
        agent: Agent::Agy,
        shell: NO_SHELL,
        ..St::default()
    };
    assert!(matches!(
        plan(&rig.opts, &st, &rig.kernel),
        Err(NoPlan::Refused { why, .. }) if why == "not-a-shell-job"
    ));
    let mut own = rig.snap(Some(C1));
    own.program = PathBuf::from("/opt/agy/bin/agy");
    assert!(
        carries_restart(&rig.opts.home, &own),
        "control: an agy at its shell"
    );
    own.shell = NO_SHELL;
    assert!(!carries_restart(&rig.opts.home, &own), "a tab's own agy");
    let control = Rig::new("tab-program-control", 137, true, false, true);
    let r = control.attempt(
        &control.snap(Some(C1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "continued");
}

#[test]
fn a_held_conversation_is_never_typed_at() {
    for (name, holder, knobs) in [
        ("lock-held", true, Knobs::default()),
        (
            "argv-held",
            false,
            Knobs {
                argv_holder: true,
                ..Knobs::default()
            },
        ),
        (
            "argv-unreadable",
            false,
            Knobs {
                argv_unreadable: true,
                ..Knobs::default()
            },
        ),
        (
            "continue-opener",
            false,
            Knobs {
                db_opener: true,
                ..Knobs::default()
            },
        ),
        (
            "openers-unreadable",
            false,
            Knobs {
                openers_unreadable: true,
                ..Knobs::default()
            },
        ),
    ] {
        let mut rig = Rig::new(name, 137, true, false, true);
        rig.kernel.holder = holder;
        rig.kernel.knobs = knobs;
        let r = rig.attempt(&rig.snap(Some(C1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, "wait:conversation-held", "{name}");
        assert!(rig.typed().is_empty(), "{name}: {:#?}", rig.typed());
        rig.kernel.holder = false;
        rig.kernel.knobs = Knobs::default();
        let r = rig.attempt(&rig.snap(Some(C1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, "continued", "{name}: {r:?}");
    }
}

#[test]
fn a_relaunch_in_flight_whose_lock_went_is_never_typed() {
    for (name, remove, want) in [
        ("gone-meanwhile", true, "failed:conversation-gone"),
        ("kept", false, "continued"),
    ] {
        let mut rig = Rig::new(name, 137, true, false, true);
        rig.kernel.holder = true;
        let snap = rig.snap(Some(C1));
        let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, "wait:conversation-held", "{name}");
        rig.kernel.holder = false;
        if remove {
            std::fs::remove_file(&rig.lock).expect("removed");
        }
        let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}");
        if remove {
            assert_eq!(rig.lines(), 0, "{name}: {:#?}", rig.typed());
        }
    }
}

#[test]
fn an_uncarryable_launch_and_a_missing_twin_are_refused() {
    let rig = Rig::new("remote", 137, true, false, true);
    let mut snap = rig.snap(Some(C1));
    snap.argv.push("--remote-control".to_string());
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert!(r.step.starts_with("failed:argv:"), "{r:?}");
    assert_eq!(rig.typed().len(), 0, "{:#?}", rig.typed());
    let rig = Rig::new("no-twin", 137, true, false, true);
    let r = after_exit_onto(
        &rig.opts,
        &rig.snap(Some(C1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
        None,
        &rig.kernel,
    );
    assert_eq!(r.step, "refused:no-managed-agy");
    let rig = Rig::new("own-install", 137, true, false, true);
    let other = rig.dir.join("opt/bin/agy");
    std::fs::create_dir_all(other.parent().expect("bin")).expect("bin");
    std::fs::write(&other, "#!/bin/sh\n").expect("agy");
    let mut snap = rig.snap(Some(C1));
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

#[test]
fn the_witness_is_the_held_presence_lock() {
    let rig = Rig::new("witness", 0, true, false, true);
    rig.kernel.exited.store(false, Ordering::SeqCst);
    rig.after.lock().expect("after").running = Some(running());
    let home = rig.config.parent().and_then(Path::parent).expect("home");
    let args = atpkg::caller_shell::ProcArgs {
        exec_path: rig.exe.to_string_lossy().into_owned(),
        argv: rig.snap(None).argv,
        env: vec![format!("HOME={}", home.display())],
    };
    let snap =
        snapshot(&rig.opts, TAB, TUI, START.into(), SHELL, &args, &rig.kernel).expect("a snapshot");
    assert_eq!(snap.session.as_deref(), Some(C1));
    assert_eq!(snap.version.as_deref(), Some(VERSION));
    assert_eq!(snap.cwd, WORKSPACE);
    let run = snap.agy().expect("an agy run");
    assert_eq!(run.config, rig.config);
    assert_eq!(run.crash.as_deref(), Some(rig.crash.as_path()));
    assert_eq!(
        run.env,
        vec![("HOME".to_string(), home.to_string_lossy().into_owned())],
        "a HOME other than the user's is carried"
    );
    assert_eq!(run.block, Some(7), "the shell's block it runs in");
    assert_eq!(snap.dialect, Some(Dialect::Zsh));
    let zero_zsh = Script {
        knobs: Knobs {
            zero_reads_as_zsh: true,
            ..Knobs::default()
        },
        ..rig_kernel(&rig)
    };
    assert_eq!(
        shell_dialect(crate::harness::relaunch::NO_SHELL, &zero_zsh),
        Some(Dialect::Zsh),
        "control: pid 0 reads as a zsh"
    );
    let own = snapshot(
        &rig.opts,
        TAB,
        TUI,
        START.into(),
        crate::harness::relaunch::NO_SHELL,
        &args,
        &zero_zsh,
    )
    .expect("a tab program's snapshot");
    assert_eq!(own.session.as_deref(), Some(C1));
    assert_eq!(
        (own.dialect, own.agy().and_then(|run| run.block)),
        (None, None)
    );
    let with = |files: Vec<PathBuf>| {
        let k = Script {
            files,
            ..rig_kernel(&rig)
        };
        k.exited.store(false, Ordering::SeqCst);
        snapshot(&rig.opts, TAB, TUI, START.into(), SHELL, &args, &k).expect("snap")
    };
    let fresh = with(vec![rig.crash.clone()]);
    assert_eq!(fresh.session, None, "a fresh agy holds no lock");
    let second = ag::presence_lock(&rig.config, C2);
    std::fs::write(&second, "").expect("second lock");
    assert_eq!(
        with(vec![rig.lock.clone(), second.clone()]).session,
        None,
        "two locks: none proven"
    );
    let elsewhere = rig.dir.join(format!("other/presence/{C1}.lock"));
    assert_eq!(with(vec![elsewhere]).session, None, "another config's");
    let other_crash = rig
        .config
        .join(ag::CRASHES)
        .join(format!("crash_{OTHER}_{CRASH_UUID}.log"));
    assert_eq!(
        with(vec![rig.lock.clone(), other_crash])
            .agy()
            .and_then(|r| r.crash.clone()),
        None,
        "another pid's crash file"
    );
    assert_eq!(
        with(vec![second, rig.crash.clone()]).session.as_deref(),
        Some(C2),
        "the control: a /clear's new lock"
    );
    let one_shot = atpkg::caller_shell::ProcArgs {
        argv: vec![
            rig.exe.to_string_lossy().into_owned(),
            "-p".into(),
            "hi".into(),
        ],
        ..args.clone()
    };
    assert_eq!(
        snapshot(
            &rig.opts,
            TAB,
            TUI,
            START.into(),
            SHELL,
            &one_shot,
            &rig.kernel
        )
        .err(),
        Some("not-a-tui")
    );
    let moved = atpkg::caller_shell::ProcArgs {
        argv: vec![
            rig.exe.to_string_lossy().into_owned(),
            format!("--gemini_dir={}", rig.dir.join("alt").display()),
        ],
        ..args.clone()
    };
    let alt = snapshot(
        &rig.opts,
        TAB,
        TUI,
        START.into(),
        SHELL,
        &moved,
        &rig.kernel,
    )
    .expect("snap");
    assert_eq!(
        alt.session, None,
        "the lock under the default config is not its"
    );
}

fn rig_kernel(rig: &Rig) -> Script {
    Script {
        exited: Arc::clone(&rig.kernel.exited),
        stopped: Arc::clone(&rig.kernel.stopped),
        relaunched: Arc::clone(&rig.kernel.relaunched),
        lock: rig.lock.clone(),
        holder: false,
        exe: rig.exe.clone(),
        files: rig.kernel.files.clone(),
        knobs: Knobs::default(),
        lock_reads: AtomicUsize::new(0),
        opener_reads: AtomicUsize::new(0),
    }
}

#[test]
fn only_its_own_crash_file_witnesses_a_crash() {
    let rig = Rig::new("own-file", 137, true, false, true);
    assert!(crash_left(&rig.config, TUI, Some(&rig.crash), 0));
    assert!(!crash_left(&rig.config, TUI, None, 0), "start unread");
    let started = now_s().saturating_sub(5) * 1000;
    assert!(crash_left(&rig.config, TUI, None, started), "written since");
    let later = (now_s() + 3600) * 1000;
    assert!(
        !crash_left(&rig.config, TUI, None, later),
        "written before it started"
    );
    std::fs::remove_file(&rig.crash).expect("removed");
    assert!(!crash_left(&rig.config, TUI, Some(&rig.crash), 0));
}

#[test]
fn aterm_ending_relaunches_the_restored_conversation() {
    let rig = Rig::new("host", 0, true, false, false);
    let mut snap = rig.snap(Some(C1));
    if let AgentRun::Agy(run) = &mut snap.run {
        run.lock = None;
        run.crash = None;
        run.block = None;
    }
    let r = rig.attempt(&snap, &ExitRecord::Unread, ExitCause::HostEnded);
    assert_eq!(r.step, "continued", "{r:?}");
    assert!(
        rig.typed()[1].contains("aterm ended while this session ran"),
        "{:#?}",
        rig.typed()
    );
    let own = Rig::new("host-control", 0, true, false, false);
    let r = own.attempt(&own.snap(Some(C1)), &ExitRecord::Unread, ExitCause::Exit);
    assert_eq!(r.step, "ended:graceful-exit");
}

#[test]
fn the_explicit_restart_is_carried_for_an_agy() {
    use crate::harness::relaunch::carries_restart;
    let rig = Rig::new("restart", 0, true, false, false);
    let snap = rig.snap(Some(C1));
    let home = &rig.opts.home;
    let twin = || Some(rig.twin.clone());
    assert!(carries_restart_onto(&snap, home, twin()));
    assert!(!carries_restart_onto(&rig.snap(None), home, twin()));
    let mut remote = rig.snap(Some(C1));
    remote.argv.push("--remote-control".to_string());
    assert!(!carries_restart_onto(&remote, home, twin()));
    let mut homeless = rig.snap(Some(C1));
    if let AgentRun::Agy(run) = &mut homeless.run {
        run.env = vec![("HOME".to_string(), "/h/a\nb".to_string())];
    }
    assert!(
        !carries_restart_onto(&homeless, home, twin()),
        "a HOME no line holds"
    );
    let mut fish = rig.snap(Some(C1));
    fish.dialect = Some(Dialect::Fish);
    assert!(carries_restart_onto(&fish, home, twin()), "a fish shell");
    for (dialect, carried) in [(Dialect::Fish, true), (Dialect::Zsh, false)] {
        let mut unnamed = rig.snap(Some(C1));
        unnamed.dialect = Some(dialect);
        unnamed.cwd = "/w/a\nb".to_string();
        assert_eq!(
            carries_restart_onto(&unnamed, home, twin()),
            carried,
            "{dialect:?}: a directory no `cd` names"
        );
    }
    let deep_dir = format!("/w/{}", "d".repeat(crate::harness::upgrade::MAX_LINE));
    let mut deep = rig.snap(Some(C1));
    deep.cwd = deep_dir.clone();
    assert!(!carries_restart_onto(&deep, home, twin()), "a cd too long");
    {
        let rig = Rig::new("restart-deep", 0, true, false, false);
        let mut deep = rig.snap(Some(C1));
        deep.cwd = deep_dir;
        let r = rig.attempt(&deep, &ExitRecord::Removed, ExitCause::Asked);
        assert!(r.step.starts_with("failed:line:the line would be"), "{r:?}");
        assert_eq!(rig.lines(), 0, "{:#?}", rig.typed());
    }
    let refused = after_exit_onto(
        &rig.opts,
        &snap,
        &ExitRecord::Removed,
        ExitCause::Asked,
        None,
        &rig.kernel,
    );
    assert_eq!(refused.step, "refused:no-managed-agy", "{refused:?}");
    assert!(
        !carries_restart_onto(&snap, home, None),
        "a store build with no twin: its relaunch refuses, so the act is not carried"
    );
    let agents = |dir: Option<PathBuf>| {
        super::super::AGENTS_DIR.with(|d| *d.borrow_mut() = dir);
    };
    agents(rig.twin.parent().map(Path::to_path_buf));
    assert!(
        carries_restart(&rig.opts.home, &snap),
        "a store build with its managed twin"
    );
    agents(Some(rig.dir.join("no-agents")));
    assert!(
        !carries_restart(&rig.opts.home, &snap),
        "a store build with no managed twin"
    );
    agents(None);
    let mut own = rig.snap(Some(C1));
    own.program = rig.dir.join("opt/bin/agy");
    assert!(carries_restart(&rig.opts.home, &own));
    own.session = None;
    assert!(!carries_restart(&rig.opts.home, &own));
    let left = rig.look(&snap);
    assert_eq!(left, ExitRecord::Removed);
    let r = rig.attempt(&snap, &left, ExitCause::Asked);
    assert_eq!(r.step, "continued", "{r:?}");
    assert!(
        rig.typed()[1].contains("someone asked aterm to restart Antigravity CLI"),
        "{:#?}",
        rig.typed()
    );
    let own = Rig::new("restart-control", 0, true, false, false);
    let r = own.attempt(&own.snap(Some(C1)), &ExitRecord::Removed, ExitCause::Exit);
    assert_eq!(r.step, "ended:graceful-exit");
    for (cwd, in_workspace, want) in [
        (WORKSPACE, true, "continued"),
        ("/w/pr\noj", true, "continued"),
        (WORKSPACE, false, "failed:line:fish"),
    ] {
        let mut rig = Rig::new("restart-fish", 0, true, false, false);
        rig.kernel.knobs.fish = true;
        rig.kernel.knobs.shell_in_workspace = in_workspace;
        rig.kernel.knobs.workspace = Some(cwd.to_string());
        let mut snap = rig.snap(Some(C1));
        snap.dialect = Some(Dialect::Fish);
        snap.cwd = cwd.to_string();
        assert!(carries_restart_onto(
            &snap,
            &rig.opts.home,
            Some(rig.twin.clone())
        ));
        let r = rig.attempt(&snap, &ExitRecord::Removed, ExitCause::Asked);
        assert!(r.step.starts_with(want), "{cwd:?} {in_workspace}: {r:?}");
        let typed = rig.typed();
        assert!(
            typed.iter().all(|l| !l.contains("cd --")),
            "{cwd:?} {in_workspace}: {typed:#?}"
        );
        assert_eq!(rig.lines(), usize::from(in_workspace), "{typed:#?}");
    }
}

#[test]
fn the_restart_predicate_asks_the_relaunch_line_at_worst() {
    let bound = crate::harness::upgrade::MAX_LINE;
    let mut rig = Rig::new("restart-bound", 0, true, false, false);
    let mut carried = std::collections::BTreeMap::new();
    for heal in [false, true] {
        if heal {
            let shelld = rig.opts.home.join(".aterm/shell.d");
            std::fs::create_dir_all(&shelld).expect("shell.d");
            for hook in ["00-atpkg.zsh", "00-atpkg.fish"] {
                std::fs::write(shelld.join(hook), "").expect("hook");
            }
        }
        for dialect in [Dialect::Zsh, Dialect::Fish] {
            rig.kernel.knobs.fish = dialect == Dialect::Fish;
            let mut n = 0usize;
            for len in 0..=bound {
                let mut snap = rig.snap(Some(C1));
                snap.dialect = Some(dialect);
                // A directory beside the shell's (`/w`), never `/w/` itself: one directory under
                // two spellings needs no cd.
                snap.cwd = format!("/x/{}", "d".repeat(len));
                let ask = carries_restart_onto(&snap, &rig.opts.home, Some(rig.twin.clone()));
                n += usize::from(ask);
                let st = St {
                    agent: Agent::Agy,
                    thread: C1.to_string(),
                    argv: snap.argv.clone(),
                    cwd: snap.cwd.clone(),
                    twin: rig.twin.to_string_lossy().into_owned(),
                    shell: SHELL,
                    agy_env: snap
                        .agy()
                        .map(|run| ag::env_words(&run.env))
                        .unwrap_or_default(),
                    ..St::default()
                };
                let mut fits = |in_workspace: bool| {
                    rig.kernel.knobs.shell_in_workspace = in_workspace;
                    rig.kernel.knobs.workspace = Some(snap.cwd.clone());
                    plan(&rig.opts, &st, &rig.kernel).is_ok()
                };
                let (there, elsewhere) = (fits(true), fits(false));
                if dialect == Dialect::Zsh {
                    assert!(!ask || there, "{heal} {len}: carried, refused in place");
                    assert_eq!(ask, elsewhere, "{heal} {len}: from another directory");
                } else {
                    assert_eq!(ask, there, "{heal} {len}: from the launch's directory");
                    assert!(!elsewhere, "{heal} {len}: fish takes no cd");
                }
            }
            carried.insert((heal, dialect == Dialect::Fish), n);
        }
    }
    let zsh = carried[&(false, false)];
    assert!(
        zsh > 0 && zsh < bound + 1,
        "the bound within the range: {carried:?}"
    );
    assert!(carried[&(false, true)] > zsh, "{carried:?}");
    assert!(
        carried[&(true, false)] < zsh,
        "the heal counted: {carried:?}"
    );
}

#[test]
fn records_are_keyed_by_tab_and_pruned_once_gone() {
    assert_eq!(key(TAB), format!("agy-{TAB}"));
    assert_eq!(Agent::Agy.record_tab(&key(TAB)), Some(TAB));
    assert_eq!(Agent::keyed(&key(TAB)), Some(Agent::Agy));
    assert!(!upgrade::is_session_id(&key(TAB)));
    let rig = Rig::new("prune", 0, true, false, false);
    let homed = St {
        agent: Agent::Agy,
        tab: TAB.to_string(),
        agy_config: "/h/.gemini/antigravity-cli".to_string(),
        agy_env: vec!["HOME=/h".to_string()],
        phase: Phase::Done,
        ..St::default()
    };
    let text = homed.to_json();
    assert!(
        text.contains("\"agy_config\":") && text.contains("\"agy_env\":"),
        "{text}"
    );
    assert_eq!(
        St::from_json(&text).map(|st| (st.agy_config, st.agy_env)),
        Some((homed.agy_config.clone(), homed.agy_env.clone()))
    );
    let muse = St {
        agent: Agent::Muse,
        ..St::default()
    }
    .to_json();
    assert!(!muse.contains("agy_"), "{muse}");
    let old = |tab: &str, phase: Phase| {
        let st = St {
            agent: Agent::Agy,
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

fn relaunch_rig(name: &str, hand_back: bool, knobs: Knobs) -> Rig {
    let mut rig = Rig::new(name, 137, true, false, true);
    rig.opts.hand_back = hand_back;
    rig.opts.timings.agy_found = Duration::from_millis(400);
    rig.opts.timings.agy_adopt = Duration::from_millis(300);
    rig.opts.timings.carry_settle = Duration::from_millis(400);
    rig.opts.timings.agy_up = Duration::from_millis(400);
    rig.opts.timings.agy_lock = Duration::from_millis(300);
    if knobs.new_dies || knobs.noappear {
        rig.after.lock().expect("after").screen = Some(shell_after());
    }
    rig.kernel.knobs = knobs;
    rig
}

#[test]
fn a_relaunch_agy_refused_is_never_typed_again() {
    for (exit, new_crash, want, lines) in [
        (2, false, "refused:resume-refused", 1),
        (127, false, "refused:resume-refused", 1),
        (0, false, "ended:relaunch-ended", 1),
        (137, false, "refused:relaunch-unwitnessed", 1),
        (137, true, "wait:resume", 2),
        (2, true, "wait:resume", 2),
    ] {
        let rig = relaunch_rig(
            "refused-relaunch",
            true,
            Knobs {
                new_dies: true,
                ..Knobs::default()
            },
        );
        let snap = rig.snap(Some(C1));
        rig.line_ran(None);
        let first = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(first.step, "wait:resume", "{exit}: {first:?}");
        assert_eq!(rig.lines(), 1);
        assert_eq!(rig.state().relaunched_pid, NEW, "found, on the record");
        if new_crash {
            rig.new_crashed();
        }
        rig.line_ran(Some(exit));
        let again = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(again.step, want, "{exit} {new_crash}: {again:?}");
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
}

#[test]
fn a_relaunch_that_never_comes_up_is_never_typed_again() {
    let knobs = Knobs {
        noappear: true,
        ..Knobs::default()
    };
    for (exit, want) in [
        (2, "refused:resume-refused"),
        (126, "refused:resume-refused"),
        (137, "refused:relaunch-unwitnessed"),
    ] {
        let rig = relaunch_rig("never", true, knobs.clone());
        let snap = rig.snap(Some(C1));
        rig.line_ran(Some(exit));
        let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{exit}: {r:?}");
        let again = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(again.step, want, "{exit}: {again:?}");
        assert_eq!(rig.lines(), 1, "{exit}: {:#?}", rig.typed());
    }
}

#[test]
fn a_relaunch_stopped_before_adoption_is_never_typed_again() {
    for (status, killed) in [
        (ctrl_z(), false),
        (sigstop(), false),
        (ctrl_z(), true),
        (1, false),
    ] {
        let mut rig = relaunch_rig(
            "stopped-relaunch",
            true,
            Knobs {
                out_of_tab: true,
                ..Knobs::default()
            },
        );
        let snap = rig.snap(Some(C1));
        rig.line_ran(Some(status));
        rig.new_crashed();
        let first = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(first.step, "ended:relaunch-ended", "{status}: {first:?}");
        let alive = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(alive.step, "ended:relaunch-ended", "{status}: {alive:?}");
        if status == 1 {
            assert_eq!(rig.lines(), 1, "{status}: {:#?}", rig.typed());
            continue;
        }
        rig.kernel.knobs.new_dies = true;
        if !killed {
            std::fs::remove_file(
                rig.config
                    .join(ag::CRASHES)
                    .join(format!("crash_{NEW}_{CRASH_UUID}.log")),
            )
            .expect("its crash file removed by its quit");
        }
        let gone = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(
            gone.step, "ended:relaunch-ended",
            "{status} {killed}: {gone:?}"
        );
        assert_eq!(rig.lines(), 1, "{status} {killed}: {:#?}", rig.typed());
    }
    let control = relaunch_rig(
        "crashed-relaunch",
        true,
        Knobs {
            new_dies: true,
            ..Knobs::default()
        },
    );
    let snap = control.snap(Some(C1));
    control.line_ran(None);
    let _ = control.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    control.new_crashed();
    control.line_ran(Some(137));
    let _ = control.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(control.lines(), 2, "{:#?}", control.typed());
}

#[test]
fn a_relaunch_that_started_fresh_is_told_and_typed_nothing() {
    for hand_back in [true, false] {
        let rig = relaunch_rig(
            "fresh",
            hand_back,
            Knobs {
                new_lockless: true,
                ..Knobs::default()
            },
        );
        let r = rig.attempt(&rig.snap(Some(C1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(
            r.step, "refused:started-fresh",
            "hand_back={hand_back}: {r:?}"
        );
        assert_eq!(rig.continuations(), 0, "{:#?}", rig.typed());
        assert_eq!(rig.state().resumed_pid, 0, "never adopted");
        assert_eq!(
            rig.state().phase,
            Phase::Failed("started-fresh".to_string())
        );
    }
    assert!(matches!(
        crate::harness::relaunch::outcome("refused:started-fresh"),
        crate::harness::relaunch::Outcome::Cannot(_)
    ));
    let control = relaunch_rig("held", true, Knobs::default());
    let r = control.attempt(
        &control.snap(Some(C1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!((r.step.as_str(), r.pid), ("adopted", NEW), "{r:?}");
    let typed = relaunch_rig("held-typed", false, Knobs::default());
    let r = typed.attempt(&typed.snap(Some(C1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "continued", "{r:?}");
}

#[test]
fn the_typing_fence_names_what_it_refuses() {
    use aterm_phase::agy::fixtures as fx;
    let rig = relaunch_rig("fence", false, Knobs::default());
    rig.kernel.relaunched.store(true, Ordering::SeqCst);
    let fence = |screen: String| {
        rig.relaunched_shows(Some(screen), None);
        let mut c = connect(&rig.opts, TAB).expect("the instance");
        typing_fence(&mut c, TAB).map(|_| ())
    };
    for (name, text, want) in [
        ("box", fx::BOX_EXEC, "box"),
        ("busy", fx::BUSY, "busy"),
        ("exit-pending", fx::EXIT_CONFIRM, "busy"),
        ("draft", fx::DRAFT, "draft"),
        ("bg-task", fx::lab::BG_TASK, "busy"),
        ("bg-timer", fx::lab::BG_TIMER, "busy"),
        ("bg-subagent", fx::lab::BG_SUBAGENT, "busy"),
        (
            "bg-task-and-subagent",
            fx::lab::BG_TASK_AND_SUBAGENT,
            "busy",
        ),
        ("bg-daemon", fx::lab::BG_DAEMON, "busy"),
        ("bg-draft", fx::lab::BG_DRAFT, "busy"),
        ("bg-busy", fx::lab::BG_BUSY, "busy"),
    ] {
        assert_eq!(fence(at_caret(text)), Err(want), "{name}");
    }
    assert_eq!(fence(agy_idle()), Ok(()), "the control: at rest");
    assert_eq!(
        fence(at_caret(fx::lab::BG_TASK_ENDED)),
        Ok(()),
        "the control: the task ended"
    );
}

#[test]
fn no_continuation_into_a_busy_drafted_or_held_agy() {
    use aterm_phase::agy::fixtures as fx;
    let person = "OK schema=1 hold=0 hand=- integration=on human_ms=400";
    let cases: Vec<(&str, Option<String>, Option<&str>, Knobs)> = vec![
        ("busy", Some(at_caret(fx::BUSY)), None, Knobs::default()),
        (
            "background-task",
            Some(at_caret(fx::lab::BG_TASK)),
            None,
            Knobs::default(),
        ),
        ("draft", Some(at_caret(fx::DRAFT)), None, Knobs::default()),
        ("box", Some(at_caret(fx::BOX_EXEC)), None, Knobs::default()),
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
    ];
    for (name, screen, status, knobs) in cases {
        let rig = relaunch_rig(name, false, knobs);
        rig.relaunched_shows(screen, status);
        let r = rig.attempt(&rig.snap(Some(C1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(rig.lines(), 1, "{name}: {:#?}", rig.typed());
        assert_eq!(rig.continuations(), 0, "{name}: {r:?} {:#?}", rig.typed());
        assert_ne!(r.step, "continued", "{name}");
    }
    let control = relaunch_rig("plain", false, Knobs::default());
    let r = control.attempt(
        &control.snap(Some(C1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "continued", "{r:?}");
    assert_eq!(control.continuations(), 1);
}

#[test]
fn only_the_lines_own_agy_is_adopted() {
    let other = Knobs {
        new_argv: Some(vec![
            "/p/agents/agy".to_string(),
            format!("--conversation={C2}"),
        ]),
        ..Knobs::default()
    };
    let orphan = Knobs {
        new_orphan: true,
        ..Knobs::default()
    };
    for (name, knobs) in [("other-conversation", other), ("orphan", orphan)] {
        for hand_back in [true, false] {
            let rig = relaunch_rig(name, hand_back, knobs.clone());
            let r = rig.attempt(&rig.snap(Some(C1)), &ExitRecord::Crashed, ExitCause::Exit);
            assert_eq!(r.step, "wait:resume", "{name} hand_back={hand_back}: {r:?}");
            assert_eq!(rig.state().resumed_pid, 0, "{name}: nothing adopted");
            assert_eq!(rig.continuations(), 0, "{name}");
        }
    }
    let spelled = Knobs {
        new_argv: Some(vec![
            "/p/agents/agy".to_string(),
            "-conversation".to_string(),
            C1.to_string(),
        ]),
        ..Knobs::default()
    };
    let control = relaunch_rig("ours", true, spelled);
    let r = control.attempt(
        &control.snap(Some(C1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!((r.step.as_str(), r.pid), ("adopted", NEW), "{r:?}");
    assert_eq!(control.state().resumed_pid, NEW);
}

#[test]
fn the_conversations_holders_are_read_again_right_before_the_line() {
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
            "late-opener",
            Knobs {
                db_opener_from_read: Some(2),
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
            "wait:conversation-held",
        ),
    ] {
        let rig = relaunch_rig(name, true, knobs);
        let r = rig.attempt(&rig.snap(Some(C1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(rig.lines(), 0, "{name}: {:#?}", rig.typed());
    }
    let control = relaunch_rig("free", true, Knobs::default());
    let r = control.attempt(
        &control.snap(Some(C1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "adopted", "{r:?}");
    assert_eq!(control.lines(), 1);
}

#[test]
fn each_image_goes_to_its_own_lanes_reader() {
    use crate::harness::relaunch::{SnapshotLane, snapshot_lane};
    for (image, lane) in [
        (
            "/p/store/agy/1000001000004000000/bin/agy",
            SnapshotLane::Agy,
        ),
        ("/p/agents/agy", SnapshotLane::Agy),
        ("/p/store/muse/1/bin/muse", SnapshotLane::Muse),
        ("/opt/homebrew/bin/gemini", SnapshotLane::None),
        ("/usr/local/bin/codex", SnapshotLane::None),
        ("/Users//u/.local/bin/claude", SnapshotLane::Claude),
    ] {
        assert_eq!(snapshot_lane(Some(Path::new(image))), lane, "{image}");
    }
}

#[test]
fn a_relaunch_whose_pid_was_taken_is_read_by_its_line() {
    for (reborn, want) in [
        (true, "refused:resume-refused"),
        (false, "ended:relaunch-ended"),
    ] {
        let mut rig = relaunch_rig(
            "pid-taken",
            true,
            Knobs {
                new_dies: true,
                ..Knobs::default()
            },
        );
        let snap = rig.snap(Some(C1));
        rig.line_ran(None);
        let first = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(first.step, "wait:resume", "{reborn}: {first:?}");
        assert_eq!(
            (rig.state().relaunched_pid, rig.state().relaunched_start),
            (NEW, "new-birth".to_string()),
            "found, its start on the record"
        );
        rig.kernel.knobs = Knobs {
            new_reborn: reborn,
            out_of_tab: !reborn,
            ..Knobs::default()
        };
        rig.line_ran(Some(1));
        let again = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(again.step, want, "{reborn}: {again:?}");
        assert_eq!(rig.lines(), 1, "{reborn}: {:#?}", rig.typed());
    }
}

#[test]
fn tier1_the_real_agy_exit_read_conforms_to_the_model() {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let m = aterm_models_harness::harness_agy_exit_witness_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut rig = Rig::new("tier1", 0, true, false, false);
    let draw = |rig: &mut Rig, exit: i64, word: bool, later: bool, swept: bool| {
        let status = match exit {
            1 => 137,
            2 => 0,
            3 => ctrl_z(),
            _ => 2,
        };
        let crash_left = matches!(exit, 1 | 3 | 5) && !swept;
        rig.reset(status, word, later, crash_left);
        rig.kernel.stopped.store(exit == 3, Ordering::SeqCst);
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
        let witness = (st["witness"] == 1).then_some(C1);
        let (word, later, swept) = (st["word"] == 1, st["later"] == 1, st["swept"] == 1);
        if st["seen"] == 0 {
            looks += 1;
            let look = if word && !later && st["exit"] != 3 {
                "Look"
            } else {
                "LookGone"
            };
            let mut after = st.clone();
            assert!(m.fire(look, &mut after), "{look} at {st:?}");
            draw(&mut rig, st["exit"], word, later, swept);
            let left = rig.look(&rig.snap(witness));
            assert_eq!(left, kept(after["kept"]), "the look at {st:?}");
            for mutant in ["LookAsClaude", "RefusalAsCrash", "CrashFileIgnored"] {
                let mut wrong = st.clone();
                if buggy.fire(mutant, &mut wrong) && wrong["kept"] != after["kept"] {
                    assert_ne!(left, kept(wrong["kept"]), "{mutant} at {st:?}");
                    *caught.entry(mutant).or_default() += 1;
                }
            }
            continue;
        }
        if !attempted.insert((
            st["exit"],
            st["witness"],
            st["kept"],
            st["word"],
            st["later"],
            st["swept"],
        )) {
            continue;
        }
        attempts += 1;
        let mut after = st.clone();
        assert!(m.fire("Decide", &mut after), "Decide at {st:?}");
        draw(&mut rig, st["exit"], word, later, swept);
        let r = rig.attempt(&rig.snap(witness), &kept(st["kept"]), ExitCause::Exit);
        let relaunched = r.step == "continued";
        assert_eq!(
            relaunched,
            after["decided"] == 1,
            "the attempt at {st:?}: {r:?}"
        );
        for mutant in ["StopAsCrash", "DecideFresh"] {
            let mut wrong = st.clone();
            if buggy.fire(mutant, &mut wrong) && wrong["decided"] != after["decided"] {
                assert_ne!(relaunched, wrong["decided"] == 1, "{mutant} at {st:?}");
                *caught.entry(mutant).or_default() += 1;
            }
        }
    }
    assert!(
        looks > 0 && attempts > 0,
        "{looks} looks, {attempts} attempts"
    );
    for mutant in [
        "LookAsClaude",
        "RefusalAsCrash",
        "CrashFileIgnored",
        "StopAsCrash",
        "DecideFresh",
    ] {
        assert!(
            caught.get(mutant).copied().unwrap_or(0) >= 1,
            "{mutant} is never caught: {caught:?} ({looks} looks, {attempts} attempts)"
        );
    }
}

#[path = "upgrade_agy_live_drive_tests.rs"]
mod live_tests;

#[test]
fn an_in_flight_agy_relaunch_waits_out_a_hand_and_counts_staleness_from_seen_exit() {
    let rig = Rig::new("hand-and-exit-clock", 137, true, false, true);
    std::fs::create_dir_all(state_dir(&rig.opts)).expect("state");
    let snap = rig.snap(Some(C1));
    let now = now_s();
    let signal = now - STALE_S - 60;
    let base = St {
        agent: Agent::Agy,
        thread: C1.to_string(),
        agy_config: rig.config.to_string_lossy().into_owned(),
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

    save(&rig.opts, &key(TAB), &base);
    rig.after.lock().expect("after").before_status =
        Some("OK schema=1 hold=1 hand=- integration=on".to_string());
    let held = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(held.step, WAIT_HAND_LIFTS, "{held:?}");
    assert_eq!(rig.lines(), 0);
    let st = rig.state();
    assert_eq!(st.phase, Phase::Exiting { at_s: signal });
    assert!(st.hand_seen_s >= now && st.exited_at >= now, "{st:?}");

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

    rig.kernel.relaunched.store(false, Ordering::SeqCst);
    let fresh_exit = St {
        exited_at: now - 10,
        ..base.clone()
    };
    save(&rig.opts, &key(TAB), &fresh_exit);
    let r = carry_in_flight_with(
        &rig.opts,
        Report {
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
