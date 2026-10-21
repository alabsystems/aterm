// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The agy lane's driver over scripted screens and a scripted kernel: a
//! stand-in aterm instance on a real socket (every request recorded, nothing
//! typed anywhere) whose screen is the shell after the agy's exit, and the
//! relaunched agy once the relaunch line is typed; a kernel whose agy has
//! exited (or stopped) and whose relaunch appears under the shell when its
//! line is; and the conversation's presence lock and the dead pid's crash
//! file on disk, as 1.4.0 lays them out. Screens are 1.4.0's (`aterm-phase`
//! fixtures).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::super::{Job, parse_exit_status};
use super::*;
use crate::harness::relaunch::{AgentRun, AgyRun, ExitCause, ExitRecord, Snapshot, exit_record};
use crate::supervise::ScratchDir;

const TAB: &str = "s-0123456789abcdef4a67";
const TOKEN: &str = "0badf00d0badf00d";
/// The conversation the crashed agy held; another, a `/clear` moved it to.
const C1: &str = "eaeec4fb-4e43-47da-b46f-8a10c0fee186";
const C2: &str = "5f2b9c3e-7d41-4a8e-9b2c-1e3d5f7a9b0c";
/// The crash file's own uuid (never the conversation's).
const CRASH_UUID: &str = "9c1f0d6e-2b7a-4c55-9a3e-1f2e3d4c5b6a";
/// Pids no real process of this test has: the kernel is scripted.
const TUI: u32 = 4_200_001;
const SHELL: u32 = 4_200_002;
const NEW: u32 = 4_200_003;
const OTHER: u32 = 4_200_004;
/// The store build atpkg encodes 1.4.0 as.
const BUILD: &str = "1000001000004000000";
const VERSION: &str = "1.4.0";
/// The kernel start the snapshot names (`ps -o lstart`, squashed).
const START: &str = "Sat Oct 17 06:40:00 2026";
/// The launch's directory (its workspace), and the shell's own directory.
const WORKSPACE: &str = "/w/proj";
const SHELL_CWD: &str = "/w";

/// Ctrl-Z's status: `128` plus this platform's SIGTSTP (`146` on macOS,
/// measured; `148` on Linux).
fn ctrl_z() -> i64 {
    128 + i64::from(libc::SIGTSTP)
}

/// SIGSTOP's status: `128` plus this platform's SIGSTOP (`145` on macOS).
fn sigstop() -> i64 {
    128 + i64::from(libc::SIGSTOP)
}

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("agd", name)
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

/// A fixture as the stand-in serves it, the cursor on its caret row.
fn at_caret(text: &str) -> String {
    let rows = fixture_rows(text);
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('>'))
        .unwrap_or(rows.len().saturating_sub(1));
    screen_json(&rows, (caret, 2), 20)
}

/// The relaunched agy at rest on its replayed turn: 1.4.0's end of a turn,
/// the cursor on its empty caret row.
fn agy_idle() -> String {
    at_caret(aterm_phase::agy::fixtures::END_OF_TURN)
}

/// The shell after the agy's exit: its crash's trace, the prompt.
fn shell_after() -> String {
    let rows: Vec<String> = ["% sh agy.sh", "zsh: killed     sh agy.sh", "% "]
        .iter()
        .map(|r| (*r).to_string())
        .collect();
    screen_json(&rows, (2, 2), 900)
}

/// The shell integration's last two blocks: the command that ran the agy
/// (block 7) finished with `exit`, then the prompt entering — `word` false:
/// the prompt not back yet; `later`: another command's block (9) the newest
/// since.
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

/// The shell's blocks once the relaunch line ran in block 8 (the prompt
/// block [`blocks`] names newest): `exit` `None` while its agy runs, else
/// that line's status and the prompt after it.
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
    screen: Option<String>,
    status: Option<String>,
    blocks: Option<String>,
    /// The shell's blocks BEFORE the line, in place of the exit's
    /// ([`blocks`]): the agy still running in its block ([`running`]).
    running: Option<String>,
}

/// The shell's blocks while the agy runs in block 7, the one [`blocks`]
/// names finished once it exits.
fn running() -> String {
    r#"{"blocks":[{"id":7,"state":"executing","exit":null,"prompt":2,"cmd":2,"cmdcol":2,"out":3,"end":null,"cmdline":"sh agy.sh"}]}"#.to_string()
}

/// What the stand-in serves, and what it saw.
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

/// A control socket at `<dir>/t.sock` standing in for the aterm instance of
/// [`TAB`], and the requests it was asked, in order.
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
                    w.after_exit.clone()
                }
            ),
            "cell" => "OK > ccd3db 1e1e1e none".to_string(),
            "status" => served(&after.status, &w.status),
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

/// The scripted kernel: [`TUI`] was [`SHELL`]'s foreground job and has
/// exited (or is stopped, alive: `stopped`); its relaunch [`NEW`] appears
/// under the shell when its line is typed, holding the conversation's lock.
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
    /// `0` lives, as the live kernel's `kill(0, 0)` answers (the caller's
    /// own process group).
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
        // Asked under the config the conversation's lock is in, and nowhere
        // else.
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

/// One scripted rig: a home with agy's config (the conversation's presence
/// lock, and the dead pid's crash file where it is left), a managed store
/// build and its twin, the state directory, the stand-in instance and the
/// kernel.
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
}

impl Rig {
    /// `status`: the exit status the shell's block names; `word`, `later` as
    /// [`blocks`]; `crash_left`: the dead pid's crash file still there (a
    /// SIGKILL's, measured; a panic's only if it leaves it, unmeasured; every
    /// graceful way out removes it).
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

    /// The rig [`Rig::new`] makes for `status`, `word`, `later` and
    /// `crash_left`, on this rig's scratch tree, store build and stand-in:
    /// agy's config made afresh (the conversation's presence lock, the dead
    /// pid's crash file where it is left, nothing else), the state and home
    /// directories gone, the stand-in serving the exit's blocks and nothing
    /// after the line, its log empty, and the kernel's agy exited, not
    /// stopped, nothing relaunched, no other holder and no knob turned. A
    /// walk over many worlds resets one rig per world instead of making and
    /// removing one: the scratch tree and the stand-in's socket and thread
    /// were most of its time.
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

    /// The snapshot the host took of the agy while it ran: launched by its
    /// store path with `--effort high`, a dropped `--agent`, its own
    /// `--conversation` of another conversation (which a `/clear` moved
    /// past), its conversation `conversation` (`None`: no witness), its
    /// crash file, the block it ran in.
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

    /// [`exit_record`] over the real look at this rig's exit. Nothing a look
    /// asks moves the stand-in or the kernel, so the world stands still while
    /// it is looked at: the second look is real and must read what the first
    /// did, and every later one is the first's again. `exit_record`'s loop runs
    /// whole over them (its waits and their count are `relaunch_tests`'), but
    /// an undecided exit's 81 looks are two connects to the stand-in, not 81.
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

    /// The shell's blocks once the relaunch line ran ([`line_blocks`]).
    fn line_ran(&self, exit: Option<i64>) {
        self.after.lock().expect("after").blocks = Some(line_blocks(exit));
    }

    /// The relaunched agy's screen and the tab's `status` from now on.
    fn relaunched_shows(&self, screen: Option<String>, status: Option<&str>) {
        let mut after = self.after.lock().expect("after");
        after.screen = screen;
        after.status = status.map(str::to_string);
    }

    /// The relaunch lines typed (the turns naming `--conversation=`).
    fn lines(&self) -> usize {
        self.typed()
            .iter()
            .filter(|l| l.contains("'--conversation="))
            .count()
    }

    /// The continuations typed.
    fn continuations(&self) -> usize {
        self.typed()
            .iter()
            .filter(|l| l.contains("[aterm harness]"))
            .count()
    }

    /// The relaunched agy's own crash file, left (it crashed).
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

/// A CRASH (SIGKILL, 137, its crash file left) IS RELAUNCHED ON ITS
/// CONVERSATION, IN ITS LAUNCH DIRECTORY, ON THE MANAGED TWIN: the line `cd`s
/// to the launch's directory and types the twin with the launch's `--effort`
/// carried and `--conversation=<uuid>` of the witnessed conversation, behind
/// the `HOME` the process ran under — never the store path argv[0] named, its
/// `--agent` or its own stale `-conversation` — and once the relaunched agy
/// holds the conversation's lock and reads idle, the continuation is typed and
/// submitted (`continued`). The record is keyed `agy-<tab>`.
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

/// The exit's word is the shell's: the finished block that ends where the
/// entering prompt begins, its `exit` — of the block the agy ran in.
/// NEGATIVE CONTROL: another command's block newest since is no word for it.
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

/// EVERY GRACEFUL WAY OUT (`0`: its own exit, someone's SIGTERM, SIGHUP or
/// SIGINT, the crash file removed) IS LEFT, AND SO IS A REFUSED START (`2`,
/// no crash file) AND AN END NO CRASH FILE WITNESSES (`137`, the file swept by
/// another agy's start). An end `2` whose crash file is left (a Go panic
/// that leaves it, unmeasured) is a crash.
/// NEGATIVE CONTROL: the status alone (any nonzero a crash) would relaunch the
/// refusal and the unwitnessed end; nothing is typed for any left exit.
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
        // A table that reads the status alone (any nonzero a crash).
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

/// CTRL-Z ([`ctrl_z`]): agy is stopped and ALIVE, holding its lock and its crash
/// file — never a crash, never relaunched (`wait:not-exited`, which the host
/// leaves as a program still there). One gone with that word (killed while
/// stopped, outside its block) is left too (`ended:stopped`), its crash file
/// notwithstanding. NEGATIVE CONTROL: the crash file alone would relaunch
/// both.
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
    // Gone with its block's Ctrl-Z status and its crash file left.
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

/// A crash whose process held no proven conversation (a fresh agy that took
/// no turn) is relaunched by nothing — a fresh `agy` would start another
/// conversation — and neither is one whose presence lock is gone from its
/// config. NEGATIVE CONTROL: the same crash with its witness is relaunched.
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

/// A TAB'S OWN AGY (`aterm -e agy`, `exec agy`: shell
/// [`crate::harness::relaunch::NO_SHELL`]) that crashed is relaunched by
/// nothing, as a tab's own Muse Code or Claude Code is: no shell is left in
/// its tab to type the line into, its plan is refused for good (never waited
/// on), and the explicit restart is not carried for it. NEGATIVE CONTROLS:
/// the same crash with a shell is relaunched, and the same agy run from no
/// store (no twin asked) at a shell has its restart carried.
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
    // The restart's gate asks the shell itself, not only the dialect a
    // snapshot of no shell lacks.
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

/// A CONVERSATION ANOTHER LIVE AGY HAS OPEN is never resumed a second time:
/// its lock held, or — a second agy opened on it while another held it holds
/// no lock, even once that holder is gone (measured) — its argv naming it,
/// or its database held by an agy that holds no presence lock (opened with
/// `-c` or an in-TUI `/resume`, its argv naming nothing: measured on 1.4.2),
/// or any of these unreadable. NEGATIVE CONTROL: free by all three, the same
/// record is relaunched.
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

/// A RELAUNCH LEFT IN FLIGHT WHOSE CONVERSATION'S LOCK HAS GONE SINCE is
/// stopped, never typed (agy would answer `conversation not found` and start
/// afresh): the first attempt waited on a holder, the lock file was removed
/// meanwhile, and the carried attempt fails `conversation-gone`. NEGATIVE
/// CONTROL: the same wait with the lock kept is carried on and relaunched.
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

/// A launch whose flags a relaunch cannot carry (`--remote-control`) is
/// refused with its word, nothing typed; a store build with no twin to type
/// is relaunched by nothing. NEGATIVE CONTROL: a program run from no store is
/// relaunched on itself.
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

/// THE WITNESS: the one presence lock under its config the process holds
/// open names its conversation, and the one crash file of its pid it holds is
/// its crash file. NEGATIVE CONTROLS: a fresh agy holds no lock (no
/// conversation yet); two locks name none; a lock under another config, and
/// another pid's crash file, are not its; a one-shot is no TUI.
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
    // A tab's own agy (`exec agy`, [`crate::harness::relaunch::NO_SHELL`])
    // keeps its witness for its readers, and no shell's dialect or block is
    // read for it, even where pid 0 reads as a zsh and the tab names a
    // running block. NEGATIVE CONTROLS: the same kernel reads pid 0's dialect
    // as zsh, and at its shell (above) its dialect and block are read.
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
    // The hidden flags move the config the witness reads.
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

/// The rig's kernel, again, for a test that varies it.
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

/// THE CRASH FILE IS THE VERY ONE ITS PROCESS HELD: where the snapshot saw
/// none, a crash file of its pid counts only when written since it started
/// (a dead pid's file an earlier process of the same number left is no
/// witness). NEGATIVE CONTROL: the file it held, or one written since its
/// start, is.
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

/// A RESTORED LAYOUT'S RELAUNCH (aterm's own end): no shell word is asked,
/// the conversation is named again by its id under its config, and the
/// relaunch says aterm ended. NEGATIVE CONTROL: the same snapshot's own exit,
/// clean, is left.
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

/// THE EXPLICIT RESTART IS CARRIED FOR AN AGY: its SIGTERM is one of agy's
/// graceful ways out, mid-turn too (measured), so the act's end (its stamp's
/// cause) is relaunched on the conversation though its exit is clean — and
/// the act asks the relaunch's own predicate (`carries_restart_onto`), so it
/// ends only what that relaunch then brings back: at a fish shell too, whose
/// relaunch from the launch's own directory types no `cd` — a directory no
/// `cd` could name included. The act's gate (`carries_restart`) reads the
/// managed twin for a store build. NEGATIVE CONTROLS: an agy with no
/// witnessed conversation, a launch its line cannot carry, a store build
/// with no managed twin to type (whose relaunch says
/// `refused:no-managed-agy`, through the act's gate too), a `HOME` no line
/// can hold, and at zsh a directory no `cd` can name or one too long for the
/// `cd` its relaunch from another directory types (which refuses the line
/// for its length) are not; the same clean exit with no stamp is left; and a
/// fish shell in another directory than the launch's refuses the `cd` its
/// relaunch then needs — which no snapshot tells, so the predicate asks
/// fish's line without it.
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
    // A launch directory too long for the `cd` a zsh's relaunch from
    // another directory types: not carried, as that relaunch refuses it.
    // As long as the host kernel's line bound (1000 bytes on macOS, 4000 on
    // Linux) on its own, so the `cd` alone passes it.
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
    // The store build with no twin: the act's own end, relaunched by nothing.
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
    // The act's own gate on the store build: carried through the managed
    // twin it reads for one, in atpkg's `agents/`; not with none there.
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
    // The act's own gate, on a program run from no store (no twin read):
    // the same predicate.
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
    // At a fish shell: from the launch's own directory the act's end is
    // relaunched with no `cd` (in a directory no `cd` could name too); from
    // another, the `cd` it needs is refused.
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

/// THE RESTART'S PREDICATE ASKS THE LINE ITS RELAUNCH TYPES AT WORST, heal
/// included (`carries_restart_onto` against [`plan`], over launch
/// directories of every length up to past the tty's bound): at zsh, where it
/// carries the act the relaunch's line fits from any directory, and from
/// another directory than the launch's (the `cd` typed) exactly where; at
/// fish, exactly where the relaunch from the launch's own directory (no `cd`)
/// fits, while from another it is refused at every length (the predicate's
/// one boundary). NEGATIVE CONTROLS: the bound falls inside the range at
/// zsh, fish carries lengths zsh's `cd` cannot, and at zsh the heal, once
/// written, carries fewer.
#[test]
fn the_restart_predicate_asks_the_relaunch_line_at_worst() {
    // Every directory length up to the host kernel's line bound (1000 bytes on
    // macOS, 4000 on Linux), which the longest of them passes once its `cd`
    // and relaunch are typed.
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
                snap.cwd = format!("/w/{}", "d".repeat(len));
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

/// The records are the lane's: keyed `agy-<tab>`, read back by their tab,
/// never a Claude Code conversation's key, the config and the carried home
/// round-tripping; a settled record of a tab proven gone, unwritten a week,
/// is pruned. NEGATIVE CONTROLS: a live tab's, an in-flight one, and another
/// lane's record (which writes neither key) are kept.
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

/// A rig for the relaunch's own end: the crash relaunched (`hand_back` as
/// the host types it), its waits short — every one here ends on its bound
/// only where nothing is ever coming.
fn relaunch_rig(name: &str, hand_back: bool, knobs: Knobs) -> Rig {
    let mut rig = Rig::new(name, 137, true, false, true);
    rig.opts.hand_back = hand_back;
    rig.opts.timings.agy_found = Duration::from_millis(400);
    rig.opts.timings.agy_adopt = Duration::from_millis(300);
    rig.opts.timings.carry_settle = Duration::from_millis(400);
    rig.opts.timings.agy_up = Duration::from_millis(400);
    rig.opts.timings.agy_lock = Duration::from_millis(300);
    // A relaunched agy that never stays up leaves the shell's prompt.
    if knobs.new_dies || knobs.noappear {
        rig.after.lock().expect("after").screen = Some(shell_after());
    }
    rig.kernel.knobs = knobs;
    rig
}

/// A RELAUNCH AGY REFUSED IS NEVER TYPED AGAIN (critic X4): the relaunched
/// agy came up under the shell and was gone before it led the tab — its
/// line's own block then said `2` with no crash file of its pid (a carried
/// flag a newer build rejects) — and the exit's next attempt reads that
/// line's word: `refused:resume-refused`, the line typed once. So is a line
/// the shell could not run (`127`), and one that ended with no crash file
/// (`137`, unwitnessed) stops too. NEGATIVE CONTROL: the same relaunch whose
/// own agy left its crash file (it crashed) is typed again.
#[test]
fn a_relaunch_agy_refused_is_never_typed_again() {
    for (exit, new_crash, want, lines) in [
        (2, false, "refused:resume-refused", 1),
        (127, false, "refused:resume-refused", 1),
        (0, false, "ended:relaunch-ended", 1),
        (137, false, "refused:relaunch-unwitnessed", 1),
        // Typed again; that relaunch too is gone at once, its own line
        // (block 9) still running as this attempt looks.
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

/// A RELAUNCH THAT NEVER COMES UP is decided by its line's own end AT ONCE,
/// never waited out and typed again: agy refused it (`2`), or the shell could
/// not run it (`126`): `refused:resume-refused`. One whose agy was never
/// found, its pid unknown, that ended `137` left no crash file anything can
/// read as its: it stops too (`refused:relaunch-unwitnessed`), where Muse
/// Code's lane types a `137` again — the crash an agy line is typed again for
/// is one its own crash file witnesses
/// (`a_relaunch_agy_refused_is_never_typed_again`). No agy found is no live
/// relaunch, though the kernel answers pid `0` alive (`kill(0, 0)`, as the
/// stand-in does).
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

/// A RELAUNCH STOPPED BEFORE IT WAS ADOPTED IS NEVER TYPED AGAIN: its line's
/// block ends while its agy lives (Ctrl-Z's status, SIGSTOP's, or a word the
/// table does not name — `1` here — from a shell that says a stop otherwise),
/// the agy alive, holding its lock and the crash file it made. That is
/// `ended:relaunch-ended` at once, and so is every later attempt at the exit:
/// while it lives, after the person `fg`s it and quits it (its crash file
/// removed), and after it is killed while stopped (its crash file left, its
/// block's word a stop's). NEGATIVE CONTROL: a line whose agy is gone with
/// `137` and its crash file left crashed, and is typed again.
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
            // A word no table reads as a stop decides only while its agy
            // lives.
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

/// THE RELAUNCHED AGY THAT HOLDS NO LOCK STARTED A FRESH CONVERSATION
/// (another `HOME`, a hidden flag not carried: `conversation not found`):
/// never adopted, nothing typed into it, the person told
/// (`refused:started-fresh`, the host's `Cannot`). NEGATIVE CONTROL: one that
/// takes the lock is adopted, and carried on (`continued`) where the step
/// types.
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

/// THE TYPING FENCE NAMES WHAT IT REFUSES, each arm on its own: a box `box`,
/// a turn running `busy`, an exit pending (one key from gone) `busy`, a draft
/// `draft`. agy's OWN BACKGROUND WORK is `busy` too (agy 1.4.0's lab
/// screens): a turn ended over its task panel (`  ● [hh:mm:ss] <what>
/// running`, a timer, a subagent `Working`, a daemon) is no rest — its reader
/// reads the composer above the panel and the work as the agent's own, busy
/// over the ended turn, and agy takes a turn of its own when the work ends —
/// and so is a draft held over the panel and a turn sent meanwhile. NEGATIVE
/// CONTROLS: the relaunched agy at rest is fenced on its read, and so is the
/// same screen once its task ended (no panel, no count).
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

/// NOTHING IS TYPED INTO A RELAUNCHED AGY THAT IS NOT IDLE, EMPTY AND UNHELD:
/// a turn running, its own background work running on under an ended turn
/// (the task panel), a draft in its composer, a box over it, a person's hand
/// within the grace, a process the host's roster does not show leading the
/// tab — each leaves the continuation untyped. NEGATIVE CONTROL: the plain
/// relaunch types it (`continued`).
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

/// ONLY OUR OWN RELAUNCH IS ADOPTED: an `agy` under the shell resuming
/// another conversation, or one resuming this conversation that is no child
/// of the shell the line was typed at, is never taken for the relaunch.
/// NEGATIVE CONTROL: the shell's child resuming the conversation, any
/// spelling, is adopted.
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

/// WHO HOLDS THE CONVERSATION IS READ RIGHT BEFORE THE LINE TOO: a holder
/// that appears after the relaunch's first look leaves the line untyped, so
/// does an agy that opened the conversation's database with no lock after
/// that look (`-c`, `/resume`), and so does a lock whose holders cannot be
/// read. NEGATIVE CONTROL: free at both looks, the line is typed.
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

/// Each agent's foreground process goes to its own lane's reader: agy to the
/// agy lane, Muse Code to its own, an image itself named `gemini` (no such
/// build was measured: Gemini CLI runs as `node`) to none ([`NO_LANE`]) —
/// never read as Claude Code. NEGATIVE CONTROLS: a vendor Codex to none; an
/// unnamed program to Claude Code's records.
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
        assert_eq!(snapshot_lane(Some(Path::new(image)), &[]), lane, "{image}");
    }
}

/// A RELAUNCH WHOSE NUMBER ANOTHER PROCESS TOOK IS NO LIVE RELAUNCH: its
/// agy was found (its kernel start on the record) and is gone, its line
/// ended with a word no table reads as a stop (`1`), and another process —
/// of another start, no agy under the shell — lives under that pid. The
/// line's own word decides (`refused:resume-refused`), never the stop a live
/// relaunch is. NEGATIVE CONTROL: the very relaunch alive, out of the tab
/// (its start the recorded one), is that stop (`ended:relaunch-ended`).
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

/// Tier-1 of `HarnessAgyExitWitness` (aterm-models-harness
/// `harness_agy_exit_witness_model`): every look of the model — a SIGKILL
/// (`137`, its crash file left or swept), a graceful way out (`0`, the file
/// removed), Ctrl-Z ([`ctrl_z`], alive, its file held), a refused start (`2`, no
/// file), a panic as the model has one (`2`, its file left or swept;
/// unmeasured); witnessed or not; the
/// shell's word drawn, not yet, or another command's block newest since — is
/// replayed through the real `look_at_exit` and `exit_record` over a stand-in
/// tab and the crash file on disk, and keeps what the model's does; every
/// attempt goes through the real `after_exit`, and relaunches exactly where
/// the model's `Decide` does. Negative controls: where the `Buggy = 1` model
/// looks at the exit as Claude Code's (`LookAsClaude`), reads a refusal as a
/// crash (`RefusalAsCrash`), ignores the crash file (`CrashFileIgnored`),
/// reads Ctrl-Z as a crash (`StopAsCrash`) or relaunches with no witness
/// (`DecideFresh`), the real code decides otherwise — each caught on its own.
#[test]
fn tier1_the_real_agy_exit_read_conforms_to_the_model() {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let m = aterm_models_harness::harness_agy_exit_witness_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    // One rig, reset to each world in turn ([`Rig::reset`]).
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

// The live upgrade's quiet flow, driven over the same stand-ins.
#[path = "upgrade_agy_live_drive_tests.rs"]
mod live_tests;
