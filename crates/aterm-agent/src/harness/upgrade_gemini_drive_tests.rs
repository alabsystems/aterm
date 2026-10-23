// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the Gemini CLI upgrade driver over scripted screens, process tables, and session files.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::super::{Job, parse_exit_status};
use super::upgrade::Dialect;
use super::*;
use crate::harness::relaunch::{AgentRun, ExitCause, ExitRecord, GeminiRun, Snapshot, exit_record};
use crate::supervise::ScratchDir;

const TAB: &str = "s-0123456789abcdef4a67";
const TOKEN: &str = "0badf00d0badf00d";
/// The session the crashed Gemini CLI ran; another (a `/clear`'s, an older
const S1: &str = "11edd4f1-7048-4ff7-8ff2-83cce0095a7c";
const S2: &str = "88409a32-5b1e-4c2a-9d7f-0e1f2a3b4c5d";
/// Pids no real process of this test has: the kernel is scripted.
const TUI: u32 = 4_300_001;
const SHELL: u32 = 4_300_002;
const NEW: u32 = 4_300_003;
const OTHER: u32 = 4_300_004;
const CHILD: u32 = 4_300_005;
/// The kernel start the snapshot names (`ps -o lstart`, squashed).
const START: &str = "Sun Oct 18 06:40:00 2026";
/// The node the parent runs under, and the script its argv names.
const NODE: &str = "/opt/homebrew/opt/node/bin/node";
const SCRIPT: &str = "/opt/homebrew/bin/gemini";
/// The shell's own directory.
const SHELL_CWD: &str = "/w";

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("gmd", name)
}

/// Now, unix milliseconds, to the millisecond.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
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

/// A fixture as the stand-in serves it, its provenance line dropped, the
fn at_caret(text: &str) -> String {
    let rows: Vec<String> = text.lines().skip(1).map(str::to_string).collect();
    let caret = rows
        .iter()
        .rposition(|r| [" > ", " * ", " ! "].iter().any(|c| r.starts_with(c)))
        .unwrap_or(rows.len().saturating_sub(1));
    screen_json(&rows, (caret, 3), 20)
}

/// The relaunched Gemini CLI at rest on its replayed turn: 0.46.0's end of a
fn gemini_idle() -> String {
    at_caret(aterm_phase::gemini::fixtures::END_OF_TURN)
}

/// The shell after the Gemini CLI's exit: its crash's trace, the prompt.
fn shell_after() -> String {
    let rows: Vec<String> = ["% gemini", "zsh: killed     gemini", "% "]
        .iter()
        .map(|r| (*r).to_string())
        .collect();
    screen_json(&rows, (2, 2), 900)
}

/// The shell integration's last two blocks: the command that ran the Gemini
fn blocks(exit: i64, word: bool, later: bool) -> String {
    let entering = r#"{"id":8,"state":"entering","exit":null,"prompt":4,"cmd":4,"cmdcol":2,"out":null,"end":null,"cmdline":""}"#;
    if !word {
        return format!(r#"{{"blocks":[{entering}]}}"#);
    }
    if later {
        return r#"{"blocks":[{"id":9,"state":"complete","exit":2,"prompt":4,"cmd":4,"cmdcol":2,"out":5,"end":6,"cmdline":"make"},{"id":10,"state":"entering","exit":null,"prompt":6,"cmd":6,"cmdcol":2,"out":null,"end":null,"cmdline":""}]}"#.to_string();
    }
    format!(
        r#"{{"blocks":[{{"id":7,"state":"complete","exit":{exit},"prompt":2,"cmd":2,"cmdcol":2,"out":3,"end":4,"cmdline":"gemini"}},{entering}]}}"#
    )
}

/// The shell's blocks once the relaunch line ran in block 8: `exit` `None`
fn line_blocks(exit: Option<i64>) -> String {
    let line = r#""cmdline":" ( cd -- '/w/proj' && exec 'gemini' '--resume' 's1' )""#;
    match exit {
        None => format!(
            r#"{{"blocks":[{{"id":8,"state":"executing","exit":null,"prompt":4,"cmd":4,"cmdcol":2,"out":5,"end":null,{line}}}]}}"#
        ),
        Some(exit) => format!(
            r#"{{"blocks":[{{"id":8,"state":"complete","exit":{exit},"prompt":4,"cmd":4,"cmdcol":2,"out":5,"end":6,{line}}},{{"id":9,"state":"entering","exit":null,"prompt":6,"cmd":6,"cmdcol":2,"out":null,"end":null,"cmdline":""}}]}}"#
        ),
    }
}

/// An ISO stamp of unix milliseconds `ms`, as the CLI writes one.
fn iso(ms: u64) -> String {
    format!(
        "{}.{:03}Z",
        aterm_types::rfc3339::format_rfc3339(ms / 1000).trim_end_matches('Z'),
        ms % 1000
    )
}

/// A session header as 0.46.0 writes one, at `at` (unix ms).
fn header(session: &str, hash: &str, at: u64) -> String {
    format!(
        r#"{{"sessionId":"{session}","projectHash":"{hash}","startTime":"{}","lastUpdated":"{}","kind":"main"}}"#,
        iso(at),
        iso(at)
    )
}

/// The `$set.messages` a launch writes after its header: the CLI's own
fn context(at: u64, id: &str) -> String {
    format!(
        r#"{{"$set":{{"lastUpdated":"{}","messages":[{{"id":"{id}","timestamp":"{}","type":"user","content":[{{"text":"<session_context>\nThis is the Gemini CLI.\n</session_context>"}}]}}]}}}}"#,
        iso(at),
        iso(at)
    )
}

/// One turn: a person's message and the model's reply, each with its
fn turn(at: u64, user: &str, reply: &str) -> String {
    format!(
        "{{\"id\":\"{user}\",\"timestamp\":\"{t}\",\"type\":\"user\",\"content\":[{{\"text\":\"remember DELTA-FOUR\"}}]}}\n\
         {{\"$set\":{{\"lastUpdated\":\"{t}\"}}}}\n\
         {{\"id\":\"{reply}\",\"timestamp\":\"{t}\",\"type\":\"gemini\",\"content\":\"ok\"}}\n\
         {{\"$set\":{{\"lastUpdated\":\"{t}\"}}}}",
        t = iso(at)
    )
}

/// What the project's session files say, each `false` as the plain crash
#[derive(Clone, Copy, Debug, Default)]
struct Files {
    /// A second header since its start (a `/clear` into [`S2`]).
    second: bool,
    /// Its resume would load an empty duplicate: the older file was not
    dup: bool,
    /// The session took no turn in any file.
    no_turn: bool,
    /// No header since its start (its duplicate not written).
    no_header: bool,
    /// It was a bare `gemini`: [`S1`]'s turn-holding file holds nothing of
    bare: bool,
    /// Another launch of the project ([`S2`]) begun 30 s after its start,
    late: bool,
    /// An older session ([`S2`]) of the project switched onto since its start
    switched: bool,
    /// [`S2`] resumed by `gemini --resume` before its start: its duplicate,
    resumed: bool,
    /// An older session ([`S2`]) a start wrote its summary into since, after
    summary: bool,
}

/// What the stand-in serves once the relaunch line is typed, which a test
#[derive(Default)]
struct After {
    screen: Option<String>,
    status: Option<String>,
    blocks: Option<String>,
}

/// What the stand-in serves, and what it does as the relaunch line is typed.
struct World {
    /// The shell's blocks after the exit ([`blocks`]), which a reset rig
    blocks2: Arc<Mutex<String>>,
    after_exit: String,
    after_relaunch: String,
    status: String,
    relaunched: Arc<AtomicBool>,
    relaunched_ms: Arc<AtomicU64>,
    after: Arc<Mutex<After>>,
    /// Where the relaunched process's resume writes its duplicate, and the
    chats: PathBuf,
    hash: String,
    new_session: Arc<Mutex<Option<String>>>,
    /// How long after the line its duplicate's header is begun, ms.
    header_after: Arc<AtomicU64>,
}

/// The turn-holding file of [`S1`] (`write_files`'s older one).
fn turn_file() -> String {
    format!("session-2026-10-18T05-40-{}.jsonl", &S1[..8])
}

/// A control socket at `<dir>/t.sock` standing in for the aterm instance of
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
            "sessions" => format!("OK 1\nlocal {TAB} 0 idle node"),
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
                served(&after.blocks, &w.blocks2.lock().expect("blocks"))
            ),
            "blocktext" => "OK 0".to_string(),
            "line" => "OK % ".to_string(),
            "lease" => "ERR unscripted".to_string(),
            "turn" => {
                if line.contains("'--resume'") && !relaunched {
                    let at = now_ms();
                    w.relaunched_ms.store(at, Ordering::SeqCst);
                    if let Some(session) = w.new_session.lock().expect("new").clone() {
                        let name = format!("session-relaunched-{}.jsonl", &session[..8]);
                        let begun = at + w.header_after.load(Ordering::SeqCst);
                        let text = format!(
                            "{}\n{}\n",
                            header(&session, &w.hash, begun),
                            context(begun, "ctx-new")
                        );
                        std::fs::write(w.chats.join(name), text).expect("duplicate");
                        let older = w.chats.join(turn_file());
                        if let Ok(was) = std::fs::read_to_string(&older)
                            .map_err(drop)
                            .and_then(|was| if session == S1 { Ok(was) } else { Err(()) })
                        {
                            let bump =
                                format!(r#"{{"$set":{{"lastUpdated":"{}"}}}}"#, iso(at + 530));
                            std::fs::write(&older, format!("{was}{bump}\n")).expect("bump");
                        }
                    }
                    w.relaunched.store(true, Ordering::SeqCst);
                }
                "OK 0 turn submitted=1 status=settled seq=1 id=1".to_string()
            }
            _ => "ERR unscripted".to_string(),
        }
    })
}

/// What a test varies about the relaunch, each `false`/`None` as the plain
#[derive(Clone, Debug, Default)]
struct Knobs {
    /// The host's roster never shows [`NEW`] leading the tab.
    out_of_tab: bool,
    /// [`NEW`] is listed under the shell but gone at once.
    new_dies: bool,
    /// No Gemini CLI ever comes up under the shell.
    noappear: bool,
    /// The argv [`NEW`] runs with in place of `--resume` [`S1`].
    new_argv: Option<Vec<String>>,
    /// [`NEW`]'s parent is another process than the shell.
    new_orphan: bool,
    /// The crashed parent's child [`CHILD`] lives on, orphaned, reading the
    orphan_reads: Option<usize>,
    /// Another live Gemini CLI of the project ([`OTHER`]) names [`S1`] in its
    argv_holder: bool,
    /// The process table cannot be read.
    table_unreadable: bool,
    /// Another live Gemini CLI of the project started 50 ms after the
    together: bool,
    /// [`NEW`]'s number was taken by another process.
    new_reborn: bool,
    /// The tab's shell is in the project, not its own directory.
    shell_in_project: bool,
}

/// The scripted kernel: [`TUI`] was [`SHELL`]'s foreground job and has
struct Script {
    exited: Arc<AtomicBool>,
    relaunched: Arc<AtomicBool>,
    relaunched_ms: Arc<AtomicU64>,
    started_ms: u64,
    project: String,
    knobs: Knobs,
    orphan_looks: AtomicUsize,
}

impl Kernel for Script {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        Some((Job::Foreground, SHELL))
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        match pid {
            NEW if self.knobs.new_orphan => Some(1),
            NEW | TUI => Some(SHELL),
            CHILD => Some(1),
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

impl GeminiKernel for Script {
    fn alive(&self, pid: u32) -> bool {
        match pid {
            0 => true,
            TUI => !self.exited.load(Ordering::SeqCst),
            NEW => {
                self.knobs.new_reborn
                    || (self.relaunched.load(Ordering::SeqCst)
                        && !self.knobs.new_dies
                        && !self.knobs.noappear)
            }
            CHILD => self.knobs.orphan_reads.is_some(),
            OTHER => self.knobs.argv_holder || self.knobs.together,
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
            NEW => Some(atpkg::caller_shell::ProcArgs {
                exec_path: NODE.to_string(),
                argv: self.new_argv(),
                env: Vec::new(),
            }),
            _ => None,
        }
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        matches!(pid, TUI | NEW).then(|| PathBuf::from(NODE))
    }

    fn cwd(&self, pid: u32) -> Option<String> {
        Some(if pid == SHELL && !self.knobs.shell_in_project {
            SHELL_CWD.to_string()
        } else {
            self.project.clone()
        })
    }

    fn start_ms(&self, pid: u32) -> Option<u64> {
        match pid {
            TUI => Some(self.started_ms),
            NEW => Some(self.relaunched_ms.load(Ordering::SeqCst)),
            _ => None,
        }
    }

    fn child_of(&self, pid: u32) -> Option<u32> {
        (pid == TUI).then_some(CHILD)
    }

    fn reads_terminal(&self, child: u32) -> bool {
        let look = self.orphan_looks.fetch_add(1, Ordering::SeqCst);
        child == CHILD && self.knobs.orphan_reads.is_some_and(|n| look < n)
    }

    fn shell_has_terminal(&self, _: u32) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    fn gemini_children(&self, _: u32) -> Vec<(u32, Vec<String>)> {
        if self.relaunched.load(Ordering::SeqCst) && !self.knobs.noappear && !self.knobs.new_reborn
        {
            vec![(NEW, self.new_argv())]
        } else {
            Vec::new()
        }
    }

    fn project_geminis(&self, root: &str) -> Option<Vec<(u32, Vec<String>, u64)>> {
        if self.knobs.table_unreadable {
            return None;
        }
        let mut out = Vec::new();
        if root == self.project {
            if self.knobs.argv_holder {
                out.push((
                    OTHER,
                    vec![
                        NODE.to_string(),
                        SCRIPT.to_string(),
                        "--resume".to_string(),
                        S1.to_string(),
                    ],
                    self.started_ms + 3_600_000,
                ));
            }
            if self.knobs.together {
                out.push((
                    OTHER,
                    vec![NODE.to_string(), SCRIPT.to_string()],
                    self.started_ms + 50,
                ));
            }
        }
        Some(out)
    }

    fn in_tab(&self, _: &mut Client, pid: u32, _: &str) -> bool {
        !(pid == NEW && self.knobs.out_of_tab) && GeminiKernel::alive(self, pid)
    }
}

impl Script {
    fn new_argv(&self) -> Vec<String> {
        self.knobs.new_argv.clone().unwrap_or_else(|| {
            vec![
                NODE.to_string(),
                SCRIPT.to_string(),
                "--model".to_string(),
                "gemini-3.5-flash".to_string(),
                "--resume".to_string(),
                S1.to_string(),
            ]
        })
    }
}

/// One scripted rig: a Gemini home with the project's chats directory and its
struct Rig {
    dir: ScratchDir,
    opts: Opts,
    kernel: Script,
    asked: crate::harness::FakeInstanceLog,
    home: PathBuf,
    chats: PathBuf,
    project: String,
    hash: String,
    blocks2: Arc<Mutex<String>>,
    after: Arc<Mutex<After>>,
    new_session: Arc<Mutex<Option<String>>>,
    header_after: Arc<AtomicU64>,
    /// The harness's waits on the test's thread run on their own steps
    _waits: crate::harness::relaunch::VirtualWaits,
}

impl Rig {
    /// `status`: the exit status the shell's block names; `word`, `later` as
    fn new(name: &str, status: i64, word: bool, later: bool, files: Files) -> Rig {
        let dir = scratch(name);
        let base = std::fs::canonicalize(&*dir).expect("canonical scratch");
        let project_dir = base.join("proj");
        std::fs::create_dir_all(&project_dir).expect("project");
        let project = project_dir.to_string_lossy().into_owned();
        let hash = ug::project_hash(&project);
        let home = base.join("home/.gemini");
        let slug = home.join("tmp/proj");
        let chats = slug.join("chats");
        std::fs::create_dir_all(&chats).expect("chats");
        std::fs::write(slug.join(".project_root"), &project).expect("marker");
        std::fs::write(
            home.join("projects.json"),
            format!(r#"{{"projects":{{"{project}":"proj"}}}}"#),
        )
        .expect("registry");
        let started_ms = now_ms() - 60_000;
        write_files(&chats, &hash, started_ms, files);
        let exited = Arc::new(AtomicBool::new(true));
        let relaunched = Arc::new(AtomicBool::new(false));
        let relaunched_ms = Arc::new(AtomicU64::new(0));
        let after = Arc::new(Mutex::new(After::default()));
        let new_session = Arc::new(Mutex::new(Some(S1.to_string())));
        let header_after = Arc::new(AtomicU64::new(500));
        let blocks2 = Arc::new(Mutex::new(blocks(status, word, later)));
        let w = World {
            blocks2: Arc::clone(&blocks2),
            after_exit: shell_after(),
            after_relaunch: gemini_idle(),
            status: "OK schema=1 hold=0 hand=- integration=on".to_string(),
            relaunched: Arc::clone(&relaunched),
            relaunched_ms: Arc::clone(&relaunched_ms),
            after: Arc::clone(&after),
            chats: chats.clone(),
            hash: hash.clone(),
            new_session: Arc::clone(&new_session),
            header_after: Arc::clone(&header_after),
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
            _waits: crate::harness::relaunch::virtual_waits(),
            kernel: Script {
                exited,
                relaunched,
                relaunched_ms,
                started_ms,
                project: project.clone(),
                knobs: Knobs::default(),
                orphan_looks: AtomicUsize::new(0),
            },
            asked,
            blocks2,
            after,
            home,
            chats,
            project,
            hash,
            new_session,
            header_after,
            opts,
            dir,
        }
    }

    /// The rig [`Rig::new`] makes for `status`, `word`, `later` and `files`, on
    fn reset(&mut self, status: i64, word: bool, later: bool, files: Files) {
        for dir in [&self.opts.state, &self.opts.home, &self.chats] {
            match std::fs::remove_dir_all(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => panic!("{}: {e}", dir.display()),
            }
        }
        std::fs::create_dir_all(&self.chats).expect("chats");
        let started_ms = now_ms() - 60_000;
        write_files(&self.chats, &self.hash, started_ms, files);
        *self.blocks2.lock().expect("blocks") = blocks(status, word, later);
        *self.after.lock().expect("after") = After::default();
        *self.new_session.lock().expect("new") = Some(S1.to_string());
        self.header_after.store(500, Ordering::SeqCst);
        self.asked.lock().expect("log").clear();
        let k = &mut self.kernel;
        k.exited.store(true, Ordering::SeqCst);
        k.relaunched.store(false, Ordering::SeqCst);
        k.relaunched_ms.store(0, Ordering::SeqCst);
        k.started_ms = started_ms;
        k.knobs = Knobs::default();
        k.orphan_looks.store(0, Ordering::SeqCst);
    }

    /// The snapshot the host took of the Gemini CLI while it ran: launched
    fn snap(&self, session: Option<&str>) -> Snapshot {
        Snapshot {
            stored_login: false,
            profile: None,
            tab: TAB.to_string(),
            pid: TUI,
            start: START.to_string(),
            shell: SHELL,
            program: PathBuf::from(NODE),
            argv: vec![
                NODE.to_string(),
                SCRIPT.to_string(),
                "--model".to_string(),
                "gemini-3.5-flash".to_string(),
                "--resume".to_string(),
                S1.to_string(),
                "-i".to_string(),
                "carry on".to_string(),
                "fix it".to_string(),
            ],
            session: session.map(str::to_string),
            cwd: self.project.clone(),
            version: Some("0.46.0".to_string()),
            run: AgentRun::Gemini(GeminiRun {
                home: self.home.clone(),
                chats: Some(self.chats.clone()),
                env: Vec::new(),
                started_ms: self.kernel.started_ms,
                child: CHILD,
                unwitnessed: None,
                block: Some(7),
            }),
            dialect: Some(Dialect::Zsh),
            login: None,
            home: None,
            claude_dir: None,
        }
    }

    fn attempt(&self, snap: &Snapshot, left: &ExitRecord, cause: ExitCause) -> Report {
        after_exit_onto(&self.opts, snap, left, cause, &self.kernel)
    }

    /// [`exit_record`] over the real look at this rig's exit. Nothing a look
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

    /// The relaunched Gemini CLI's screen and the tab's `status` from now on.
    fn relaunched_shows(&self, screen: Option<String>, status: Option<&str>) {
        let mut after = self.after.lock().expect("after");
        after.screen = screen;
        after.status = status.map(str::to_string);
    }

    /// The relaunch lines typed (the turns naming `--resume`).
    fn lines(&self) -> usize {
        self.typed()
            .iter()
            .filter(|l| l.contains("'--resume'"))
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

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&*self.dir);
    }
}

/// The project's session files as `files` says, around a parent started at
fn write_files(chats: &Path, hash: &str, start: u64, files: Files) {
    let write = |name: &str, text: String| {
        std::fs::write(chats.join(name), text).expect("session file");
    };
    let mut older = vec![
        header(S1, hash, start - 3_600_000),
        context(start - 3_600_000, "ctx1"),
    ];
    if !files.no_turn {
        older.push(turn(start - 3_000_000, "u1", "g1"));
    }
    if !files.bare {
        if !files.dup {
            older.push(format!(
                r#"{{"$set":{{"lastUpdated":"{}"}}}}"#,
                iso(start + 930)
            ));
        }
        older.push(format!(r#"{{"$set":{{"sessionId":"{S1}"}}}}"#));
    }
    write(&turn_file(), older.join("\n") + "\n");
    if !files.no_header {
        write(
            &format!("session-2026-10-18T06-40-{}.jsonl", &S1[..8]),
            format!(
                "{}\n{}\n",
                header(S1, hash, start + 900),
                context(start + 900, "ctx2")
            ),
        );
    }
    if files.second {
        write(
            &format!("session-2026-10-18T06-41-{}.jsonl", &S2[..8]),
            format!(
                "{}\n{}\n",
                header(S2, hash, start + 30_000),
                context(start + 30_000, "ctx3")
            ),
        );
    }
    if files.late {
        write(
            &format!("session-2026-10-18T06-45-{}.jsonl", &S2[..8]),
            format!(
                "{}\n{}\n{}\n",
                header(S2, hash, start + 30_000),
                context(start + 30_000, "ctx5"),
                turn(start + 40_000, "u5", "g5")
            ),
        );
    }
    if files.switched || files.resumed || files.summary {
        let mut parts = vec![
            header(S2, hash, start - 7_200_000),
            context(start - 7_200_000, "ctx4"),
            turn(start - 7_000_000, "u4", "g4"),
        ];
        if files.switched || files.resumed {
            parts.push(format!(r#"{{"$set":{{"sessionId":"{S2}"}}}}"#));
        }
        if files.summary {
            parts.push(
                r#"{"$set":{"summary":"Lab stub session","memoryScratchpad":{"version":1}}}"#
                    .to_string(),
            );
        }
        write(
            &format!("session-2026-10-18T04-00-{}.jsonl", &S2[..8]),
            parts.join("\n") + "\n",
        );
    }
    if files.resumed {
        write(
            &format!("session-2026-10-18T05-00-{}.jsonl", &S2[..8]),
            format!(
                "{}\n{}\n",
                header(S2, hash, start - 3_600_000),
                context(start - 3_600_000, "ctx6")
            ),
        );
    }
}

/// A CRASH IS RESUMED ON ITS SESSION, IN ITS PROJECT, THROUGH THE SHELL'S OWN
#[test]
fn a_crashed_gemini_cli_is_resumed_on_its_session_in_its_project() {
    for status in [1, 137] {
        let rig = Rig::new("crash", status, true, false, Files::default());
        let snap = rig.snap(Some(S1));
        let left = rig.look(&snap);
        assert_eq!(left, ExitRecord::Crashed, "{status}");
        let r = rig.attempt(&snap, &left, ExitCause::Exit);
        assert_eq!(r.step, "continued", "{status}: {r:?}");
        assert_eq!(r.session, format!("gemini-{TAB}"));
        let typed = rig.typed();
        assert_eq!(typed.len(), 2, "{typed:#?}");
        let line = &typed[0];
        assert!(
            line.contains(&format!("cd -- '{}' && exec 'gemini' ", rig.project)),
            "{line}"
        );
        assert!(
            line.contains(&format!("'--model' 'gemini-3.5-flash' '--resume' '{S1}'")),
            "{line}"
        );
        assert!(
            !line.contains(NODE) && !line.contains("Cellar") && !line.contains(SCRIPT),
            "the shell's own gemini: {line}"
        );
        assert!(
            !line.contains("'-i'") && !line.contains("carry on") && !line.contains("fix it"),
            "never a prompt again: {line}"
        );
        assert_eq!(line.matches("'--resume'").count(), 1, "{line}");
        assert!(
            typed[1].contains("[aterm harness] Relaunched: Gemini CLI exited"),
            "{}",
            typed[1]
        );
        let st = rig.state();
        assert_eq!(
            (
                st.agent,
                st.thread.as_str(),
                st.cwd.as_str(),
                st.twin.as_str()
            ),
            (Agent::Gemini, S1, rig.project.as_str(), "gemini")
        );
        assert_eq!(st.gemini_child, CHILD);
        assert_eq!(st.outcome, "gemini on 0.46.0 · the same session resumed");
    }
}

/// The exit's word is the shell's: the finished block that ends where the
#[test]
fn the_gemini_exit_is_read_off_its_command_block() {
    assert_eq!(
        parse_exit_status("OK", &blocks(137, true, false), Some(7)),
        Some(137)
    );
    assert_eq!(
        parse_exit_status("OK", &blocks(0, true, true), Some(7)),
        None
    );
}

/// EVERY GRACEFUL WAY OUT (`0`; a cancel's `130`) IS LEFT, A REFUSED START
#[test]
fn a_graceful_exit_a_refused_start_and_an_unwitnessed_end_are_never_relaunched() {
    let mut status_alone_differs = 0;
    for (status, want) in [
        (0, "ended:graceful-exit"),
        (130, "ended:graceful-exit"),
        (41, "ended:refused-start"),
        (42, "ended:refused-start"),
        (52, "ended:refused-start"),
        (143, "ended:exit-unwitnessed"),
        (1, "continued"),
        (137, "continued"),
    ] {
        let rig = Rig::new("left", status, true, false, Files::default());
        let snap = rig.snap(Some(S1));
        let left = rig.look(&snap);
        let r = rig.attempt(&snap, &left, ExitCause::Exit);
        assert_eq!(r.step, want, "{status}: {left:?} {r:?}");
        if want.starts_with("ended:") {
            assert!(rig.typed().is_empty(), "{status}: {:#?}", rig.typed());
        }
        let table_crash = ug::exit_kind(status) == ug::ExitKind::Crashed;
        assert_eq!(table_crash, want == "continued", "{status}");
        if status != 0 && status != 130 && !table_crash {
            status_alone_differs += 1;
        }
    }
    assert_eq!(
        status_alone_differs, 4,
        "the status alone would relaunch the three refusals and the unwitnessed end"
    );
    let rig = Rig::new("refused-launch", 42, true, false, Files::default());
    let snap = rig.snap(None);
    assert_eq!(
        crate::harness::relaunch::launch_ended(&snap, &rig.look(&snap)),
        Some("refused-start")
    );
}

/// A worker's launch as its spawn types it (`harness::worker`): its model,
fn worker_snap(rig: &Rig, id: &str, trust: &Path) -> Snapshot {
    let mut snap = rig.snap(Some(S1));
    snap.argv = vec![
        NODE.to_string(),
        SCRIPT.to_string(),
        "--model".to_string(),
        "gemini-3.5-flash".to_string(),
        "--session-id".to_string(),
        id.to_string(),
        "--prompt-interactive=fix it\n(Started by `s-1`)".to_string(),
    ];
    if let AgentRun::Gemini(run) = &mut snap.run {
        run.env = vec![(
            ug::TRUSTED_FOLDERS_ENV.to_string(),
            trust.to_string_lossy().into_owned(),
        )];
    }
    snap
}

/// The project's one session file as a worker's first start writes it:
fn worker_files(rig: &Rig, begun_ms: i64) {
    std::fs::remove_dir_all(&rig.chats).expect("chats");
    std::fs::create_dir_all(&rig.chats).expect("chats");
    let start = rig.kernel.started_ms;
    let begun = start.saturating_add_signed(begun_ms);
    std::fs::write(
        rig.chats
            .join(format!("session-2026-10-18T06-40-{}.jsonl", &S1[..8])),
        format!(
            "{}\n{}\n{}\n",
            header(S1, &rig.hash, begun),
            context(begun, "ctx-w"),
            turn(begun + 4_000, "uw", "gw")
        ),
    )
    .expect("session file");
}

/// A WORKER'S OWN RESTART IS FINISHED ON ITS SESSION, NOTHING TYPED: Gemini
#[test]
fn a_workers_own_restart_is_finished_on_its_session_with_nothing_typed() {
    let rig = Rig::new("restart", 42, true, false, Files::default());
    worker_files(&rig, 900);
    let trust = rig.dir.join("s-1-1.trust.json");
    std::fs::write(&trust, "{}").expect("trust file");
    let snap = worker_snap(&rig, S1, &trust);
    let left = rig.look(&snap);
    assert_eq!(left, ExitRecord::Restarted);
    assert!(left.crashed(), "owed a relaunch as a crash is");
    assert!(look_at_exit(&rig.opts, &snap, &rig.kernel).restarted_itself);
    assert_eq!(crate::harness::relaunch::launch_ended(&snap, &left), None);
    let r = rig.attempt(&snap, &left, ExitCause::Exit);
    assert_eq!(r.step, "done:resumed", "{r:?}\n{:#?}", rig.typed());
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "the line alone: {typed:#?}");
    let line = &typed[0];
    assert!(
        line.contains(&format!("'--model' 'gemini-3.5-flash' '--resume' '{S1}'")),
        "{line}"
    );
    assert!(
        !line.contains("session-id") && !line.contains("prompt-interactive"),
        "{line}"
    );
    assert!(
        line.contains(ug::TRUSTED_FOLDERS_ENV) && line.contains(&*trust.to_string_lossy()),
        "{line}"
    );
    let st = rig.state();
    assert!(st.quiet);
    assert_eq!(st.thread, S1);

    let rig = Rig::new("restart-untrusted", 42, true, false, Files::default());
    worker_files(&rig, 900);
    let snap = worker_snap(&rig, S1, &rig.dir.join("gone.trust.json"));
    let r = rig.attempt(&snap, &rig.look(&snap), ExitCause::Exit);
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert!(
        !rig.typed()[0].contains(ug::TRUSTED_FOLDERS_ENV),
        "{:#?}",
        rig.typed()
    );

    for (name, begun, id, resume) in [
        ("restart-written-before", -3_600_000, S1, false),
        ("restart-begun-under-another-id", 900, S2, false),
        ("restart-names-a-resume", 900, S1, true),
    ] {
        let rig = Rig::new(name, 42, true, false, Files::default());
        worker_files(&rig, begun);
        let mut snap = worker_snap(&rig, id, &trust);
        if resume {
            snap.argv
                .splice(2..2, ["--resume".to_string(), S1.to_string()]);
        }
        let left = rig.look(&snap);
        assert_eq!(left, ExitRecord::Refused, "{name}");
        assert!(
            !look_at_exit(&rig.opts, &snap, &rig.kernel).restarted_itself,
            "{name}"
        );
        let r = rig.attempt(&snap, &left, ExitCause::Exit);
        assert_eq!(r.step, "ended:refused-start", "{name}: {r:?}");
        assert!(rig.typed().is_empty(), "{name}: {:#?}", rig.typed());
    }
    let rig = Rig::new("restart-resume-launch", 42, true, false, Files::default());
    let snap = rig.snap(Some(S1));
    let left = rig.look(&snap);
    assert_eq!(left, ExitRecord::Refused);
    assert_eq!(
        rig.attempt(&snap, &left, ExitCause::Exit).step,
        "ended:refused-start"
    );
}

/// The project's one session file as a start that took no turn writes it:
fn turnless_files(rig: &Rig) {
    std::fs::remove_dir_all(&rig.chats).expect("chats");
    std::fs::create_dir_all(&rig.chats).expect("chats");
    let begun = rig.kernel.started_ms + 900;
    std::fs::write(
        rig.chats
            .join(format!("session-2026-10-18T06-40-{}.jsonl", &S1[..8])),
        format!(
            "{}\n{}\n",
            header(S1, &rig.hash, begun),
            context(begun, "ctx-t")
        ),
    )
    .expect("session file");
}

/// A BARE LAUNCH'S OWN RESTART BEFORE ITS FIRST TURN IS RESUMED ON ITS
#[test]
fn a_bare_restart_before_its_first_turn_is_resumed_on_its_session() {
    let bare = |rig: &Rig| {
        let mut snap = rig.snap(Some(S1));
        snap.argv = vec![
            NODE.to_string(),
            SCRIPT.to_string(),
            "--model".to_string(),
            "gemini-3.5-flash".to_string(),
            "--session-id".to_string(),
            S1.to_string(),
        ];
        snap
    };
    let rig = Rig::new("restart-turnless", 42, true, false, Files::default());
    turnless_files(&rig);
    let snap = bare(&rig);
    let left = rig.look(&snap);
    assert_eq!(left, ExitRecord::Restarted);
    assert!(look_at_exit(&rig.opts, &snap, &rig.kernel).restarted_itself);
    let r = rig.attempt(&snap, &left, ExitCause::Exit);
    assert_eq!(r.step, "done:resumed", "{r:?}\n{:#?}", rig.typed());
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "the line alone: {typed:#?}");
    assert!(
        typed[0].contains(&format!("'--model' 'gemini-3.5-flash' '--resume' '{S1}'"))
            && !typed[0].contains("session-id"),
        "{}",
        typed[0]
    );
    assert!(rig.state().quiet);

    let rig = Rig::new(
        "restart-turnless-prompted",
        42,
        true,
        false,
        Files::default(),
    );
    turnless_files(&rig);
    let mut snap = bare(&rig);
    snap.argv.push("--prompt-interactive=fix it".to_string());
    let left = rig.look(&snap);
    assert_eq!(left, ExitRecord::Restarted, "still its own restart");
    let r = rig.attempt(&snap, &left, ExitCause::Exit);
    assert_eq!(r.step, "refused:no-turn", "{r:?}");
    assert!(rig.typed().is_empty(), "{:#?}", rig.typed());

    let rig = Rig::new(
        "crash-turnless",
        1,
        true,
        false,
        Files {
            no_turn: true,
            ..Files::default()
        },
    );
    let snap = rig.snap(Some(S1));
    let left = rig.look(&snap);
    assert_eq!(left, ExitRecord::Crashed);
    let r = rig.attempt(&snap, &left, ExitCause::Exit);
    assert_eq!(r.step, "refused:no-turn", "{r:?}");
    assert!(rig.typed().is_empty(), "{:#?}", rig.typed());

    let rig = Rig::new("crash-turnless-bare", 1, true, false, Files::default());
    turnless_files(&rig);
    let snap = bare(&rig);
    let left = rig.look(&snap);
    assert_eq!(left, ExitRecord::Crashed);
    *rig.blocks2.lock().expect("blocks") = blocks(1, true, true);
    let r = rig.attempt(&snap, &left, ExitCause::Exit);
    assert_eq!(r.step, "refused:no-turn", "{r:?}");
    assert!(rig.typed().is_empty(), "{:#?}", rig.typed());
}

/// WHAT THE LOOK READ AT A WORKER'S EXIT STANDS WHEN ITS WORD IS GONE BY THE
#[test]
fn what_the_look_read_at_an_exit_stands_when_its_word_is_gone() {
    for (name, status, worker, word_stays, record, want) in [
        ("restart-word-gone", 42, true, false, None, "done:resumed"),
        ("restart-word-stays", 42, true, true, None, "done:resumed"),
        ("worker-crash-word-gone", 1, true, false, None, "continued"),
        ("worker-crash-word-stays", 1, true, true, None, "continued"),
        ("resume-crash-word-gone", 1, false, false, None, "continued"),
        (
            "crash-record-restart-word-stays",
            42,
            true,
            true,
            Some(ExitRecord::Crashed),
            "done:resumed",
        ),
        (
            "crash-record-restart-word-gone",
            42,
            true,
            false,
            Some(ExitRecord::Crashed),
            "continued",
        ),
    ] {
        let rig = Rig::new(name, status, true, false, Files::default());
        let snap = if worker {
            worker_files(&rig, 900);
            worker_snap(&rig, S1, &rig.dir.join("gone.trust.json"))
        } else {
            rig.snap(Some(S1))
        };
        let looked = rig.look(&snap);
        let read = if status == 42 {
            ExitRecord::Restarted
        } else {
            ExitRecord::Crashed
        };
        assert_eq!(looked, read, "{name}: the look");
        if !word_stays {
            *rig.blocks2.lock().expect("blocks") = blocks(status, true, true);
        }
        let left = record.unwrap_or(looked);
        let r = rig.attempt(&snap, &left, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}\n{:#?}", rig.typed());
        assert_eq!(rig.lines(), 1, "{name}: {:#?}", rig.typed());
        assert_eq!(
            rig.continuations(),
            usize::from(want == "continued"),
            "{name}: {:#?}",
            rig.typed()
        );
    }
}

/// A SESSION THE START-TIME RULE CANNOT NAME IS TOLD, NEVER TYPED
#[test]
fn a_session_the_rule_cannot_name_is_told_never_typed() {
    for (name, files, knobs, snapped, want) in [
        (
            "cleared",
            Files {
                second: true,
                ..Files::default()
            },
            Knobs::default(),
            Some(S1),
            "refused:no-witness",
        ),
        (
            "no-header",
            Files {
                no_header: true,
                ..Files::default()
            },
            Knobs::default(),
            None,
            "refused:no-witness",
        ),
        (
            "switched",
            Files {
                switched: true,
                ..Files::default()
            },
            Knobs::default(),
            Some(S1),
            "refused:no-witness",
        ),
        (
            "together",
            Files::default(),
            Knobs {
                together: true,
                ..Knobs::default()
            },
            Some(S1),
            "refused:no-witness",
        ),
        (
            "moved-since",
            Files::default(),
            Knobs::default(),
            Some(S2),
            "refused:no-witness",
        ),
        (
            "switched-then-summary",
            Files {
                switched: true,
                summary: true,
                ..Files::default()
            },
            Knobs::default(),
            Some(S1),
            "refused:no-witness",
        ),
        (
            "summary",
            Files {
                summary: true,
                ..Files::default()
            },
            Knobs::default(),
            Some(S1),
            "continued",
        ),
        (
            "resumed-then-summary",
            Files {
                resumed: true,
                summary: true,
                ..Files::default()
            },
            Knobs::default(),
            Some(S1),
            "continued",
        ),
        (
            "plain",
            Files::default(),
            Knobs::default(),
            None,
            "continued",
        ),
    ] {
        let mut rig = Rig::new(name, 1, true, false, files);
        rig.kernel.knobs = knobs;
        let r = rig.attempt(&rig.snap(snapped), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}");
        if want != "continued" {
            assert!(rig.typed().is_empty(), "{name}: {:#?}", rig.typed());
        }
    }
    assert!(matches!(
        crate::harness::relaunch::outcome("refused:no-witness"),
        crate::harness::relaunch::Outcome::Cannot(_)
    ));
}

/// A BARE `gemini` THAT DIED BEFORE ITS OWN HEADER, ANOTHER LAUNCH OF THE
#[test]
fn another_launchs_late_header_is_never_resumed_as_its_own() {
    for (name, files, want) in [
        (
            "late",
            Files {
                no_header: true,
                bare: true,
                late: true,
                ..Files::default()
            },
            "refused:no-witness",
        ),
        ("own", Files::default(), "continued"),
    ] {
        let rig = Rig::new(name, 1, true, false, files);
        let mut snap = rig.snap(None);
        snap.argv = vec![NODE.to_string(), SCRIPT.to_string()];
        let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}");
        if want != "continued" {
            assert_eq!(rig.lines(), 0, "{name}: {:#?}", rig.typed());
        }
    }
}

/// THE PRE-CHECK: A RESUME THAT WOULD LOAD A COPY MISSING TURNS IS TOLD,
#[test]
fn a_resume_the_precheck_refuses_is_told_never_typed() {
    for (name, files, want) in [
        (
            "duplicate",
            Files {
                dup: true,
                ..Files::default()
            },
            "refused:history-unsafe",
        ),
        (
            "no-turn",
            Files {
                no_turn: true,
                ..Files::default()
            },
            "refused:no-turn",
        ),
        ("safe", Files::default(), "continued"),
    ] {
        let rig = Rig::new(name, 1, true, false, files);
        let snap = rig.snap(Some(S1));
        let told = told(&snap);
        assert_eq!(
            told.is_some(),
            want == "continued",
            "{name}: only a safe resume is told as a line: {told:?}"
        );
        let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}");
        if want != "continued" {
            assert_eq!(rig.lines(), 0, "{name}: {:#?}", rig.typed());
            let kept = load(&rig.opts, &key(TAB));
            assert!(kept.is_none(), "{name}: {kept:#?}");
        }
    }
    for step in ["refused:history-unsafe", "refused:no-turn"] {
        assert!(matches!(
            crate::harness::relaunch::outcome(step),
            crate::harness::relaunch::Outcome::Cannot(_)
        ));
    }
}

/// THE PRE-CHECK IS READ AGAIN RIGHT BEFORE THE LINE: an attempt that waited
#[test]
fn the_precheck_is_read_again_right_before_the_line() {
    for (name, duplicate, want) in [
        ("duplicated-meanwhile", true, "refused:history-unsafe"),
        ("unchanged", false, "continued"),
    ] {
        let mut rig = Rig::new(name, 1, true, false, Files::default());
        rig.kernel.knobs.argv_holder = true;
        let snap = rig.snap(Some(S1));
        let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, "wait:session-held", "{name}: {r:?}");
        rig.kernel.knobs.argv_holder = false;
        if duplicate {
            std::fs::write(
                rig.chats
                    .join(format!("session-2026-10-18T07-30-{}.jsonl", &S1[..8])),
                format!(
                    "{}\n{}\n",
                    header(S1, &rig.hash, now_ms() + 5_000),
                    context(now_ms() + 5_000, "ctx9")
                ),
            )
            .expect("a newer duplicate");
        }
        let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(rig.lines(), usize::from(!duplicate), "{name}");
    }
}

/// THE ORPHAN: a parent SIGKILLed leaves its child alive; nothing is typed
#[test]
fn nothing_is_typed_while_the_orphan_reads_the_terminal() {
    let mut rig = Rig::new("orphan-reads", 137, true, false, Files::default());
    rig.kernel.knobs.orphan_reads = Some(usize::MAX);
    let snap = rig.snap(Some(S1));
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "wait:orphan-reads", "{r:?}");
    assert_eq!(rig.lines(), 0, "{:#?}", rig.typed());
    let mut rig = Rig::new("orphan-quiet", 137, true, false, Files::default());
    rig.kernel.knobs.orphan_reads = Some(2);
    let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "continued", "{r:?}");
    assert_eq!(rig.lines(), 1);
    let control = Rig::new("no-orphan", 1, true, false, Files::default());
    let r = control.attempt(
        &control.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "continued", "{r:?}");
}

/// A SESSION ANOTHER LIVE GEMINI CLI RUNS is never resumed a second time:
#[test]
fn a_held_session_is_never_typed_at() {
    for (name, knobs, want) in [
        (
            "argv-held",
            Knobs {
                argv_holder: true,
                ..Knobs::default()
            },
            "wait:session-held",
        ),
        (
            "table-unreadable",
            Knobs {
                table_unreadable: true,
                ..Knobs::default()
            },
            "refused:no-witness",
        ),
    ] {
        let mut rig = Rig::new(name, 1, true, false, Files::default());
        rig.kernel.knobs = knobs;
        let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(rig.lines(), 0, "{name}: {:#?}", rig.typed());
    }
}

/// A launch whose flags a relaunch cannot carry (`--worktree`, a flag the
#[test]
fn an_uncarryable_launch_is_refused() {
    for extra in ["--worktree", "--approvalMode"] {
        let rig = Rig::new("uncarryable", 1, true, false, Files::default());
        let mut snap = rig.snap(Some(S1));
        snap.argv.push(extra.to_string());
        snap.argv.push("x".to_string());
        let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert!(r.step.starts_with("failed:argv:"), "{extra}: {r:?}");
        assert_eq!(rig.lines(), 0, "{:#?}", rig.typed());
    }
}

/// THE WITNESS OF A LIVE SNAPSHOT: the start-time rule over the project's
#[test]
fn the_snapshot_names_the_session_by_the_start_time_rule() {
    let rig = Rig::new("snapshot", 0, true, false, Files::default());
    rig.kernel.exited.store(false, Ordering::SeqCst);
    let home = rig.home.parent().expect("home");
    let args = |argv: Vec<String>, env: Vec<String>| atpkg::caller_shell::ProcArgs {
        exec_path: NODE.to_string(),
        argv,
        env,
    };
    let plain = args(
        rig.snap(None).argv,
        vec![format!("HOME={}", home.display())],
    );
    let snap = snapshot(
        &rig.opts,
        TAB,
        TUI,
        START.into(),
        SHELL,
        &plain,
        &rig.kernel,
    )
    .expect("a snapshot");
    assert_eq!(snap.session.as_deref(), Some(S1));
    assert_eq!(snap.cwd, rig.project);
    let run = snap.gemini().expect("a Gemini CLI run");
    assert_eq!(run.chats.as_deref(), Some(rig.chats.as_path()));
    assert_eq!(run.child, CHILD);
    assert_eq!(run.unwitnessed, None);
    assert_eq!(
        run.env,
        vec![("HOME".to_string(), home.to_string_lossy().into_owned())],
        "a HOME other than the user's is carried"
    );
    let moved = args(
        rig.snap(None).argv,
        vec![format!("GEMINI_CLI_HOME={}", rig.dir.join("alt").display())],
    );
    let alt = snapshot(
        &rig.opts,
        TAB,
        TUI,
        START.into(),
        SHELL,
        &moved,
        &rig.kernel,
    )
    .expect("a snapshot");
    assert_eq!(alt.session, None);
    assert_eq!(
        alt.gemini().and_then(|r| r.unwitnessed),
        Some(ug::NoWitness::NoChats)
    );
    let cleared = Rig::new(
        "snapshot-cleared",
        0,
        true,
        false,
        Files {
            second: true,
            ..Files::default()
        },
    );
    cleared.kernel.exited.store(false, Ordering::SeqCst);
    let home = cleared.home.parent().expect("home");
    let snap = snapshot(
        &cleared.opts,
        TAB,
        TUI,
        START.into(),
        SHELL,
        &args(
            cleared.snap(None).argv,
            vec![format!("HOME={}", home.display())],
        ),
        &cleared.kernel,
    )
    .expect("a snapshot");
    assert_eq!(snap.session, None);
    assert_eq!(
        snap.gemini().and_then(|r| r.unwitnessed),
        Some(ug::NoWitness::TwoHeaders)
    );
    let one_shot = args(
        vec![
            NODE.to_string(),
            SCRIPT.to_string(),
            "-p".into(),
            "hi".into(),
        ],
        Vec::new(),
    );
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
    let other = args(
        vec![NODE.to_string(), "/w/server.js".to_string()],
        Vec::new(),
    );
    assert_eq!(
        snapshot(
            &rig.opts,
            TAB,
            TUI,
            START.into(),
            SHELL,
            &other,
            &rig.kernel
        )
        .err(),
        Some("not-gemini")
    );
}

/// THE PROJECT DIRECTORY: the slug `projects.json` maps the project to where
#[test]
fn the_project_directory_is_the_slug_whose_marker_names_it() {
    let rig = Rig::new("slug", 0, true, false, Files::default());
    assert_eq!(
        project_dir(&rig.home, &rig.project),
        Some(rig.home.join("tmp/proj"))
    );
    std::fs::remove_file(rig.home.join("projects.json")).expect("no registry");
    assert_eq!(
        project_dir(&rig.home, &rig.project),
        Some(rig.home.join("tmp/proj")),
        "found by its marker alone"
    );
    let twin = rig.home.join("tmp/proj-1");
    std::fs::create_dir_all(&twin).expect("twin");
    std::fs::write(twin.join(".project_root"), &rig.project).expect("marker");
    assert_eq!(project_dir(&rig.home, &rig.project), None, "two claim it");
    std::fs::write(twin.join(".project_root"), "/elsewhere").expect("marker");
    std::fs::write(
        rig.home.join("projects.json"),
        format!(r#"{{"projects":{{"{}":"proj-1"}}}}"#, rig.project),
    )
    .expect("registry");
    assert_eq!(
        project_dir(&rig.home, &rig.project),
        Some(rig.home.join("tmp/proj")),
        "a mapping whose marker names another project is passed by"
    );
    assert_eq!(project_dir(&rig.home, ""), None);
}

/// A RESTORED LAYOUT'S RELAUNCH (aterm's own end): no shell word is asked,
#[test]
fn aterm_ending_relaunches_the_restored_session() {
    let rig = Rig::new("host", 0, true, false, Files::default());
    let mut snap = rig.snap(Some(S1));
    if let AgentRun::Gemini(run) = &mut snap.run {
        run.child = 0;
        run.block = None;
    }
    let r = rig.attempt(&snap, &ExitRecord::Unread, ExitCause::HostEnded);
    assert_eq!(r.step, "continued", "{r:?}");
    assert!(
        rig.typed()[1].contains("aterm ended while this session ran"),
        "{:#?}",
        rig.typed()
    );
    let own = Rig::new("host-control", 0, true, false, Files::default());
    let r = own.attempt(&own.snap(Some(S1)), &ExitRecord::Unread, ExitCause::Exit);
    assert_eq!(r.step, "ended:graceful-exit");
}

/// THE EXPLICIT RESTART IS NEVER CARRIED FOR A GEMINI CLI: the act signals
#[test]
fn the_explicit_restart_is_never_carried_for_a_gemini_cli() {
    use crate::harness::relaunch::carries_restart;
    let rig = Rig::new("restart", 0, true, false, Files::default());
    assert!(!carries_restart(&rig.opts.home, &rig.snap(Some(S1))));
}

/// A TAB'S OWN GEMINI CLI (`aterm -e gemini`, `exec gemini`: shell
#[test]
fn a_tabs_own_gemini_cli_is_relaunched_by_nothing() {
    use crate::harness::relaunch::NO_SHELL;
    let rig = Rig::new("tab-program", 1, true, false, Files::default());
    rig.kernel.exited.store(false, Ordering::SeqCst);
    let home = rig.home.parent().expect("home");
    let args = atpkg::caller_shell::ProcArgs {
        exec_path: NODE.to_string(),
        argv: rig.snap(None).argv,
        env: vec![format!("HOME={}", home.display())],
    };
    let read = |shell: u32| {
        snapshot(&rig.opts, TAB, TUI, START.into(), shell, &args, &rig.kernel).expect("a snapshot")
    };
    let own = read(NO_SHELL);
    assert_eq!(own.session.as_deref(), Some(S1), "its readers keep it");
    assert_eq!(
        (own.dialect, own.gemini().and_then(|run| run.block)),
        (None, None)
    );
    let at_shell = read(SHELL);
    assert_eq!(at_shell.dialect, Some(Dialect::Zsh), "control: its shell's");
    rig.kernel.exited.store(true, Ordering::SeqCst);
    let snap = Snapshot {
        shell: NO_SHELL,
        dialect: None,
        ..rig.snap(Some(S1))
    };
    let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "refused:not-a-shell-job", "{r:?}");
    assert!(rig.typed().is_empty());
    let st = St {
        agent: Agent::Gemini,
        shell: NO_SHELL,
        thread: S1.to_string(),
        cwd: rig.project.clone(),
        argv: rig.snap(Some(S1)).argv,
        ..St::default()
    };
    assert!(matches!(
        plan(&rig.opts, &st, &rig.kernel),
        Err(NoPlan::Refused { why, .. }) if why == "not-a-shell-job"
    ));
    let control = Rig::new("tab-program-control", 1, true, false, Files::default());
    let r = control.attempt(
        &control.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!(r.step, "continued", "control: at its shell: {r:?}");
}

/// The records are the lane's: keyed `gemini-<tab>`, read back by their tab,
#[test]
fn records_are_keyed_by_tab_and_pruned_once_gone() {
    assert_eq!(key(TAB), format!("gemini-{TAB}"));
    assert_eq!(Agent::Gemini.record_tab(&key(TAB)), Some(TAB));
    assert_eq!(Agent::keyed(&key(TAB)), Some(Agent::Gemini));
    assert!(!upgrade::is_session_id(&key(TAB)));
    let rig = Rig::new("prune", 0, true, false, Files::default());
    let homed = St {
        agent: Agent::Gemini,
        tab: TAB.to_string(),
        gemini_chats: "/h/.gemini/tmp/proj/chats".to_string(),
        gemini_env: vec!["GEMINI_CLI_HOME=/g".to_string()],
        gemini_child: CHILD,
        phase: Phase::Done,
        ..St::default()
    };
    let text = homed.to_json();
    assert!(
        text.contains("\"gemini_chats\":")
            && text.contains("\"gemini_env\":")
            && text.contains("\"gemini_child\":"),
        "{text}"
    );
    assert_eq!(
        St::from_json(&text).map(|st| (st.gemini_chats, st.gemini_env, st.gemini_child)),
        Some((
            homed.gemini_chats.clone(),
            homed.gemini_env.clone(),
            homed.gemini_child
        ))
    );
    let agy = St {
        agent: Agent::Agy,
        ..St::default()
    }
    .to_json();
    assert!(!agy.contains("gemini_"), "{agy}");
    let old = |tab: &str, phase: Phase| {
        let st = St {
            agent: Agent::Gemini,
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
fn relaunch_rig(name: &str, hand_back: bool, knobs: Knobs) -> Rig {
    let mut rig = Rig::new(name, 1, true, false, Files::default());
    rig.opts.hand_back = hand_back;
    rig.opts.timings.gemini_found = Duration::from_millis(400);
    rig.opts.timings.gemini_adopt = Duration::from_millis(300);
    rig.opts.timings.carry_settle = Duration::from_millis(400);
    rig.opts.timings.gemini_up = Duration::from_millis(400);
    rig.opts.timings.gemini_witness = Duration::from_millis(600);
    if knobs.new_dies || knobs.noappear {
        rig.after.lock().expect("after").screen = Some(shell_after());
    }
    rig.kernel.knobs = knobs;
    rig
}

/// A RESUME GEMINI CLI REFUSED IS NEVER TYPED AGAIN (critic X4): the
#[test]
fn a_resume_gemini_refused_is_never_typed_again() {
    for (exit, want, lines) in [
        (42, "refused:resume-refused", 1),
        (127, "refused:resume-refused", 1),
        (1, "refused:resume-refused", 1),
        (0, "ended:relaunch-ended", 1),
        (143, "refused:relaunch-unwitnessed", 1),
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
}

/// A RELAUNCH THAT NEVER COMES UP is decided by its line's own end AT ONCE,
#[test]
fn a_relaunch_that_never_comes_up_is_never_typed_again() {
    let knobs = Knobs {
        noappear: true,
        ..Knobs::default()
    };
    for exit in [42, 126] {
        let rig = relaunch_rig("never", true, knobs.clone());
        let snap = rig.snap(Some(S1));
        rig.line_ran(Some(exit));
        let r = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, "refused:resume-refused", "{exit}: {r:?}");
        let again = rig.attempt(&snap, &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(again.step, "refused:resume-refused", "{exit}: {again:?}");
        assert_eq!(rig.lines(), 1, "{exit}: {:#?}", rig.typed());
    }
}

/// THE RELAUNCHED PROCESS THE RULE NAMES ANOTHER SESSION FOR is never
#[test]
fn a_relaunch_on_another_session_is_told_and_typed_nothing() {
    for hand_back in [true, false] {
        let rig = relaunch_rig("fresh", hand_back, Knobs::default());
        *rig.new_session.lock().expect("new") = Some(S2.to_string());
        let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(
            r.step, "refused:started-other",
            "hand_back={hand_back}: {r:?}"
        );
        assert_eq!(rig.continuations(), 0, "{:#?}", rig.typed());
        assert_eq!(rig.state().resumed_pid, 0, "never adopted");
    }
    let control = relaunch_rig("adopted", true, Knobs::default());
    let r = control.attempt(
        &control.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!((r.step.as_str(), r.pid), ("adopted", NEW), "{r:?}");
    let typed = relaunch_rig("adopted-typed", false, Knobs::default());
    let r = typed.attempt(&typed.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "continued", "{r:?}");
}

/// A RELAUNCHED PROCESS NO FILE NAMES THE SESSION FOR is adopted on its
#[test]
fn no_continuation_into_a_relaunch_no_file_names() {
    for (name, new_session, after, want) in [
        ("unwitnessed", None, 500, "done:no-continue"),
        ("witnessed", Some(S1), 500, "continued"),
        ("witnessed-late", Some(S1), 5_000, "continued"),
    ] {
        let rig = relaunch_rig(name, false, Knobs::default());
        *rig.new_session.lock().expect("new") = new_session.map(str::to_string);
        rig.header_after.store(after, Ordering::SeqCst);
        let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(rig.lines(), 1, "{name}: the line typed once");
        assert_eq!(
            rig.continuations(),
            usize::from(want == "continued"),
            "{name}: {:#?}",
            rig.typed()
        );
        assert_eq!(rig.state().resumed_pid, NEW, "{name}: adopted");
        let outcome = rig.state().outcome;
        let unwitnessed = want == "done:no-continue";
        assert_eq!(
            (
                outcome.contains("no continuation typed"),
                outcome.ends_with("the same session resumed")
            ),
            (unwitnessed, !unwitnessed),
            "{name}: {outcome}"
        );
    }
}

/// THE TYPING FENCE NAMES WHAT IT REFUSES, each arm on its own: a box `box`,
#[test]
fn the_typing_fence_names_what_it_refuses() {
    use aterm_phase::gemini::fixtures as fx;
    let rig = relaunch_rig("fence", false, Knobs::default());
    rig.kernel.relaunched.store(true, Ordering::SeqCst);
    let fence = |screen: String| {
        rig.relaunched_shows(Some(screen), None);
        let mut c = connect(&rig.opts, TAB).expect("the instance");
        typing_fence(&mut c, TAB)
    };
    for (name, text, want) in [
        ("box", fx::BOX_SHELL, "box"),
        ("busy", fx::BUSY_THINKING, "busy"),
        ("menu", fx::SLASH_MENU, "busy"),
        ("draft", fx::DRAFT, "draft"),
        ("shell-mode", fx::SHELL_MODE, "busy"),
    ] {
        assert_eq!(
            fence(at_caret(text)).map(|(_, caret)| caret),
            Err(want),
            "{name}"
        );
    }
    assert_eq!(
        fence(gemini_idle()).map(|(_, caret)| caret),
        Ok('>'),
        "the control: at rest"
    );
    assert_eq!(
        fence(at_caret(fx::YOLO_END_OF_TURN)).map(|(_, caret)| caret),
        Ok('*'),
        "the control: YOLO's caret"
    );
}

/// NOTHING IS TYPED INTO A RELAUNCHED GEMINI CLI THAT IS NOT IDLE, EMPTY AND
#[test]
fn no_continuation_into_a_busy_drafted_or_held_gemini_cli() {
    use aterm_phase::gemini::fixtures as fx;
    let person = "OK schema=1 hold=0 hand=- integration=on human_ms=400";
    let cases: Vec<(&str, Option<String>, Option<&str>, Knobs)> = vec![
        (
            "busy",
            Some(at_caret(fx::BUSY_THINKING)),
            None,
            Knobs::default(),
        ),
        ("draft", Some(at_caret(fx::DRAFT)), None, Knobs::default()),
        ("box", Some(at_caret(fx::BOX_SHELL)), None, Knobs::default()),
        (
            "shell-mode",
            Some(at_caret(fx::SHELL_MODE)),
            None,
            Knobs::default(),
        ),
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
    let typed = control.typed();
    assert!(
        typed[1].contains("submit=guarded:^\\s>\\s"),
        "the guard one blank before the caret: {}",
        typed[1]
    );
}

/// THE RELAUNCH'S CONTINUATION IS GUARDED ON THE CHIP IT IS DRAWN AS: past
#[test]
fn a_continuation_past_the_paste_fold_is_guarded_on_its_chip() {
    use aterm_phase::gemini::{fixtures as fx, folded_paste};
    let rig = relaunch_rig("fold", false, Knobs::default());
    let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
    assert_eq!(r.step, "continued", "{r:?}");
    let typed = rig.typed();
    let turn = &typed[1];
    let (_, text) = turn
        .split_once(&format!(" {TURN_WAIT} "))
        .expect("the note after the turn's options");
    assert!(
        text.starts_with("[aterm harness] Relaunched: Gemini CLI exited"),
        "{text}"
    );
    let chip = folded_paste(text).expect("the crash note is past the fold");
    let guard = composer_guard_at('>', CARET_COLUMN, &chip);
    assert!(
        turn.contains(&format!("submit=guarded:{guard} idle=600 ")),
        "the chip's guard: {turn}"
    );

    let rows = aterm_phase::prompt::fixtures::screen(fx::FOLDED_PASTE);
    let measured = folded_paste(&"x".repeat(538)).expect("538 characters fold");
    let at_chip = aterm_observe::row_matcher(&composer_guard_at('>', CARET_COLUMN, &measured))
        .expect("compiles");
    let hits: Vec<&String> = rows.iter().filter(|r| at_chip.matches(r)).collect();
    assert_eq!(
        hits,
        [" > [Pasted Text: 538 chars]"],
        "the measured caret row"
    );
    let words =
        aterm_observe::row_matcher(&composer_guard_at('>', CARET_COLUMN, text)).expect("compiles");
    assert!(
        !rows.iter().any(|r| words.matches(r)),
        "the control: the words' guard matches no row of the folded screen"
    );
    let short = "[aterm harness] keep going";
    assert_eq!(folded_paste(short), None, "the control: within the fold");
}

/// ONLY OUR OWN RELAUNCH IS ADOPTED: a Gemini CLI under the shell resuming
#[test]
fn only_the_lines_own_gemini_cli_is_adopted() {
    let other = Knobs {
        new_argv: Some(vec![
            NODE.to_string(),
            SCRIPT.to_string(),
            "--resume".to_string(),
            S2.to_string(),
        ]),
        ..Knobs::default()
    };
    let orphan = Knobs {
        new_orphan: true,
        ..Knobs::default()
    };
    for (name, knobs) in [("other-session", other), ("orphan", orphan)] {
        for hand_back in [true, false] {
            let rig = relaunch_rig(name, hand_back, knobs.clone());
            let r = rig.attempt(&rig.snap(Some(S1)), &ExitRecord::Crashed, ExitCause::Exit);
            assert_eq!(r.step, "wait:resume", "{name} hand_back={hand_back}: {r:?}");
            assert_eq!(rig.state().resumed_pid, 0, "{name}: nothing adopted");
            assert_eq!(rig.continuations(), 0, "{name}");
        }
    }
    let spelled = Knobs {
        new_argv: Some(vec![
            NODE.to_string(),
            SCRIPT.to_string(),
            format!("-r={S1}"),
        ]),
        ..Knobs::default()
    };
    let control = relaunch_rig("ours", true, spelled);
    let r = control.attempt(
        &control.snap(Some(S1)),
        &ExitRecord::Crashed,
        ExitCause::Exit,
    );
    assert_eq!((r.step.as_str(), r.pid), ("adopted", NEW), "{r:?}");
}

/// Each process goes to its own lane's reader: a `node` running Gemini CLI's
#[test]
fn each_process_goes_to_its_own_lanes_reader() {
    use crate::harness::relaunch::{SnapshotLane, snapshot_lane};
    let argv = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
    for (image, args, lane) in [
        (NODE, argv(&[NODE, SCRIPT]), SnapshotLane::Gemini),
        (
            NODE,
            argv(&[
                NODE,
                "/opt/homebrew/Cellar/gemini-cli/0.46.0/libexec/lib/node_modules/@google/gemini-cli/bundle/gemini.js",
            ]),
            SnapshotLane::Gemini,
        ),
        (
            NODE,
            argv(&[
                NODE,
                "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            ]),
            SnapshotLane::Claude,
        ),
        (
            "/opt/homebrew/bin/gemini",
            argv(&["gemini"]),
            SnapshotLane::None,
        ),
        ("/p/agents/agy", argv(&["agy"]), SnapshotLane::Agy),
    ] {
        assert_eq!(
            snapshot_lane(Some(Path::new(image)), &args),
            lane,
            "{image} {args:?}"
        );
    }
}

/// THE LINE A PERSON IS TOLD: Gemini CLI's own `gemini --resume <uuid>` in
#[test]
fn the_told_line_is_safe_to_run() {
    let rig = Rig::new("told", 1, true, false, Files::default());
    let want = format!("cd {} && gemini --resume {S1}", rig.project);
    assert_eq!(told(&rig.snap(Some(S1))).as_deref(), Some(want.as_str()));
    assert_eq!(
        told(&rig.snap(None)).as_deref(),
        Some(want.as_str()),
        "the argv's session"
    );
    let mut nameless = rig.snap(None);
    nameless.argv.truncate(2);
    assert_eq!(told(&nameless), None);
    let unsafe_rig = Rig::new(
        "told-unsafe",
        1,
        true,
        false,
        Files {
            dup: true,
            ..Files::default()
        },
    );
    assert_eq!(told(&unsafe_rig.snap(Some(S1))), None);
}

/// Tier-1 of `HarnessGeminiExitWitness` (aterm-models-harness
#[test]
#[allow(clippy::too_many_lines)]
fn tier1_the_real_gemini_exit_read_conforms_to_the_model() {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let m = aterm_models_harness::harness_gemini_exit_witness_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    let mut rig = Rig::new("tier1", 0, true, false, Files::default());
    let draw = |rig: &mut Rig, st: &BTreeMap<&'static str, i64>, resumed: bool| {
        let (exit, rule) = (st["exit"], st["rule"]);
        let status = match exit {
            1 => 1,
            2 => 0,
            3 if !resumed => 41,
            3 | 6 => 42,
            4 => 143,
            _ => 137,
        };
        rig.reset(
            status,
            st["word"] == 1,
            st["later"] == 1,
            Files {
                second: rule == 1,
                no_header: rule == 2,
                bare: rule == 2,
                late: rule == 2,
                switched: rule == 3,
                summary: rule == 3,
                dup: st["dup"] == 1,
                no_turn: st["turnless"] == 1,
                ..Files::default()
            },
        );
    };
    let snapped = |rig: &Rig, st: &BTreeMap<&'static str, i64>, resumed: bool| {
        let (rule, exit, prompt) = (st["rule"], st["exit"], st["prompt"] == 1);
        let mut snap = rig.snap((rule != 2).then_some(S1));
        let mut argv = vec![NODE.to_string(), SCRIPT.to_string()];
        if rule == 2 && exit != 6 {
            if prompt {
                argv.push("--prompt-interactive=fix it".to_string());
            }
        } else if resumed && exit != 6 {
            argv.extend(["--model", "gemini-3.5-flash", "--resume", S1].map(str::to_string));
            if prompt {
                argv.extend(["-i", "carry on", "fix it"].map(str::to_string));
            }
        } else {
            argv.extend(["--model", "gemini-3.5-flash", "--session-id", S1].map(str::to_string));
            if prompt {
                argv.push("--prompt-interactive=fix it".to_string());
            }
        }
        snap.argv = argv;
        snap
    };
    let kept = |kept: i64| match kept {
        1 => ExitRecord::Crashed,
        2 => ExitRecord::Removed,
        4 => ExitRecord::Refused,
        5 => ExitRecord::Restarted,
        _ => ExitRecord::Unread,
    };
    let (mut looks, mut attempts, mut restarts, mut resumed_crashes) = (0, 0, 0, 0);
    let mut caught: BTreeMap<&str, usize> = BTreeMap::new();
    for resumed in [false, true] {
        let mut seen = BTreeSet::new();
        let mut attempted = BTreeSet::new();
        let mut queue = VecDeque::from([m.init_state()]);
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
            if st["exit"] == 0
                || st["decided"] > 0
                || (resumed && (st["exit"] == 6 || st["rule"] == 2))
            {
                continue;
            }
            let (word, later) = (st["word"] == 1, st["later"] == 1);
            if st["seen"] == 0 {
                if !looked_once(&mut attempted, &st, "look") {
                    continue;
                }
                looks += 1;
                let look = if word && !later { "Look" } else { "LookGone" };
                let mut after = st.clone();
                assert!(m.fire(look, &mut after), "{look} at {st:?}");
                draw(&mut rig, &st, resumed);
                let left = rig.look(&snapped(&rig, &st, resumed));
                assert_eq!(
                    left,
                    kept(after["kept"]),
                    "the look at {st:?} (resumed={resumed})"
                );
                for mutant in [
                    "LookAsClaude",
                    "RefusalAsCrash",
                    "ChildDeathAsRefusal",
                    "RestartAsRefusal",
                ] {
                    let mut wrong = st.clone();
                    if buggy.fire(mutant, &mut wrong) && wrong["kept"] != after["kept"] {
                        assert_ne!(left, kept(wrong["kept"]), "{mutant} at {st:?}");
                        *caught.entry(mutant).or_default() += 1;
                    }
                }
                continue;
            }
            if !looked_once(&mut attempted, &st, "attempt") {
                continue;
            }
            attempts += 1;
            let mut after = st.clone();
            assert!(m.fire("Decide", &mut after), "Decide at {st:?}");
            draw(&mut rig, &st, resumed);
            let r = rig.attempt(
                &snapped(&rig, &st, resumed),
                &kept(st["kept"]),
                ExitCause::Exit,
            );
            let continued = r.step == "continued";
            let relaunched = continued || r.step == "done:resumed";
            assert_eq!(
                relaunched,
                after["decided"] == 1,
                "the attempt at {st:?} (resumed={resumed}): {r:?}\n{:#?}",
                rig.typed()
            );
            assert_eq!(
                continued,
                after["cont"] == 1,
                "the continuation at {st:?} (resumed={resumed}): {r:?}"
            );
            if st["exit"] == 6 && relaunched {
                restarts += 1;
            }
            if st["exit"] != 6 && st["later"] == 1 && continued {
                resumed_crashes += 1;
            }
            if !relaunched {
                assert_eq!(
                    rig.lines(),
                    0,
                    "nothing typed at {st:?}: {:#?}",
                    rig.typed()
                );
            }
            for mutant in [
                "NonUniqueTrusted",
                "LateHeaderTrusted",
                "SwitchHiddenBySummary",
                "PrecheckSkipped",
                "TurnlessRestartLeft",
                "TurnlessResumed",
            ] {
                let mut wrong = st.clone();
                if buggy.fire(mutant, &mut wrong) && wrong["decided"] != after["decided"] {
                    assert_ne!(relaunched, wrong["decided"] == 1, "{mutant} at {st:?}");
                    *caught.entry(mutant).or_default() += 1;
                }
            }
            for mutant in ["RestartContinued", "GoneWordReadAsRestart"] {
                let mut wrong = st.clone();
                if buggy.fire(mutant, &mut wrong)
                    && (wrong["decided"], wrong["cont"]) != (after["decided"], after["cont"])
                {
                    assert_ne!(
                        (relaunched, continued),
                        (wrong["decided"] == 1, wrong["cont"] == 1),
                        "{mutant} at {st:?} (resumed={resumed})"
                    );
                    *caught.entry(mutant).or_default() += 1;
                }
            }
        }
    }
    assert!(
        looks > 0 && attempts > 0 && restarts > 0 && resumed_crashes > 0,
        "{looks} looks, {attempts} attempts, {restarts} restarts resumed, \
         {resumed_crashes} crashes continued past a word gone"
    );
    for mutant in [
        "LookAsClaude",
        "RefusalAsCrash",
        "ChildDeathAsRefusal",
        "RestartAsRefusal",
        "NonUniqueTrusted",
        "LateHeaderTrusted",
        "SwitchHiddenBySummary",
        "PrecheckSkipped",
        "TurnlessRestartLeft",
        "TurnlessResumed",
        "GoneWordReadAsRestart",
        "RestartContinued",
    ] {
        assert!(
            caught.get(mutant).copied().unwrap_or(0) >= 1,
            "{mutant} is never caught: {caught:?} ({looks} looks, {attempts} attempts)"
        );
    }
}

/// Whether the model state `st` is the first of its kind `what` (a look, an
fn looked_once(
    done: &mut std::collections::BTreeSet<(&'static str, [i64; 9])>,
    st: &std::collections::BTreeMap<&'static str, i64>,
    what: &'static str,
) -> bool {
    done.insert((
        what,
        [
            st["exit"],
            st["rule"],
            st["dup"],
            st["turnless"],
            st["prompt"],
            st["word"],
            st["later"],
            st["kept"],
            st["seen"],
        ],
    ))
}
