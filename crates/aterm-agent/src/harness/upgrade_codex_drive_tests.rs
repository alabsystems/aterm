// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Codex lane's driver over scripted screens and a scripted kernel: a
//! stand-in aterm instance on a real socket (every request recorded, nothing
//! typed anywhere) whose screen turns from Codex's idle composer into the
//! shell with the TUI's exit hint when `/exit` is typed, and into the
//! relaunched Codex when the relaunch line is; and a kernel whose TUI dies
//! when `/exit` is typed and whose relaunch appears when its line is. Screens
//! and hints are Codex 0.157.0/0.157.1's.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::harness::upgrade::Request;
use crate::supervise::ScratchDir;

const TAB: &str = "s-c0dec0dec0dec0dec0de";
const TOKEN: &str = "0badf00d0badf00d";
/// The thread a daemon-mode exit names; the embedded session's thread.
const T1: &str = "01a0dc36-1dc1-7ee2-be91-11bc323e377c";
const T2: &str = "01a0dc3a-5c71-7143-bcee-58def0a15dfd";
/// Pids no real process of this test has: the kernel is scripted, and a real
/// `ps` of them reads nothing.
const TUI: u32 = 4_000_001;
const SHELL: u32 = 4_000_002;
const NEW: u32 = 4_000_003;

fn v(s: &str) -> Version {
    Version::parse(s).expect("version")
}

/// A fresh directory per CALL, removed when it drops: two tests share a name
/// (`Rig::new("daemon-busy")` and `daemon_home("daemon-busy")`) and run on
/// parallel threads, so a per-name directory would let one wipe the other's.
fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("cxd", name)
}

/// One `text --json` body: `rows`, the cursor at `(row, col)`, `seq` and a
/// generation, and no person's input to the tab.
fn screen_json(rows: &[&str], cursor: (usize, usize), seq: u64) -> String {
    let rows = aterm_json::to_string(&aterm_json::Value::Array(
        rows.iter().map(|r| aterm_json::Value::from(*r)).collect(),
    ))
    .expect("rows");
    format!(
        r#"{{"rows":{rows},"cursor":{{"row":{},"col":{}}},"seq":{seq},"gen":"1.{seq}","human_ms":null,"first":0}}"#,
        cursor.0, cursor.1
    )
}

/// Codex 0.157's idle screen: the card, the composer's dim placeholder with
/// the cursor at column 2, the footer.
fn codex_idle(seq: u64) -> String {
    screen_json(
        &[
            "╭─────────────────────────────────────────────╮",
            "│ >_ OpenAI Codex (v0.157.0)                  │",
            "╰─────────────────────────────────────────────╯",
            "› say hello",
            "■ Conversation interrupted - tell the model what to do differently.",
            "",
            "› Ask Codex to do anything",
            "",
            "  GPT-6-Astra default · /w",
            "  ← for agents · ? for shortcuts",
        ],
        (6, 2),
        seq,
    )
}

/// [`codex_idle`] with a goal being pursued: the footer under the input line
/// says `Pursuing goal (…)`, as Codex 0.158.0 draws it.
fn codex_idle_goal(seq: u64) -> String {
    screen_json(
        &[
            "╭─────────────────────────────────────────────╮",
            "│ >_ OpenAI Codex (v0.157.0)                  │",
            "╰─────────────────────────────────────────────╯",
            "› say hello",
            "■ Conversation interrupted - tell the model what to do differently.",
            "",
            "› Ask Codex to do anything",
            "",
            "  GPT-6-Astra default · /w                    Pursuing goal (10d 3h 14m)",
            "  ← for agents · ? for shortcuts",
        ],
        (6, 2),
        seq,
    )
}

/// The same screen with a typed draft: the caret moved home, the text drawn
/// in plain attributes (the `cell` reply says which).
fn codex_draft(seq: u64) -> String {
    screen_json(
        &[
            "› say hello",
            "",
            "› my draft here",
            "",
            "  GPT-6-Astra default · /w",
        ],
        (2, 2),
        seq,
    )
}

/// The shell after the TUI's exit: an older exit's hint (another thread)
/// above the command that ran this one, then this one's hint `hint`, then
/// the prompt with the cursor after it.
fn shell_after(hint: &[&str]) -> String {
    let mut rows = vec![
        "Reconnect: codex resume 01a0dc99-0000-7000-8000-000000000099".to_string(),
        "Stop the current turn: run codex agents, select this task, and press x.".to_string(),
        "% codex".to_string(),
    ];
    rows.extend(hint.iter().map(|r| (*r).to_string()));
    rows.push("% ".to_string());
    let at = rows.len() - 1;
    let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
    screen_json(&refs, (at, 2), 900)
}

/// The shell integration's last two blocks for [`shell_after`]: the command
/// that ran the TUI (its output the hint rows), then the prompt entering.
fn blocks_after(hint_rows: usize) -> (String, String) {
    let cmd = 2;
    let out = cmd + 1;
    let end = out + hint_rows;
    let done = format!(
        r#"{{"id":7,"state":"complete","exit":0,"prompt":{cmd},"cmd":{cmd},"cmdcol":2,"out":{out},"end":{end},"cmdline":"codex"}}"#
    );
    let entering = format!(
        r#"{{"id":8,"state":"entering","exit":null,"prompt":{end},"cmd":{end},"cmdcol":2,"out":null,"end":null,"cmdline":""}}"#
    );
    (
        format!(r#"{{"blocks":[{done},{entering}]}}"#),
        format!(r#"{{"blocks":[{entering}]}}"#),
    )
}

/// What the stand-in serves, and what it saw.
struct World {
    /// Codex's screen before `/exit`, the shell's after it, Codex's after
    /// the relaunch line.
    before: String,
    after_exit: String,
    after_relaunch: String,
    blocks2: String,
    blocks1: String,
    /// The `cell` reply at the caret.
    cell: &'static str,
    /// The output rows of the command block that ran the TUI (`blocktext`).
    block_out: Vec<String>,
    /// The `status` reply.
    status: String,
    /// Appended to the `status` reply as it is asked (a person's stamp, a
    /// hold): what changes while a test runs.
    status_extra: Arc<Mutex<String>>,
    /// A `turn` types its text and does not submit it, answering this (a
    /// missed guard's `skipped reason=guard`, or an Enter written and not seen,
    /// `submitted=0 pressed=1`), and the composer then shows that text
    /// ([`World::left`]) until the test clears it; the lane never takes it
    /// back.
    miss: Option<&'static str>,
    /// What the lane left typed in the composer, as the stand-in draws it.
    left: Arc<Mutex<Option<String>>>,
    /// The screen the tab comes to show while a test runs — what a turn's end
    /// leaves on it, a wall — in place of [`World::before`], once set.
    shows: Arc<Mutex<Option<String>>>,
    /// The `lease acquire …` reply (a `lease release` is `OK lease released`
    /// after an acquire it granted).
    lease: &'static str,
    exited: Arc<AtomicBool>,
    relaunched: Arc<AtomicBool>,
    asked: Arc<Mutex<Vec<String>>>,
    /// A goal on the tab's Codex, as the stand-in draws it (`None`: the
    /// screens above, as they are): its footer and the relaunched Codex's
    /// `Resume paused goal?` box, moved by the goal's keys and commands.
    goal: Option<Arc<Mutex<GoalWorld>>>,
}

/// A Codex goal as the stand-in draws it ([`World::goal`]).
#[derive(Debug, Default)]
pub(super) struct GoalWorld {
    /// The goal is paused (its footer `Goal paused (/goal resume)`), not
    /// pursued (`Pursuing goal (…)`).
    pub(super) paused: bool,
    /// Codex takes a typed `/goal pause` (its footer then paused); `false`:
    /// it never shows.
    pub(super) takes: bool,
    /// The relaunched Codex's `Resume paused goal?` box stands.
    pub(super) boxed: bool,
    /// The person stamp its screens carry (`human_ms`), `None`: nobody.
    pub(super) human_ms: Option<u64>,
    /// A goal turn's status row stands over the composer (`• Working (0s •
    /// esc to interrupt)`), as it does for the whole of a turn's head.
    pub(super) working: bool,
    /// How many screen reads after the relaunch show its startup frame (the
    /// composer, no footer, `Resuming session…`) before its paused goal's box
    /// (about 200 ms on 0.158.0).
    pub(super) startup: u32,
    /// What the goal was given, in order: `pause`, `resume` (typed), `enter`,
    /// `esc` (keys).
    pub(super) seen: Vec<String>,
}

/// The tab's Codex with a goal on its footer, pursued or `paused`, at its
/// idle-looking frame (a goal turn's first moments), the person stamp
/// `human`; with a goal turn's status row over the composer where `working`
/// (`• Working (0s • esc to interrupt)`, as
/// `codex-0.158.0-goal-busy-live.txt` draws it mid-turn), and its composer
/// holding `draft`, the cursor after it, where there is one (the lane's own
/// words left there, [`World::left`]).
pub(super) fn codex_goal_frame(
    paused: bool,
    working: bool,
    human: Option<u64>,
    draft: Option<&str>,
    seq: u64,
) -> String {
    let goal = if paused {
        aterm_phase::anchors::anchor_text("codex.goal.paused").to_string()
    } else {
        format!(
            "{}10d 3h 14m)",
            aterm_phase::anchors::anchor_text("codex.goal.pursuing")
        )
    };
    let footer = format!("  GPT-6-Astra default · /w                    {goal}");
    let status = if working {
        "• Working (0s • esc to interrupt)"
    } else {
        ""
    };
    let composer = format!("› {}", draft.unwrap_or("Ask Codex to do anything"));
    let json = screen_json(
        &[
            "╭─────────────────────────────────────────────╮",
            "│ >_ OpenAI Codex (v0.157.0)                  │",
            "╰─────────────────────────────────────────────╯",
            "• Continuing toward the goal.",
            "",
            status,
            "",
            &composer,
            "",
            &footer,
            "  ← for agents · ? for shortcuts",
        ],
        (7, draft.map_or(2, |d| 2 + d.chars().count())),
        seq,
    );
    match human {
        Some(ms) => json.replace(r#""human_ms":null"#, &format!(r#""human_ms":{ms}"#)),
        None => json,
    }
}

/// The relaunched Codex opening on its paused goal's box (the vendor's own
/// render, `aterm_phase::codex::fixtures::GOAL_RESUME`), the cursor on its
/// focused option.
pub(super) fn codex_goal_box(seq: u64) -> String {
    let rows = aterm_phase::prompt::fixtures::screen(aterm_phase::codex::fixtures::GOAL_RESUME);
    let at = rows
        .iter()
        .position(|r| r.starts_with("› 1."))
        .expect("the focused option");
    let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
    screen_json(&refs, (at, 0), seq)
}

/// The relaunched Codex's startup frame, drawn before its paused goal's box
/// (0.158.0, `codex resume <thread>` over a paused goal: `Resuming session…`
/// over the composer, no footer, about 0.3 s in; the box at 0.5 s).
fn codex_startup(seq: u64) -> String {
    screen_json(
        &[
            "│ model:     loading   /model to change │",
            "│ directory: /w                          │",
            "╰────────────────────────────────────────╯",
            "",
            "  Resuming session…",
            "",
            "› Ask Codex to do anything",
            "",
            "  ? for shortcuts",
        ],
        (6, 2),
        seq,
    )
}

/// Codex's composer holding `text`, typed: the caret row, the cursor after
/// it (the cell at column 2 reads `none`).
fn codex_holding(text: &str, seq: u64) -> String {
    let caret = format!("› {text}");
    let at = 2 + text.chars().count();
    screen_json(
        &["› say hello", "", &caret, "", "  GPT-6-Astra default · /w"],
        (2, at),
        seq,
    )
}

/// A control socket at `<dir>/t.sock` standing in for the aterm instance of
/// [`TAB`] (the Claude lane's stand-in, answering as the host answers).
fn instance(dir: &Path, w: World) -> String {
    let sock = dir.join("t.sock");
    std::fs::write(dir.join("t.sock.token"), format!("{TOKEN}\n")).expect("token");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let Ok(mut out) = conn.try_clone() else {
                continue;
            };
            let mut lines = std::io::BufReader::new(conn).lines();
            let auth = format!("AUTH {TOKEN}");
            if !lines.next().is_some_and(|l| l.is_ok_and(|l| l == auth)) {
                continue;
            }
            for line in lines {
                let Ok(line) = line else { break };
                let verb = line
                    .split_whitespace()
                    .find(|w| !w.starts_with('@'))
                    .unwrap_or("");
                let exited = w.exited.load(Ordering::SeqCst);
                let relaunched = w.relaunched.load(Ordering::SeqCst);
                let left = w.left.lock().ok().and_then(|l| l.clone());
                let goal = w.goal.as_ref().and_then(|g| {
                    let mut g = g.lock().ok()?;
                    // The relaunch's startup frames, one per screen read.
                    let startup = verb == "text" && relaunched && g.startup > 0;
                    if startup {
                        g.startup -= 1;
                    }
                    Some((g.paused, g.boxed, g.human_ms, g.working, startup))
                });
                let reply = match verb {
                    "sessions" => format!("OK 1\nlocal {TAB} 0 idle codex"),
                    // A goal on the tab's Codex: its footer, and the box the
                    // relaunched Codex opens on while the goal is paused.
                    "text" if goal.is_some() && (relaunched || !exited) => {
                        let (paused, boxed, human, working, startup) = goal.unwrap_or_default();
                        format!(
                            "OK {}",
                            if startup {
                                codex_startup(39)
                            } else if boxed {
                                codex_goal_box(40)
                            } else {
                                codex_goal_frame(
                                    paused,
                                    working,
                                    human,
                                    left.as_deref(),
                                    if paused { 12 } else { 11 },
                                )
                            }
                        )
                    }
                    "text" => format!(
                        "OK {}",
                        if relaunched {
                            w.after_relaunch.clone()
                        } else if exited {
                            w.after_exit.clone()
                        } else if let Some(t) = &left {
                            codex_holding(t, 30)
                        } else if let Some(shown) = w.shows.lock().ok().and_then(|s| s.clone()) {
                            shown
                        } else {
                            w.before.clone()
                        }
                    ),
                    "cell" if left.is_some() => "OK / d0d0d0 2d2f33 none".to_string(),
                    "cell" => w.cell.to_string(),
                    "status" => format!(
                        "{}{}",
                        w.status,
                        w.status_extra.lock().map(|e| e.clone()).unwrap_or_default()
                    ),
                    "help" if line.contains("help key") => {
                        "OK 2\nkey  key [if-gen=<epoch>.<seq>] [if=<re>] <name>\n     FENCES"
                            .to_string()
                    }
                    "help" => "OK 1\nturn [if-gen=<epoch>.<seq>] <text>".to_string(),
                    "blocks" if line.contains("blocks 2") => format!("OK {}", w.blocks2),
                    "blocks" => format!("OK {}", w.blocks1),
                    "blocktext" => std::iter::once(format!("OK {}", w.block_out.len()))
                        .chain(w.block_out.iter().cloned())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    "line" => "OK % ".to_string(),
                    "lease" if line.contains(" lease release ") => {
                        if w.lease.starts_with("OK lease acquired") {
                            "OK lease released".to_string()
                        } else {
                            "OK lease none".to_string()
                        }
                    }
                    "lease" => w.lease.to_string(),
                    "turn" if w.miss.is_some() => {
                        // What the host does on a missed guard: the text is
                        // typed and its Enter not pressed.
                        let text = line
                            .split_once(&format!(" {TURN_WAIT} "))
                            .map_or("", |(_, t)| t)
                            .to_string();
                        if let Ok(mut l) = w.left.lock() {
                            *l = Some(text);
                        }
                        w.miss.unwrap_or_default().to_string()
                    }
                    // The goal's keys: Enter on the resume box resumes the
                    // goal, Esc stops a goal turn and pauses the goal.
                    "key"
                        if w.goal.is_some()
                            && (line.ends_with(" enter") || line.ends_with(" esc")) =>
                    {
                        if let Some(Ok(mut g)) = w.goal.as_ref().map(|g| g.lock()) {
                            if line.ends_with(" enter") && g.boxed {
                                g.boxed = false;
                                g.paused = false;
                                g.seen.push("enter".to_string());
                            } else if line.ends_with(" esc") {
                                g.paused = true;
                                g.seen.push("esc".to_string());
                            }
                        }
                        "OK seq=41".to_string()
                    }
                    "turn" => {
                        if line.ends_with(" /exit") {
                            w.exited.store(true, Ordering::SeqCst);
                        } else if line.contains("'resume'") || line.contains("/agents/codex'") {
                            w.relaunched.store(true, Ordering::SeqCst);
                            // `codex resume` over a paused goal opens on its box.
                            if let Some(Ok(mut g)) = w.goal.as_ref().map(|g| g.lock()) {
                                g.boxed = g.paused;
                            }
                        } else if let Some(Ok(mut g)) = w.goal.as_ref().map(|g| g.lock()) {
                            if line.ends_with(" /goal pause") {
                                g.seen.push("pause".to_string());
                                if g.takes {
                                    g.paused = true;
                                }
                            } else if line.ends_with(" /goal resume") {
                                g.seen.push("resume".to_string());
                                g.paused = false;
                            }
                        }
                        "OK 0 turn submitted=1 status=settled seq=1 id=1".to_string()
                    }
                    _ => "ERR unscripted".to_string(),
                };
                if let Ok(mut asked) = w.asked.lock() {
                    asked.push(line);
                }
                if writeln!(out, "{reply}").is_err() {
                    break;
                }
            }
        }
    });
    sock.to_string_lossy().into_owned()
}

/// The scripted kernel: [`TUI`] is [`SHELL`]'s foreground job on a tab's
/// terminal, dies when `/exit` is typed, and the relaunch [`NEW`] appears
/// under the shell when its line is — with `argv`.
struct Script {
    exited: Arc<AtomicBool>,
    relaunched: Arc<AtomicBool>,
    /// The thread lock the TUI holds (embedded), and the new TUI after it.
    lock: Option<PathBuf>,
    new_argv: Vec<String>,
    background: Vec<String>,
    /// The session leaders under the TUI ([`CodexKernel::terminals`]).
    terminals: Vec<String>,
    /// What runs under the TUI, named for the notice ([`CodexKernel::held`]).
    held: Vec<upgrade::Held>,
    /// The `TZ` each pid was started with ([`CodexKernel::tz`]; `None`
    /// inside: none named): a pid not listed has an environment that cannot
    /// be read.
    tz: Vec<(u32, Option<String>)>,
    /// The executable [`SHELL`] runs ([`CodexKernel::args`]): `/bin/zsh`.
    shell_exe: &'static str,
    /// The image [`TUI`] runs ([`CodexKernel::exe`]): none read.
    exe: Option<PathBuf>,
}

impl Kernel for Script {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        Some((Job::Foreground, SHELL))
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        (pid == NEW || pid == TUI).then_some(SHELL)
    }

    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        // A tab shell reparented to pid 1 by a live update: aterm's.
        Some((1, "launchd".to_string()))
    }
}

impl CodexKernel for Script {
    fn alive(&self, pid: u32) -> bool {
        match pid {
            TUI => !self.exited.load(Ordering::SeqCst),
            NEW => self.relaunched.load(Ordering::SeqCst),
            _ => pid == SHELL,
        }
    }

    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        (pid == SHELL).then(|| atpkg::caller_shell::ProcArgs {
            exec_path: self.shell_exe.to_string(),
            argv: vec![format!("-{}", base_name(self.shell_exe))],
            env: Vec::new(),
        })
    }

    fn tz(&self, pid: u32) -> Option<Option<String>> {
        self.tz
            .iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, tz)| tz.clone())
    }

    fn exe(&self, pid: u32) -> Option<PathBuf> {
        self.exe.clone().filter(|_| pid == TUI)
    }

    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>> {
        Some(match (pid, &self.lock) {
            (TUI, Some(lock)) if !self.exited.load(Ordering::SeqCst) => vec![lock.clone()],
            // The relaunch holds the thread again, as `holders_of` says.
            (NEW, Some(lock)) if self.relaunched.load(Ordering::SeqCst) => vec![lock.clone()],
            _ => Vec::new(),
        })
    }

    fn holders_of(&self, file: &Path) -> Option<Vec<u32>> {
        if self.lock.as_deref() != Some(file) {
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
    }

    fn codex_children(&self, _: u32) -> Vec<(u32, Vec<String>)> {
        if self.relaunched.load(Ordering::SeqCst) {
            vec![(NEW, self.new_argv.clone())]
        } else {
            Vec::new()
        }
    }

    fn background(&self, _: u32) -> Vec<String> {
        self.background.clone()
    }

    fn terminals(&self, _: u32) -> Vec<String> {
        self.terminals.clone()
    }

    fn held(&self, _: u32) -> Vec<upgrade::Held> {
        self.held.clone()
    }

    fn daemon_clients(&self, _: u32) -> Option<Vec<u32>> {
        Some(vec![TUI])
    }

    fn cwd(&self, _: u32) -> Option<String> {
        Some("/w".to_string())
    }

    fn in_tab(&self, _: &mut Client, _: u32, _: &str) -> bool {
        true
    }
}

/// One scripted rig: a home with the thread's rollout, a state directory, the
/// stand-in instance, the kernel, and the TUI and target a sweep found.
struct Rig {
    dir: ScratchDir,
    opts: Opts,
    tui: Tui,
    target: Target,
    kernel: Script,
    asked: Arc<Mutex<Vec<String>>>,
    rollout: PathBuf,
    /// What the stand-in's composer holds of the lane's own typing.
    left: Arc<Mutex<Option<String>>>,
    /// Appended to the tab's `status` reply ([`World::status_extra`]).
    status_extra: Arc<Mutex<String>>,
    /// What the tab comes to show ([`World::shows`]).
    shows: Arc<Mutex<Option<String>>>,
}

impl Rig {
    fn new(name: &str, thread: &str, embedded: bool, world: impl FnOnce(&mut World)) -> Rig {
        let dir = scratch(name);
        let home = dir.join("home");
        let codex = home.join(".codex");
        let day = codex.join("sessions/2026/09/25");
        std::fs::create_dir_all(&day).expect("sessions");
        std::fs::create_dir_all(codex.join("thread-writer-locks")).expect("locks");
        let codex = canonical(&codex);
        let rollout = day.join(format!("rollout-2026-09-25T22-35-29-{thread}.jsonl"));
        std::fs::write(
            &rollout,
            [
                r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"1"}}"#,
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"say hello"}]}}"#,
                r#"{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"1","reason":"interrupted"}}"#,
                "",
            ]
            .join("\n"),
        )
        .expect("rollout");
        let exited = Arc::new(AtomicBool::new(false));
        let relaunched = Arc::new(AtomicBool::new(false));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let left = Arc::new(Mutex::new(None));
        let status_extra = Arc::new(Mutex::new(String::new()));
        let shows = Arc::new(Mutex::new(None));
        let hint = if embedded {
            vec![
                "To continue this session, run:".to_string(),
                format!("  codex resume {thread}"),
            ]
        } else {
            vec![
                "Disconnected from this task. Any running work continues.".to_string(),
                format!("Reconnect: codex resume {thread}"),
                "Stop the current turn: run codex agents, select this task, and press x."
                    .to_string(),
            ]
        };
        let hint_refs: Vec<&str> = hint.iter().map(String::as_str).collect();
        let (blocks2, blocks1) = blocks_after(hint.len());
        let mut w = World {
            before: codex_idle(10),
            after_exit: shell_after(&hint_refs),
            after_relaunch: codex_idle(20),
            blocks2,
            blocks1,
            cell: "OK A 9b9b9c 2d2f33 dim",
            block_out: hint.clone(),
            status: "OK schema=1 hold=0 hand=- integration=on".to_string(),
            status_extra: Arc::clone(&status_extra),
            miss: None,
            left: Arc::clone(&left),
            shows: Arc::clone(&shows),
            lease: "ERR unscripted",
            exited: Arc::clone(&exited),
            relaunched: Arc::clone(&relaunched),
            asked: Arc::clone(&asked),
            goal: None,
        };
        world(&mut w);
        let sock = instance(&dir, w);
        let opts = Opts {
            home,
            state: dir.join("state"),
            sock: Some(sock),
            only_sid: None,
            dry_run: false,
            human_grace_s: 120,
            hand_back: false,
            background: false,
            aterm_state: None,
            // The ask-and-drain flow these tests pin (`upgrade_ask = true`).
            ask: true,
            several_logins: false,
            instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
        };
        let lock = embedded.then(|| {
            codex
                .join("thread-writer-locks")
                .join(format!("{thread}.lock"))
        });
        let target = Target {
            twin: dir.join("prefix/agents/codex"),
            exe: dir.join("prefix/store/codex/b/bin/codex"),
            version: v("0.157.1"),
        };
        let new_argv = vec![
            "/store/codex/1000000000157000001/bin/codex".to_string(),
            "resume".to_string(),
            thread.to_string(),
        ];
        age(&rollout);
        Rig {
            tui: Tui {
                pid: TUI,
                tab: TAB.to_string(),
                claim: None,
                argv: vec!["/store/codex/1000000000157000000/bin/codex".to_string()],
                version: v("0.157.0"),
                home: codex,
            },
            target,
            kernel: Script {
                exited,
                relaunched,
                lock,
                new_argv,
                background: Vec::new(),
                terminals: Vec::new(),
                held: Vec::new(),
                // The TUI names no `TZ`: the machine's own zone.
                tz: vec![(TUI, None)],
                shell_exe: "/bin/zsh",
                exe: None,
            },
            asked,
            rollout,
            left,
            status_extra,
            shows,
            opts,
            dir,
        }
    }

    /// A pending state for this TUI whose screen has been quiet for a minute:
    /// what the sweep's own earlier ticks leave.
    fn settle(&self, seq: u64) {
        let now = now_s();
        let st = St {
            agent: Agent::Codex,
            from: "0.157.0".into(),
            to: "0.157.1".into(),
            source: "managed".into(),
            salt: now - 60,
            pending_since: now - 60,
            last_seq: seq,
            seq_since_s: now - 60,
            notice_pid: TUI,
            notice_start: String::new(),
            tab: TAB.into(),
            ..St::default()
        };
        save(&self.opts, &key(TAB), &st);
    }

    fn visit(&self, daemon: Option<DaemonView>) -> Report {
        visit(&self.opts, &self.tui, &self.target, daemon, &self.kernel)
    }

    fn state(&self) -> St {
        load(&self.opts, &key(TAB)).expect("a state")
    }

    fn typed(&self) -> Vec<String> {
        self.asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
            .unwrap_or_default()
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().map(|a| a.clone()).unwrap_or_default()
    }

    fn ledger_steps(&self) -> Vec<String> {
        std::fs::read_to_string(super::super::ledger_path(&self.opts))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .filter_map(|v| v.get("step")?.as_str().map(str::to_owned))
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `path` written a minute ago: a rollout past the settling window.
fn age(path: &Path) {
    let when = std::time::SystemTime::now() - Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(when))
        .expect("mtime");
}

/// A daemon on the managed build: what a daemon-mode client waits for.
fn daemon_current() -> Option<DaemonView> {
    daemon_at(Some("0.157.1"), "")
}

/// A daemon running `running`, its step's wait `wait`.
fn daemon_at(running: Option<&str>, wait: &str) -> Option<DaemonView> {
    Some(DaemonView {
        running: running.map(v),
        wait: wait.to_string(),
        pid: 0,
        threads: Vec::new(),
    })
}

#[test]
fn a_daemon_client_is_exited_by_a_typed_exit_and_resumed_on_the_managed_twin() {
    let rig = Rig::new("daemon-swap", T1, false, |_| {});
    rig.settle(10);
    let k = InFlight(&rig.kernel, &rig.opts, Mutex::new(None));
    let r = visit(&rig.opts, &rig.tui, &rig.target, daemon_current(), &k);
    assert_eq!(r.step, "done", "{r:?}\n{:#?}", rig.asked());
    // The record in flight, the TUI gone and its exit not read yet, carries
    // no line (a line planned with the placeholder thread would resume
    // `00000000-…`).
    let in_flight =
        k.2.into_inner()
            .expect("the record")
            .expect("the relaunch looked for the shell's prompt");
    assert!(matches!(in_flight.phase, Phase::Exiting { .. }));
    assert_eq!(
        in_flight.line, "",
        "no line until the exit names the thread"
    );
    assert_eq!(r.pid, NEW);
    let typed = rig.typed();
    assert_eq!(typed.len(), 2, "{typed:#?}");
    // `/exit`, fenced on the read that judged the composer empty and guarded
    // on the composer's row by either of its marks (`›`, and 0.158.0's `»`).
    assert!(
        typed[0].contains("if-gen=1.10 yield=0.2 submit=guarded:^[›»]\\s/exit\\s*$ ")
            && typed[0].ends_with(" /exit"),
        "{}",
        typed[0]
    );
    // The relaunch: the managed twin, `resume` and the thread the TUI's own
    // hint named — never the older exit's still on the screen.
    let line = &typed[1];
    assert!(
        line.contains(&format!("{}' 'resume' '{T1}'", rig.target.twin.display())),
        "{line}"
    );
    assert!(
        !line.contains("01a0dc99"),
        "the older exit's thread: {line}"
    );
    // Nothing signals a Codex process.
    assert!(!rig.asked().iter().any(|l| l.contains("signal")));
    let st = rig.state();
    assert_eq!(st.phase, Phase::Done);
    assert_eq!(st.mode, "daemon");
    assert_eq!(st.thread, T1);
    assert_eq!(st.resumed_pid, NEW);
    assert_eq!(
        rig.ledger_steps(),
        vec!["exit-typed", "relaunched", "done"],
        "one ledger row per act"
    );
}

/// ND1 for a Codex: the harness's hard hand is taken before the `/exit`'s
/// last look and kept through the `/exit`, the bare shell and the relaunch
/// line, both typed under it on its own connection, and given back once the
/// relaunched TUI is carried on. Negative control: another driver's hold on
/// the tab makes the restart wait `held`, typing nothing and giving nothing
/// back.
#[test]
fn a_codex_restart_holds_the_tab_from_its_last_look_to_the_relaunched_tui() {
    let held = "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1";
    let rig = Rig::new("codex-hand", T1, false, |w| w.lease = held);
    rig.settle(10);
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "done", "{r:?}\n{:#?}", rig.asked());
    let asked = rig.asked();
    let at = |needle: &str| asked.iter().position(|l| l.contains(needle));
    let taken = at(" lease acquire ").expect("the hand taken");
    assert!(asked[taken].ends_with(" hard"), "{}", asked[taken]);
    let exit = at(" /exit").expect("the /exit");
    let line = asked
        .iter()
        .rposition(|l| l.contains(" turn "))
        .expect("the relaunch line");
    assert!(taken < exit && exit < line, "{asked:#?}");
    assert!(
        !asked[taken..=line]
            .iter()
            .any(|l| l.contains(" lease release ")),
        "never let go: {asked:#?}"
    );
    assert!(
        asked[line..].iter().any(|l| l.contains(" lease release ")),
        "given back: {asked:#?}"
    );
    // Negative control: another driver holds the tab.
    let rig = Rig::new("codex-hand-theirs", T1, false, |w| {
        w.lease = "ERR lease held holder=orchestrator expires_in_ms=5000";
    });
    rig.settle(10);
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "wait:held", "{r:?}");
    assert!(rig.typed().is_empty(), "{:#?}", rig.asked());
    assert!(!rig.asked().iter().any(|l| l.contains(" lease release ")));
}

#[test]
fn a_daemon_client_waits_for_its_daemon_and_for_every_quiet_gate() {
    // The daemon's own move goes first.
    let rig = Rig::new("daemon-first", T1, false, |_| {});
    rig.settle(10);
    // ... and the client says what the daemon waits on, not a bare
    // `daemon-first`.
    assert_eq!(
        rig.visit(daemon_at(Some("0.157.0"), "busy-thread")).step,
        "wait:daemon-first:busy-thread"
    );
    assert_eq!(
        rig.visit(daemon_at(None, "daemon-version")).step,
        "wait:daemon-first:daemon-version"
    );
    assert_eq!(
        rig.state().wait,
        "daemon-first:daemon-version",
        "the owner's view carries the daemon's word"
    );
    assert!(rig.typed().is_empty());
    // No daemon at all and no lock held: nothing proves the mode.
    assert_eq!(rig.visit(None).step, "wait:no-daemon");
    // Its daemon on the build, its own thread running a turn its screen does
    // not show (the kernel names it: the daemon's one thread, this TUI its one
    // client; a goal-mode Codex begins its next turn within milliseconds of
    // every end, its screen idle for seconds): its `/exit` might stop that
    // turn, so nothing is typed, at any rung.
    let hidden = Some(DaemonView {
        running: Some(v("0.157.1")),
        wait: String::new(),
        pid: DAEMON,
        threads: vec![root_turn(T1, TurnState::Busy)],
    });
    assert_eq!(rig.visit(hidden).step, "wait:daemon-turn");
    assert!(rig.typed().is_empty());
    // A screen that moved since the last tick is still settling.
    let moved = Rig::new("daemon-settling", T1, false, |_| {});
    moved.settle(9);
    assert_eq!(moved.visit(daemon_current()).step, "wait:settling");
    assert!(moved.typed().is_empty());
    // A person's draft (the caret moved home: the cell reads `none`).
    let draft = Rig::new("daemon-draft", T1, false, |w| {
        w.before = codex_draft(10);
        w.cell = "OK m d0d0d0 2d2f33 none";
    });
    draft.settle(10);
    assert_eq!(draft.visit(daemon_current()).step, "wait:draft");
    assert!(
        draft.typed().is_empty(),
        "nothing is ever typed over a draft"
    );
    // A turn running on screen.
    let busy = Rig::new("daemon-busy", T1, false, |w| {
        w.before = screen_json(
            &[
                "› say hello",
                "• Working (7s • esc to interrupt)",
                "",
                "› Ask Codex to do anything",
                "",
                "  GPT-6-Astra default · /w",
            ],
            (3, 2),
            10,
        );
    });
    busy.settle(10);
    assert_eq!(busy.visit(daemon_current()).step, "wait:not-idle");
    assert_eq!(
        busy.state().wait,
        "not-idle:busy",
        "the owner's view says which"
    );
    assert!(busy.typed().is_empty());
}

#[test]
fn a_tui_that_names_no_thread_is_never_relaunched_into_another() {
    // Negative control: the TUI printed nothing as it left. The older exit's
    // hint is still above the prompt, and read from the whole screen it would
    // resume another conversation.
    let rig = Rig::new("no-hint", T1, false, |w| {
        w.after_exit = shell_after(&[]);
        w.block_out = Vec::new();
        let (b2, b1) = blocks_after(0);
        w.blocks2 = b2;
        w.blocks1 = b1;
    });
    rig.settle(10);
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "failed:no-resume-hint");
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "only `/exit` was typed: {typed:#?}");
    assert!(!rig.kernel.relaunched.load(Ordering::SeqCst));
    assert_eq!(rig.state().phase, Phase::Failed("no-resume-hint".into()));
    // The owner is told why, and that the work runs on in the daemon.
    assert_eq!(
        rig.ledger_steps(),
        vec!["exit-typed", "failed:no-resume-hint"]
    );
}

#[test]
fn an_embedded_conversation_is_asked_first_and_moved_only_on_its_ready_answer() {
    let rig = Rig::new("embedded", T2, true, |_| {});
    rig.settle(10);
    // First: the notice, typed at the idle point.
    let r = rig.visit(None);
    assert_eq!(r.step, "announced:1", "{r:?}");
    let st = rig.state();
    assert_eq!(st.mode, "embedded");
    assert_eq!(st.thread, T2);
    let typed = rig.typed();
    assert_eq!(typed.len(), 1);
    assert!(typed[0].contains(cx::ANNOUNCE_HEAD), "{}", typed[0]);
    assert!(typed[0].contains(&st.marker));
    // No answer yet: it waits, and types nothing.
    rig.settle_keeping(&st);
    assert_eq!(rig.visit(None).step, "wait:awaiting-ready");
    assert_eq!(rig.typed().len(), 1);
    // The agent answers READY in its rollout.
    let answer = format!(
        r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Stopped.\n{}"}}]}}}}"#,
        st.marker
    );
    let mut text = std::fs::read_to_string(&rig.rollout).expect("rollout");
    text.push_str(&answer);
    text.push('\n');
    std::fs::write(&rig.rollout, text).expect("rollout");
    // Written a moment ago, it is still settling; a minute later it is not.
    rig.settle_keeping(&rig.state());
    assert_eq!(rig.visit(None).step, "wait:settling");
    assert_eq!(rig.typed().len(), 1);
    age(&rig.rollout);
    rig.settle_keeping(&rig.state());
    let r = rig.visit(None);
    // The continuation typed: `continued`, the harness's own turn.
    assert_eq!(r.step, "continued", "{r:?}\n{:#?}", rig.asked());
    let typed = rig.typed();
    assert_eq!(
        typed.len(),
        4,
        "notice, /exit, relaunch, continuation: {typed:#?}"
    );
    assert!(typed[1].ends_with(" /exit"));
    assert!(
        typed[2].contains(&format!("'resume' '{T2}'")),
        "{}",
        typed[2]
    );
    assert!(
        typed[3].contains("[aterm harness] Upgraded: this session was restarted on Codex 0.157.1"),
        "{}",
        typed[3]
    );
    assert!(!rig.asked().iter().any(|l| l.contains("signal")));
    assert_eq!(
        rig.ledger_steps(),
        vec!["announced:1", "exit-typed", "relaunched", "continued"]
    );
}

/// THE CODEX TWINS OF THE TYPED ACTS: the
/// notice and the continuation are typed under the harness's own hand — the
/// wire is `lease acquire … hard`, the fence's `help turn`, its screen read,
/// the `turn`, `lease release`, nothing of aterm's between the last read and
/// the `turn` — so the window's presence layer reads aterm's own hand and
/// raises no row. NEGATIVE CONTROL: a tab another driver holds is a wait
/// (`wait:held`), nothing typed, nothing given back.
#[test]
fn the_codex_notice_and_the_continuation_are_typed_under_the_harnesss_hand() {
    let held = "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1";
    let rig = Rig::new("embedded-under-hand", T2, true, |w| w.lease = held);
    rig.settle(10);
    let r = rig.visit(None);
    assert_eq!(r.step, "announced:1", "{r:?}\n{:#?}", rig.asked());
    let asked = rig.asked();
    let pos = |needle: &str| asked.iter().position(|l| l.contains(needle));
    let acquire = pos(" lease acquire ").expect("the hand taken");
    assert!(asked[acquire].ends_with(" hard"), "{}", asked[acquire]);
    let notice = pos(cx::ANNOUNCE_HEAD).expect("the notice");
    let release = pos(" lease release ").expect("the hand given back");
    let help = asked[acquire..notice]
        .iter()
        .position(|l| l.contains("help") && l.contains("turn"))
        .map(|i| i + acquire)
        .expect("the fence probes `turn` before its read");
    assert!(
        acquire < help && help < notice && notice < release,
        "{asked:#?}"
    );
    // Nothing of aterm's between the fence's last read and the turn: no
    // request that is not a read of the screen or the composer's cell.
    let read = asked[help + 1..notice]
        .iter()
        .rposition(|l| l.contains(" text") || l.contains(" screen"))
        .map(|i| i + help + 1)
        .expect("the fence's read");
    assert!(
        asked[read + 1..notice].iter().all(|l| l.contains(" cell ")),
        "only the composer's dim cell between the last read and the turn: {:#?}",
        &asked[read..=notice]
    );
    assert_eq!(release, notice + 1, "given back at once: {asked:#?}");
    // The READY answer, then the move, then the continuation: one hand from
    // the relaunch line to the carry-on, given back once at the end.
    let st = rig.state();
    let answer = format!(
        r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Stopped.\n{}"}}]}}}}"#,
        st.marker
    );
    let mut text = std::fs::read_to_string(&rig.rollout).expect("rollout");
    text.push_str(&answer);
    text.push('\n');
    std::fs::write(&rig.rollout, text).expect("rollout");
    age(&rig.rollout);
    rig.settle_keeping(&rig.state());
    let r = rig.visit(None);
    assert_eq!(r.step, "continued", "{r:?}\n{:#?}", rig.asked());
    let asked = rig.asked();
    let last_turn = asked
        .iter()
        .rposition(|l| l.contains(" turn ") && l.contains("Upgraded"))
        .expect("the continuation");
    let taken = asked[..last_turn]
        .iter()
        .rposition(|l| l.contains(" lease acquire "))
        .expect("a hand before the continuation");
    assert!(
        !asked[taken..last_turn]
            .iter()
            .any(|l| l.contains(" lease release ")),
        "the continuation is typed under a hand still held: {asked:#?}"
    );
    assert!(
        asked[last_turn..]
            .iter()
            .any(|l| l.contains(" lease release ")),
        "and given back after it: {asked:#?}"
    );
    // NEGATIVE CONTROL: another driver holds the tab.
    let rig = Rig::new("embedded-theirs", T2, true, |w| {
        w.lease = "ERR lease held holder=orchestrator expires_in_ms=5000";
    });
    rig.settle(10);
    let r = rig.visit(None);
    assert_eq!(r.step, "wait:held", "{r:?}");
    assert!(rig.typed().is_empty(), "{:#?}", rig.asked());
    assert!(!rig.asked().iter().any(|l| l.contains(" lease release ")));
}

#[test]
fn work_still_running_under_an_embedded_session_is_waited_for_ready_or_not() {
    let mut rig = Rig::new("embedded-bg", T2, true, |_| {});
    rig.kernel.background = vec!["zsh".to_string()];
    rig.settle(10);
    assert_eq!(rig.visit(None).step, "announced:1");
    let st = rig.state();
    let mut text = std::fs::read_to_string(&rig.rollout).expect("rollout");
    text.push_str(&format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{}"}}}}"#,
        st.marker
    ));
    text.push('\n');
    std::fs::write(&rig.rollout, text).expect("rollout");
    age(&rig.rollout);
    rig.settle_keeping(&st);
    assert_eq!(rig.visit(None).step, "wait:background");
    assert_eq!(
        rig.typed().len(),
        1,
        "no `/exit` while a shell runs under it"
    );
}

#[test]
fn the_owners_word_holds_or_hurries_a_codex_upgrade() {
    let rig = Rig::new("owner", T1, false, |_| {});
    rig.settle(10);
    let mut st = rig.state();
    st.request = Request::Skip("0.157.1".into());
    st.request_tab = TAB.into();
    save(&rig.opts, &key(TAB), &st);
    assert_eq!(rig.visit(daemon_current()).step, "wait:skipped");
    // `--now` waives the settling window: a screen that just moved.
    let now = Rig::new("owner-now", T1, false, |_| {});
    now.settle(9);
    let mut st = now.state();
    st.request = Request::Now;
    st.request_tab = TAB.into();
    save(&now.opts, &key(TAB), &st);
    assert_eq!(now.visit(daemon_current()).step, "done");
}

impl Rig {
    /// Keep `st` (an announced notice) as the sweep left it, its screen
    /// quiet for a minute.
    fn settle_keeping(&self, st: &St) {
        let st = St {
            seq_since_s: now_s() - 60,
            ..st.clone()
        };
        save(&self.opts, &key(TAB), &st);
    }
}

#[test]
fn only_a_tabs_foreground_codex_is_a_candidate() {
    let procs = vec![
        (100, 10, "codex".to_string()),
        (200, 20, "codex".to_string()),
        (300, 30, "zsh".to_string()),
        (400, 40, "codex".to_string()),
    ];
    // The host: the tab's foreground group's leader, when it is a codex.
    let tabs = vec![
        LiveTab {
            sid: "s-a".into(),
            fgpgid: Some(100),
        },
        LiveTab {
            sid: "s-b".into(),
            fgpgid: Some(300),
        },
        LiveTab {
            sid: "s-c".into(),
            fgpgid: None,
        },
    ];
    let found = candidates(
        Some(&tabs),
        &procs,
        |pid| Some((pid + 1, i64::from(pid), i64::from(pid))),
        |_| None,
    );
    assert_eq!(
        found,
        vec![Found {
            pid: 100,
            tab: "s-a".into(),
            claim: Some(HostClaim {
                tab: "s-a".into(),
                group: 100,
                parent: 101,
            }),
        }]
    );
    // Its environment naming another tab vetoes the claim.
    assert!(
        candidates(
            Some(&tabs),
            &procs,
            |pid| Some((1, i64::from(pid), i64::from(pid))),
            |_| Some("s-z".into())
        )
        .is_empty()
    );
    // A hand-run sweep: the environment's tab, and only a foreground leader;
    // the suspended one (400, its group not the terminal's) is no candidate.
    let env = |pid: u32| match pid {
        100 | 400 => Some("s-a".to_string()),
        200 => Some("s-b".to_string()),
        _ => None,
    };
    let found = candidates(
        None,
        &procs,
        |pid| {
            Some(match pid {
                400 => (1, 400, 100),
                p => (1, i64::from(p), i64::from(p)),
            })
        },
        env,
    );
    let tabs: Vec<(u32, &str)> = found.iter().map(|f| (f.pid, f.tab.as_str())).collect();
    assert_eq!(tabs, vec![(100, "s-a"), (200, "s-b")]);
    // Two foreground leaders claiming one tab: neither.
    let both = candidates(
        None,
        &procs,
        |pid| Some((1, i64::from(pid), i64::from(pid))),
        env,
    );
    assert_eq!(both.iter().map(|f| f.pid).collect::<Vec<_>>(), vec![200]);
}

/// A fake `codex` for the daemon's verb: it lays the managed package into the
/// daemon's releases, re-points `daemon.pid` at a new pid, disarms the
/// vendor's updater, and answers as 0.157.1 does, recording its argv and its
/// whole environment first.
fn fake_update_verb(dir: &Path, home: &Path, new_pid: u32) -> PathBuf {
    let exe = dir.join("store/codex/b/bin/codex");
    std::fs::create_dir_all(exe.parent().expect("bin")).expect("bin");
    let rel = home.join("packages/app-server-daemon/releases/local-abc");
    let script = format!(
        "/usr/bin/env > '{dir}/verb.env'\n\
         echo \"$@\" > '{dir}/verb.args'\n\
         /bin/mkdir -p '{rel}/bin'\n\
         : > '{rel}/bin/codex'\n\
         /bin/cat > '{rel}/codex-package.json' <<'EOF'\n{pkg}\nEOF\n\
         /bin/rm -f '{home}/packages/app-server-daemon/auto-update-version'\n\
         echo '{{\"pid\":{new_pid}}}' > '{home}/app-server-daemon/daemon.pid'\n\
         echo 'Replace installed daemon version 0.157.0 with CLI version 0.157.1.'\n\
         echo '{{\"status\":\"updated\",\"installedVersion\":\"0.157.1\",\"runningVersion\":\"0.157.1\"}}'",
        dir = dir.display(),
        rel = rel.display(),
        home = home.display(),
        pkg = PACKAGE.replace("0.157.0", "0.157.1"),
    );
    // A link of the one stand-in, its first exec no assessment of a new file.
    super::super::tests::sh_script(&exe, &script);
    exe
}

/// The verb inherits nothing but its stdio: a descriptor this process holds
/// without close-on-exec (a stand-in for the GUI's shader-cache files) must
/// not reach the vendor's verb, so it cannot reach the daemon the verb
/// starts, which outlives the GUI. The verb reports what it finds open before
/// it answers: its stdout is the detector's positive control, and the answer
/// being read proves the verb ran.
#[test]
fn the_daemon_update_verb_inherits_nothing_but_its_stdio() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    let dir = scratch("verb-fds");
    let null = std::fs::File::open("/dev/null").expect("open /dev/null");
    // `F_DUPFD` clears close-on-exec on the copy; the floor keeps the number
    // clear of the ones a shell takes for itself while it runs a script.
    // SAFETY: `null` is live for the call; `F_DUPFD` returns a fresh
    // descriptor or -1 and touches no memory.
    let raw = unsafe { libc::fcntl(null.as_raw_fd(), libc::F_DUPFD, 64) };
    assert!(raw >= 64, "F_DUPFD: {}", std::io::Error::last_os_error());
    // SAFETY: `raw` was just created here and nothing else owns it.
    let held = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `F_GETFD` reads one descriptor's flag word.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    assert_eq!(
        flags & libc::FD_CLOEXEC,
        0,
        "PRECONDITION: the stand-in must be inheritable, or the test proves nothing"
    );
    let exe = dir.join("codex");
    let report = dir.join("fds");
    // A link of the one stand-in, its first exec no assessment of a new file
    // (the body is read by the shell, and closed, before it runs).
    super::super::tests::sh_script(
        &exe,
        &format!(
            "r=''\n\
             [ -e /dev/fd/1 ] && r='stdout '\n\
             [ -e /dev/fd/{raw} ] && r=\"${{r}}held \"\n\
             printf '%send' \"$r\" > '{report}'\n\
             echo '{{\"status\":\"updated\",\"installedVersion\":\"0.157.1\",\
             \"runningVersion\":\"0.157.1\"}}'",
            report = report.display(),
        ),
    );

    // A long ceiling: the verb exits at once, and a first exec can wait on
    // the system's code assessment under load (the stand-in's, once a process).
    let answer = update_daemon(&exe, &[], Duration::from_secs(120));
    drop(held);
    assert_eq!(
        answer.map(|v| v.to_string()),
        Ok("0.157.1".to_string()),
        "PRECONDITION: the verb must run and answer"
    );
    assert_eq!(
        std::fs::read_to_string(&report).expect("the verb's report"),
        "stdout end",
        "fd {raw}, held without close-on-exec, reached the vendor's verb"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

const PACKAGE: &str = r#"{"layoutVersion":1,"version":"0.157.0","entrypoint":"bin/codex"}"#;
const DAEMON: u32 = 4_000_010;
const DAEMON_NEW: u32 = 4_000_011;

/// The daemon's kernel: [`DAEMON`] runs the 0.157.0 release and holds `locks`
/// (and their rollouts); after the verb, [`DAEMON_NEW`] runs the new release.
struct DaemonScript {
    home: PathBuf,
    open: Vec<PathBuf>,
    env: Vec<String>,
    /// The session leaders under the daemon: its background terminals.
    terminals: Vec<String>,
    /// The Codex processes attached to it (`None`: unreadable).
    clients: Option<Vec<u32>>,
    /// The working directories of processes, by pid (none: unreadable).
    cwds: Vec<(u32, String)>,
}

impl Kernel for DaemonScript {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        None
    }
    fn parent(&self, _: u32) -> Option<u32> {
        None
    }
    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        None
    }
}

impl CodexKernel for DaemonScript {
    fn alive(&self, pid: u32) -> bool {
        pid == DAEMON || pid == DAEMON_NEW
    }
    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        self.alive(pid).then(|| atpkg::caller_shell::ProcArgs {
            exec_path: String::new(),
            argv: [
                "codex",
                "app-server",
                "--listen",
                "unix://",
                "--managed-daemon",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            env: self.env.clone(),
        })
    }
    fn exe(&self, pid: u32) -> Option<PathBuf> {
        let rel = self.home.join("packages/app-server-daemon/releases");
        match pid {
            DAEMON => Some(rel.join("0.157.0-aarch64-apple-darwin/bin/codex")),
            DAEMON_NEW => Some(rel.join("local-abc/bin/codex")),
            _ => None,
        }
    }
    fn open_files(&self, _: u32) -> Option<Vec<PathBuf>> {
        Some(self.open.clone())
    }
    fn holders_of(&self, _: &Path) -> Option<Vec<u32>> {
        Some(Vec::new())
    }
    fn shell_has_terminal(&self, _: u32) -> bool {
        false
    }
    fn codex_children(&self, _: u32) -> Vec<(u32, Vec<String>)> {
        Vec::new()
    }
    fn background(&self, _: u32) -> Vec<String> {
        Vec::new()
    }
    fn terminals(&self, pid: u32) -> Vec<String> {
        if pid == DAEMON {
            self.terminals.clone()
        } else {
            Vec::new()
        }
    }
    fn daemon_clients(&self, _: u32) -> Option<Vec<u32>> {
        self.clients.clone()
    }
    fn cwd(&self, pid: u32) -> Option<String> {
        self.cwds
            .iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, d)| d.clone())
    }
    fn in_tab(&self, _: &mut Client, _: u32, _: &str) -> bool {
        false
    }
}

/// A home whose daemon runs 0.157.0, unpinned, holding one thread (a
/// conversation's root, its rollout opening on its `session_meta`) whose
/// rollout ends on `last`, written `age_s` ago.
fn daemon_home(name: &str, last: &str, age_s: u64) -> (ScratchDir, Opts, DaemonScript, Target) {
    let dir = scratch(name);
    let user = dir.join("user");
    let home = user.join(".codex");
    let rel = home.join("packages/app-server-daemon/releases/0.157.0-aarch64-apple-darwin");
    std::fs::create_dir_all(rel.join("bin")).expect("release");
    std::fs::write(rel.join("bin/codex"), "").expect("exe");
    std::fs::write(rel.join(cx::PACKAGE_JSON), PACKAGE).expect("pkg");
    std::fs::write(
        home.join("packages/app-server-daemon/auto-update-version"),
        "0.157.0-aarch64-apple-darwin",
    )
    .expect("marker");
    std::fs::create_dir_all(home.join("app-server-daemon")).expect("daemon dir");
    std::fs::write(
        home.join("app-server-daemon/daemon.pid"),
        format!("{{\"pid\":{DAEMON}}}"),
    )
    .expect("pid");
    let day = home.join("sessions/2026/09/25");
    std::fs::create_dir_all(&day).expect("day");
    std::fs::create_dir_all(home.join("thread-writer-locks")).expect("locks");
    let rollout = day.join(format!("rollout-2026-09-25T22-35-29-{T1}.jsonl"));
    std::fs::write(
        &rollout,
        format!("{}\n{last}\n", cx::fixtures::root_meta(T1)),
    )
    .expect("rollout");
    let when = std::time::SystemTime::now() - Duration::from_secs(age_s);
    std::fs::File::options()
        .write(true)
        .open(&rollout)
        .and_then(|f| f.set_modified(when))
        .expect("mtime");
    let home = canonical(&home);
    let lock = home.join("thread-writer-locks").join(format!("{T1}.lock"));
    let target_exe = fake_update_verb(&dir, &home, DAEMON_NEW);
    let opts = Opts {
        home: canonical(&user),
        state: dir.join("state"),
        sock: Some(dir.join("none.sock").to_string_lossy().into_owned()),
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
        // The ask-and-drain flow these tests pin (`upgrade_ask = true`).
        ask: true,
        several_logins: false,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
    };
    let script = DaemonScript {
        home: home.clone(),
        open: vec![lock, canonical(&rollout)],
        env: vec![
            format!("HOME={}", canonical(&user).display()),
            "DAEMON_ENV=the-daemons-own".to_string(),
        ],
        terminals: Vec::new(),
        clients: Some(Vec::new()),
        cwds: Vec::new(),
    };
    let target = Target {
        twin: dir.join("agents/codex"),
        exe: target_exe,
        version: v("0.157.1"),
    };
    (dir, opts, script, target)
}

/// A conversation's root on the daemon, its turn `turn`.
fn root_turn(thread: &str, turn: TurnState) -> cx::Loaded {
    cx::Loaded {
        thread: thread.to_string(),
        turn,
        lineage: cx::Lineage::Root,
    }
}

const IDLE_LAST: &str = r#"{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"1"}}"#;
const BUSY_LAST: &str = r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"2"}}"#;

#[test]
fn the_daemon_is_moved_and_pinned_by_the_vendors_verb_with_its_own_environment() {
    let (dir, opts, k, target) = daemon_home("daemon-update", IDLE_LAST, 600);
    let home = k.home.clone();
    let (r, running) = daemon_pass(&opts, &home, &target, &[], &k).expect("a daemon");
    assert_eq!(r.step, "daemon-updated", "{r:?}");
    assert_eq!(r.session, "codex-daemon");
    assert_eq!(r.pid, DAEMON_NEW);
    assert_eq!(running.running, Some(v("0.157.1")));
    assert_eq!(running.wait, "");
    assert!(
        !home
            .join("packages/app-server-daemon/auto-update-version")
            .exists()
    );
    // The verb ran with the running daemon's environment and nothing else,
    // never this process's (the window's, in production).
    let env = std::fs::read_to_string(dir.join("verb.env")).expect("env");
    assert!(env.contains("DAEMON_ENV=the-daemons-own"), "{env}");
    let own_home = std::env::var("HOME").unwrap_or_default();
    assert!(
        !env.lines().any(|l| l == format!("HOME={own_home}")),
        "{env}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("verb.args"))
            .expect("args")
            .trim(),
        "app-server daemon update --from-cli --yes"
    );
    // Once moved: current, and nothing is run again.
    let _ = std::fs::remove_file(dir.join("verb.args"));
    let (r, _) = daemon_pass(&opts, &home, &target, &[], &k).expect("a daemon");
    assert_eq!(r.step, "current");
    assert!(!dir.join("verb.args").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_busy_or_settling_thread_holds_the_daemon_where_it_is() {
    // Negative control: a thread whose turn runs, attached to no tab at all
    // (no TUI here), so only the daemon's own lock reveals it.
    let (dir, opts, k, target) = daemon_home("daemon-busy", BUSY_LAST, 600);
    let home = k.home.clone();
    let (r, running) = daemon_pass(&opts, &home, &target, &[], &k).expect("a daemon");
    assert_eq!(r.step, "wait:busy-thread");
    assert_eq!(running.running, Some(v("0.157.0")));
    assert_eq!(running.wait, "busy-thread", "what its clients carry");
    assert!(!dir.join("verb.args").exists(), "the verb never ran");
    let _ = std::fs::remove_dir_all(&dir);
    // Idle, but written a moment ago.
    let (dir, opts, k, target) = daemon_home("daemon-settling", IDLE_LAST, 2);
    let (r, _) = daemon_pass(&opts, &k.home.clone(), &target, &[], &k).expect("a daemon");
    assert_eq!(r.step, "wait:settling");
    assert!(!dir.join("verb.args").exists());
    // A dry run says what it would do and runs nothing.
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let (dir2, _, k2, target2) = daemon_home("daemon-dry", IDLE_LAST, 600);
    let (r, _) = daemon_pass(&dry, &k2.home.clone(), &target2, &[], &k2).expect("a daemon");
    assert_eq!(r.step, "would-update-daemon");
    assert!(!dir2.join("verb.args").exists());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// A kernel that answers only what [`tuis`] asks: each pid's argv and
/// environment, and the build it runs.
struct Found2 {
    exe: PathBuf,
    homes: Vec<(u32, String)>,
    name: &'static str,
}

impl Kernel for Found2 {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        None
    }
    fn parent(&self, _: u32) -> Option<u32> {
        None
    }
    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        None
    }
}

impl CodexKernel for Found2 {
    fn alive(&self, _: u32) -> bool {
        true
    }
    fn args(&self, pid: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        let home = self.homes.iter().find(|(p, _)| *p == pid)?.1.clone();
        Some(atpkg::caller_shell::ProcArgs {
            exec_path: format!("/bin/{}", self.name),
            argv: vec!["codex".to_string()],
            env: vec![format!("HOME={home}")],
        })
    }
    fn exe(&self, _: u32) -> Option<PathBuf> {
        Some(self.exe.clone())
    }
    fn open_files(&self, _: u32) -> Option<Vec<PathBuf>> {
        None
    }
    fn holders_of(&self, _: &Path) -> Option<Vec<u32>> {
        None
    }
    fn shell_has_terminal(&self, _: u32) -> bool {
        false
    }
    fn codex_children(&self, _: u32) -> Vec<(u32, Vec<String>)> {
        Vec::new()
    }
    fn background(&self, _: u32) -> Vec<String> {
        Vec::new()
    }
    fn terminals(&self, _: u32) -> Vec<String> {
        Vec::new()
    }
    fn daemon_clients(&self, _: u32) -> Option<Vec<u32>> {
        Some(Vec::new())
    }
    fn cwd(&self, _: u32) -> Option<String> {
        None
    }
    fn in_tab(&self, _: &mut Client, _: u32, _: &str) -> bool {
        false
    }
}

#[test]
fn due_distinguishes_a_current_codex_from_an_unknown_or_claude_foreground() {
    let dir = scratch("codex-due-foreground");
    let root = dir.join("store/codex/b");
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    std::fs::write(root.join("bin/codex"), "").expect("exe");
    std::fs::write(root.join(cx::PACKAGE_JSON), PACKAGE).expect("pkg");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let opts = Opts {
        home: home.clone(),
        state: dir.join("state"),
        sock: None,
        only_sid: Some(TAB.to_string()),
        dry_run: true,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
        // The ask-and-drain flow these tests pin (`upgrade_ask = true`).
        ask: true,
        several_logins: false,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
    };
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(TUI)),
    }];
    let mut kernel = Found2 {
        exe: root.join("bin/codex"),
        homes: vec![(TUI, home.to_string_lossy().into_owned())],
        name: "codex",
    };
    let target = Target {
        twin: dir.join("agents/codex"),
        exe: root.join("bin/codex"),
        version: v("0.157.1"),
    };
    let check = |k: &Found2, target: Option<Target>| {
        due_with(
            &opts,
            &tabs,
            k,
            || target,
            |_| Some((9, i64::from(TUI), i64::from(TUI))),
            |_| None,
        )
    };
    assert_eq!(check(&kernel, Some(target.clone())), Some(Due::Yes));
    assert_eq!(
        check(
            &kernel,
            Some(Target {
                version: v("0.157.0"),
                ..target.clone()
            })
        ),
        Some(Due::No),
        "a current Codex is a conclusive answer, not an unknown foreground"
    );
    assert_eq!(check(&kernel, None), Some(Due::No), "no managed target");
    kernel.name = "claude";
    assert_eq!(
        due_with(
            &opts,
            &tabs,
            &kernel,
            || panic!("Claude must not read the Codex target"),
            |_| panic!("Claude must not inspect a Codex claim"),
            |_| None,
        ),
        None,
        "a Claude foreground still needs its own roster"
    );
    kernel.name = "codex";
    kernel.homes.clear();
    assert_eq!(check(&kernel, Some(target)), None, "unreadable foreground");
    let ambiguous = [
        tabs[0].clone(),
        LiveTab {
            sid: "s-other".to_string(),
            fgpgid: Some(i64::from(TUI)),
        },
    ];
    kernel
        .homes
        .push((TUI, home.to_string_lossy().into_owned()));
    assert_eq!(
        due_with(
            &opts,
            &ambiguous,
            &kernel,
            || panic!("an ambiguous tab must not read the Codex target"),
            |_| Some((9, i64::from(TUI), i64::from(TUI))),
            |_| None,
        ),
        None,
        "a shared foreground group proves no Codex owner"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_codex_of_another_home_is_never_this_sweeps() {
    // Negative control: a sweep discovers Codex by the machine's process
    // table, so it is scoped to its own home: only a TUI started with this
    // sweep's `$HOME` is the owner's; another is said (a tab's) or passed by
    // (a hand-run sweep's).
    let dir = scratch("homes");
    let root = dir.join("store/codex/b");
    std::fs::create_dir_all(root.join("bin")).expect("bin");
    std::fs::write(root.join("bin/codex"), "").expect("exe");
    std::fs::write(root.join(cx::PACKAGE_JSON), PACKAGE).expect("pkg");
    let mine = dir.join("mine");
    std::fs::create_dir_all(&mine).expect("home");
    let opts = Opts {
        home: mine.clone(),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: true,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
        // The ask-and-drain flow these tests pin (`upgrade_ask = true`).
        ask: true,
        several_logins: false,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
    };
    let k = Found2 {
        exe: root.join("bin/codex"),
        homes: vec![
            (1, mine.to_string_lossy().into_owned()),
            (2, "/somewhere/else".to_string()),
            (3, "/somewhere/else".to_string()),
        ],
        name: "codex",
    };
    let found = |pid, tab: &str, claimed: bool| Found {
        pid,
        tab: tab.to_string(),
        claim: claimed.then(|| HostClaim {
            tab: tab.to_string(),
            group: i64::from(pid),
            parent: 9,
        }),
    };
    let (tuis, reports) = tuis(
        &opts,
        vec![
            found(1, "s-a", false),
            found(2, "s-b", true),
            found(3, "s-c", false),
        ],
        &k,
    );
    assert_eq!(tuis.iter().map(|t| t.pid).collect::<Vec<_>>(), vec![1]);
    assert_eq!(tuis[0].version, v("0.157.0"));
    assert_eq!(tuis[0].home, canonical(&mine.join(".codex")));
    // The same rule holds the owner's view: a hand-run `--status` never
    // counts another home's Codex as a holder.
    let sessions: BTreeSet<String> = ["s-b".to_string()].into_iter().map(|t| key(&t)).collect();
    assert!(holders(&mine, &sessions, None).is_empty());
    assert_eq!(
        reports
            .iter()
            .map(|r| (r.pid, r.step.as_str()))
            .collect::<Vec<_>>(),
        vec![(2, "skip:another-home")]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An in-flight daemon-mode relaunch, as an earlier attempt left it: the TUI
/// exited and its hint was read (`thread`), and the line waited (a person
/// typing at the prompt).
fn exiting(rig: &Rig, thread: &str) -> St {
    St {
        agent: Agent::Codex,
        phase: Phase::Exiting { at_s: now_s() },
        from: "0.157.0".into(),
        to: "0.157.1".into(),
        source: "managed".into(),
        pid: TUI,
        shell: SHELL,
        tab: TAB.into(),
        mode: "daemon".into(),
        thread: thread.into(),
        codex_home: rig.tui.home.to_string_lossy().into_owned(),
        argv: rig.tui.argv.clone(),
        cwd: "/w".into(),
        twin: rig.target.twin.to_string_lossy().into_owned(),
        ..St::default()
    }
}

#[test]
fn a_relaunch_that_waited_keeps_the_thread_its_first_attempt_read() {
    // The hint has scrolled away since (the person's typing moved it): the
    // thread the first attempt read still stands.
    let rig = Rig::new("retry", T1, false, |w| {
        w.after_exit = shell_after(&[]);
        w.block_out = Vec::new();
        let (b2, b1) = blocks_after(0);
        w.blocks2 = b2;
        w.blocks1 = b1;
    });
    rig.kernel.exited.store(true, Ordering::SeqCst);
    let mut st = exiting(&rig, T1);
    let mut c = connect(&rig.opts, TAB).expect("the stand-in");
    let r = relaunch(&rig.opts, blank(&rig.tui), &mut st, &mut c, &rig.kernel);
    assert_eq!(r.step, "done", "{r:?}");
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(
        typed[0].contains(&format!("'resume' '{T1}'")),
        "{}",
        typed[0]
    );
    // Negative control: a different exit at that prompt since (another Codex
    // run and quit there) names another thread; nothing is typed on it.
    let other = Rig::new("retry-other", T2, false, |_| {});
    other.kernel.exited.store(true, Ordering::SeqCst);
    let mut st = exiting(&other, T1);
    let mut c = connect(&other.opts, TAB).expect("the stand-in");
    let r = relaunch(
        &other.opts,
        blank(&other.tui),
        &mut st,
        &mut c,
        &other.kernel,
    );
    assert_eq!(r.step, "failed:hint-mismatch");
    assert!(other.typed().is_empty());
}

// ---------------------------------------------------------------- daemon-mode tabs
//
// Each test below pins a behaviour of the daemon-mode lane, with a negative
// control beside it.

/// The daemon home of [`daemon_home`], serving one daemon-mode TUI (the
/// stand-in tab of a [`Rig`], attached to the daemon) and `world` for that
/// tab. The Rig goes with the returned tuple (its socket is the tab's).
fn served(
    name: &str,
    last: &str,
    world: impl FnOnce(&mut World),
) -> (Rig, ScratchDir, Opts, DaemonScript, Target, Tui) {
    let (dir, mut opts, mut k, target) = daemon_home(name, last, 600);
    let rig = Rig::new(&format!("{name}-tab"), T1, false, world);
    opts.sock = rig.opts.sock.clone();
    k.clients = Some(vec![TUI]);
    let tui = Tui {
        home: k.home.clone(),
        ..rig.tui.clone()
    };
    (rig, dir, opts, k, target, tui)
}

/// The tab's pending Codex state carrying the owner's `request`, as `aterm
/// harness upgrade <tab> --now|--defer|--skip` writes it.
fn owner_word(opts: &Opts, request: Request) {
    let now = now_s();
    save(
        opts,
        &key(TAB),
        &St {
            agent: Agent::Codex,
            from: "0.157.0".into(),
            to: "0.157.1".into(),
            source: "managed".into(),
            salt: now - 60,
            pending_since: now - 60,
            seq_since_s: now - 60,
            last_seq: 10,
            notice_pid: TUI,
            tab: TAB.into(),
            request,
            request_tab: TAB.into(),
            request_at: now,
            ..St::default()
        },
    );
}

/// Codex's idle screen with a person's input to the tab a second ago.
/// A person gave the tab input a minute ago: within `human_grace_s`, not
/// within the ladder's KEYS_GAP_S.
fn attended(w: &mut World) {
    w.before = codex_idle(10).replace(r#""human_ms":null"#, r#""human_ms":60000"#);
}

/// A person typed into the tab a second ago: within KEYS_GAP_S, which no rung
/// and no word waives.
fn typing(w: &mut World) {
    w.before = codex_idle(10).replace(r#""human_ms":null"#, r#""human_ms":1000"#);
}

#[test]
fn the_owners_skip_or_defer_on_a_codex_tab_holds_its_daemon_too() {
    // A `--skip` or deferral on the tab holds its daemon too: never restarted
    // onto the skipped build under it.
    for (request, held) in [
        (Request::Skip("0.157.1".into()), true),
        (Request::DeferUntil(now_s() + 3_600), true),
        // A skip of another build, and a deferral that ran out, hold nothing.
        (Request::Skip("0.157.2".into()), false),
        (Request::DeferUntil(now_s() - 1), false),
    ] {
        let (rig, dir, opts, k, target, tui) = served("own", IDLE_LAST, |_| {});
        owner_word(&opts, request.clone());
        let (r, view) = daemon_pass(
            &opts,
            &k.home.clone(),
            &target,
            std::slice::from_ref(&tui),
            &k,
        )
        .expect("a daemon");
        if held {
            assert_eq!(r.step, "wait:owner-held", "{request:?}");
            assert!(!dir.join("verb.args").exists(), "the verb never ran");
            // The tab the word is on waits on its own word.
            let c = visit(&opts, &tui, &target, Some(view), &rig.kernel);
            assert!(
                c.step == "wait:skipped" || c.step == "wait:deferred",
                "{request:?}: {c:?}"
            );
            assert!(rig.typed().is_empty());
        } else {
            assert_eq!(r.step, "daemon-updated", "{request:?}");
        }
        drop(rig);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_codex_attached_to_the_daemon_that_the_sweep_does_not_list_holds_it() {
    // The window's roster holds no Codex tab, and a Codex in another window
    // (or a pane, or one whose build could not be read) is attached to the
    // shared daemon. Nothing of its tab was asked.
    let roster = [LiveTab {
        sid: "s-0ther0ther0ther0ther".into(),
        fgpgid: Some(77),
    }];
    let run = |clients: Option<Vec<u32>>| {
        let (dir, opts, mut k, target) = daemon_home("unseen", IDLE_LAST, 600);
        k.clients = clients;
        let mut reports = Vec::new();
        sweep_with(
            &opts,
            Some(&roster),
            Vec::new(),
            Some(&target),
            &k,
            &mut reports,
        );
        let ran = dir.join("verb.args").exists();
        let _ = std::fs::remove_dir_all(&dir);
        let steps: Vec<(String, String)> =
            reports.into_iter().map(|r| (r.session, r.step)).collect();
        (steps, ran)
    };
    let daemon = |step: &str| vec![("codex-daemon".to_string(), step.to_string())];
    assert_eq!(run(Some(vec![TUI])), (daemon("wait:unseen-client"), false));
    // Unreadable is never "none attached".
    assert_eq!(run(None), (daemon("wait:clients-unreadable"), false));
    // Negative control: a daemon nobody is attached to moves.
    assert_eq!(run(Some(Vec::new())), (daemon("daemon-updated"), true));
}

#[test]
fn the_owners_now_on_a_daemon_mode_tab_waives_its_own_attended_guard_at_the_daemon() {
    // Control: a person at the tab and no word — the daemon waits, and the
    // tab's client says what for.
    let (rig, dir, opts, k, target, tui) = served("att", IDLE_LAST, attended);
    owner_word(&opts, Request::None);
    let (r, view) = daemon_pass(
        &opts,
        &k.home.clone(),
        &target,
        std::slice::from_ref(&tui),
        &k,
    )
    .expect("a daemon");
    assert_eq!(r.step, "wait:attended");
    let c = visit(&opts, &tui, &target, Some(view), &rig.kernel);
    assert_eq!(c.step, "wait:daemon-first:attended");
    assert!(rig.typed().is_empty());
    drop(rig);
    let _ = std::fs::remove_dir_all(&dir);
    // The owner's `--now` on that tab: the ladder's last rung, where a person
    // holds it only by a keystroke within KEYS_GAP_S. The daemon moves, and
    // the client after it, in one sweep.
    let (rig, dir, opts, k, target, tui) = served("now", IDLE_LAST, attended);
    owner_word(&opts, Request::Now);
    let (r, view) = daemon_pass(
        &opts,
        &k.home.clone(),
        &target,
        std::slice::from_ref(&tui),
        &k,
    )
    .expect("a daemon");
    assert_eq!(r.step, "daemon-updated");
    // One kernel reads the tab and the daemon, as the sweep's does: the
    // `/exit`'s last look reads the daemon's threads again.
    let both = Both {
        tab: &rig.kernel,
        daemon: &k,
    };
    let c = visit(&opts, &tui, &target, Some(view), &both);
    assert_eq!(c.step, "done", "{:#?}", rig.asked());
    drop(rig);
    let _ = std::fs::remove_dir_all(&dir);
    // Negative control: a keystroke a second ago holds even the owner's
    // `--now`, at the daemon and at the client.
    let (rig, dir, opts, k, target, tui) = served("now-typing", IDLE_LAST, typing);
    owner_word(&opts, Request::Now);
    let (r, _) = daemon_pass(
        &opts,
        &k.home.clone(),
        &target,
        std::slice::from_ref(&tui),
        &k,
    )
    .expect("a daemon");
    assert_eq!(r.step, "wait:attended");
    assert!(!dir.join("verb.args").exists(), "the verb never ran");
    assert!(rig.typed().is_empty());
    drop(rig);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_the_daemon_waits_on_reaches_its_clients_row_and_words() {
    // A detached thread's turn runs in the daemon (the rollout ends on
    // `task_started`): the daemon waits `busy-thread`, and the tab's row says
    // so.
    let (rig, dir, opts, k, target, tui) = served("rows", BUSY_LAST, |_| {});
    owner_word(&opts, Request::None);
    let (r, view) = daemon_pass(
        &opts,
        &k.home.clone(),
        &target,
        std::slice::from_ref(&tui),
        &k,
    )
    .expect("a daemon");
    assert_eq!(r.step, "wait:busy-thread");
    let c = visit(&opts, &tui, &target, Some(view), &rig.kernel);
    assert_eq!(c.step, "wait:daemon-first:busy-thread");
    let rows = super::super::rows(&opts);
    let row = rows
        .iter()
        .find(|r| r.session == key(TAB))
        .expect("the tab's row");
    assert_eq!(row.wait, "daemon-first:busy-thread");
    let later = now_s() + 7 * 3_600;
    let words = row.stall_words(later).expect("overdue");
    assert!(words.contains("codex agents"), "{words}");
    assert_eq!(row.remedy(later), Some(super::super::Remedy::Waits));
    drop(rig);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_terminal_under_the_daemon_holds_it_where_it_is() {
    // `1 background terminal running`, its thread idle (`task_complete`): an
    // update would restart the daemon and end it.
    let (dir, opts, mut k, target) = daemon_home("term", IDLE_LAST, 600);
    k.terminals = vec!["sleep".into()];
    let home = k.home.clone();
    let (r, view) = daemon_pass(&opts, &home, &target, &[], &k).expect("a daemon");
    assert_eq!(r.step, "wait:background-terminal");
    assert_eq!(view.wait, "background-terminal");
    assert!(!dir.join("verb.args").exists(), "the verb never ran");
    // Negative control: once it ends, the daemon moves.
    k.terminals.clear();
    let (r, _) = daemon_pass(&opts, &home, &target, &[], &k).expect("a daemon");
    assert_eq!(r.step, "daemon-updated");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_served_tabs_status_line_backs_the_kernels_count_at_the_daemon() {
    // The kernel reads no session leader (the count is the proof, this the
    // backstop): the tab's own status line says a background terminal of its
    // thread runs in this daemon.
    let footer = |w: &mut World| {
        w.before = screen_json(
            &[
                "› start bg please",
                "  1 background terminal running · /ps to view · /stop to close",
                "",
                "› Ask Codex to do anything",
                "",
                "  GPT-6-Astra default · /w",
            ],
            (3, 2),
            10,
        );
    };
    let (rig, dir, opts, k, target, tui) = served("footer", IDLE_LAST, footer);
    owner_word(&opts, Request::Now);
    let (r, _) = daemon_pass(
        &opts,
        &k.home.clone(),
        &target,
        std::slice::from_ref(&tui),
        &k,
    )
    .expect("a daemon");
    assert_eq!(r.step, "wait:background-terminal", "`--now` waives no work");
    assert!(!dir.join("verb.args").exists());
    drop(rig);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An embedded conversation announced, its READY answer in the rollout and
/// settled: every gate but what runs under it is open.
fn embedded_ready(name: &str, world: impl FnOnce(&mut World)) -> Rig {
    let rig = Rig::new(name, T2, true, world);
    rig.settle(10);
    assert_eq!(rig.visit(None).step, "announced:1");
    let st = rig.state();
    let mut text = std::fs::read_to_string(&rig.rollout).expect("rollout");
    text.push_str(&format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{}"}}}}"#,
        st.marker
    ));
    text.push('\n');
    std::fs::write(&rig.rollout, text).expect("rollout");
    age(&rig.rollout);
    rig.settle_keeping(&st);
    rig
}

#[test]
fn a_background_terminal_under_an_embedded_session_holds_its_exit_ready_or_not() {
    // `--no-daemon`, `sleep 7772` directly under the TUI (no shell between, so
    // `background` never sees it); READY answered, and the `/exit` would end
    // it.
    let mut rig = embedded_ready("emb-term", |_| {});
    rig.kernel.terminals = vec!["sleep".into()];
    assert_eq!(rig.visit(None).step, "wait:background-terminal");
    assert_eq!(rig.typed().len(), 1, "no `/exit` while it runs");
    // Codex's own status line holds it too, the kernel's count aside.
    let footer = embedded_ready("emb-foot", |w| {
        w.before = screen_json(
            &[
                "› start bg please",
                "• The server runs in the background.",
                "  1 background terminal running · /ps to view · /stop to close",
                "",
                "› Ask Codex to do anything",
                "",
                "  GPT-6-Astra default · /w",
            ],
            (4, 2),
            10,
        );
    });
    assert_eq!(footer.visit(None).step, "wait:background-terminal");
    assert_eq!(footer.typed().len(), 1);
    // Negative control: nothing runs under it; it moves, and is carried on.
    rig.kernel.terminals.clear();
    rig.settle_keeping(&rig.state());
    assert_eq!(rig.visit(None).step, "continued");
}

/// A terminal that never ends does not hold the upgrade for good: a READY
/// answer outlived by a background terminal for a whole re-ask window is
/// asked about again, and past the last ask the upgrade gives up, the Claude
/// lane's rule.
#[test]
fn a_background_terminal_that_outlives_the_answer_is_asked_again_then_given_up() {
    let mut rig = embedded_ready("emb-reask", |_| {});
    rig.kernel.terminals = vec!["sleep".into()];
    assert_eq!(rig.visit(None).step, "wait:background-terminal");
    assert_eq!(
        rig.typed().len(),
        1,
        "inside the window: waited on, not asked"
    );
    let backdated = |rig: &Rig, asks: u32| {
        let past = now_s() - upgrade::REASK_S - 5;
        let st = St {
            phase: Phase::Announced { at_s: past, asks },
            ready_since: past,
            ..rig.state()
        };
        rig.settle_keeping(&st);
    };
    backdated(&rig, 1);
    assert_eq!(
        rig.visit(None).step,
        "announced:2",
        "asked again, naming it"
    );
    assert_eq!(
        rig.typed().len(),
        2,
        "a second notice, and still no `/exit`"
    );
    backdated(&rig, upgrade::MAX_ASKS);
    assert_eq!(rig.visit(None).step, "gave-up");
    assert!(
        matches!(rig.state().phase, Phase::Failed(ref why) if why == upgrade::GAVE_UP),
        "given up, nothing ended: {:?}",
        rig.state().phase
    );
}

/// EACH CODEX NOTICE IS ONE ASK ON THE PROCESS'S RECORD (`St::notices`, the
/// owner's "Asked N times"): an embedded notice counts one, and the count
/// rides a retarget of the same process onto a newer build, whose next
/// notice counts one more.
#[test]
fn each_codex_notice_counts_one_ask_on_the_record() {
    let mut rig = Rig::new("codex-asks", T2, true, |_| {});
    rig.settle(10);
    assert_eq!(rig.visit(None).step, "announced:1");
    assert_eq!(rig.state().notices, 1);
    rig.settle_keeping(&announced_ago(&rig, upgrade::REASK_S + 5, 1));
    rig.target.version = v("0.157.2");
    assert_eq!(rig.visit(None).step, "announced:2");
    let st = rig.state();
    assert_eq!((st.to.as_str(), st.notices), ("0.157.2", 2), "{st:?}");
}

/// No stop is for good in the Codex lane either: an embedded conversation's
/// round gives up over a background terminal that outlives its READY; it
/// rests (`wait:failed:unanswered`, looked at again, nothing typed), and once
/// it has rested `RETRY_S` a new round starts (`rearmed:unanswered`, on the
/// ledger, nothing typed at that look), whose first notice the next look
/// types, with a READY marker of its own. Negative control: the owner's
/// `--skip` of the build holds the rested round however long it rests and
/// says `wait:skipped` (the host's last word, as in the Claude lane), and a
/// `--defer` not run out holds it too, looked at again (`wait:deferred`).
#[test]
fn a_codex_round_that_gave_up_rests_then_a_new_one_asks_again() {
    let mut rig = embedded_ready("emb-rearm", |_| {});
    rig.kernel.terminals = vec!["sleep".into()];
    let past = now_s() - upgrade::REASK_S - 5;
    rig.settle_keeping(&St {
        phase: Phase::Announced {
            at_s: past,
            asks: upgrade::MAX_ASKS,
        },
        ready_since: past,
        ..rig.state()
    });
    assert_eq!(rig.visit(None).step, "gave-up");
    let old_marker = rig.state().marker;
    let resting = rig.visit(None);
    assert_eq!(resting.step, "wait:failed:unanswered");
    assert!(matches!(
        super::super::after(&resting.step, 9),
        super::super::After::Later(_)
    ));
    let typed = rig.typed().len();
    let rested = St {
        failed_at: now_s() - upgrade::RETRY_S,
        ..rig.state()
    };
    // Held by the owner's word, the rested round says so as the Claude lane
    // does: `wait:skipped` is the host's last word (a newer build or the
    // owner's next word wakes it), never a `wait:failed:<why>` looked at every
    // ten minutes for ever; a deferral is looked at again until it runs out.
    for (request, word, last) in [
        (Request::Skip("0.157.1".into()), "wait:skipped", true),
        (Request::DeferUntil(now_s() + 3_600), "wait:deferred", false),
    ] {
        rig.settle_keeping(&St {
            request,
            request_tab: TAB.into(),
            ..rested.clone()
        });
        let held = rig.visit(None);
        assert_eq!(held.step, word);
        assert_eq!(
            super::super::after(&held.step, 9) == super::super::After::Finished,
            last,
            "{word}"
        );
        assert_eq!(rig.state().phase, rested.phase, "{word}: not re-armed");
    }
    rig.settle_keeping(&rested);
    assert_eq!(rig.visit(None).step, "rearmed:unanswered");
    assert_eq!(rig.typed().len(), typed, "a new round types nothing itself");
    assert_eq!(rig.state().phase, Phase::Pending);
    rig.settle_keeping(&rig.state());
    assert_eq!(rig.visit(None).step, "announced:1");
    assert_eq!(rig.typed().len(), typed + 1, "its first notice");
    let marker = rig.state().marker;
    assert!(!marker.is_empty() && marker != old_marker, "{marker}");
    assert!(
        rig.ledger_steps().iter().any(|s| s == "rearmed:unanswered"),
        "{:?}",
        rig.ledger_steps()
    );
}

#[test]
fn a_two_row_prompt_after_a_wiped_block_list_still_reads_the_tuis_own_hint() {
    // An inline Codex (`--no-alt-screen`), a `%~` / `%#` prompt, and a daemon
    // update whose reconnect wiped the tab's blocks (`{"blocks":[]}`): after
    // `/exit` only the entering prompt is marked, two rows (`prompt:98,
    // cmd:99`), and the hint above its first row must still be read.
    let entering = r#"{"blocks":[{"id":1,"state":"entering","exit":null,"prompt":98,"cmd":99,"cmdcol":2,"out":null,"end":null,"cwd":"/w","cmdline":""}]}"#;
    let rig = Rig::new("two-row", T1, false, |w| {
        w.after_exit = screen_json(
            &[
                "Reconnect: codex resume 01a0dc99-0000-7000-8000-000000000099",
                "% codex --no-alt-screen",
                "• Reconnected. No input was resent.",
                "Disconnected from this task. Any running work continues.",
                &format!("Reconnect: codex resume {T1}"),
                "Stop the current turn: run codex agents, select this task, and press x.",
                "Token usage so far: total=2 input=1 output=1",
                "/tmp/w",
                "% ",
            ],
            (8, 2),
            900,
        );
        w.blocks2 = entering.to_string();
        w.blocks1 = entering.to_string();
        w.block_out = Vec::new();
    });
    rig.settle(10);
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "done", "{:#?}", rig.asked());
    let typed = rig.typed();
    assert!(
        typed[1].contains(&format!("'resume' '{T1}'")),
        "{}",
        typed[1]
    );
}

#[test]
fn a_daemon_client_is_not_exited_where_no_marks_tell_its_hint_from_the_prompt() {
    let rig = Rig::new("no-marks", T1, false, |w| {
        w.status = "OK schema=1 hold=0 hand=- integration=off".to_string();
        w.blocks1 = r#"{"blocks":[]}"#.to_string();
        w.blocks2 = r#"{"blocks":[]}"#.to_string();
    });
    rig.settle(10);
    assert_eq!(
        rig.visit(daemon_current()).step,
        "wait:no-shell-integration"
    );
    assert!(rig.typed().is_empty(), "no `/exit` it could not follow");
    // An embedded session's thread is the kernel's: no marks needed.
    let emb = embedded_ready("no-marks-emb", |w| {
        w.status = "OK schema=1 hold=0 hand=- integration=off".to_string();
        w.blocks1 = r#"{"blocks":[]}"#.to_string();
        w.blocks2 = r#"{"blocks":[]}"#.to_string();
    });
    assert_eq!(emb.visit(None).step, "continued");
}

/// The key presses the stand-in saw.
fn keys(rig: &Rig) -> Vec<String> {
    rig.asked()
        .into_iter()
        .filter(|l| l.contains(" key "))
        .collect()
}

/// A Codex notice naming a long command is guarded on its closing word: the
/// clause naming what runs goes before the closing sentence
/// (`upgrade::with_running`, as Claude's does), so the notice ends `…ask
/// again later.` whatever runs, the agent's command quoted whole, and its
/// guarded Enter (the text's last word on the cursor's row) never waits on a
/// long path that wraps inside itself. Here Codex's exec shell stays alive
/// for a pipeline whose last word is a long log path: a lone command runs
/// with no shell between (the unified exec), and a `tail` or `sleep` is no
/// process the notice names. Negative control, on the host's own engine over
/// the wrap model (`last_wrapped_row`): the clause at the notice's end has
/// its guard miss the cursor's row at 80 columns.
#[test]
fn a_codex_notice_naming_a_long_command_is_guarded_on_its_closing_word() {
    // 157 characters: under `HELD_COMMAND_CHARS`, quoted whole; the path
    // alone is longer than an 80-column row.
    let path = "/w/target/trust-scope/debug/build/aterm-grid-4f3c2a1b9d8e7f60/out/trust-verify-report.jsonl.log";
    let held = vec![upgrade::Held {
        pid: 81_234,
        name: "zsh".to_string(),
        age_s: 8_000,
        command: format!("/bin/zsh -lc targo --unverified test -p aterm-grid 2>&1 | tee {path}"),
    }];
    let clause = upgrade::running_clause(&held);
    let mut rig = Rig::new("notice-held", T2, true, |_| {});
    rig.kernel.held = held;
    rig.settle(10);
    assert_eq!(rig.visit(None).step, "announced:1");
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(
        typed[0].contains(" submit=guarded:^[›»\\s]\\s.*later\\.\\s*$ "),
        "{}",
        typed[0]
    );
    let text = typed[0]
        .split_once(&format!(" {TURN_WAIT} "))
        .map(|(_, t)| t)
        .expect("the notice");
    assert!(
        text.ends_with(&format!("{} {}", clause.trim_end(), upgrade::NOTICE_CLOSE)),
        "{text}"
    );
    assert!(text.contains(path), "the agent's command quoted whole");
    // Negative control: the clause at the end.
    let notice = cx::prepare_prompt(&v("0.157.0"), &v("0.157.1"), &rig.state().marker);
    let old = format!("{notice}{clause}");
    let wrapped = |t: &str| super::super::tests::last_wrapped_row(&format!("› {t}"), 80);
    let guard = |t: &str| aterm_observe::row_matcher(&guard_for(t)).expect("the host compiles it");
    assert!(!guard(&old).matches(&wrapped(&old)), "{}", wrapped(&old));
    assert!(guard(text).matches(&wrapped(text)), "{}", wrapped(text));
}

/// THE CODEX NOTICE STAYS A PASTE CODEX DRAWS AS TEXT (measured 2026-09-30
/// on Codex 0.159.2: 1000 characters are drawn as they are, 1001 as
/// `[Pasted Content 1001 chars]`, a placeholder the guarded Enter and
/// `left_standing` never find, left in the person's composer as their
/// draft). Eight shells under Codex, each an hour older than the next and
/// each running a long loop: the notice as typed is within
/// [`cx::PASTE_SHOWN_MAX`], still ends in the closing sentence, and still
/// names the oldest. NEGATIVE CONTROL: the same held set through the
/// unbounded clause (`upgrade::running_clause`) is past it.
#[test]
fn a_codex_notice_naming_many_long_commands_stays_under_the_paste_placeholder() {
    let held: Vec<upgrade::Held> = (0..8u32)
        .map(|i| upgrade::Held {
            pid: 81_300 + i,
            name: "zsh".to_string(),
            age_s: 3_600 * u64::from(i + 1),
            command: format!(
                "/bin/zsh -lc while true; do targo --unverified test -p aterm-grid-{i} \
                 --lib -- --test-threads 1 2>&1 | tee -a build/logs/run-{i}.log; sleep 60; done"
            ),
        })
        .collect();
    let oldest = 81_307;
    let mut rig = Rig::new("notice-bounded", T2, true, |_| {});
    rig.kernel.held = held.clone();
    rig.settle(10);
    assert_eq!(rig.visit(None).step, "announced:1");
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "{typed:#?}");
    let text = typed[0]
        .split_once(&format!(" {TURN_WAIT} "))
        .map(|(_, t)| t)
        .expect("the notice");
    assert!(
        text.chars().count() <= cx::PASTE_SHOWN_MAX,
        "{} chars: {text}",
        text.chars().count()
    );
    assert!(text.ends_with(upgrade::NOTICE_CLOSE), "{text}");
    assert!(text.contains(&format!("pid {oldest} (zsh, 8h")), "{text}");
    // NEGATIVE CONTROL: the whole clause, unbounded.
    let notice = cx::prepare_prompt(&v("0.157.0"), &v("0.157.1"), &rig.state().marker);
    let unbounded = upgrade::with_running(&notice, &upgrade::running_clause(&held));
    assert!(
        unbounded.chars().count() > cx::PASTE_SHOWN_MAX,
        "{} chars",
        unbounded.chars().count()
    );
}

/// A MISSED GUARD ON `/exit` IS SAID AND LEFT STANDING: the guarded Enter
/// missed and `/exit` sits in the composer. Said once (`left-typed:exit`) and
/// recorded, so no look reads it as the person's draft — and NEVER TAKEN
/// BACK: a fenced `ctrl+u` would also clear a person's key the host had
/// counted and Codex had not drawn yet. No key is pressed; every later look waits on
/// it, a person's hold (`wait:left-typed:unsent`) — the watch's dry run
/// reads it alike, saving and ledgering nothing. NEGATIVE CONTROL: the
/// person clears it — the record goes, and the act waits out its back-off,
/// typed once.
#[test]
fn a_missed_guard_on_exit_is_said_and_left_standing() {
    let rig = Rig::new("gm-exit", T1, false, |w| {
        w.miss = Some("OK 0 turn skipped reason=guard submitted=0 seq=11 id=1");
    });
    rig.settle(10);
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "left-typed:exit");
    let st = rig.state();
    assert_eq!(st.phase, Phase::Pending, "nothing is in flight");
    assert_eq!(st.left_typed, CODEX_EXIT, "recorded: never read as a draft");
    assert_eq!(
        rig.left.lock().expect("left").as_deref(),
        Some(CODEX_EXIT),
        "left standing"
    );
    assert_eq!(rig.ledger_steps(), vec!["left-typed:exit"]);
    assert!(keys(&rig).is_empty(), "nothing pressed: {:#?}", keys(&rig));
    // The watch's dry run reads what a look reads, and keeps nothing.
    let before = rig.state();
    let dry = Opts {
        dry_run: true,
        ..rig.opts.clone()
    };
    let r = visit(&dry, &rig.tui, &rig.target, daemon_current(), &rig.kernel);
    assert_eq!(r.step, "wait:left-typed:unsent");
    assert_eq!(rig.state(), before, "the record as it was");
    // A look: a person's hold, pressing nothing.
    assert_eq!(rig.visit(daemon_current()).step, "wait:left-typed:unsent");
    assert!(keys(&rig).is_empty(), "nothing pressed: {:#?}", keys(&rig));
    assert_eq!(rig.ledger_steps(), vec!["left-typed:exit"]);
    // Negative control: the person clears it.
    *rig.left.lock().expect("left") = None;
    assert_eq!(rig.visit(daemon_current()).step, "wait:left-typed-backoff");
    assert_eq!(rig.state().left_typed, "", "the record goes with the text");
    assert_eq!(rig.typed().len(), 1);
}

#[test]
fn an_enter_that_did_not_submit_the_notice_is_said_and_left_standing() {
    // The embedded notice: the Enter was written and not seen (`submitted=0
    // pressed=1`), the notice still in the composer: said, recorded, and
    // never taken back.
    let rig = Rig::new("gm-notice", T2, true, |w| {
        w.miss = Some("OK 0 turn submitted=0 pressed=1 status=timeout seq=11 id=1");
    });
    rig.settle(10);
    let r = rig.visit(None);
    assert_eq!(r.step, "left-typed:notice");
    assert_eq!(rig.state().phase, Phase::Pending, "never announced");
    let left = rig
        .left
        .lock()
        .expect("left")
        .clone()
        .expect("left standing");
    assert!(left.starts_with(cx::ANNOUNCE_HEAD), "{left}");
    assert_eq!(rig.state().left_typed, left);
    assert!(keys(&rig).is_empty(), "nothing pressed: {:#?}", keys(&rig));
    assert_eq!(rig.ledger_steps(), vec!["left-typed:notice"]);
    // Negative control: a person's text in the composer is theirs; the lane's
    // record goes, and the look waits on their draft.
    let mine = Rig::new("gm-mine", T2, true, |_| {});
    mine.settle(10);
    if let Ok(mut l) = mine.left.lock() {
        *l = Some("my own words".to_string());
    }
    let mut st = mine.state();
    st.left_typed =
        cx::prepare_prompt(&v("0.157.0"), &v("0.157.1"), "ATERM-UPGRADE-READY-0badf00d");
    save(&mine.opts, &key(TAB), &st);
    assert_eq!(mine.visit(None).step, "wait:draft");
    assert!(
        keys(&mine).is_empty(),
        "nothing pressed over a person's draft"
    );
    assert_eq!(
        mine.left.lock().expect("left").as_deref(),
        Some("my own words")
    );
    assert_eq!(
        mine.state().left_typed,
        "",
        "the lane's text is gone from it"
    );
}

/// A CODEX STALE EXIT NAMES WHAT HELD ITS LINE, as Claude Code's does
/// (`a_stale_exit_names_the_wait_that_held_its_line`): the relaunch line's
/// wait in a person's words; none of the lane's other waits, and no wait,
/// give the bare line. An end no look saw is only the `/exit` typed.
#[test]
fn a_codex_stale_exit_names_the_wait_that_held_its_line() {
    let rig = Rig::new("stale-words", T1, false, |_| {});
    let now = now_s();
    let st = St {
        phase: Phase::Exiting { at_s: now - 400 },
        exited_at: now - 390,
        wait: "typing".into(),
        ..exiting(&rig, T1)
    };
    let line = |st: &St| expired(st, now, &rig.kernel);
    assert_eq!(
        line(&st),
        Some((
            "stale-exit",
            "Codex ended over 5 minutes ago and was not relaunched: the prompt it returned to \
             does not read as empty"
                .to_string()
        ))
    );
    for wait in ["", "daemon-turn"] {
        let other = St {
            wait: wait.into(),
            ..st.clone()
        };
        assert_eq!(
            line(&other).map(|(_, d)| d).as_deref(),
            Some("Codex ended over 5 minutes ago and was not relaunched"),
            "{wait:?}"
        );
    }
    let unseen = St {
        exited_at: 0,
        wait: String::new(),
        ..st.clone()
    };
    assert_eq!(
        line(&unseen).map(|(_, d)| d).as_deref(),
        Some("Codex was asked to end over 5 minutes ago and was not relaunched")
    );
    // The orphan pass stamps a first sighting now: the `/exit` is what is
    // over the bound, never the end.
    let just_seen = St {
        exited_at: now,
        wait: String::new(),
        ..st
    };
    assert_eq!(
        line(&just_seen).map(|(_, d)| d).as_deref(),
        Some("Codex was asked to end over 5 minutes ago and was not relaunched")
    );
}

#[test]
fn an_exit_the_tui_outlived_is_never_waited_on_again() {
    // An old `/exit` the TUI outlived is not waited on under the sweep lock at
    // every sweep: it expires.
    let rig = Rig::new("outlived", T1, false, |_| {});
    let mut st = exiting(&rig, "");
    st.phase = Phase::Exiting {
        at_s: now_s() - 3_600,
    };
    save(&rig.opts, &key(TAB), &st);
    let t0 = std::time::Instant::now();
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "failed:stale-exit");
    assert!(t0.elapsed() < EXIT_WAIT, "{:?}", t0.elapsed());
    assert_eq!(rig.state().phase, Phase::Failed("stale-exit".into()));
    // Two minutes old: not waited on either, and a `/exit` left in its
    // composer stands, recorded — never taken back, nothing ledgered.
    let fresh = Rig::new("outlived-2m", T1, false, |_| {});
    if let Ok(mut l) = fresh.left.lock() {
        *l = Some(CODEX_EXIT.to_string());
    }
    let mut st = exiting(&fresh, "");
    st.phase = Phase::Exiting {
        at_s: now_s() - 120,
    };
    save(&fresh.opts, &key(TAB), &st);
    let t0 = std::time::Instant::now();
    assert_eq!(fresh.visit(daemon_current()).step, "wait:exit-not-taken");
    assert!(t0.elapsed() < EXIT_WAIT);
    assert_eq!(
        fresh.left.lock().expect("left").as_deref(),
        Some(CODEX_EXIT)
    );
    let st = fresh.state();
    assert!(matches!(st.phase, Phase::Exiting { .. }));
    assert_eq!(st.left_typed, CODEX_EXIT);
    assert!(
        fresh.ledger_steps().is_empty(),
        "{:?}",
        fresh.ledger_steps()
    );
    assert!(
        keys(&fresh).is_empty(),
        "nothing pressed: {:#?}",
        keys(&fresh)
    );
}

/// The scripted kernel, reading the tab's record the moment the relaunch
/// first asks whether the shell has its terminal back: the TUI seen gone,
/// its exit not read yet.
struct InFlight<'a>(&'a Script, &'a Opts, Mutex<Option<St>>);

impl Kernel for InFlight<'_> {
    fn job(&self, p: u32) -> Option<(Job, u32)> {
        self.0.job(p)
    }
    fn parent(&self, p: u32) -> Option<u32> {
        self.0.parent(p)
    }
    fn terminal(&self, p: u32) -> Option<(u32, String)> {
        self.0.terminal(p)
    }
}

impl CodexKernel for InFlight<'_> {
    fn alive(&self, p: u32) -> bool {
        self.0.alive(p)
    }
    fn args(&self, p: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        self.0.args(p)
    }
    fn exe(&self, p: u32) -> Option<PathBuf> {
        self.0.exe(p)
    }
    fn open_files(&self, p: u32) -> Option<Vec<PathBuf>> {
        self.0.open_files(p)
    }
    fn holders_of(&self, f: &Path) -> Option<Vec<u32>> {
        self.0.holders_of(f)
    }
    fn shell_has_terminal(&self, s: u32) -> bool {
        if let Ok(mut seen) = self.2.lock()
            && seen.is_none()
        {
            *seen = load(self.1, &key(TAB));
        }
        self.0.shell_has_terminal(s)
    }
    fn codex_children(&self, s: u32) -> Vec<(u32, Vec<String>)> {
        self.0.codex_children(s)
    }
    fn background(&self, p: u32) -> Vec<String> {
        self.0.background(p)
    }
    fn terminals(&self, p: u32) -> Vec<String> {
        self.0.terminals(p)
    }
    fn daemon_clients(&self, d: u32) -> Option<Vec<u32>> {
        self.0.daemon_clients(d)
    }
    fn cwd(&self, p: u32) -> Option<String> {
        self.0.cwd(p)
    }
    fn in_tab(&self, c: &mut Client, p: u32, t: &str) -> bool {
        self.0.in_tab(c, p, t)
    }
}

#[test]
fn a_current_codex_in_the_tab_ends_a_move_that_failed_after_its_exit() {
    let rig = Rig::new("resumed-by-hand", T1, false, |_| {});
    let mut st = exiting(&rig, T1);
    st.phase = Phase::Failed("no-resume-hint".into());
    st.exited_at = now_s() - 60;
    save(&rig.opts, &key(TAB), &st);
    // The person resumed it by hand on the managed build.
    let tui = Tui {
        version: v("0.157.1"),
        ..rig.tui.clone()
    };
    let r = visit(&rig.opts, &tui, &rig.target, daemon_current(), &rig.kernel);
    assert_eq!(r.step, "current");
    assert!(load(&rig.opts, &key(TAB)).is_none(), "the record is over");
    // Negative control: a failure before any exit (the TUI never went) is
    // left alone here; the owner's view vets it against its holder.
    st.exited_at = 0;
    save(&rig.opts, &key(TAB), &st);
    assert_eq!(
        visit(&rig.opts, &tui, &rig.target, daemon_current(), &rig.kernel).step,
        "current"
    );
    assert!(load(&rig.opts, &key(TAB)).is_some());
}

// ---------------------------------------------------------------- the one host
//
// The Codex branch is taken by the session's own worker at its idle point
// (the window's host), through the same step and relaunch primitive as a
// Claude Code restart: the relaunched TUI is handed back to its supervisor,
// and the daemon a tab's step reaches asks every Codex tab it serves.

#[test]
fn the_window_hands_a_relaunched_codex_back_and_carries_it_on_at_its_next_idle_point() {
    let mut rig = embedded_ready("emb-hand", |_| {});
    rig.opts.hand_back = true;
    let r = rig.visit(None);
    assert_eq!(r.step, "adopted", "{r:?}\n{:#?}", rig.asked());
    assert_eq!(r.pid, NEW);
    // The adopted TUI is on the record: its own exit is never read as the
    // restart's (`relaunch::restarted`).
    assert_eq!(rig.state().resumed_pid, NEW);
    assert_eq!(
        rig.typed().len(),
        3,
        "the notice, `/exit` and the relaunch line — the carry-on is the loop's: {:#?}",
        rig.typed()
    );
    // The record stays in flight: what the relaunch primitive's continuation
    // step ([`super::crate::harness::relaunch::owed`]) finds for the tab.
    let Phase::Relaunched { at_s: moved } = rig.state().phase else {
        panic!("{:?}", rig.state().phase);
    };
    // A person at the keys where the loop parks next: it stays owed, and
    // nothing is typed over them.
    *rig.status_extra.lock().expect("status") = " human_ms=500".to_string();
    let carry = Opts {
        hand_back: false,
        background: false,
        ..rig.opts.clone()
    };
    let r = carry_in_flight_with(&carry, blank(&rig.tui), rig.state(), &key(TAB), &rig.kernel);
    assert_eq!(r.step, "wait:held", "{r:?}");
    assert_eq!(rig.typed().len(), 3);
    assert!(matches!(rig.state().phase, Phase::Relaunched { .. }));
    // Their hand gone: the carry-on goes, and the move is done.
    rig.status_extra.lock().expect("status").clear();
    let r = carry_in_flight_with(&carry, blank(&rig.tui), rig.state(), &key(TAB), &rig.kernel);
    assert_eq!(r.step, "continued", "{r:?}\n{:#?}", rig.asked());
    let typed = rig.typed();
    assert_eq!(typed.len(), 4, "{typed:#?}");
    assert!(
        typed[3].contains("[aterm harness] Upgraded: this session was restarted on Codex 0.157.1"),
        "{}",
        typed[3]
    );
    assert_eq!(rig.state().phase, Phase::Done);
    // The move's instant outlives the phase that carried it: the finished
    // move's record names it (audit 2026-10-09, `Row::moved_at`).
    assert_eq!(rig.state().relaunched_at, moved, "kept through Done");
    assert_eq!(
        rig.ledger_steps(),
        vec![
            "announced:1",
            "exit-typed",
            "relaunched",
            "adopted",
            "continued"
        ]
    );
    assert!(!rig.asked().iter().any(|l| l.contains("signal")));
}

/// A relaunched Codex the restart found is yours once it is gone, adopted or
/// not: two paths leave it up in the tab without an adoption stamp (an adopt
/// whose [`ADOPT_WAIT`] ran out, `wait:resume`, and a carry-on that found a
/// person at the keys, `wait:held`). The TUI the relaunch brought up is on
/// the record ([`St::resumed_pid`]) from the look that finds it, and once it
/// is gone a Codex leaving the tab is that TUI's own exit, not the restart's.
/// (`NEW` is the rig's: no process of the real kernel's, so gone to
/// `relaunch::restarted`, which reads the real one.)
#[test]
fn a_relaunched_codex_the_restart_found_is_yours_once_gone_adopted_or_not() {
    let mut rig = embedded_ready("emb-found", |_| {});
    rig.opts.hand_back = true;
    let r = rig.visit(None);
    assert_eq!(r.step, "adopted", "{r:?}\n{:#?}", rig.asked());
    assert!(
        !super::super::alive(NEW),
        "the rig's TUI is no real process"
    );
    let tab = Opts {
        only_sid: Some(TAB.to_string()),
        ..rig.opts.clone()
    };
    // Asked about the TUI that left: the one its `/exit` was typed into.
    assert_eq!(rig.state().pid, TUI);
    let restarted = || super::super::super::relaunch::restarted(&tab, true, Some(TUI));
    // The record an adopt whose ADOPT_WAIT ran out leaves: the new TUI found
    // and up, and never stamped as adopted.
    let mut st = rig.state();
    assert_eq!(
        st.relaunched_pid, NEW,
        "on the record from the look that found it"
    );
    st.resumed_pid = 0;
    save(&rig.opts, &key(TAB), &st);
    assert!(
        !restarted(),
        "the relaunched TUI the restart found, gone: its own exit"
    );
    // A person at the keys where the carry-on looks: `wait:held`, the record
    // still relaunched and unadopted, and that TUI, once gone, still its own.
    *rig.status_extra.lock().expect("status") = " human_ms=500".to_string();
    let carry = Opts {
        hand_back: false,
        background: false,
        ..rig.opts.clone()
    };
    let r = carry_in_flight_with(&carry, blank(&rig.tui), rig.state(), &key(TAB), &rig.kernel);
    assert_eq!(r.step, "wait:held", "{r:?}");
    assert!(matches!(rig.state().phase, Phase::Relaunched { .. }));
    assert_eq!(
        (rig.state().resumed_pid, rig.state().relaunched_pid),
        (0, NEW),
        "found, never adopted"
    );
    assert!(!restarted(), "the person's `/exit` is theirs");
    // Found by a visit: a relaunched TUI no earlier look recorded (the
    // relaunch's own wait ran out before it came up) is on the record from
    // the visit that finds it leading the tab, held at the keys as here, or
    // its adoption out of time.
    let mut st = rig.state();
    st.relaunched_pid = 0;
    save(&rig.opts, &key(TAB), &st);
    assert!(restarted(), "none recorded: the old TUI's exit, read late");
    let relaunched_tui = Tui {
        pid: NEW,
        argv: rig.kernel.new_argv.clone(),
        ..rig.tui.clone()
    };
    let r = visit(&carry, &relaunched_tui, &rig.target, None, &rig.kernel);
    assert_eq!(r.step, "wait:held", "{r:?}");
    assert_eq!(
        (rig.state().resumed_pid, rig.state().relaunched_pid),
        (0, NEW),
        "found by the visit, never adopted"
    );
    assert!(!restarted(), "the person's `/exit` is theirs");
    // A carry of the old TUI's exit made after it finds the restart LANDED:
    // closed as the relaunch on exit closes it, no hand held on the tab for
    // a TUI that is gone, and never `refused:no-restart-in-flight`, which the
    // host says as a restart that cannot land.
    assert_eq!(
        super::super::super::relaunch::carry_restart(&tab, true, Some(TUI)).step,
        "landed",
        "no hand held on the tab for a TUI that is gone"
    );
    assert_eq!(
        rig.state().phase,
        Phase::Failed("exited-before-continuing".to_string())
    );
    // Gone by its own orderly exit before its continuation: the relaunched
    // TUI's own exit closes the upgrade's record as one overtaken — never the
    // upgrade's failure after ITS exit, "The upgrade ended it and aterm could
    // not start it again" marked and notified for a day.
    use crate::harness::relaunch::{ExitCause, ExitRecord};
    rig.kernel.relaunched.store(false, Ordering::SeqCst);
    let gone = crate::harness::relaunch::Snapshot {
        pid: NEW,
        ..crashed_snap(&rig, Some(T2), true)
    };
    let r = after_exit(
        &rig.opts,
        &gone,
        &ExitRecord::Removed,
        false,
        ExitCause::Exit,
        &rig.kernel,
    );
    assert_eq!(r.step, "ended:graceful-exit", "{r:?}");
    let st = rig.state();
    assert!(
        st.is_upgrade() && matches!(st.phase, Phase::Failed(_)),
        "{st:?}"
    );
    let rows = super::super::rows(&rig.opts);
    assert!(
        rows.iter()
            .all(|row| !row.failed_after_exit() && row.interrupt(now_s()).is_none()),
        "no failure after its exit: exited_at={}",
        st.exited_at
    );
}

#[test]
fn a_daemon_reached_from_one_tabs_step_asks_every_codex_tab_it_serves() {
    // The window's step is one tab's, taken at its idle point; the daemon it
    // reaches serves every Codex tab of the window. Each of those is asked,
    // and none is a client the pass does not list.
    const OTHER: &str = "s-0ther0ther0ther0ther";
    const TUI2: u32 = 4_000_009;
    let run = |listed: bool| {
        let (rig, dir, mut opts, mut k, target, tui) = served("one-of-two", IDLE_LAST, |_| {});
        opts.only_sid = Some(TAB.to_string());
        k.clients = Some(vec![TUI, TUI2]);
        let other = Tui {
            pid: TUI2,
            tab: OTHER.to_string(),
            ..tui.clone()
        };
        let tuis = if listed { vec![tui, other] } else { vec![tui] };
        let mut reports = Vec::new();
        sweep_with(&opts, None, tuis, Some(&target), &k, &mut reports);
        let ran = dir.join("verb.args").exists();
        drop(rig);
        let _ = std::fs::remove_dir_all(&dir);
        (reports, ran)
    };
    let (reports, ran) = run(true);
    let daemon = reports
        .iter()
        .find(|r| r.session == "codex-daemon")
        .expect("the daemon's report");
    assert_eq!(daemon.step, "daemon-updated", "{reports:#?}");
    assert!(ran, "the vendor's verb ran");
    // Only the stepping tab's client is visited: the other tab's worker takes
    // its own step at its own idle point.
    assert!(reports.iter().any(|r| r.tab == TAB), "{reports:#?}");
    assert!(!reports.iter().any(|r| r.tab == OTHER), "{reports:#?}");
    // Negative control: the other tab's Codex attached and not listed; the
    // daemon waits for a client it did not ask.
    let (reports, ran) = run(false);
    let daemon = reports
        .iter()
        .find(|r| r.session == "codex-daemon")
        .expect("the daemon's report");
    assert_eq!(daemon.step, "wait:unseen-client", "{reports:#?}");
    assert!(!ran);
}

/// A Codex daemon behind the managed Codex, or on it with the vendor's
/// updater armed, makes its tab due: the latter for the pin alone, and at
/// most once every PIN_LOOK_S (an armed updater can restart the daemon
/// mid-turn). The look the pin takes is stamped by the daemon step when it
/// does not pin (a goal keeps a thread running: `wait:pin:busy-thread`), so
/// a goal that never pauses is asked about once an hour, not at every turn
/// end. Negative controls: pinned on the managed build, ahead of it, or no
/// daemon at all: nothing to move or pin.
#[test]
fn a_codex_daemon_behind_or_on_the_vendors_updater_is_due_and_the_pin_alone_boundedly() {
    let (dir, opts, k, target) = daemon_home("due", IDLE_LAST, 600);
    let home = k.home.clone();
    let now = now_s();
    // Older than the managed Codex: its move.
    assert_eq!(daemon_owes(Some(v("0.157.0")), false, &target), Owes::Move);
    assert!(daemon_due(&opts, &home, &target, &k, now));
    let _ = std::fs::remove_dir_all(&dir);
    // The managed build, the vendor's updater still armed: the pin alone.
    let (dir, opts, k, target) = current_unpinned("due-pin", IDLE_LAST);
    let home = k.home.clone();
    assert_eq!(daemon_owes(Some(v("0.157.1")), false, &target), Owes::Pin);
    assert!(
        daemon_due(&opts, &home, &target, &k, now),
        "due for the pin"
    );
    // A look for the pin that could not pin (a thread running): not due
    // again for PIN_LOOK_S, then due again — the pin is never given up.
    let (busy_dir, busy_opts, busy_k, busy_target) = current_unpinned("due-busy", BUSY_LAST);
    let busy_home = busy_k.home.clone();
    let (r, _) = daemon_pass(&busy_opts, &busy_home, &busy_target, &[], &busy_k).expect("a daemon");
    assert_eq!(r.step, "wait:pin:busy-thread", "said as the pin's");
    assert!(
        !upgrade_drive_owns_turn_ends(&r.step),
        "the pin's wait owns no turn end"
    );
    assert!(!busy_dir.join("verb.args").exists(), "the verb never ran");
    // The pin alone raises nothing the owner sees and owns no turn end: the
    // tab's step, the pass over its current TUI, says the pin's word, and no
    // upgrade of the tab is recorded (no row, no tab mark, no Warn).
    {
        let (rig, rig_dir, mut opts, _, _, tui) = served("pin-silent", BUSY_LAST, |_| {});
        let (pin_dir, _, pin_k, pin_target) = current_unpinned("pin-silent-d", BUSY_LAST);
        opts.only_sid = Some(TAB.to_string());
        let current = Tui {
            version: v("0.157.1"),
            home: pin_k.home.clone(),
            ..tui
        };
        let mut reports = Vec::new();
        sweep_with(
            &opts,
            None,
            vec![current],
            Some(&pin_target),
            &pin_k,
            &mut reports,
        );
        let step = super::super::pick(&reports, TAB).map(|r| r.step.clone());
        assert_eq!(
            step.as_deref(),
            Some("wait:pin:busy-thread"),
            "{reports:#?}"
        );
        assert!(reports.iter().any(|r| r.tab == TAB && r.step == "current"));
        assert!(!upgrade_drive_owns_turn_ends("wait:pin:busy-thread"));
        assert!(
            super::super::rows(&opts).is_empty(),
            "no upgrade recorded for the tab: nothing on the glass"
        );
        drop(rig);
        let _ = std::fs::remove_dir_all(&rig_dir);
        let _ = std::fs::remove_dir_all(&pin_dir);
    }
    let at = pin_looked_at(&busy_opts, &busy_home).expect("the look stamped");
    let due_at = |t: u64| daemon_due(&busy_opts, &busy_home, &busy_target, &busy_k, t);
    assert!(!due_at(at + 60));
    assert!(!due_at(at + PIN_LOOK_S - 1));
    assert!(due_at(at + PIN_LOOK_S), "asked again an hour on");
    // Negative control: the move of a daemon behind is never bounded so.
    let behind_target = Target {
        version: v("0.157.2"),
        ..busy_target.clone()
    };
    assert!(daemon_due(
        &busy_opts,
        &busy_home,
        &behind_target,
        &busy_k,
        at + 60
    ));
    // A dry run (the upgrade watch's read) stamps no look.
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    note_pin_look(&opts, &home, None);
    note_pin_look(&dry, &home, Some(now));
    assert_eq!(
        pin_looked_at(&opts, &home),
        None,
        "a dry run writes no stamp"
    );
    // The pin taken (every thread idle): the stamp goes, and the daemon owes
    // nothing more.
    note_pin_look(&opts, &home, Some(now));
    assert_eq!(pin_looked_at(&opts, &home), Some(now), "a real look stamps");
    let (r, _) = daemon_pass(&opts, &home, &target, &[], &k).expect("a daemon");
    assert_eq!(r.step, "daemon-updated", "{r:?}");
    assert_eq!(
        pin_looked_at(&opts, &home),
        None,
        "the stamp goes with the pin"
    );
    assert!(!daemon_due(&opts, &home, &target, &k, now));
    // Negative controls: pinned on the managed build, ahead of it, or no
    // daemon at all: nothing to move or pin.
    assert_eq!(
        daemon_owes(Some(v("0.157.1")), true, &target),
        Owes::Nothing
    );
    let older = Target {
        version: v("0.156.1"),
        ..target.clone()
    };
    assert_eq!(
        daemon_owes(Some(v("0.157.1")), false, &older),
        Owes::Nothing
    );
    assert_eq!(daemon_owes(None, false, &target), Owes::Nothing);
    std::fs::remove_file(home.join("app-server-daemon/daemon.pid")).expect("no daemon");
    assert!(!daemon_due(&opts, &home, &target, &k, now));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&busy_dir);
}

/// [`daemon_home`] whose daemon runs the managed build (0.157.1, the rigs'
/// target) with the vendor's updater still armed: what owes the pin alone.
fn current_unpinned(name: &str, last: &str) -> (ScratchDir, Opts, DaemonScript, Target) {
    let (dir, opts, k, target) = daemon_home(name, last, 600);
    let rel = k
        .home
        .join("packages/app-server-daemon/releases/0.157.0-aarch64-apple-darwin");
    std::fs::write(
        rel.join(cx::PACKAGE_JSON),
        PACKAGE.replace("0.157.0", "0.157.1"),
    )
    .expect("pkg");
    (dir, opts, k, target)
}

/// [`super::super::owns_turn_ends`] after any number of waits in a row, a
/// person's grace of two minutes (read only for an attended word).
fn upgrade_drive_owns_turn_ends(step: &str) -> bool {
    (0..8).any(|waits| super::super::owns_turn_ends(step, waits, 120))
}

#[test]
fn a_background_terminals_break_takes_the_notice_and_ends_nothing() {
    // An embedded session whose finished turn left a background terminal (its
    // status line under the turn's end, so its loop reads the turn running)
    // is told of its upgrade at that break, the notice alone; its READY
    // answer waits for an idle point, where the `/exit` still waits out the
    // terminal. A daemon-mode client, which gets no notice, types nothing
    // there.
    let with_terminal = |w: &mut World| {
        w.before = screen_json(
            &[
                "› start bg please",
                "",
                "• The server runs in the background.",
                "",
                "  1:41 AM",
                "",
                "  1 background terminal running · /ps to view · /stop to close",
                "",
                "› Ask Codex to do anything",
                "",
                "  GPT-6-Astra default · /w",
            ],
            (8, 2),
            10,
        );
    };
    let mut rig = Rig::new("emb-break", T2, true, with_terminal);
    rig.kernel.terminals = vec!["sleep".into()];
    rig.opts.background = true;
    rig.settle(9);
    assert_eq!(rig.visit(None).step, "announced:1", "{:#?}", rig.asked());
    assert_eq!(rig.typed().len(), 1);
    // READY answered: at the break, still nothing more.
    let st = rig.state();
    let mut text = std::fs::read_to_string(&rig.rollout).expect("rollout");
    text.push_str(&format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{}"}}}}"#,
        st.marker
    ));
    text.push('\n');
    std::fs::write(&rig.rollout, text).expect("rollout");
    age(&rig.rollout);
    rig.settle_keeping(&st);
    assert_eq!(rig.visit(None).step, "wait:background");
    assert_eq!(rig.typed().len(), 1, "no `/exit` at the break");
    // At an idle point, the terminal still running holds the `/exit`.
    rig.opts.background = false;
    rig.settle_keeping(&rig.state());
    assert_eq!(rig.visit(None).step, "wait:background-terminal");
    assert_eq!(rig.typed().len(), 1);
    // Negative control: a daemon-mode client at the break types nothing.
    let mut daemon = Rig::new("daemon-break", T1, false, with_terminal);
    daemon.opts.background = true;
    daemon.settle(9);
    assert_eq!(daemon.visit(daemon_current()).step, "wait:background");
    assert!(daemon.typed().is_empty());
    assert!(!daemon.asked().iter().any(|l| l.contains("signal")));
}

#[test]
fn at_a_break_a_daemon_mode_client_waits_background_and_its_daemon_is_only_read() {
    // At a break the daemon pass is skipped, but the daemon is read (its
    // build, from its files), never stepped: a daemon-mode client waits
    // `background`, not `wait:no-daemon`, and the vendor's verb never runs.
    let (rig, dir, mut opts, k, target, tui) = served("break-read", IDLE_LAST, |_| {});
    opts.background = true;
    assert_eq!(daemon_build(&k.home, &k), Some((Some(v("0.157.0")), false)));
    let mut reports = Vec::new();
    sweep_with(
        &opts,
        None,
        vec![tui.clone()],
        Some(&target),
        &k,
        &mut reports,
    );
    assert!(
        !reports
            .iter()
            .any(|r| r.session.starts_with("codex-daemon")),
        "no daemon step at a break: {reports:#?}"
    );
    assert!(!dir.join("verb.args").exists(), "the verb never ran");
    // The client, its daemon read: `background`, nothing typed.
    rig.settle(10);
    let read = DaemonView {
        running: Some(v("0.157.0")),
        wait: String::new(),
        pid: 0,
        threads: Vec::new(),
    };
    let r = visit(&opts, &tui, &target, Some(read), &rig.kernel);
    assert_eq!(r.step, "wait:background", "{r:?}");
    assert!(rig.typed().is_empty());
    // Negative control: with no daemon read, the same client reads none.
    let r = visit(&opts, &tui, &target, None, &rig.kernel);
    assert_eq!(r.step, "wait:no-daemon");
    drop(rig);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the ladder, driven
//
// These drive the real visit across two looks (its own reads of the screen,
// the status line, the person's stamp, the kernel and the daemon), with the
// clock between the looks moved by rewinding the state the first look saved
// (the still run, the screen's `seq`), as a later look finds it.

/// A kernel that is the tab's ([`Script`]) for the TUI, its shell and its
/// relaunch, and the daemon's ([`DaemonScript`]) for the daemon — the one a
/// daemon-mode client's visit reads both through: its daemon's attached
/// Codex, and at the `/exit`'s last look the daemon's threads.
struct Both<'a> {
    tab: &'a Script,
    daemon: &'a DaemonScript,
}

impl Both<'_> {
    fn daemons(pid: u32) -> bool {
        pid == DAEMON || pid == DAEMON_NEW
    }
}

impl Kernel for Both<'_> {
    fn job(&self, p: u32) -> Option<(Job, u32)> {
        self.tab.job(p)
    }
    fn parent(&self, p: u32) -> Option<u32> {
        self.tab.parent(p)
    }
    fn terminal(&self, p: u32) -> Option<(u32, String)> {
        self.tab.terminal(p)
    }
}

impl CodexKernel for Both<'_> {
    fn alive(&self, p: u32) -> bool {
        if Self::daemons(p) {
            self.daemon.alive(p)
        } else {
            self.tab.alive(p)
        }
    }
    fn args(&self, p: u32) -> Option<atpkg::caller_shell::ProcArgs> {
        if Self::daemons(p) {
            self.daemon.args(p)
        } else {
            self.tab.args(p)
        }
    }
    fn exe(&self, p: u32) -> Option<PathBuf> {
        if Self::daemons(p) {
            self.daemon.exe(p)
        } else {
            self.tab.exe(p)
        }
    }
    fn open_files(&self, p: u32) -> Option<Vec<PathBuf>> {
        if Self::daemons(p) {
            self.daemon.open_files(p)
        } else {
            self.tab.open_files(p)
        }
    }
    fn holders_of(&self, f: &Path) -> Option<Vec<u32>> {
        self.tab.holders_of(f)
    }
    fn shell_has_terminal(&self, s: u32) -> bool {
        self.tab.shell_has_terminal(s)
    }
    fn codex_children(&self, s: u32) -> Vec<(u32, Vec<String>)> {
        self.tab.codex_children(s)
    }
    fn background(&self, p: u32) -> Vec<String> {
        self.tab.background(p)
    }
    fn terminals(&self, p: u32) -> Vec<String> {
        if Self::daemons(p) {
            self.daemon.terminals(p)
        } else {
            self.tab.terminals(p)
        }
    }
    fn daemon_clients(&self, d: u32) -> Option<Vec<u32>> {
        self.daemon.daemon_clients(d)
    }
    fn cwd(&self, p: u32) -> Option<String> {
        self.tab.cwd(p)
    }
    fn in_tab(&self, c: &mut Client, p: u32, t: &str) -> bool {
        self.tab.in_tab(c, p, t)
    }
}

/// [`served`] whose daemon already runs the managed build, pinned: the
/// daemon step says `current`, and the client's own gate decides. The pass
/// keeps its state where the rig's helpers read and write it.
fn served_current(
    name: &str,
    world: impl FnOnce(&mut World),
) -> (Rig, ScratchDir, Opts, DaemonScript, Target, Tui) {
    let (rig, dir, mut opts, k, target, tui) = served(name, IDLE_LAST, world);
    // One state directory for the rig's helpers and the pass.
    opts.state.clone_from(&rig.opts.state);
    let rel = k
        .home
        .join("packages/app-server-daemon/releases/0.157.0-aarch64-apple-darwin");
    std::fs::write(
        rel.join(cx::PACKAGE_JSON),
        PACKAGE.replace("0.157.0", "0.157.1"),
    )
    .expect("pkg");
    let _ = std::fs::remove_file(
        k.home
            .join("packages/app-server-daemon/auto-update-version"),
    );
    (rig, dir, opts, k, target, tui)
}

/// Another session's thread on the daemon of `k` — its conversation's root:
/// its lock and its rollout (ending on `last`) open in the daemon.
fn another_session(k: &mut DaemonScript, last: &str) {
    loaded(k, T2, &cx::fixtures::root_meta(T2), last);
}

/// A thread on the daemon of `k` whose rollout opens on `meta` (its
/// `session_meta`) and ends on `last`: its lock and its rollout open in the
/// daemon.
fn loaded(k: &mut DaemonScript, thread: &str, meta: &str, last: &str) {
    let day = k.home.join("sessions/2026/09/25");
    let rollout = day.join(format!("rollout-2026-09-25T22-35-30-{thread}.jsonl"));
    std::fs::write(&rollout, format!("{meta}\n{last}\n")).expect("rollout");
    k.open.push(
        k.home
            .join("thread-writer-locks")
            .join(format!("{thread}.lock")),
    );
    k.open.push(canonical(&rollout));
}

/// The rollout of `thread` on the daemon of `k` ends on `last` now, its
/// first line — its `session_meta` — kept.
fn now_ends(k: &DaemonScript, thread: &str, last: &str) {
    let rollout = k
        .open
        .iter()
        .find(|p| p.to_string_lossy().ends_with(&format!("{thread}.jsonl")))
        .expect("its rollout");
    let text = std::fs::read_to_string(rollout).expect("rollout");
    let meta = text.lines().next().expect("its first line");
    std::fs::write(rollout, format!("{meta}\n{last}\n")).expect("rewritten");
}

/// The state a look saved, as a look [`upgrade::QUIET_S`] (20 s) later finds
/// it: the still run begun 20 s earlier (and every thread its record found
/// running with it), and the screen's `seq` changed since the last read when
/// `repainted` (the screen's own quiet restarting, as a goal's counter or a
/// footer's clock restarts it).
fn twenty_seconds_on(rig: &Rig, repainted: bool) {
    let mut st = rig.state();
    assert!(st.still_since != 0, "the first look began a still run");
    st.still_since -= upgrade::QUIET_S;
    st.still_at -= upgrade::QUIET_S;
    st.still_busy = st
        .still_busy
        .split_whitespace()
        .filter_map(|e| {
            let (thread, since) = e.split_once('@')?;
            Some(format!(
                "{thread}@{}",
                since.parse::<u64>().ok()? - upgrade::QUIET_S
            ))
        })
        .collect::<Vec<_>>()
        .join(" ");
    if repainted {
        st.last_seq = 9;
        st.seq_since_s = now_s();
    }
    save(&rig.opts, &key(TAB), &st);
}

/// A pending upgrade `behind_s` behind, the screen's `seq` last read as
/// `last_seq` (the stand-in draws `seq` 10) since `seq_since_s`.
fn behind(rig: &Rig, behind_s: u64, last_seq: u64, seq_since_s: u64) {
    let now = now_s();
    save(
        &rig.opts,
        &key(TAB),
        &St {
            agent: Agent::Codex,
            from: "0.157.0".into(),
            to: "0.157.1".into(),
            source: "managed".into(),
            salt: now - behind_s,
            pending_since: now - behind_s,
            last_seq,
            seq_since_s,
            notice_pid: TUI,
            tab: TAB.into(),
            ..St::default()
        },
    );
}

/// The ladder, driven: the real visit of a daemon-mode Codex across two
/// looks. A repaint at Settled moves: the screen's own quiet restarted at each
/// look (its `seq` moved) while its words stood still 20 s, so the Settled
/// rung takes the reader's run of looks and the second look types the
/// `/exit`. Negative control: the same two looks at the Prefer rung wait
/// `settling` and type nothing.
#[test]
fn the_real_visit_moves_at_settled_on_a_repaint_and_waits_at_prefer() {
    for (behind_s, moves) in [(upgrade::RUNG_SETTLED_S + 60, true), (600, false)] {
        let rig = Rig::new(&format!("ladder-repaint-{moves}"), T1, false, |_| {});
        behind(&rig, behind_s, 9, now_s());
        assert_eq!(rig.visit(daemon_current()).step, "wait:settling");
        assert!(rig.typed().is_empty());
        twenty_seconds_on(&rig, true);
        let r = rig.visit(daemon_current());
        if moves {
            assert_eq!(r.step, "done", "{:#?}", rig.asked());
            assert!(rig.typed()[0].ends_with(" /exit"));
        } else {
            assert_eq!(
                r.step, "wait:settling",
                "Prefer asks the screen's own quiet"
            );
            assert!(rig.typed().is_empty());
        }
    }
}

/// The ladder, driven: a person near the tab at KeysOnly moves (their input a
/// minute old, within `human_grace_s`, not within KEYS_GAP_S). The first look,
/// at Settled, waits for them (`attended`, said once as
/// `held-back:attended`); the same person an hour behind no longer holds it,
/// and the next look types the `/exit`.
#[test]
fn the_real_visit_lets_a_person_near_the_tab_go_by_at_keys_only() {
    let rig = Rig::new("ladder-near", T1, false, attended);
    behind(&rig, upgrade::RUNG_SETTLED_S + 60, 10, now_s() - 60);
    assert_eq!(rig.visit(daemon_current()).step, "held-back:attended");
    assert!(rig.typed().is_empty());
    let mut st = rig.state();
    st.pending_since = now_s() - upgrade::RUNG_KEYS_S - 60;
    st.salt = st.pending_since;
    save(&rig.opts, &key(TAB), &st);
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "done", "{:#?}", rig.asked());
}

/// The ladder, driven: a keystroke within 20 s at Land waits, look after
/// look, nothing typed. Negative control: the same rung with the person's
/// input a minute old moves at the first look.
#[test]
fn the_real_visit_waits_a_keystroke_out_at_land() {
    let rig = Rig::new("ladder-typing", T1, false, typing);
    behind(&rig, upgrade::RUNG_LAND_S + 60, 10, now_s() - 60);
    assert_eq!(rig.visit(daemon_current()).step, "held-back:attended");
    assert_eq!(rig.visit(daemon_current()).step, "wait:attended");
    assert!(rig.typed().is_empty(), "never typed within KEYS_GAP_S");
    let rig = Rig::new("ladder-near-land", T1, false, attended);
    behind(&rig, upgrade::RUNG_LAND_S + 60, 10, now_s() - 60);
    assert_eq!(rig.visit(daemon_current()).step, "done");
}

/// Another Codex session's turn holds an idle client only until its screen
/// places it there. Two daemon-mode tabs on one daemon, this one idle at its
/// placeholder, the other's conversation's root running a turn: the kernel
/// cannot say which conversation is this client's (two roots, two clients),
/// so the first look waits `daemon-busy`, worded as a turn that may be this
/// tab's own (a subagent) or another session's, never as a goal to pause
/// here; a look 20 s (`QUIET_S`) later, the other root still running,
/// another Codex attached and this screen's words unchanged, places it in the
/// other session and types the `/exit`. Negative controls: a thread that
/// began running only at the second look is not placed (a goal's next turn
/// streams nothing at first); with this tab's thread the daemon's only one,
/// running, the kernel names it this tab's own (`daemon-turn`).
#[test]
fn another_sessions_turn_holds_an_idle_client_only_until_its_screen_places_it() {
    const TUI2: u32 = 4_000_009;
    let setup = |name: &str, others: &str| {
        let (rig, dir, mut opts, mut k, target, tui) = served_current(name, |_| {});
        opts.only_sid = Some(TAB.to_string());
        another_session(&mut k, others);
        k.clients = Some(vec![TUI, TUI2]);
        let other = Tui {
            pid: TUI2,
            tab: "s-0ther0ther0ther0ther".to_string(),
            ..tui.clone()
        };
        let (r, view) = daemon_pass(&opts, &k.home.clone(), &target, &[tui.clone(), other], &k)
            .expect("a daemon");
        assert_eq!(r.step, "current", "{r:?}");
        rig.settle(10);
        (rig, dir, opts, k, target, tui, view)
    };
    // Three cases of their own, two of them each waiting out a relaunch's
    // fixed waits: side by side.
    let setup = &setup;
    super::super::tests::side_by_side(vec![
        Box::new(move || {
            let (rig, dir, opts, k, target, tui, view) = setup("two-threads", BUSY_LAST);
            assert_eq!(
                view.threads,
                vec![
                    root_turn(T1, TurnState::Idle),
                    root_turn(T2, TurnState::Busy)
                ]
            );
            let both = Both {
                tab: &rig.kernel,
                daemon: &k,
            };
            let r = visit(&opts, &tui, &target, Some(view.clone()), &both);
            assert_eq!(r.step, "wait:daemon-busy", "{r:?}");
            assert!(rig.typed().is_empty());
            let row = super::super::rows(&opts)
                .into_iter()
                .find(|r| r.tab == TAB)
                .expect("its row");
            let words = row.waiting_line("2:15 PM");
            assert!(
                words.contains("a subagent of this conversation")
                    && words.contains("another Codex session's")
                    && !words.contains("most often"),
                "{words}"
            );
            assert!(
                !words.contains("goal") && !words.contains("pause"),
                "{words}"
            );
            // 20 s on, the other thread still running: placed, and moved.
            twenty_seconds_on(&rig, false);
            let r = visit(&opts, &tui, &target, Some(view.clone()), &both);
            assert_eq!(r.step, "done", "{r:?}\n{:#?}", rig.asked());
            drop(rig);
            let _ = std::fs::remove_dir_all(&dir);
        }),
        Box::new(move || {
            // Negative control: the other thread idle at the first look (Settled,
            // the screen repainting), running at the second: found running only
            // then, it is not placed; still running 20 s later, it is.
            let (rig, dir, opts, k, target, tui, idle_view) = setup("two-threads-late", IDLE_LAST);
            behind(&rig, upgrade::RUNG_SETTLED_S + 60, 9, now_s());
            let both = Both {
                tab: &rig.kernel,
                daemon: &k,
            };
            assert_eq!(
                visit(&opts, &tui, &target, Some(idle_view.clone()), &both).step,
                "wait:settling"
            );
            twenty_seconds_on(&rig, true);
            let running = DaemonView {
                threads: vec![
                    root_turn(T1, TurnState::Idle),
                    root_turn(T2, TurnState::Busy),
                ],
                ..idle_view
            };
            let r = visit(&opts, &tui, &target, Some(running.clone()), &both);
            assert_eq!(r.step, "wait:daemon-busy", "{r:?}");
            assert!(rig.typed().is_empty());
            twenty_seconds_on(&rig, true);
            let r = visit(&opts, &tui, &target, Some(running), &both);
            assert_eq!(r.step, "done", "{r:?}");
            drop(rig);
            let _ = std::fs::remove_dir_all(&dir);
        }),
        Box::new(move || {
            // Negative control: this tab's thread the daemon's only one, running.
            let (rig, dir, opts, mut k, target, tui) = served_current("own-thread", |_| {});
            now_ends(&k, T1, BUSY_LAST);
            k.clients = Some(vec![TUI]);
            let (_, view) = daemon_pass(
                &opts,
                &k.home.clone(),
                &target,
                std::slice::from_ref(&tui),
                &k,
            )
            .expect("a daemon");
            rig.settle(10);
            let both = Both {
                tab: &rig.kernel,
                daemon: &k,
            };
            assert_eq!(
                visit(&opts, &tui, &target, Some(view), &both).step,
                "wait:daemon-turn"
            );
            assert!(rig.typed().is_empty());
            drop(rig);
            let _ = std::fs::remove_dir_all(&dir);
        }),
    ]);
}

/// A goal on this screen places nothing: the same two daemon-mode tabs as
/// [`another_sessions_turn_holds_an_idle_client_only_until_its_screen_places_it`],
/// the other's thread running, but this tab's footer says `Pursuing goal
/// (…)`, so this client's own goal turn may be running where its screen reads
/// idle, and a thread that runs through the still run may be its own. Look
/// after look the real visit waits `daemon-busy`, keeps no record of the
/// run's threads, and types nothing. Negative control: the same looks with no
/// goal in the footer place the other session's turn at the second look and
/// move.
#[test]
fn a_goal_on_this_screen_places_no_other_sessions_turn() {
    const TUI2: u32 = 4_000_009;
    for goal in [true, false] {
        let world = move |w: &mut World| {
            if goal {
                w.before = codex_idle_goal(10);
            }
        };
        let (rig, dir, mut opts, mut k, target, tui) =
            served_current(&format!("goal-places-{goal}"), world);
        opts.only_sid = Some(TAB.to_string());
        another_session(&mut k, BUSY_LAST);
        k.clients = Some(vec![TUI, TUI2]);
        let other = Tui {
            pid: TUI2,
            tab: "s-0ther0ther0ther0ther".to_string(),
            ..tui.clone()
        };
        let (_, view) = daemon_pass(&opts, &k.home.clone(), &target, &[tui.clone(), other], &k)
            .expect("a daemon");
        rig.settle(10);
        let both = Both {
            tab: &rig.kernel,
            daemon: &k,
        };
        let r = visit(&opts, &tui, &target, Some(view.clone()), &both);
        assert_eq!(r.step, "wait:daemon-busy", "{goal}: {r:?}");
        let mut steps = Vec::new();
        for _ in 0..3 {
            twenty_seconds_on(&rig, false);
            steps.push(visit(&opts, &tui, &target, Some(view.clone()), &both).step);
            if goal {
                assert!(rig.state().still_busy.is_empty(), "no record under a goal");
            }
        }
        if goal {
            assert_eq!(steps, ["wait:daemon-busy"; 3], "never placed under a goal");
            assert!(rig.typed().is_empty(), "nothing typed");
        } else {
            assert_eq!(steps[0], "done", "{:#?}", rig.asked());
        }
        drop(rig);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Three subagents of one conversation (stand-ins), and one of another's.
const S1: &str = "01a0e8a1-0000-7000-8000-000000000001";
const S2: &str = "01a0e8a1-0000-7000-8000-000000000002";
const S3: &str = "01a0e8a1-0000-7000-8000-000000000003";
const S4: &str = "01a0e8a1-0000-7000-8000-000000000004";

/// A subagent `thread` that `parent` (in the tree of `root`) spawned, on the
/// daemon of `k`, its rollout ending on `last`.
fn subagent(k: &mut DaemonScript, thread: &str, parent: &str, root: &str, last: &str) {
    loaded(
        k,
        thread,
        &cx::fixtures::spawned_meta(thread, parent, root, 1),
        last,
    );
}

/// The real visit's steps at three looks 20 s apart (the second and third
/// with the still run rewound, [`twenty_seconds_on`]), stopping at a move.
fn three_looks(
    rig: &Rig,
    opts: &Opts,
    tui: &Tui,
    target: &Target,
    view: &DaemonView,
    k: &Both<'_>,
) -> Vec<String> {
    let mut steps = vec![visit(opts, tui, target, Some(view.clone()), k).step];
    for _ in 0..2 {
        if steps.last().is_some_and(|s| s == "done") {
            break;
        }
        twenty_seconds_on(rig, false);
        steps.push(visit(opts, tui, target, Some(view.clone()), k).step);
    }
    steps
}

/// One conversation on the daemon (the root this tab's TUI resumed and three
/// subagents it spawned, each rollout opening on its `session_meta`), this
/// TUI the daemon's only client. The pass reads each thread's spawn tree and
/// the kernel names the conversation, so a subagent's turn is this tab's
/// own: running while the root idles, the real visit waits `daemon-turn` at
/// every look 20 s apart (`goal` under a goal's footer) and types nothing;
/// the root running while the subagents idle, the same. All idle, it moves,
/// and where the tab's shell integration marks do not reach aterm
/// (`integration=degraded`) the kernel names the conversation's root,
/// recorded before the `/exit` and resumed.
#[test]
fn the_tabs_own_subagents_hold_it_and_the_kernel_names_their_root() {
    let degraded = |w: &mut World| {
        w.status = "OK schema=1 hold=0 hand=- integration=degraded".to_string();
        w.blocks1 = r#"{"blocks":[]}"#.to_string();
        w.blocks2 = r#"{"blocks":[]}"#.to_string();
    };
    // Five cases of their own, the moving ones each waiting out a relaunch's
    // fixed waits: side by side.
    super::super::tests::side_by_side(
        [
            ("sub", false, true, false, true),
            ("subg", false, true, true, true),
            ("root", true, false, false, true),
            ("idle", false, false, false, true),
            ("nomk", false, false, false, false),
        ]
        .into_iter()
        .map(
            |(case, root_busy, sub_busy, goal, marks)| -> Box<dyn FnOnce() + Send> {
                Box::new(move || {
                    let world = move |w: &mut World| {
                        if goal {
                            w.before = codex_idle_goal(10);
                        }
                        if !marks {
                            degraded(w);
                        }
                    };
                    let (rig, dir, mut opts, mut k, target, tui) =
                        served_current(&format!("os-{case}"), world);
                    opts.only_sid = Some(TAB.to_string());
                    if root_busy {
                        now_ends(&k, T1, BUSY_LAST);
                    }
                    for (s, busy) in [(S1, sub_busy), (S2, false), (S3, false)] {
                        subagent(&mut k, s, T1, T1, if busy { BUSY_LAST } else { IDLE_LAST });
                    }
                    k.clients = Some(vec![TUI]);
                    let (_, view) = daemon_pass(
                        &opts,
                        &k.home.clone(),
                        &target,
                        std::slice::from_ref(&tui),
                        &k,
                    )
                    .expect("a daemon");
                    assert_eq!(view.threads.len(), 4, "{case}");
                    assert!(
                        view.threads[1..]
                            .iter()
                            .all(|l| l.lineage == cx::Lineage::Spawned(T1.to_string())),
                        "{case}: {:?}",
                        view.threads
                    );
                    rig.settle(10);
                    let both = Both {
                        tab: &rig.kernel,
                        daemon: &k,
                    };
                    let steps = three_looks(&rig, &opts, &tui, &target, &view, &both);
                    if root_busy || sub_busy {
                        let want = if goal {
                            "wait:goal"
                        } else {
                            "wait:daemon-turn"
                        };
                        assert_eq!(steps, [want; 3], "{case}");
                        assert!(rig.typed().is_empty(), "{case}: nothing typed");
                        let row = super::super::rows(&opts)
                            .into_iter()
                            .find(|r| r.tab == TAB)
                            .expect("its row");
                        let words = row.waiting_line("2:15 PM");
                        assert!(words.contains("this tab's Codex"), "{case}: {words}");
                    } else {
                        assert_eq!(steps, ["done"], "{case}: {:#?}", rig.asked());
                        let typed = rig.typed();
                        assert!(
                            typed[1].contains(&format!("'resume' '{T1}'")),
                            "{case}: the root resumed: {}",
                            typed[1]
                        );
                        if !marks {
                            let ledger = std::fs::read_to_string(super::super::ledger_path(&opts))
                                .unwrap_or_default();
                            assert!(
                                ledger.contains(&format!("its thread {T1} named by the kernel")),
                                "{ledger}"
                            );
                        }
                    }
                    drop(rig);
                    let _ = std::fs::remove_dir_all(&dir);
                })
            },
        )
        .collect(),
    );
}

/// An idle conversation behind a line longer than the tail: a turn ended,
/// then one very large `item_completed` line (a command that finished after
/// it) and settings rows. In the one-conversation shape and on a daemon
/// holding the one thread, the pass reads the root's turn back past that
/// line: idle, and the tab moves at the first look, the root resumed.
/// Controls, each waiting `daemon-turn` with nothing typed: a turn started
/// before the same line (busy), a line whose output quotes a turn's end after
/// a turn started (text, never an event), and a turn's end further back than
/// the reader looks ([`TURN_BACK_BYTES`]: Unknown, counted running, fail
/// closed).
#[test]
fn an_idle_conversation_behind_a_line_longer_than_the_tail_moves() {
    const STARTED: &str = r#"{"timestamp":"2026-09-27T20:30:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t9"}}"#;
    const ENDED: &str = r#"{"timestamp":"2026-09-27T20:45:24.812Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t9","last_agent_message":null}}"#;
    let item = |size: usize, quoted: &str| {
        let output = format!("{quoted}{}", "x".repeat(size));
        format!(
            r#"{{"timestamp":"2026-09-27T20:56:02.000Z","type":"event_msg","payload":{{"type":"item_completed","turn_id":"t9","item":{{"type":"commandExecution","aggregated_output":{}}}}}}}"#,
            aterm_json::to_string(&aterm_json::Value::from(output.as_str())).expect("json")
        )
    };
    let (big, beyond, quoting) = (
        item(300_000, ""),
        item(usize::try_from(TURN_BACK_BYTES).expect("bytes"), ""),
        item(300_000, ENDED),
    );
    let idle = [STARTED, ENDED, big.as_str()];
    let busy = [ENDED, STARTED, big.as_str()];
    let quoted = [ENDED, STARTED, quoting.as_str()];
    let far = [STARTED, ENDED, beyond.as_str()];
    // Five cases of their own, the moving ones each waiting out a relaunch's
    // fixed waits: side by side.
    let mut cases: Vec<Box<dyn FnOnce() + Send + '_>> = Vec::new();
    for (case, subs, last, want) in [
        ("idle", true, idle, TurnState::Idle),
        ("alone", false, idle, TurnState::Idle),
        ("busy", true, busy, TurnState::Busy),
        ("quoted", true, quoted, TurnState::Busy),
        ("far", true, far, TurnState::Unknown),
    ] {
        cases.push(Box::new(move || {
            let (rig, dir, mut opts, mut k, target, tui) =
                served_current(&format!("long-{case}"), |_| {});
            opts.only_sid = Some(TAB.to_string());
            now_ends(&k, T1, &last.join("\n"));
            if subs {
                for s in [S1, S2, S3] {
                    subagent(&mut k, s, T1, T1, IDLE_LAST);
                }
            }
            k.clients = Some(vec![TUI]);
            let (_, view) = daemon_pass(
                &opts,
                &k.home.clone(),
                &target,
                std::slice::from_ref(&tui),
                &k,
            )
            .expect("a daemon");
            assert_eq!(view.threads[0].thread, T1, "{case}");
            assert_eq!(view.threads[0].turn, want, "{case}");
            rig.settle(10);
            let both = Both {
                tab: &rig.kernel,
                daemon: &k,
            };
            let steps = three_looks(&rig, &opts, &tui, &target, &view, &both);
            if want == TurnState::Idle {
                assert_eq!(steps, ["done"], "{case}: {:#?}", rig.asked());
                let typed = rig.typed();
                assert!(
                    typed[1].contains(&format!("'resume' '{T1}'")),
                    "{case}: the root resumed: {}",
                    typed[1]
                );
            } else {
                assert_eq!(steps, ["wait:daemon-turn"; 3], "{case}");
                assert!(rig.typed().is_empty(), "{case}: nothing typed");
            }
            drop(rig);
            let _ = std::fs::remove_dir_all(&dir);
        }));
    }
    super::super::tests::side_by_side(cases);
}

/// Negative controls for the one-conversation shape: where the kernel names
/// no conversation, what the lane can never place holds the tab for as long
/// as it runs, look after look, and nothing is typed: two roots and two TUIs
/// with a running subagent of the other conversation (its root idle) or of
/// this tab's own (another conversation idle beside it), since a subagent is
/// never placed; and a second root running with this TUI the daemon's only
/// client (a thread no tab shows, or this tab's own while its screen shows
/// another thread), not provably another session's. Each waits
/// `daemon-busy`, worded without presuming whose turn it is. With the marks
/// gone, two roots name nothing: the `/exit` waits `no-shell-integration`
/// even when every thread idles.
#[test]
fn what_the_lane_never_places_holds_the_tab_where_the_kernel_names_nothing() {
    const TUI2: u32 = 4_000_009;
    for case in ["theirs", "own", "alone"] {
        let (rig, dir, mut opts, mut k, target, tui) =
            served_current(&format!("np-{case}"), |_| {});
        opts.only_sid = Some(TAB.to_string());
        let mut tuis = vec![tui.clone()];
        match case {
            // Another tab's conversation's subagent.
            "theirs" => {
                another_session(&mut k, IDLE_LAST);
                subagent(&mut k, S4, T2, T2, BUSY_LAST);
            }
            // This tab's conversation's subagent, another conversation idle.
            "own" => {
                another_session(&mut k, IDLE_LAST);
                subagent(&mut k, S1, T1, T1, BUSY_LAST);
            }
            // A second root running, this TUI the daemon's only client.
            _ => another_session(&mut k, BUSY_LAST),
        }
        if case == "alone" {
            k.clients = Some(vec![TUI]);
        } else {
            k.clients = Some(vec![TUI, TUI2]);
            tuis.push(Tui {
                pid: TUI2,
                tab: "s-0ther0ther0ther0ther".to_string(),
                ..tui.clone()
            });
        }
        let (_, view) = daemon_pass(&opts, &k.home.clone(), &target, &tuis, &k).expect("a daemon");
        rig.settle(10);
        let both = Both {
            tab: &rig.kernel,
            daemon: &k,
        };
        let steps = three_looks(&rig, &opts, &tui, &target, &view, &both);
        assert_eq!(steps, ["wait:daemon-busy"; 3], "{case}");
        assert!(rig.typed().is_empty(), "{case}: nothing typed");
        assert!(
            !rig.state().still_busy.is_empty(),
            "{case}: the still run did find it running through 20 s"
        );
        drop(rig);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Two roots, the marks gone, every thread idle: nothing names the root.
    let (rig, dir, opts, mut k, target, tui) = served_current("np-nomk", |w: &mut World| {
        w.status = "OK schema=1 hold=0 hand=- integration=degraded".to_string();
        w.blocks1 = r#"{"blocks":[]}"#.to_string();
        w.blocks2 = r#"{"blocks":[]}"#.to_string();
    });
    another_session(&mut k, IDLE_LAST);
    subagent(&mut k, S1, T1, T1, IDLE_LAST);
    let (_, view) = daemon_pass(
        &opts,
        &k.home.clone(),
        &target,
        std::slice::from_ref(&tui),
        &k,
    )
    .expect("a daemon");
    rig.settle(10);
    let both = Both {
        tab: &rig.kernel,
        daemon: &k,
    };
    assert_eq!(
        visit(&opts, &tui, &target, Some(view), &both).step,
        "wait:no-shell-integration"
    );
    assert!(rig.typed().is_empty());
    drop(rig);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A turn begun since the sweep read the daemon holds the `/exit`: the gate
/// passed on the daemon as the pass read it (this tab's thread, the daemon's
/// only one, idle), and between that read and the `/exit` the thread began a
/// turn (a goal's next one begins within milliseconds of the last one's
/// end). The `/exit`'s last look reads the daemon again, finds this tab's own
/// thread running, and waits `changed`, typing nothing; so does a daemon it
/// can no longer read. Negative control: nothing begun since, and the same
/// visit moves.
#[test]
fn a_turn_begun_since_the_sweeps_read_holds_the_exit() {
    for case in ["idle", "began", "gone"] {
        let (rig, dir, opts, k, target, tui) =
            served_current(&format!("since-read-{case}"), |_| {});
        let (_, view) = daemon_pass(
            &opts,
            &k.home.clone(),
            &target,
            std::slice::from_ref(&tui),
            &k,
        )
        .expect("a daemon");
        assert_eq!(view.threads, vec![root_turn(T1, TurnState::Idle)]);
        rig.settle(10);
        match case {
            "began" => now_ends(&k, T1, BUSY_LAST),
            "gone" => std::fs::remove_file(k.home.join("app-server-daemon/daemon.pid"))
                .expect("the daemon's pid file"),
            _ => {}
        }
        let both = Both {
            tab: &rig.kernel,
            daemon: &k,
        };
        let r = visit(&opts, &tui, &target, Some(view), &both);
        if case == "idle" {
            assert_eq!(r.step, "done", "{:#?}", rig.asked());
        } else {
            assert_eq!(r.step, "wait:changed", "{case}: {r:?}");
            assert!(rig.typed().is_empty(), "{case}: nothing typed");
        }
        drop(rig);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// No shell integration, and the kernel names the thread
/// (`integration=degraded`, no blocks): the rows a daemon-mode client prints
/// as it leaves cannot be told from the prompt after them, but its daemon
/// holds one thread and this TUI is its one client, so that thread is this
/// TUI's, recorded before the `/exit` (its ledger row says so) and resumed.
/// Negative controls: a second thread on the daemon, or a second Codex
/// attached to it: the `/exit` waits `no-shell-integration`, typing nothing,
/// and the wait is a blocker whose clock stands through the other waits a
/// later look finds (a goal's `daemon-turn`), until a look finds the marks
/// back.
#[test]
fn without_shell_integration_the_kernel_names_the_thread_or_the_exit_waits() {
    let no_marks = |w: &mut World| {
        w.status = "OK schema=1 hold=0 hand=- integration=degraded".to_string();
        w.blocks1 = r#"{"blocks":[]}"#.to_string();
        w.blocks2 = r#"{"blocks":[]}"#.to_string();
    };
    // Three cases of their own, each waiting out a relaunch's fixed waits:
    // side by side.
    let mut cases: Vec<Box<dyn FnOnce() + Send + '_>> = Vec::new();
    cases.push(Box::new(move || {
        let (rig, dir, opts, k, target, tui) = served_current("kernel-thread", no_marks);
        let (_, view) = daemon_pass(
            &opts,
            &k.home.clone(),
            &target,
            std::slice::from_ref(&tui),
            &k,
        )
        .expect("a daemon");
        rig.settle(10);
        let both = Both {
            tab: &rig.kernel,
            daemon: &k,
        };
        let r = visit(&opts, &tui, &target, Some(view), &both);
        assert_eq!(r.step, "done", "{r:?}\n{:#?}", rig.asked());
        let typed = rig.typed();
        assert!(
            typed[1].contains(&format!("'resume' '{T1}'")),
            "{}",
            typed[1]
        );
        let ledger = std::fs::read_to_string(super::super::ledger_path(&opts)).unwrap_or_default();
        assert!(ledger.contains("named by the kernel"), "{ledger}");
        drop(rig);
        let _ = std::fs::remove_dir_all(&dir);
    }));
    for (name, second_thread, second_client) in [
        ("kernel-two-threads", true, false),
        ("kernel-two-clients", false, true),
    ] {
        cases.push(Box::new(move || {
            let (rig, dir, opts, mut k, target, tui) = served_current(name, no_marks);
            if second_thread {
                another_session(&mut k, IDLE_LAST);
            }
            if second_client {
                k.clients = Some(vec![TUI, 4_000_009]);
            }
            let (_, view) = daemon_pass(
                &opts,
                &k.home.clone(),
                &target,
                std::slice::from_ref(&tui),
                &k,
            )
            .expect("a daemon");
            rig.settle(10);
            let both = Both {
                tab: &rig.kernel,
                daemon: &k,
            };
            let r = visit(&opts, &tui, &target, Some(view.clone()), &both);
            assert_eq!(r.step, "wait:no-shell-integration", "{name}: {r:?}");
            assert!(rig.typed().is_empty(), "{name}");
            let st = rig.state();
            assert_eq!(
                (st.wait.as_str(), st.blocked.as_str()),
                ("no-shell-integration", "no-shell-integration")
            );
            // Another wait meanwhile keeps the blocker's clock; past
            // BLOCKED_AFTER_S the row is `blocked`.
            let mut st = rig.state();
            st.blocked_since -= super::super::upgrade_status::BLOCKED_AFTER_S;
            st.wait = "daemon-turn".to_string();
            st.wait_since = now_s();
            save(&opts, &key(TAB), &st);
            let row = super::super::rows(&opts)
                .into_iter()
                .find(|r| r.tab == TAB)
                .expect("its row");
            assert_eq!(
                row.stall(now_s()).as_deref(),
                Some("blocked:no-shell-integration"),
                "{name}"
            );
            // The marks back: the next look clears it.
            *rig.status_extra.lock().expect("status") = " integration=on".to_string();
            let r = visit(&opts, &tui, &target, Some(view), &both);
            assert_eq!(r.step, "done", "{name}: {r:?}");
            drop(rig);
            let _ = std::fs::remove_dir_all(&dir);
        }));
    }
    super::super::tests::side_by_side(cases);
}

/// A screen no read returns is recorded: the wait and its blocker's clock are
/// saved, and past BLOCKED_AFTER_S the row is `blocked:screen-unreadable`. A
/// later look that reads the screen clears it.
#[test]
fn an_unreadable_screen_is_recorded_and_blocks_after_half_an_hour() {
    let rig = Rig::new("unreadable", T1, false, |w| {
        w.before = "not a screen".to_string()
    });
    rig.settle(10);
    assert_eq!(rig.visit(daemon_current()).step, "wait:screen-unreadable");
    let mut st = rig.state();
    assert_eq!(
        (st.wait.as_str(), st.blocked.as_str()),
        ("screen-unreadable", "screen-unreadable")
    );
    st.blocked_since -= super::super::upgrade_status::BLOCKED_AFTER_S;
    save(&rig.opts, &key(TAB), &st);
    let row = super::super::rows(&rig.opts)
        .into_iter()
        .find(|r| r.tab == TAB)
        .expect("its row");
    assert_eq!(
        row.stall(now_s()).as_deref(),
        Some("blocked:screen-unreadable")
    );
    // Negative control: a readable screen at the next look ends it.
    let readable = Rig::new("unreadable-then-read", T1, false, |_| {});
    let mut st = rig.state();
    st.seq_since_s = now_s() - 60;
    st.last_seq = 10;
    save(&readable.opts, &key(TAB), &st);
    let _ = readable.visit(daemon_current());
    assert_eq!(readable.state().blocked, "", "cleared by a look that read");
}

// The goal pause through the lane's own driver.
#[path = "upgrade_goal_drive_tests.rs"]
mod goal;

// Codex's usage wall and the reset it names (design record C9's Codex half).
#[path = "upgrade_codex_limit_tests.rs"]
mod limit_wall;

// ------------------------------------------------------ relaunch on exit
//
// A Codex that crashed out of its tab is relaunched on its thread, as a
// Claude Code is. A SIGKILLed TUI's command block reads `exit 137` and prints
// no hint; a `/exit` reads `exit 0` and prints its hint; an embedded TUI's
// `/exit` removes its thread's writer lock, and SIGKILL, SIGTERM, SIGINT and
// SIGHUP all leave it, so the lock is no word on the exit and the shell's
// status alone decides.

/// A Codex TUI that ran in the rig's tab, as the relaunch's snapshot read it
/// while it ran: `thread` the one it held (`embedded`) or named.
fn crashed_snap(
    rig: &Rig,
    thread: Option<&str>,
    embedded: bool,
) -> crate::harness::relaunch::Snapshot {
    crate::harness::relaunch::Snapshot {
        tab: TAB.to_string(),
        pid: TUI,
        start: "Sat Sep 27 14:19:56 2026".to_string(),
        shell: SHELL,
        program: rig.target.exe.clone(),
        argv: rig.tui.argv.clone(),
        session: thread.map(str::to_string),
        cwd: "/w".to_string(),
        version: Some("0.157.0".to_string()),
        codex: Some(crate::harness::relaunch::CodexRun {
            home: rig.tui.home.clone(),
            embedded,
            started_ms: 0,
            ambiguous: false,
            // The command block that ran it ([`blocks_after`]'s `codex`).
            block: Some(7),
        }),
        // The rig's shell, read as the run's snapshot reads it.
        dialect: shell_dialect(SHELL, &rig.kernel, Vec::new),
        login: None,
        home: None,
        claude_dir: None,
    }
}

/// The rig's tab after a crash: the shell back, the command block that ran
/// the TUI finished with `exit` and no hint under it (`zsh: killed …`).
fn after_crash(w: &mut World, exit: i64) {
    w.after_exit = shell_after(&["zsh: killed     codex"]);
    w.block_out = vec!["zsh: killed     codex".to_string()];
    let (b2, b1) = blocks_after(1);
    w.blocks2 = b2.replace(r#""exit":0"#, &format!(r#""exit":{exit}"#));
    w.blocks1 = b1;
}

/// A restart whose resume line was never typed, overtaken by a Codex a
/// person started by hand in its tab: that process's exit closes it as
/// resumed elsewhere, never as aterm's relaunch quitting (Claude Code's rule
/// too). NEGATIVE CONTROL: a record of the same phase relaunched closes as
/// the relaunched Codex exiting.
#[test]
fn a_hand_resumed_codex_closes_a_restart_as_resumed_elsewhere() {
    use crate::harness::relaunch::{ExitCause, ExitRecord};
    for (phase, want) in [
        (Phase::Exiting { at_s: now_s() }, "failed:resumed-elsewhere"),
        (
            Phase::Relaunched { at_s: now_s() },
            "failed:exited-before-continuing",
        ),
    ] {
        let rig = Rig::new("hand-resumed", T2, true, |w| after_crash(w, 0));
        rig.kernel.exited.store(true, Ordering::SeqCst);
        let st = St {
            phase: phase.clone(),
            pid: TUI + 1,
            ..exiting(&rig, T2)
        };
        save(&rig.opts, &key(TAB), &st);
        let snap = crashed_snap(&rig, Some(T2), true);
        let r = after_exit(
            &rig.opts,
            &snap,
            &ExitRecord::Removed,
            false,
            ExitCause::Exit,
            &rig.kernel,
        );
        assert_eq!(r.step, "ended:graceful-exit", "{phase:?}");
        assert!(
            rig.ledger_steps().contains(&want.to_string()),
            "{:#?}",
            rig.ledger_steps()
        );
        assert!(rig.typed().is_empty(), "{:#?}", rig.typed());
    }
}

/// THE EXPLICIT RESTART IS OFFERED ONLY WHERE THIS LANE BRINGS BACK WHAT IT
/// ENDS ([`crate::harness::relaunch::carries_restart`]; review of
/// `fix/restart-advice`, 2026-10-08): a daemon client whose thread the
/// snapshot could not name — none begun, its daemon unreadable, or another
/// client in its directory (`ambiguous`) — is read again at exit, and a read
/// that fails refuses its relaunch with nothing typed, leaving the Codex the
/// act ended at its shell. The predicate answers `false` for both, so the act
/// refuses it (`not-relaunchable`) and no word names it. A thread named — the
/// `exit-stall` case of `a_crashed_codex_is_relaunched_on_its_thread_and_told_why`,
/// relaunched on it — or held answers `true`.
#[test]
fn the_restart_is_carried_only_for_a_codex_whose_thread_is_settled() {
    use crate::harness::relaunch::{ExitCause, ExitRecord, carries_restart};
    let rig = Rig::new("asked-unnamed", T2, false, |w| after_crash(w, 143));
    rig.kernel.exited.store(true, Ordering::SeqCst);
    for ambiguous in [false, true] {
        let mut snap = crashed_snap(&rig, None, false);
        if let Some(run) = snap.codex.as_mut() {
            run.ambiguous = ambiguous;
        }
        assert!(!carries_restart(&snap), "ambiguous={ambiguous}");
        let r = after_exit(
            &rig.opts,
            &snap,
            &ExitRecord::Removed,
            false,
            ExitCause::Asked,
            &rig.kernel,
        );
        assert!(
            r.step.starts_with("refused:"),
            "ambiguous={ambiguous}: {r:?}"
        );
        assert!(rig.typed().is_empty(), "{:#?}", rig.typed());
    }
    assert!(
        carries_restart(&crashed_snap(&rig, Some(T2), false)),
        "named"
    );
    assert!(carries_restart(&crashed_snap(&rig, None, true)), "held");
}

/// …AND ONLY WHERE THE LANE'S PLAN WOULD WRITE ITS LINE
/// ([`crate::harness::relaunch::carries_restart`]; review of 0ce4678a5,
/// 2026-10-09). A Codex whose thread was settled but which was launched with
/// a flag its resume cannot carry (`--worktree`, `--remote`) or that the
/// rewrite does not know, with a kept flag no line can hold (a control
/// character), or in a shell no line is written for (`sh`, tcsh) was offered
/// the restart and carried it: the act's signals ended it, and its relaunch
/// refused (`failed:argv:…`, `failed:line:…`) or waited for ever
/// (`wait:shell-dialect`), nothing typed. Each snapshot — the run's own
/// ([`snapshot`]), its shell's dialect read as the plan reads it — now
/// answers `false`, paired with the lane's word on that exit. CONTROL: the
/// same Codex launched plain in a zsh tab is carried, and its exit relaunched
/// on its thread.
#[test]
fn the_restart_is_carried_only_where_the_codex_lanes_plan_writes_its_line() {
    use crate::harness::relaunch::{ExitCause, ExitRecord, carries_restart};
    // (rig, embedded, the shell, the launch's words after its program, the
    // shell's dialect read, the lane's word on the exit)
    type Case = (
        &'static str,
        bool,
        &'static str,
        &'static [&'static str],
        Option<Dialect>,
        &'static str,
    );
    let cases: [Case; 7] = [
        (
            "worktree",
            true,
            "/bin/zsh",
            &["--worktree"],
            Some(Dialect::Zsh),
            "failed:argv:not-resumable --worktree",
        ),
        (
            "unknown-flag",
            true,
            "/bin/zsh",
            &["--full-auto"],
            Some(Dialect::Zsh),
            "failed:argv:unknown-flag --full-auto",
        ),
        (
            "remote",
            false,
            "/bin/bash",
            &["--remote", "ws://h:1", "resume", T2],
            Some(Dialect::Bash),
            "failed:argv:not-resumable --remote",
        ),
        (
            "control-char",
            true,
            "/bin/zsh",
            &["-c", "notice=\"a\u{7}b\""],
            Some(Dialect::Zsh),
            "failed:line:a control character in the line",
        ),
        ("sh", true, "/bin/sh", &[], None, "wait:shell-dialect"),
        ("tcsh", true, "/bin/tcsh", &[], None, "wait:shell-dialect"),
        // CONTROL.
        (
            "plain",
            true,
            "/bin/zsh",
            &[],
            Some(Dialect::Zsh),
            "continued",
        ),
    ];
    super::super::tests::side_by_side(
        cases
            .into_iter()
            .map(|(name, embedded, shell, rest, dialect, want)| {
                Box::new(move || {
                    let mut rig = Rig::new(&format!("carry-{name}"), T2, embedded, |w| {
                        after_crash(w, 143);
                    });
                    rig.kernel.shell_exe = shell;
                    rig.kernel.exe = Some(rig.target.exe.clone());
                    let argv: Vec<String> = std::iter::once(rig.tui.argv[0].as_str())
                        .chain(rest.iter().copied())
                        .map(str::to_string)
                        .collect();
                    let args = atpkg::caller_shell::ProcArgs {
                        exec_path: argv[0].clone(),
                        argv,
                        env: Vec::new(),
                    };
                    let mut snap = snapshot(
                        &rig.opts,
                        TAB,
                        TUI,
                        "Sat Sep 27 14:19:56 2026".to_string(),
                        SHELL,
                        &args,
                        &rig.kernel,
                    )
                    .expect("the run's snapshot");
                    // The rig's build has no manifest to read its version
                    // from ([`cx::package_version`]); the carry-on names it.
                    snap.version = Some("0.157.0".to_string());
                    assert_eq!(snap.session.as_deref(), Some(T2), "{name}: settled");
                    assert_eq!(snap.dialect, dialect, "{name}: {shell}");
                    assert_eq!(carries_restart(&snap), want == "continued", "{name}");
                    rig.kernel.exited.store(true, Ordering::SeqCst);
                    let r = after_exit(
                        &rig.opts,
                        &snap,
                        &ExitRecord::Removed,
                        false,
                        ExitCause::Asked,
                        &rig.kernel,
                    );
                    assert_eq!(r.step, want, "{name}: {r:?}");
                    let typed = rig.typed();
                    if want == "continued" {
                        assert_eq!(typed.len(), 2, "{typed:#?}");
                        assert!(
                            typed[0].contains(&format!(
                                "{}' 'resume' '{T2}'",
                                rig.target.exe.display()
                            )),
                            "relaunched on its thread: {typed:#?}"
                        );
                        assert!(
                            typed[1].contains("Restarted: someone asked aterm to restart Codex"),
                            "the restart's carry-on: {typed:#?}"
                        );
                    } else {
                        assert!(typed.is_empty(), "{name}: {typed:#?}");
                    }
                }) as Box<dyn FnOnce() + Send>
            })
            .collect(),
    );
}

/// A crashed embedded Codex (SIGKILL: `exit 137`, its thread's lock left
/// behind) is relaunched on the thread it held (`<build> resume <thread>`,
/// the build it ran with `[harness] upgrade` off) and, its new TUI holding
/// the thread again, told why (the relaunch's carry-on, not the upgrade's).
/// Negative controls: the same exit read as its own (`exit 0`, a `/exit`) or
/// someone's signal, and an exit no word came for, are left alone with
/// nothing typed, unless the stall's remedy ended it (U1), or aterm itself
/// ended while it ran and the next launch reopened its tab (its hangup read
/// as its own orderly exit, which it was not).
#[test]
fn a_crashed_codex_is_relaunched_on_its_thread_and_told_why() {
    use crate::harness::relaunch::{ExitCause, ExitRecord};
    // Six cases, each a rig of its own whose relaunches each wait out the
    // shell's prompt (600 ms) and the new TUI's first idle: side by side
    // (4.3 s one after another, measured 2026-10-05).
    super::super::tests::side_by_side(vec![
        Box::new(|| {
            let rig = Rig::new("exit-crash", T2, true, |w| after_crash(w, 137));
            rig.kernel.exited.store(true, Ordering::SeqCst);
            let snap = crashed_snap(&rig, Some(T2), true);
            let r = after_exit(
                &rig.opts,
                &snap,
                &ExitRecord::Crashed,
                false,
                ExitCause::Exit,
                &rig.kernel,
            );
            assert_eq!(r.step, "continued", "{r:?}\n{:#?}", rig.asked());
            let typed = rig.typed();
            assert_eq!(typed.len(), 2, "{typed:#?}");
            assert!(
                typed[0].contains(&format!("{}' 'resume' '{T2}'", rig.target.exe.display())),
                "the build it ran, on its thread: {}",
                typed[0]
            );
            assert!(
                typed[1].contains("[aterm harness] Relaunched: Codex exited without anyone asking"),
                "the relaunch's carry-on: {}",
                typed[1]
            );
            let st = rig.state();
            assert_eq!((st.mode.as_str(), st.thread.as_str()), ("embedded", T2));
            assert_eq!(st.cause, crate::harness::relaunch::CAUSE_EXIT);
            assert!(!rig.asked().iter().any(|l| l.contains("signal")));
        }),
        Box::new(|| {
            // Negative controls: its own exit, someone's signal, no word at all.
            for (left, want) in [
                (ExitRecord::Removed, "ended:graceful-exit"),
                (ExitRecord::Unread, "ended:exit-unwitnessed"),
            ] {
                let rig = Rig::new("exit-left", T2, true, |w| {
                    after_crash(w, 0);
                    // No word at the attempt either: the prompt is not back.
                    if left == ExitRecord::Unread {
                        w.blocks2 = blocks_after(1).1;
                    }
                });
                rig.kernel.exited.store(true, Ordering::SeqCst);
                let snap = crashed_snap(&rig, Some(T2), true);
                let r = after_exit(&rig.opts, &snap, &left, false, ExitCause::Exit, &rig.kernel);
                assert_eq!(r.step, want);
                assert!(rig.typed().is_empty(), "{:#?}", rig.typed());
            }
        }),
        Box::new(|| {
            // …unless the stall's remedy ended it: relaunched, told so.
            let rig = Rig::new("exit-stall", T2, true, |w| after_crash(w, 143));
            rig.kernel.exited.store(true, Ordering::SeqCst);
            let snap = crashed_snap(&rig, Some(T2), true);
            assert!(crate::harness::relaunch::carries_restart(&snap));
            let r = after_exit(
                &rig.opts,
                &snap,
                &ExitRecord::Removed,
                false,
                ExitCause::Stall,
                &rig.kernel,
            );
            assert_eq!(r.step, "continued", "{r:?}");
            assert!(rig.typed()[1].contains("Relaunched: Codex stopped reading its input"));
        }),
        Box::new(|| {
            // …or aterm's own end took its tab: relaunched in the reopened one, told
            // so (the continuation is the relaunch's, never the upgrade's).
            let rig = Rig::new("exit-host", T2, true, |w| after_crash(w, 129));
            rig.kernel.exited.store(true, Ordering::SeqCst);
            let snap = crashed_snap(&rig, Some(T2), true);
            let r = after_exit(
                &rig.opts,
                &snap,
                &ExitRecord::Unread,
                false,
                ExitCause::HostEnded,
                &rig.kernel,
            );
            assert_eq!(r.step, "continued", "{r:?}");
            assert!(
                rig.typed()[1].contains("Relaunched: aterm ended while this session ran"),
                "{:#?}",
                rig.typed()
            );
            assert_eq!(rig.state().cause, crate::harness::relaunch::CAUSE_HOST);
        }),
        Box::new(|| {
            // The crash tried again once the TUI its relaunch brought up and handed
            // back is gone before its continuation: closed and relaunched afresh — carried
            // on, the try waited 90 s on a dead TUI, the harness's hand on the tab,
            // until the record went stale. NEGATIVE CONTROL: while that TUI lives, a
            // try finds it again and types nothing.
            let mut rig = Rig::new("exit-landed-gone", T2, true, |w| after_crash(w, 137));
            rig.opts.hand_back = true;
            rig.kernel.exited.store(true, Ordering::SeqCst);
            let snap = crashed_snap(&rig, Some(T2), true);
            let again = |rig: &Rig| {
                after_exit(
                    &rig.opts,
                    &snap,
                    &ExitRecord::Crashed,
                    false,
                    ExitCause::Exit,
                    &rig.kernel,
                )
            };
            assert_eq!(again(&rig).step, "adopted");
            let lines = rig.typed().len();
            assert_eq!(
                (again(&rig).step, rig.typed().len()),
                ("adopted".to_string(), lines),
                "found again"
            );
            rig.kernel.relaunched.store(false, Ordering::SeqCst);
            assert_eq!(
                (again(&rig).step, rig.typed().len()),
                ("adopted".to_string(), lines + 1),
                "relaunched afresh: {:#?}",
                rig.ledger_steps()
            );
        }),
        Box::new(|| {
            // The same crash with its word read only at the first attempt (`Unread`
            // at the exit: a park held the parser, or the prompt came back late): the
            // retry decides on
            // what that attempt read, the record its relaunch filed naming this TUI.
            // By then the tab's newest finished block is the relaunched TUI's own,
            // and read again the crash was dropped `exit-unwitnessed`, nothing typed
            // and nothing said.
            let mut rig = Rig::new("exit-late-landed", T2, true, |w| after_crash(w, 137));
            rig.opts.hand_back = true;
            rig.kernel.exited.store(true, Ordering::SeqCst);
            let late = |rig: &Rig| {
                let snap = crashed_snap(rig, Some(T2), true);
                after_exit(
                    &rig.opts,
                    &snap,
                    &ExitRecord::Unread,
                    false,
                    ExitCause::Exit,
                    &rig.kernel,
                )
                .step
            };
            assert_eq!(late(&rig), "adopted");
            let mut retry = Rig::new("exit-late-retry", T2, true, |w| {
                after_crash(w, 137);
                w.blocks2 = blocks_after(1)
                    .0
                    .replace(r#""id":7"#, r#""id":9"#)
                    .replace(r#""id":8"#, r#""id":10"#);
            });
            retry.opts.hand_back = true;
            retry.kernel.exited.store(true, Ordering::SeqCst);
            save(&retry.opts, &key(TAB), &rig.state());
            assert_eq!(
                (late(&retry), retry.typed().len()),
                ("adopted".to_string(), 1),
                "relaunched afresh: {:#?}",
                retry.ledger_steps()
            );
        }),
    ]);
}

/// TIER-1 of `HarnessCodexExitWitness` (aterm-spec
/// `harness_codex_exit_witness_model`): EVERY look of the model — a crash
/// (`exit 137`), its own `/exit` (`exit 0`, its thread lock removed) or
/// someone's signal (`exit 143`, the lock left, as measured), with the
/// shell's word drawn or not yet, or drawn and another command's block
/// newest since — replayed through the REAL `look_at_exit` and
/// `exit_record` over a stand-in tab (the lock on disk and the blocks drawn
/// as the model has them): the look keeps what the model's does (a crash,
/// no crash, or no word within its wait). And EVERY attempt — what the look
/// kept, the tab as it stands then — through the REAL `after_exit`, which
/// relaunches (`continued`) exactly where the model's `Decide` does: a word
/// that came after the look is read then, never another command's. NEGATIVE CONTROLS: where the
/// `Buggy = 1` model reads the lock (`LookAtLock` relaunches someone's
/// signal) or reads silence as a graceful exit (`LookAsClaude` leaves a
/// crash), the real look reads no word at all; where it decides on the look
/// alone (`DecideOnLook`) or on the newest block
/// (`DecideOnNewest`), the real attempt decides otherwise.
#[test]
fn tier1_the_real_codex_exit_read_conforms_to_the_model() {
    use crate::harness::relaunch::{ExitCause, ExitRecord, exit_record};
    use std::collections::{BTreeSet, VecDeque};
    let m = aterm_spec::derive::harness_codex_exit_witness_model();
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    // The tab the model's state draws: the TUI's exit status, its word drawn
    // or not, a later command's block newest (a failure), its lock.
    let draw = |name: String, exit: i64, word: bool, later: bool, lock: bool| {
        let status = match exit {
            1 => 137,
            2 => 0,
            _ => 143,
        };
        let rig = Rig::new(&name, T2, true, |w| {
            after_crash(w, status);
            if !word {
                w.blocks2 = blocks_after(1).1;
            } else if later {
                w.blocks2 = blocks_after(1)
                    .0
                    .replace(r#""id":7"#, r#""id":9"#)
                    .replace(r#""id":8"#, r#""id":10"#)
                    .replace(r#""exit":0"#, r#""exit":2"#)
                    .replace(r#""cmdline":"codex""#, r#""cmdline":"make""#);
            }
        });
        rig.kernel.exited.store(true, Ordering::SeqCst);
        if lock {
            let locks = rig.tui.home.join("thread-writer-locks");
            std::fs::write(locks.join(format!("{T2}.lock")), "").expect("the lock on disk");
        }
        rig
    };
    let kept = |kept: i64| match kept {
        1 => ExitRecord::Crashed,
        2 => ExitRecord::Removed,
        _ => ExitRecord::Unread,
    };
    let mut seen = BTreeSet::new();
    let mut attempted = BTreeSet::new();
    let mut queue = VecDeque::from([m.init_state()]);
    let (mut looks, mut attempts, mut caught) = (0, 0, 0);
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
        if st["seen"] == 0 {
            looks += 1;
            let word = st["word"] == 1;
            let later = st["later"] == 1;
            let look = if word && !later { "Look" } else { "LookGone" };
            let mut after = st.clone();
            assert!(m.fire(look, &mut after), "{look} at {st:?}");
            let rig = draw(
                format!("witness-look-{looks}"),
                st["exit"],
                word,
                later,
                st["lock"] == 1,
            );
            let snap = crashed_snap(&rig, Some(T2), true);
            let left = exit_record(
                || Some(look_at_exit(&rig.opts, &snap, &rig.kernel)),
                |_| true,
            );
            assert_eq!(left, kept(after["kept"]), "the look at {st:?}");
            // Negative controls: the Buggy looks at the same point.
            if !word {
                let mut at_lock = st.clone();
                assert!(buggy.fire("LookAtLock", &mut at_lock));
                let mut as_claude = st.clone();
                assert!(buggy.fire("LookAsClaude", &mut as_claude));
                assert_ne!(at_lock["kept"], 3, "the lock read as a word");
                assert_eq!(as_claude["kept"], 2, "silence read as a graceful exit");
                assert_eq!(left, ExitRecord::Unread, "the real look reads no word");
            }
            continue;
        }
        // An attempt on what the look kept: a word kept reads nothing of the
        // tab, which stands as the look found it; none kept reads it as it
        // stands now.
        let (word, later) = if st["kept"] <= 2 {
            (true, false)
        } else {
            (st["word"] == 1, st["later"] == 1)
        };
        if !attempted.insert((st["exit"], st["kept"], word, later)) {
            continue;
        }
        attempts += 1;
        let mut after = st.clone();
        assert!(m.fire("Decide", &mut after), "Decide at {st:?}");
        let rig = draw(
            format!("witness-attempt-{attempts}"),
            st["exit"],
            word,
            later,
            st["lock"] == 1,
        );
        let snap = crashed_snap(&rig, Some(T2), true);
        let r = after_exit(
            &rig.opts,
            &snap,
            &kept(st["kept"]),
            false,
            ExitCause::Exit,
            &rig.kernel,
        );
        let relaunched = r.step == "continued";
        assert_eq!(
            relaunched,
            after["decided"] == 1,
            "the attempt at {st:?}: {r:?}"
        );
        // Negative controls: the Buggy attempts where they decide otherwise.
        for mutant in ["DecideOnLook", "DecideOnNewest"] {
            let mut wrong = st.clone();
            if buggy.fire(mutant, &mut wrong) && wrong["decided"] != after["decided"] {
                assert_ne!(relaunched, wrong["decided"] == 1, "{mutant} at {st:?}");
                caught += 1;
            }
        }
    }
    // A crash, its own exit and someone's signal, each told, untold, and
    // told with another command run since.
    assert_eq!(looks, 9, "{seen:?}");
    // What the look kept, and — none kept — the tab at the attempt.
    assert_eq!((attempts, caught), (12, 4), "{seen:?}");
}

/// The exit's word is the shell's: the finished block that ends where the
/// entering prompt begins, its `exit` (none while the prompt is not back,
/// none for an older block); a crash is every status but the TUI's own `0`
/// and someone's HUP/INT/TERM.
#[test]
fn the_codex_exit_is_read_off_its_command_block() {
    let (b2, _) = blocks_after(1);
    assert_eq!(parse_exit_status("OK", &b2, None), Some(0));
    let killed = b2.replace(r#""exit":0"#, r#""exit":137"#);
    assert_eq!(parse_exit_status("OK", &killed, None), Some(137));
    // No entering prompt yet, an older block, an ERR: no word.
    let (_, entering_only) = blocks_after(1);
    assert_eq!(parse_exit_status("OK", &entering_only, None), None);
    let older = b2.replace(r#""end":4"#, r#""end":3"#);
    assert_eq!(parse_exit_status("OK", &older, None), None);
    assert_eq!(parse_exit_status("ERR no such session", "", None), None);
    for (exit, crashed) in [
        (0, false),
        (129, false),
        (130, false),
        (143, false),
        (137, true),
        (139, true),
        (134, true),
        (1, true),
        (101, true),
    ] {
        assert_eq!(cx::crashed_status(exit), crashed, "{exit}");
    }
}

/// A daemon-mode client that named no thread is resumed on the one thread its
/// daemon holds that was begun in its directory after it started (UUIDv7
/// ids). Negative controls: one begun before it started, or elsewhere, is not
/// it (`Ok(None)`); two that fit are never guessed between; another live
/// client of the daemon in the same directory makes the one thread that fits
/// no answer (it may be that client's), as does one whose directory cannot be
/// read, or clients that cannot be listed; with no daemon to read, nothing is
/// told.
#[test]
fn a_daemon_clients_thread_is_the_one_its_daemon_began_for_it() {
    let (_dir, _opts, mut k, _target) = daemon_home("daemon-thread", IDLE_LAST, 60);
    let home = k.home.clone();
    let meta = |cwd: &str, thread: &str| {
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{thread}","cwd":"{cwd}","originator":"codex-tui"}}}}"#
        )
    };
    let day = home.join("sessions/2026/09/25");
    std::fs::write(
        day.join(format!("rollout-2026-09-25T22-35-29-{T1}.jsonl")),
        format!("{}\n{IDLE_LAST}\n", meta("/w", T1)),
    )
    .expect("rollout");
    let born = cx::thread_created_ms(T1).expect("a UUIDv7");
    // The client itself among the daemon's, and one elsewhere: it.
    const ME: u32 = 7001;
    k.clients = Some(vec![ME, 7002]);
    k.cwds = vec![(ME, "/w".to_string()), (7002, "/elsewhere".to_string())];
    assert_eq!(
        daemon_thread(&home, "/w", born - 500, ME, &k),
        Ok(Some(T1.to_string()))
    );
    // Negative controls: another client in the same directory, one whose
    // directory is unreadable, clients that cannot be listed.
    k.cwds[1].1 = "/w".to_string();
    assert_eq!(
        daemon_thread(&home, "/w", born - 500, ME, &k),
        Err("thread-ambiguous")
    );
    k.cwds.truncate(1);
    assert_eq!(
        daemon_thread(&home, "/w", born - 500, ME, &k),
        Err("thread-ambiguous")
    );
    k.clients = None;
    assert_eq!(
        daemon_thread(&home, "/w", born - 500, ME, &k),
        Err("clients-unreadable")
    );
    k.clients = Some(vec![ME]);
    // Negative controls: begun before the client started, or elsewhere.
    assert_eq!(daemon_thread(&home, "/w", born + 5_000, ME, &k), Ok(None));
    assert_eq!(
        daemon_thread(&home, "/elsewhere", born - 500, ME, &k),
        Ok(None)
    );
    // A second thread in the same directory, begun after the client too.
    let other = T2;
    std::fs::write(
        day.join(format!("rollout-2026-09-25T22-35-30-{other}.jsonl")),
        format!("{}\n", meta("/w", other)),
    )
    .expect("rollout");
    k.open.push(
        home.join("thread-writer-locks")
            .join(format!("{other}.lock")),
    );
    assert_eq!(
        daemon_thread(
            &home,
            "/w",
            born.min(cx::thread_created_ms(other).unwrap()) - 500,
            ME,
            &k
        ),
        Err("thread-ambiguous")
    );
    // No daemon: nothing told.
    std::fs::remove_file(home.join("app-server-daemon/daemon.pid")).expect("rm pid");
    assert_eq!(
        daemon_thread(&home, "/w", born - 500, ME, &k),
        Err("no-daemon")
    );
}

// ---------------------------------------------------------------- the prune

/// Opts for a prune over `dir`'s state, nothing live about it.
fn prune_opts(dir: &Path) -> Opts {
    Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 0,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
    }
}

/// `st` filed as `opts`'s record `name`, last written `ago` seconds before now.
fn filed_as(opts: &Opts, name: &str, st: &St, ago: u64) -> PathBuf {
    save(opts, name, st);
    let path = state_dir(opts).join(format!("{name}.json"));
    let when = std::time::SystemTime::now() - Duration::from_secs(ago);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|f| f.set_modified(when))
        .expect("mtime");
    path
}

/// A Codex record of `tab` in `phase` (with the owner's `request`), filed as
/// `codex-<tab>.json` and last written `ago` seconds before now.
fn filed(opts: &Opts, tab: &str, phase: Phase, request: Request, ago: u64) -> PathBuf {
    let st = St {
        agent: Agent::Codex,
        tab: tab.to_string(),
        phase,
        request,
        from: "0.157.0".to_string(),
        to: "0.158.0".to_string(),
        ..St::default()
    };
    filed_as(opts, &key(tab), &st, ago)
}

/// A rendezvous registry under `dir` naming `live` (`graph/<sid>`, the
/// entries `aterm-gui`'s `proxy::write_graph_entry` writes).
fn registry(dir: &Path, live: &[&str]) -> PathBuf {
    let graph = dir.join("run").join("aterm").join("graph");
    std::fs::create_dir_all(&graph).expect("graph");
    for sid in live {
        std::fs::write(graph.join(sid), "sock /x/aterm-1.sock\nnonce 00\npid 1\n").expect("entry");
    }
    graph
}

/// A record's projection onto `HarnessUpgradeStateCollection`, from what the
/// test set up (never from the code under test): its tab alive or proven
/// gone, its round's stage, whether its week has passed. `aged` stays at its
/// initial 0: `Prune`'s guard never reads it.
fn collection_state(
    model: &aterm_spec::derive::Model,
    tab_lives: bool,
    phase: &Phase,
    request: &Request,
    week_old: bool,
) -> aterm_spec::interp::State {
    let mut s = model.init_state();
    let stage = match phase {
        Phase::Exiting { .. } | Phase::Relaunched { .. } => 1,
        Phase::Done | Phase::Failed(_) => 2,
        Phase::Pending if *request == Request::None => 2,
        _ => 0,
    };
    s.insert("tab", i64::from(tab_lives));
    s.insert("stage", stage);
    s.insert("old", i64::from(week_old));
    s.insert("kept", 1);
    s
}

/// One pass's decision on one record held to `Prune`'s guard, both ways: a
/// removal the model does not allow breaks safety, and a record the model
/// lets go that a pass with a clean reading kept breaks the prune's weak
/// fairness (every such pass takes it). After a removal every invariant
/// holds.
fn collection_conforms(
    model: &aterm_spec::derive::Model,
    before: &aterm_spec::interp::State,
    removed: bool,
) -> Result<(), String> {
    let enabled = model.action_enabled("Prune", before);
    if removed != enabled {
        return Err(format!(
            "the real prune {} a record the model {}: {before:?}",
            if removed { "removed" } else { "kept" },
            if enabled { "lets go" } else { "keeps" }
        ));
    }
    if removed {
        let mut after = before.clone();
        assert!(model.fire("Prune", &mut after));
        for inv in &model.invariants {
            if !model.check_invariant(inv.name, &after) {
                return Err(format!(
                    "`{}` broken after the removal: {after:?}",
                    inv.name
                ));
            }
        }
    }
    Ok(())
}

/// Tier-1: the real `prune`, reading the real registry over records on disk,
/// bound to the derived `HarnessUpgradeStateCollection`. A closed tab's
/// settled record goes once it has sat a week (done, failed, or pending with
/// no word of the owner's), and nothing else does. Negative controls: a live
/// tab's record (the host's own, and another window's that only the registry
/// names) and a restart in flight survive, however old; the model's
/// `PruneLiveTab` member would take the live one; and the Claude-only
/// `prune_states` is caught keeping a record the model lets go.
#[test]
#[allow(clippy::too_many_lines)]
fn the_prune_collects_a_closed_tabs_settled_codex_record_and_nothing_else() {
    const HERE: &str = "s-0a0a0a0a0a0a"; // the host's own live tab
    const THERE: &str = "s-0b0b0b0b0b0b"; // another window's: the registry alone names it
    let old = super::super::STATE_KEEP_S + 3_600;
    let model = aterm_spec::derive::harness_upgrade_state_collection_model();
    let dir = scratch("codex-prune");
    let opts = prune_opts(&dir);
    let graph = registry(&dir, &[HERE, THERE]);
    let known_live = vec![HERE.to_string()];

    // (tab, its tab lives, phase, the owner's word, sat a week, removed?)
    let skip = || Request::Skip("0.158.0".to_string());
    let failed = |why: &str| Phase::Failed(why.to_string());
    let none = || Request::None;
    #[rustfmt::skip]
    let cases: Vec<(&str, bool, Phase, Request, bool, bool)> = vec![
        ("s-dead00000001", false, Phase::Done,                         none(), true,  true),
        ("s-dead00000002", false, failed("unanswered"),                none(), true,  true),
        ("s-dead00000003", false, Phase::Pending,                      none(), true,  true),
        (HERE,             true,  Phase::Done,                         none(), true,  false),
        (THERE,            true,  failed("no-resume"),                 none(), true,  false),
        ("s-dead00000004", false, Phase::Exiting { at_s: 5 },          none(), true,  false),
        ("s-dead00000005", false, Phase::Relaunched { at_s: 5 },       none(), true,  false),
        ("s-dead00000006", false, Phase::Announced { at_s: 5, asks: 1 }, none(), true, false),
        ("s-dead00000007", false, Phase::Pending,                      skip(), true,  false),
        ("s-dead00000008", false, Phase::Done,                         none(), false, false),
    ];
    let paths: Vec<PathBuf> = cases
        .iter()
        .map(|(tab, _, phase, request, week_old, _)| {
            let ago = if *week_old { old } else { 0 };
            filed(&opts, tab, phase.clone(), request.clone(), ago)
        })
        .collect();
    // Beside them, outside the model's projection and never the Codex
    // prune's: a Claude record filed under a Codex name, a Codex record whose
    // name and tab disagree, a Claude conversation's record, and a file that
    // is no record.
    let done = |agent: Agent, tab: &str| St {
        agent,
        tab: tab.to_string(),
        phase: Phase::Done,
        ..St::default()
    };
    let strangers = [
        filed_as(
            &opts,
            &key("s-dead00000009"),
            &done(Agent::Claude, "s-dead00000009"),
            old,
        ),
        filed_as(
            &opts,
            &key("s-dead0000000a"),
            &done(Agent::Codex, "s-dead0000000b"),
            old,
        ),
        filed_as(
            &opts,
            "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0",
            &done(Agent::Claude, ""),
            old,
        ),
    ];
    let models = state_dir(&opts).join("models.json");
    std::fs::write(&models, "{}").expect("models");
    let now = now_s();

    // Nothing proves a tab gone, so nothing goes: a pass that knows no live
    // tab, a registry missing a tab the pass knows is live (another runtime
    // directory), no registry at all, a dry run, and a break of the agent's
    // background work, whatever they read.
    let unvouched = ["s-0c0c0c0c0c0c".to_string()];
    prune(&opts, now, registry_life(Some(&graph), &[]));
    prune(&opts, now, registry_life(Some(&graph), &unvouched));
    prune(&opts, now, registry_life(None, &known_live));
    for quiet in [
        Opts {
            dry_run: true,
            ..prune_opts(&dir)
        },
        Opts {
            background: true,
            ..prune_opts(&dir)
        },
    ] {
        prune(&quiet, now, registry_life(Some(&graph), &known_live));
    }
    assert!(
        paths.iter().chain(&strangers).all(|p| p.exists()) && models.exists(),
        "nothing proven gone, nothing gone"
    );
    let life = registry_life(Some(&graph), &known_live);
    assert_eq!(life(HERE), TabLife::Live, "the pass saw it live");
    assert_eq!(life(THERE), TabLife::Live, "the registry names it");
    assert_eq!(life("s-dead00000001"), TabLife::Gone);
    for odd in ["s-../../etc", "s-", "t-0a0a", "s-dead/0a0a"] {
        assert_eq!(life(odd), TabLife::Unknown, "{odd} is no tab id");
    }
    assert_eq!(
        registry_life(Some(&graph), &unvouched)("s-dead00000001"),
        TabLife::Unknown,
        "a registry that misses a live tab vouches for nothing"
    );

    // The Claude-only prune keeps a closed tab's settled week-old Codex
    // record, which the model lets go.
    super::super::prune_states(&opts, &[], now);
    let dead_done = collection_state(&model, false, &Phase::Done, &Request::None, true);
    assert!(paths[0].exists(), "the Claude lane passes Codex records by");
    assert!(
        collection_conforms(&model, &dead_done, !paths[0].exists()).is_err(),
        "the bind catches the lane that kept it for ever"
    );
    // (It took the settled Claude records: those are its own.)
    assert!(!strangers[0].exists() && !strangers[2].exists());
    // So the Claude record under a Codex name the Codex pass must pass by is
    // filed again here, after the Claude lane took the first: settled, a week
    // old, its tab proven gone, everything but a Codex record.
    let claude_under_codex = filed_as(
        &opts,
        &key("s-dead0000000c"),
        &done(Agent::Claude, "s-dead0000000c"),
        old,
    );

    // The pass.
    let befores: Vec<_> = cases
        .iter()
        .map(|(_, lives, phase, request, week_old, _)| {
            collection_state(&model, *lives, phase, request, *week_old)
        })
        .collect();
    prune(&opts, now, registry_life(Some(&graph), &known_live));
    for ((case, before), path) in cases.iter().zip(&befores).zip(&paths) {
        let removed = !path.exists();
        assert_eq!(
            removed, case.5,
            "{} {:?}: removed={removed}",
            case.0, case.2
        );
        collection_conforms(&model, before, removed)
            .unwrap_or_else(|why| panic!("{} {:?}: {why}", case.0, case.2));
    }
    assert!(
        strangers[1].exists(),
        "a record whose name and tab disagree"
    );
    assert!(
        claude_under_codex.exists(),
        "a Claude record is never the Codex prune's, whatever its name"
    );
    assert!(
        models.exists(),
        "a file that is no record is not the prune's"
    );

    // Negative controls: the live tabs' records and the restarts in flight
    // survive, and the model's `PruneLiveTab` member would take a live one.
    assert!(
        paths[3].exists() && paths[4].exists(),
        "live tabs' records stay"
    );
    assert!(
        paths[5].exists() && paths[6].exists(),
        "restarts in flight stay"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert!(buggy.action_enabled("PruneLiveTab", &befores[3]));
    assert!(!model.action_enabled("Prune", &befores[3]));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A registry speaks only for its own state root: the prune reads the
/// rendezvous registry only when it and the state root are both the person's
/// defaults or both private. A private runtime directory over the default
/// state root (a private instance with the person's `$HOME` and no state root
/// of its own) reads none of the person's tabs and would call each one gone.
#[test]
fn a_registry_vouches_only_for_a_state_root_of_its_own_kind() {
    for (private_registry, private_state, vouches) in [
        (false, false, true),
        (true, true, true),
        (true, false, false),
        (false, true, false),
    ] {
        assert_eq!(
            registry_vouches_for_state(private_registry, private_state),
            vouches,
            "registry private = {private_registry}, state private = {private_state}"
        );
    }
}

// ------------------------------------------------------------ the usage wall

/// A real Codex screen at a usage wall (`aterm_phase::codex::fixtures`,
/// 0.159.2) as the stand-in serves it: its rows, the cursor at the composer's
/// caret, column 2, where 0.159.2 draws the placeholder.
fn measured_screen(text: &str, seq: u64) -> String {
    served_screen(&aterm_phase::prompt::fixtures::screen(text), seq)
}

/// `rows` as the stand-in serves a Codex screen: the cursor at the composer's
/// caret, column 2.
fn served_screen(rows: &[String], seq: u64) -> String {
    let caret = aterm_phase::codex::composer(rows).expect("the screen's composer");
    let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
    screen_json(&refs, (caret, 2), seq)
}

/// The rows of a usage wall's block on a screen: from its `■` to the blank
/// row under it.
fn wall_block(rows: &[String], what: &str) -> std::ops::Range<usize> {
    let at = rows
        .iter()
        .position(|r| r.starts_with('■'))
        .unwrap_or_else(|| panic!("{what}: a wall block"));
    let end = (at..rows.len())
        .find(|&i| rows[i].trim().is_empty())
        .unwrap_or_else(|| panic!("{what}: a blank row under the wall block"));
    at..end
}

/// `rows` with the wall's block dropped and nothing else touched: the same
/// screen once the wall is gone, which is what every negative control below
/// is made of.
fn block_gone(rows: &[String], what: &str) -> Vec<String> {
    let mut gone = rows.to_vec();
    gone.drain(wall_block(rows, what));
    gone
}

/// [`measured_screen`] of the wall screen `text` with its block gone.
fn measured_screen_block_gone(text: &str, what: &str, seq: u64) -> String {
    let rows = aterm_phase::prompt::fixtures::screen(text);
    served_screen(&block_gone(&rows, what), seq)
}

/// The three panes the wall is fixtured mid-turn in, its row wrapped after
/// `purchase more`, after `or try again`, and not at all.
const WALL_MID_TURN: [(&str, &str); 3] = [
    (
        aterm_phase::codex::fixtures::WALL_MID_TURN_100X30_0_159,
        "100 columns",
    ),
    (
        aterm_phase::codex::fixtures::WALL_MID_TURN_0_159,
        "120 columns",
    ),
    (
        aterm_phase::codex::fixtures::WALL_MID_TURN_160X50_0_159,
        "160 columns",
    ),
];

/// The two wall screens once the live upgrade's notice was typed as a turn:
/// the turn's own end, and `codex resume` redrawing it.
const WALL_AT_THE_NOTICE: [(&str, &str); 2] = [
    (
        aterm_phase::codex::fixtures::WALL_AFTER_NOTICE_0_159,
        "after the notice's turn",
    ),
    (
        aterm_phase::codex::fixtures::WALL_RESUMED_0_159,
        "redrawn by codex resume",
    ),
];

/// The round's READY marker.
const READY: &str = "ATERM-UPGRADE-READY-0badf00d";

/// The turn `user` typed ended on the usage wall, as 0.159.2 writes it to the
/// rollout (ids dropped): the turn starts, the message is its user's, and it
/// completes carrying the wall's error.
fn walled_rollout(rig: &Rig, user: &str) {
    let user = aterm_json::to_string(&aterm_json::Value::from(user)).expect("text");
    let mut text = std::fs::read_to_string(&rig.rollout).expect("rollout");
    for line in [
        r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"2"}}"#.to_string(),
        format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":{user}}}]}}}}"#
        ),
        r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"2","last_agent_message":null,"error":{"message":"You’ve hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again later.","codex_error_info":"usage_limit_exceeded"}}}"#.to_string(),
    ] {
        text.push_str(&line);
        text.push('\n');
    }
    std::fs::write(&rig.rollout, text).expect("rollout");
    age(&rig.rollout);
}

/// The notice the round types, as the rollout of a conversation it met the
/// wall in holds it.
fn notice_text() -> String {
    cx::prepare_prompt(&v("0.157.0"), &v("0.157.1"), READY)
}

/// A round announced `ago_s` seconds back, on its `asks`-th notice.
fn announced_ago(rig: &Rig, ago_s: u64, asks: u32) -> St {
    St {
        phase: Phase::Announced {
            at_s: now_s() - ago_s,
            asks,
        },
        marker: READY.into(),
        ..rig.state()
    }
}

/// What the limit fact ([`upgrade::limited`]) says of one wall screen, and of
/// its controls: the wall screen `text` reads limited and reads no login
/// wall; the same screen with its block gone does not read limited; and the
/// same screen with the block naming a wall that is no limit (a full context,
/// a busy model) does not either, though the same reader reads it as a wall.
fn assert_the_wall_reads_limited_and_only_it_does(text: &str, what: &str) {
    let rows = aterm_phase::prompt::fixtures::screen(text);
    assert!(upgrade::limited(Agent::Codex, &rows), "{what}");
    assert!(!upgrade::login_wall(Agent::Codex, &rows), "{what}");
    let gone = block_gone(&rows, what);
    assert!(!upgrade::limited(Agent::Codex, &gone), "{what}: block gone");
    for other in [
        "■ Codex ran out of room in the model's context window. Start a new thread or clear earlier history before retrying.",
        "■ Selected model is at capacity. Please try a different model.",
    ] {
        let mut swapped = rows.clone();
        swapped.splice(wall_block(&rows, what), [other.to_string()]);
        assert!(
            aterm_phase::codex::wall(&swapped).is_some(),
            "{what}: the reader still reads a wall: {other}"
        );
        assert!(!upgrade::limited(Agent::Codex, &swapped), "{what}: {other}");
    }
}

/// The usage wall of a real Codex is the limit fact ([`upgrade::limited`],
/// which [`Facts::limited`] is read from): 0.159.2's `■ You’ve hit your usage
/// limit …` block as the last thing on the screen (after a turn that ran a
/// command, after a notice typed as a turn, redrawn by `codex resume`) in
/// every pane. Negative controls: the fresh idle screen; each wall screen
/// with its block gone; and each wall screen with the block naming a wall
/// that is no limit (a full context, a busy model), which the same reader
/// reads as a wall and the limit fact does not. All six screens come from a
/// loopback 429 and wear the wording that names no reset; the other wording
/// is [`the_usage_wall_of_an_earlier_codex_reads_limited_with_and_without_a_reset`].
#[test]
fn the_usage_wall_of_a_real_codex_reads_limited_on_every_screen_it_was_measured_on() {
    use aterm_phase::codex::fixtures as fx;
    for (text, what) in WALL_MID_TURN.iter().chain(WALL_AT_THE_NOTICE.iter()) {
        assert_the_wall_reads_limited_and_only_it_does(text, what);
    }
    let idle = aterm_phase::prompt::fixtures::screen(fx::IDLE_0_159);
    assert!(!upgrade::limited(Agent::Codex, &idle), "the idle control");
}

/// The usage wall reads limited whether or not it names a reset, on two
/// earlier screens: 0.157.1's real `HIT_LIMIT_NO_RESET` (a loopback 429: `try
/// again later`), and 0.156.1's `HIT_LIMIT`, hand-built from the binary's
/// strings (the Pro plan's text, `try again at Sep 25th, 2026 3:05 PM.`,
/// wrapped to column 0), since no real reset-bearing wall was captured. The
/// visit-level tests run on the 0.159.2 panes alone; the gate they pin is
/// this fact, so it must not depend on the reset. The same controls as the
/// 0.159.2 test.
#[test]
fn the_usage_wall_of_an_earlier_codex_reads_limited_with_and_without_a_reset() {
    use aterm_phase::codex::fixtures as fx;
    for (text, what, reset) in [
        (
            fx::HIT_LIMIT_NO_RESET,
            "0.157.1, measured, no reset named",
            false,
        ),
        (fx::HIT_LIMIT, "0.156.1, hand-built, a reset named", true),
    ] {
        assert_the_wall_reads_limited_and_only_it_does(text, what);
        let rows = aterm_phase::prompt::fixtures::screen(text);
        let wall = aterm_phase::codex::wall(&rows).expect(what);
        assert_eq!(wall.reset.is_some(), reset, "{what}: {wall:?}");
    }
}

/// A round whose notice was typed as a turn that the wall ended, the rollout
/// carrying that walled turn and the tab showing `screen`: what the sweep's
/// own earlier ticks leave, its screen quiet for a minute.
fn walled_notice_round(name: &str, screen: String) -> Rig {
    let rig = Rig::new(name, T2, true, |w| w.before = screen);
    walled_rollout(&rig, &notice_text());
    rig.settle(10);
    rig
}

/// An embedded conversation announced whose READY was answered, and settled,
/// hours ago, the tab coming to show `screen` at the next look.
fn ready_hours_ago_showing(name: &str, screen: String) -> Rig {
    let rig = embedded_ready(name, |_| {});
    rig.settle_keeping(&St {
        ready_since: now_s() - 3 * upgrade::REASK_S,
        ..rig.state()
    });
    *rig.shows.lock().expect("shows") = Some(screen);
    rig
}

/// A quiet Codex (`upgrade_ask` off: nothing asked, the `/exit` alone is the
/// move) showing `screen`, the rollout carrying a walled turn.
fn quiet_showing(name: &str, screen: String) -> Rig {
    let mut quiet = Rig::new(name, T2, true, |w| w.before = screen);
    quiet.opts.ask = false;
    walled_rollout(&quiet, &notice_text());
    quiet.settle(10);
    quiet
}

/// A Codex at its usage wall is never asked or re-asked, and its clocks hold:
/// what Codex takes typed at the wall is a turn that fails and stays in
/// history, so a notice per re-ask window would all arrive together, unread,
/// when the limit resets. The screen ([`Facts::limited`]) keeps them out: a
/// pending round in every pane types nothing; a round announced hours ago,
/// on each of its [`upgrade::MAX_ASKS`] asks, waits `limited` (no re-ask, no
/// give-up, no key), and its re-ask window is restarted at every look, so
/// the agent gets the whole of it once a turn gets past the wall. The
/// screen's word has no end here: the `■` block stays the last block until a
/// turn goes through (a person's, or the supervise loop's continue at the
/// reset). These screens name no reset and this is the ask flow, so nothing
/// lifts the wait; the one bound is the quiet upgrade's grace, which lifts a
/// wall that names its reset an hour after it (class C9 of
/// `docs/DESIGN-upgrade-never-stuck-2026-09-28.md`; pinned in
/// `upgrade_codex_limit_tests.rs`). Negative controls: the same rounds on the
/// wall's own screens with the block gone (the block the only difference)
/// type the notice, ask again on each ask and give up on the last; and a
/// fresh screen with no wall.
#[test]
fn a_codex_at_its_usage_wall_is_never_asked_or_re_asked_and_its_clocks_hold() {
    for (text, what) in WALL_MID_TURN {
        let rig = Rig::new("wall-pending", T2, true, |w| {
            w.before = measured_screen(text, 10);
        });
        walled_rollout(&rig, "TOOLTHEN429 please");
        rig.settle(10);
        let r = rig.visit(None);
        assert_eq!(r.step, "wait:limited", "{what}: {r:?}");
        assert_eq!(rig.state().phase, Phase::Pending, "{what}");
        assert!(rig.typed().is_empty(), "{what}: {:?}", rig.typed());
        assert!(keys(&rig).is_empty(), "{what}: {:?}", keys(&rig));
        // Control: this pane, this rollout, the block gone.
        let gone = Rig::new("wall-pending-gone", T2, true, |w| {
            w.before = measured_screen_block_gone(text, what, 10);
        });
        walled_rollout(&gone, "TOOLTHEN429 please");
        gone.settle(10);
        let r = gone.visit(None);
        assert_eq!(r.step, "announced:1", "{what}: block gone: {r:?}");
        assert_eq!(gone.typed().len(), 1, "{what}: block gone: the notice");
    }
    for (text, what) in WALL_AT_THE_NOTICE {
        let rig = walled_notice_round("wall-announced", measured_screen(text, 10));
        for asks in 1..=upgrade::MAX_ASKS {
            rig.settle_keeping(&announced_ago(&rig, 3 * upgrade::REASK_S, asks));
            let r = rig.visit(None);
            assert_eq!(r.step, "wait:limited", "{what}: asks {asks}: {r:?}");
            assert!(rig.typed().is_empty(), "{what}: {:?}", rig.typed());
            assert!(keys(&rig).is_empty(), "{what}: {:?}", keys(&rig));
            match rig.state().phase {
                Phase::Announced { at_s, asks: kept } => {
                    assert_eq!(kept, asks, "{what}: no ask spent");
                    assert!(
                        now_s().saturating_sub(at_s) < 5,
                        "{what}: the window restarted at this look, not {}s ago",
                        now_s().saturating_sub(at_s)
                    );
                }
                p => panic!("{what}: asks {asks}: {p:?}"),
            }
        }
        // Control: this screen, this rollout, the block gone — every ask
        // spent, in order, and the last one given up on.
        let gone = walled_notice_round(
            "wall-announced-gone",
            measured_screen_block_gone(text, what, 10),
        );
        for asks in 1..=upgrade::MAX_ASKS {
            gone.settle_keeping(&announced_ago(&gone, 3 * upgrade::REASK_S, asks));
            let r = gone.visit(None);
            if asks < upgrade::MAX_ASKS {
                assert_eq!(
                    r.step,
                    format!("announced:{}", asks + 1),
                    "{what}: block gone: asks {asks}: {r:?}"
                );
                assert_eq!(
                    gone.typed().len(),
                    asks as usize,
                    "{what}: block gone: asks {asks}: one notice per ask"
                );
            } else {
                assert_eq!(r.step, "gave-up", "{what}: block gone: asks {asks}: {r:?}");
                assert_eq!(
                    gone.typed().len(),
                    (upgrade::MAX_ASKS - 1) as usize,
                    "{what}: block gone: the last ask given up on, not repeated"
                );
            }
        }
    }
    // Controls: a screen with no wall, the same rounds.
    let clear = Rig::new("wall-clear", T2, true, |_| {});
    clear.settle(10);
    assert_eq!(clear.visit(None).step, "announced:1");
    assert_eq!(clear.typed().len(), 1, "the pending round types its notice");
    clear.settle_keeping(&announced_ago(&clear, upgrade::REASK_S + 5, 1));
    assert_eq!(clear.visit(None).step, "announced:2");
    assert_eq!(clear.typed().len(), 2, "the window run out: asked again");
    clear.settle_keeping(&announced_ago(
        &clear,
        upgrade::REASK_S + 5,
        upgrade::MAX_ASKS,
    ));
    assert_eq!(clear.visit(None).step, "gave-up");
    assert_eq!(
        clear.typed().len(),
        2,
        "the last ask given up on, not repeated"
    );
}

/// A Codex at its usage wall is never exited, asked or not: the restart would
/// drop the vendor's own wait and resume into the wall, so the gate every
/// restart passes ([`upgrade::gate_restart`], the reducer's `quiet_step`)
/// waits `limited` first, before READY, the ladder's settle or the owner's
/// notice. Asked (`upgrade_ask`): a notice typed, its READY answered and
/// settled, the wall on the screen: no `/exit`, no relaunch, and the READY's
/// clock restarted at the look. Quiet (the default): the same wall types
/// nothing at all. This also keeps [`carry_on`] off a wall in practice: a
/// relaunched Codex replays a wall only where one came up between the last
/// look and the `/exit`. Negative controls: each lane on the wall's own
/// screen with the block gone (the block the only difference) moves, the
/// asked one to `continued`, the quiet one to its `/exit`, as does each on a
/// fresh screen with no wall.
#[test]
fn a_codex_at_its_usage_wall_is_not_exited_whether_it_was_asked_or_not() {
    // Ten rigs of their own, the moving ones each waiting out a relaunch's
    // fixed waits (the shell's prompt, the new TUI's first idle): side by
    // side (3.7 s one after another, measured 2026-10-05).
    let mut cases: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
    for (text, what) in WALL_AT_THE_NOTICE {
        // Asked: the notice went out on a clear screen and was answered, hours
        // ago; the wall is what the tab shows at the next look.
        cases.push(Box::new(move || {
            let rig = ready_hours_ago_showing("wall-ready", measured_screen(text, 10));
            let r = rig.visit(None);
            assert_eq!(r.step, "wait:limited", "{what}: {r:?}");
            assert_eq!(rig.typed().len(), 1, "{what}: the notice, no `/exit`");
            assert!(!rig.asked().iter().any(|l| l.contains("signal")), "{what}");
            assert!(
                now_s().saturating_sub(rig.state().ready_since) < 5,
                "{what}: the READY's clock restarted at this look: {}",
                rig.state().ready_since
            );
        }));
        // Quiet: nothing asked, the exit alone is the move.
        cases.push(Box::new(move || {
            let quiet = quiet_showing("wall-quiet", measured_screen(text, 10));
            let r = quiet.visit(None);
            assert_eq!(r.step, "wait:limited", "{what}: {r:?}");
            assert!(quiet.typed().is_empty(), "{what}: {:?}", quiet.typed());
            assert!(keys(&quiet).is_empty(), "{what}: {:?}", keys(&quiet));
        }));
        // Controls: the same two lanes, the block gone.
        cases.push(Box::new(move || {
            let gone = ready_hours_ago_showing(
                "wall-ready-gone",
                measured_screen_block_gone(text, what, 10),
            );
            let r = gone.visit(None);
            assert_eq!(r.step, "continued", "{what}: block gone: {r:?}");
            assert_eq!(
                gone.typed().len(),
                4,
                "{what}: block gone: notice, /exit, relaunch, continuation: {:?}",
                gone.typed()
            );
        }));
        cases.push(Box::new(move || {
            let quiet = quiet_showing(
                "wall-quiet-gone",
                measured_screen_block_gone(text, what, 10),
            );
            quiet.visit(None);
            assert!(
                quiet.typed().iter().any(|l| l.ends_with(" /exit")),
                "{what}: block gone: the quiet lane's `/exit`: {:?}",
                quiet.typed()
            );
        }));
    }
    // Controls: the same two lanes with no wall on the screen.
    cases.push(Box::new(|| {
        let clear = embedded_ready("wall-ready-clear", |_| {});
        assert_eq!(clear.visit(None).step, "continued");
    }));
    cases.push(Box::new(|| {
        let mut quiet = Rig::new("wall-quiet-clear", T2, true, |_| {});
        quiet.opts.ask = false;
        quiet.settle(10);
        quiet.visit(None);
        assert!(
            quiet.typed().iter().any(|l| l.ends_with(" /exit")),
            "the quiet lane's `/exit`: {:?}",
            quiet.typed()
        );
    }));
    super::super::tests::side_by_side(cases);
}

/// A CRASHED CODEX IS RELAUNCHED ON ITS OWN PROGRAM WHILE THE OWNER'S WORD
/// HOLDS THE MANAGED BUILD (the Codex half): a `Skip` of
/// that build, or a `Not today` standing, kept the visit off it but not the
/// relaunch on exit, which moved the crashed Codex straight onto it.
/// NEGATIVE CONTROLS: no word, a deferral run out, a skip of another build
/// and `--now` move it; `[harness] upgrade = false` and an older managed
/// build never do.
#[test]
fn a_crashed_codex_is_relaunched_off_the_build_its_owner_holds() {
    let now = 1_790_000_000_u64;
    let target = |v: &str| Target {
        twin: PathBuf::from("/p/agents/codex"),
        exe: PathBuf::from("/p/store/codex/1/bin/codex"),
        version: Version::parse(v).expect("version"),
    };
    let running = Version::parse("0.158.0").expect("running");
    let onto = |upgrade: bool, hold: Option<upgrade::Request>| {
        relaunch_onto(
            Some(target("0.159.2")),
            upgrade,
            Some(&running),
            hold.as_ref(),
            now,
        )
        .map(|t| t.version.to_string())
    };
    let moved = Some("0.159.2".to_string());
    assert_eq!(
        onto(true, Some(upgrade::Request::Skip("0.159.2".into()))),
        None
    );
    assert_eq!(
        onto(true, Some(upgrade::Request::DeferUntil(now + 60))),
        None
    );
    // NEGATIVE CONTROLS.
    assert_eq!(onto(true, None), moved);
    assert_eq!(
        onto(true, Some(upgrade::Request::DeferUntil(now - 1))),
        moved
    );
    assert_eq!(
        onto(true, Some(upgrade::Request::Skip("0.159.1".into()))),
        moved
    );
    assert_eq!(onto(true, Some(upgrade::Request::Now)), moved);
    assert_eq!(onto(false, None), None, "the switch off");
    assert!(
        relaunch_onto(Some(target("0.157.0")), true, Some(&running), None, now).is_none(),
        "never onto an older build"
    );
}

/// A CRASHED CODEX RELAUNCHED OFF THE BUILD ITS OWNER HOLDS IS STILL HELD AT
/// THE RELAUNCHED TUI'S NEXT VISIT (end to end): the crash
/// relaunched the TUI on its own program while the owner's Skip held the
/// managed build, and the relaunch's record kept the hold and the move clock
/// — but it still named the TUI that crashed, so the relaunched TUI's first
/// visit read it as another TUI's ([`own_prior`]) and minted a round with no
/// word and its clock at zero: under the quiet flow, a move onto the very
/// build the owner had skipped, one launch late. The relaunch now hands its
/// record to the TUI it brought up ([`found`]): its visit says
/// `wait:skipped`, and the session is as far behind as before the crash.
/// NEGATIVE CONTROLS: a crash relaunched on its own program with no word —
/// the next visit moves the TUI, its age kept: past the waiting record's
/// look (`would-exit`), where a round minted afresh still shows its record
/// first. And A WORD GIVEN TO ANOTHER TUI: the owner skipped
/// the build for a Codex the person has since quit, and the TUI they started
/// after it crashed before its first visit re-minted the record. The crash
/// read that record unfiltered, kept its word and its clock, and [`found`]
/// stamped them onto the relaunched TUI, held `wait:skipped` by a word
/// nobody gave it. Now the record is read only as the crashed TUI's (its pid
/// and kernel start, as [`own_prior`] and [`owner_word_on`] read it): the
/// relaunch keeps neither, and its next visit moves it.
#[test]
fn a_crashed_codex_relaunched_off_a_held_build_is_still_held_at_its_next_visit() {
    use crate::harness::relaunch::{ExitCause, ExitRecord};
    for (word_given, own, name) in [
        (true, true, "crash-held"),
        (false, true, "crash-unheld"),
        (true, false, "crash-other-tui"),
    ] {
        let held = word_given && own;
        let mut rig = Rig::new(name, T2, true, |w| after_crash(w, 137));
        rig.opts.ask = false;
        rig.settle(10);
        let behind = now_s() - 7_200;
        let snap = crashed_snap(&rig, Some(T2), true);
        let word = word_given.then(|| Request::Skip(rig.target.version.to_string()));
        rig.settle_keeping(&St {
            request: word.clone().unwrap_or_default(),
            request_tab: if word_given {
                TAB.to_string()
            } else {
                String::new()
            },
            request_at: if word_given { now_s() - 600 } else { 0 },
            pending_since: behind,
            // The TUI the record is about: the one that crashed, or one the
            // person quit before starting it.
            notice_pid: if own { snap.pid } else { snap.pid + 7 },
            notice_start: snap.start.clone(),
            ..rig.state()
        });
        rig.kernel.exited.store(true, Ordering::SeqCst);
        // Relaunched on its own program either way: the switch off (which
        // build the hold keeps it off is `relaunch_onto`'s, pinned above,
        // and the machine's managed Codex is no test's).
        let r = after_exit(
            &rig.opts,
            &snap,
            &ExitRecord::Crashed,
            false,
            ExitCause::Exit,
            &rig.kernel,
        );
        assert_eq!(r.to, "0.157.0(same)", "{name}: {r:?}");
        assert_eq!(r.step, "continued", "{name}: {r:?}\n{:#?}", rig.asked());
        let st = rig.state();
        assert_eq!(st.source, SAME_PROGRAM, "{name}");
        assert_eq!(st.resumed_pid, NEW, "{name}");
        assert_eq!(st.notice_pid, NEW, "{name}: the relaunched TUI's record");
        if !own {
            assert_eq!(st.request, Request::None, "{name}: another TUI's word");
            assert_ne!(st.behind_since(), behind, "{name}: another TUI's clock");
        }
        let relaunched = Tui {
            pid: NEW,
            argv: rig.kernel.new_argv.clone(),
            ..rig.tui.clone()
        };
        if held {
            let r = visit(&rig.opts, &relaunched, &rig.target, None, &rig.kernel);
            assert_eq!(r.step, "wait:skipped", "{name}: {r:?}");
            let st = rig.state();
            assert_eq!(st.request_for(TAB), word.clone().expect("the word"));
            assert_eq!(st.behind_since(), behind, "{name}: the move clock");
            assert_eq!(st.notice_pid, NEW, "{name}");
        } else {
            let dry = Opts {
                dry_run: true,
                ..rig.opts.clone()
            };
            let r = visit(&dry, &relaunched, &rig.target, None, &rig.kernel);
            // Its age kept, past the settle; another TUI's clock not kept,
            // its round at the first rung, settling — and neither held.
            let want = if own { "would-exit" } else { "wait:settling" };
            assert_eq!(r.step, want, "{name}: {r:?}");
        }
    }
}

/// A QUIET CODEX MOVE AFTER A ROUND THAT GAVE UP IS CARRIED ON ACROSS A NEWER
/// BUILD (the retarget): the quiet flow gave up a round an
/// older build announced, and a newer managed Codex landed during its rest.
/// The retarget of the stopped round ([`St::retarget`]'s stopped arm) built
/// a fresh round with neither the notice's stamp ([`St::noticed_at`]) nor
/// the give-up ([`St::last_stop`]), so the TUI told to wind down was ended
/// with `/exit` and resumed with nothing typed (`done:resumed`). Both are
/// now carried: its move types the continuation — after a stamped notice,
/// and after a give-up an older build recorded with no stamp. NEGATIVE
/// CONTROL: a round that stopped for another reason with no notice ever
/// typed is resumed with nothing typed after.
#[test]
fn a_quiet_codex_move_after_a_given_up_round_a_newer_build_retargets_types_its_continuation() {
    for (why, noticed, name) in [
        (upgrade::GAVE_UP, true, "rt-noticed"),
        (upgrade::GAVE_UP, false, "rt-unstamped"),
        ("signal-refused", false, "rt-unasked"),
    ] {
        let mut rig = Rig::new(name, T2, true, |_| {});
        rig.opts.ask = false;
        rig.settle(10);
        // The stopped round moved to 0.157.1, resting; 0.157.2 landed since.
        rig.settle_keeping(&St {
            phase: Phase::Failed(why.to_string()),
            failed_at: now_s(),
            streak_why: why.to_string(),
            stop_streak: 1,
            noticed_at: if noticed { now_s() - 7_200 } else { 0 },
            ..rig.state()
        });
        rig.target.version = v("0.157.2");
        // The retarget's look: a new round, its screen not settled yet.
        assert_eq!(rig.visit(None).step, "wait:settling", "{name}");
        assert_eq!(rig.state().phase, Phase::Pending, "{name}");
        rig.settle_keeping(&rig.state());
        let r = rig.visit(None);
        let typed = rig.typed();
        assert!(
            typed.iter().any(|l| l.ends_with(" /exit")),
            "{name}: the quiet `/exit`: {r:?} {typed:?}"
        );
        let carried = typed.iter().any(|l| l.contains("Upgraded:"));
        if why == upgrade::GAVE_UP {
            assert_eq!(r.step, "continued", "{name}: {r:?} {typed:?}");
            assert!(carried, "{name}: {typed:?}");
        } else {
            assert_ne!(r.step, "continued", "{name}: {r:?}");
            assert!(!carried, "{name}: {typed:?}");
        }
    }
}

/// A QUIET CODEX MOVE AFTER A ROUND THAT GAVE UP ON ITS NOTICES IS CARRIED ON
/// (the Codex half): the quiet flow gives up a round an
/// older aterm (or `upgrade_ask`) announced once the agent's own work held it
/// past the notice's window, and re-arms it after the rest — a pending round
/// whose agent was asked to wind down and was never told to go on: this lane
/// types no release line. Its quiet `/exit` was followed by nothing
/// (`done:resumed`). Now that move types the continuation — and so does one
/// whose notice the owner's hold ended, or `--now` re-armed after a give-up
/// (the notice's stamp, [`St::noticed_at`], with no stop recorded).
/// NEGATIVE CONTROL: a pending round nothing was asked of is resumed with
/// nothing typed after.
#[test]
fn a_quiet_codex_move_after_a_given_up_round_types_its_continuation() {
    for (asked, name) in [
        (Some(true), "quiet-after-giveup"),
        (Some(false), "quiet-after-a-notice"),
        (None, "quiet-unasked"),
    ] {
        let mut rig = Rig::new(name, T2, true, |_| {});
        rig.opts.ask = false;
        rig.settle(10);
        match asked {
            Some(true) => rig.settle_keeping(&St {
                last_stop: upgrade::GAVE_UP.to_string(),
                ..rig.state()
            }),
            Some(false) => rig.settle_keeping(&St {
                noticed_at: now_s() - 7_200,
                ..rig.state()
            }),
            None => {}
        }
        assert_eq!(rig.state().phase, Phase::Pending, "{name}");
        let r = rig.visit(None);
        let typed = rig.typed();
        assert!(
            typed.iter().any(|l| l.ends_with(" /exit")),
            "{name}: the quiet `/exit`: {typed:?}"
        );
        let carried = typed.iter().any(|l| l.contains("Upgraded:"));
        if asked.is_some() {
            assert_eq!(r.step, "continued", "{name}: {r:?} {typed:?}");
            assert!(carried, "{name}: {typed:?}");
        } else {
            assert_ne!(r.step, "continued", "{name}: {r:?}");
            assert!(!carried, "{name}: {typed:?}");
        }
    }
}

/// A RELAUNCHED CODEX THAT REPLAYS THE WALL IS CARRIED ON ONCE, NEVER STACKED:
/// [`carry_on`] is not gated on the usage wall, on purpose (its doc says
/// why), so the restart that comes back onto a replayed wall types its one
/// continuation there — a turn that fails and stays in history — and the
/// record is done: the next looks, however many, type and press nothing, and
/// the restart's earlier lines (the notice, the `/exit`, the relaunch) are the
/// same as anywhere. This pins the fact that decision rests on, `one line per
/// restart`. NEGATIVE CONTROL: the same restart onto a screen with no wall —
/// the same four lines, one of them the continuation, the same record, the
/// same quiet after it.
#[test]
fn a_relaunched_codex_that_replays_the_wall_is_carried_on_once_and_never_stacked() {
    let replayed = measured_screen(aterm_phase::codex::fixtures::WALL_RESUMED_0_159, 20);
    for (after_relaunch, what) in [
        (replayed, "onto a replayed wall"),
        (codex_idle(20), "clear"),
    ] {
        let rig = embedded_ready("wall-replayed", |w| w.after_relaunch = after_relaunch);
        let r = rig.visit(None);
        assert_eq!(r.step, "continued", "{what}: {r:?}\n{:#?}", rig.asked());
        let typed = rig.typed();
        assert_eq!(
            typed.len(),
            4,
            "{what}: notice, /exit, relaunch, one continuation: {typed:#?}"
        );
        assert_eq!(
            typed
                .iter()
                .filter(|l| l.contains("[aterm harness] Upgraded"))
                .count(),
            1,
            "{what}: {typed:#?}"
        );
        assert_eq!(rig.state().phase, Phase::Done, "{what}: nothing left owed");
        assert_eq!(
            rig.ledger_steps(),
            vec!["announced:1", "exit-typed", "relaunched", "continued"],
            "{what}"
        );
        // Never stacked: later looks type and press nothing more.
        let pressed = keys(&rig);
        for look in 1..=3 {
            let r = rig.visit(None);
            assert_eq!(
                rig.typed().len(),
                4,
                "{what}: look {look} ({}) typed again: {:#?}",
                r.step,
                rig.typed()
            );
            assert_eq!(keys(&rig), pressed, "{what}: look {look} ({})", r.step);
            assert_eq!(rig.state().phase, Phase::Done, "{what}: look {look}");
        }
    }
}
