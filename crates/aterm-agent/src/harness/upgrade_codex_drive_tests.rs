// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE CODEX LANE'S DRIVER over scripted screens and a scripted kernel: a
//! stand-in aterm instance on a real socket (every request recorded, nothing
//! typed anywhere) whose screen turns from Codex's idle composer into the
//! shell with the TUI's exit hint when `/exit` is typed, and into the
//! relaunched Codex when the relaunch line is; and a kernel whose TUI dies
//! when `/exit` is typed and whose relaunch appears when its line is. Every
//! screen and hint is the one measured on Codex 0.157.0/0.157.1 (2026-09-25).

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::harness::upgrade::Request;

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

/// A fresh directory per CALL: two tests share a name (`Rig::new("daemon-busy")` and
/// `daemon_home("daemon-busy")`), and the harness runs them on parallel threads of one
/// process, so a per-name directory let one wipe the other's mid-pass.
fn scratch(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("aterm-cxd-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    d
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

/// Codex 0.157's idle screen, measured: the card, the composer's dim
/// placeholder with the cursor at column 2, the footer.
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

/// The shell after the TUI's exit: an OLDER exit's hint (another thread)
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
    /// A `turn` types its text and does NOT submit it, answering this — a
    /// missed guard's `skipped reason=guard`, or an Enter written and not
    /// seen (`submitted=0 pressed=1`) — and the composer then shows that
    /// text ([`World::left`]) until a fenced, guarded `ctrl+u` clears it.
    miss: Option<&'static str>,
    /// What the lane left typed in the composer, as the stand-in draws it.
    left: Arc<Mutex<Option<String>>>,
    /// The `lease acquire …` reply (a `lease release` is `OK lease released`
    /// after an acquire it granted).
    lease: &'static str,
    exited: Arc<AtomicBool>,
    relaunched: Arc<AtomicBool>,
    asked: Arc<Mutex<Vec<String>>>,
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
                let reply = match verb {
                    "sessions" => format!("OK 1\nlocal {TAB} 0 idle codex"),
                    "text" => format!(
                        "OK {}",
                        if relaunched {
                            w.after_relaunch.clone()
                        } else if exited {
                            w.after_exit.clone()
                        } else if let Some(t) = &left {
                            codex_holding(t, 30)
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
                    // A fenced, guarded `ctrl+u`: the composer's line is
                    // cleared, as Codex 0.157 clears it.
                    "key"
                        if line.contains("if-gen=")
                            && line.contains("if=")
                            && line.ends_with(" ctrl+u") =>
                    {
                        if let Ok(mut l) = w.left.lock() {
                            *l = None;
                        }
                        "OK seq=31".to_string()
                    }
                    "turn" if w.miss.is_some() => {
                        // What the host does on a missed guard: the text is
                        // TYPED and its Enter not pressed.
                        let text = line
                            .split_once(&format!(" {TURN_WAIT} "))
                            .map_or("", |(_, t)| t)
                            .to_string();
                        if let Ok(mut l) = w.left.lock() {
                            *l = Some(text);
                        }
                        w.miss.unwrap_or_default().to_string()
                    }
                    "turn" => {
                        if line.ends_with(" /exit") {
                            w.exited.store(true, Ordering::SeqCst);
                        } else if line.contains("'resume'") || line.contains("/agents/codex'") {
                            w.relaunched.store(true, Ordering::SeqCst);
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
            exec_path: "/bin/zsh".to_string(),
            argv: vec!["-zsh".to_string()],
            env: Vec::new(),
        })
    }

    fn exe(&self, _: u32) -> Option<PathBuf> {
        None
    }

    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>> {
        Some(match (pid, &self.lock) {
            (TUI, Some(lock)) if !self.exited.load(Ordering::SeqCst) => vec![lock.clone()],
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
    dir: PathBuf,
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
            lease: "ERR unscripted",
            exited: Arc::clone(&exited),
            relaunched: Arc::clone(&relaunched),
            asked: Arc::clone(&asked),
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
            },
            asked,
            rollout,
            left,
            status_extra,
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
    })
}

#[test]
fn a_daemon_client_is_exited_by_a_typed_exit_and_resumed_on_the_managed_twin() {
    let rig = Rig::new("daemon-swap", T1, false, |_| {});
    rig.settle(10);
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "done", "{r:?}\n{:#?}", rig.asked());
    assert_eq!(r.pid, NEW);
    let typed = rig.typed();
    assert_eq!(typed.len(), 2, "{typed:#?}");
    // `/exit`, fenced on the read that judged the composer empty and guarded
    // on the composer's row.
    assert!(
        typed[0].contains("if-gen=1.10 yield=0.2 submit=guarded:^›\\s/exit\\s*$ ")
            && typed[0].ends_with(" /exit"),
        "{}",
        typed[0]
    );
    // The relaunch: the managed twin, `resume` and the thread the TUI's OWN
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
    // NOTHING SIGNALS A CODEX PROCESS.
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

/// ND1 FOR A CODEX (the review of the first cut: the Codex restart's
/// `/exit`-to-relaunch bare shell took no hand, and the `aterm help` text
/// said every restart did): the harness's HARD hand is taken before the
/// `/exit`'s last look and kept — never let go — through the `/exit`, the
/// bare shell and the relaunch line, both typed under it on its own
/// connection, and given back once the relaunched TUI is carried on.
/// NEGATIVE CONTROL: another driver's hold on the tab — the restart waits
/// `held`, types nothing and gives nothing back.
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
    // NEGATIVE CONTROL: another driver holds the tab.
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
    // ... and the client says what the daemon waits on (review of
    // 2026-09-26: a bare `daemon-first` named nothing the owner could act on).
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
    // NEGATIVE CONTROL: the TUI printed nothing as it left. The older exit's
    // hint is still above the prompt, and read from the whole screen it would
    // resume ANOTHER conversation.
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
    // `--now` waives the settling window: a screen that JUST moved.
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
    // The host: the tab's foreground group's LEADER, when it is a codex.
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
    // Its environment naming ANOTHER tab vetoes the claim.
    assert!(
        candidates(
            Some(&tabs),
            &procs,
            |pid| Some((1, i64::from(pid), i64::from(pid))),
            |_| Some("s-z".into())
        )
        .is_empty()
    );
    // A hand-run sweep: the environment's tab, and only a foreground leader
    // — the suspended one (400, its group not the terminal's) is no
    // candidate, and could never write over the foreground one's notice.
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
/// vendor's updater, and answers as 0.157.1 does — recording its argv and
/// its whole environment first.
fn fake_update_verb(dir: &Path, home: &Path, new_pid: u32) -> PathBuf {
    let exe = dir.join("store/codex/b/bin/codex");
    std::fs::create_dir_all(exe.parent().expect("bin")).expect("bin");
    let rel = home.join("packages/app-server-daemon/releases/local-abc");
    let script = format!(
        "#!/bin/sh\n\
         /usr/bin/env > '{dir}/verb.env'\n\
         echo \"$@\" > '{dir}/verb.args'\n\
         /bin/mkdir -p '{rel}/bin'\n\
         : > '{rel}/bin/codex'\n\
         /bin/cat > '{rel}/codex-package.json' <<'EOF'\n{pkg}\nEOF\n\
         /bin/rm -f '{home}/packages/app-server-daemon/auto-update-version'\n\
         echo '{{\"pid\":{new_pid}}}' > '{home}/app-server-daemon/daemon.pid'\n\
         echo 'Replace installed daemon version 0.157.0 with CLI version 0.157.1.'\n\
         echo '{{\"status\":\"updated\",\"installedVersion\":\"0.157.1\",\"runningVersion\":\"0.157.1\"}}'\n",
        dir = dir.display(),
        rel = rel.display(),
        home = home.display(),
        pkg = PACKAGE.replace("0.157.0", "0.157.1"),
    );
    std::fs::write(&exe, script).expect("verb");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    exe
}

/// THE VERB INHERITS NOTHING BUT ITS STDIO. A descriptor this process holds
/// without close-on-exec — the stand-in for the Metal shader-cache files the
/// GUI holds (measured 2026-09-27) — must not reach the vendor's verb, so it
/// cannot reach the daemon the verb starts, which outlives the GUI (the
/// product fd-hygiene sweep of 2026-09-27). The verb reports what it finds
/// open before it answers: its stdout is the detector's positive control,
/// and the answer being read proves the verb ran.
#[test]
fn the_daemon_update_verb_inherits_nothing_but_its_stdio() {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::fs::PermissionsExt as _;
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
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\n\
             r=''\n\
             [ -e /dev/fd/1 ] && r='stdout '\n\
             [ -e /dev/fd/{raw} ] && r=\"${{r}}held \"\n\
             printf '%send' \"$r\" > '{report}'\n\
             echo '{{\"status\":\"updated\",\"installedVersion\":\"0.157.1\",\
             \"runningVersion\":\"0.157.1\"}}'\n",
            report = report.display(),
        ),
    )
    .expect("verb");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    // A long ceiling: the verb exits at once, and a first exec of a fresh
    // script can wait on the system's code assessment under load.
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
    fn cwd(&self, _: u32) -> Option<String> {
        None
    }
    fn in_tab(&self, _: &mut Client, _: u32, _: &str) -> bool {
        false
    }
}

/// A home whose daemon runs 0.157.0, unpinned, holding one thread whose
/// rollout ends on `last` — written `age_s` ago.
fn daemon_home(name: &str, last: &str, age_s: u64) -> (PathBuf, Opts, DaemonScript, Target) {
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
    std::fs::write(&rollout, format!("{last}\n")).expect("rollout");
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
    };
    let target = Target {
        twin: dir.join("agents/codex"),
        exe: target_exe,
        version: v("0.157.1"),
    };
    (dir, opts, script, target)
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
    // The verb ran with the RUNNING DAEMON's environment and nothing else —
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
    // NEGATIVE CONTROL: a thread whose turn runs — attached to no tab at all
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

#[test]
fn the_codex_lane_holds_no_signal_primitive() {
    // Codex's TUI installs no signal handler: SIGTERM, SIGINT and SIGHUP end
    // it with the tab left in the alternate screen, kitty keyboard, mouse
    // reporting and bracketed paste (the research's pty run). The lane exits
    // it by a typed `/exit` alone — pinned here, by the source.
    for (name, text) in [
        (
            "upgrade_codex_drive.rs",
            include_str!("upgrade_codex_drive.rs"),
        ),
        ("upgrade_codex.rs", include_str!("upgrade_codex.rs")),
    ] {
        for banned in ["libc::kill", "signal term", "SIGTERM)", "terminate("] {
            assert!(!text.contains(banned), "{name} holds `{banned}`");
        }
    }
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
    // NEGATIVE CONTROL for a real hazard: a sweep discovers Codex by the
    // machine's process table, so a TEST's sweep (a scratch home) found a real
    // throwaway Codex on this machine, and its daemon, until the sweep was
    // scoped to its own home. Only a TUI started with this sweep's `$HOME` is
    // the owner's; another is said (a tab's) or passed by (a hand-run sweep's).
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
    // THE SAME RULE HOLDS THE OWNER'S VIEW: a hand-run `--status` never
    // counts another home's Codex as a holder (the lab's throwaway TUI ran
    // on this machine while this suite did).
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
    // NEGATIVE CONTROL: a DIFFERENT exit at that prompt since (another Codex
    // run and quit there) names another thread — nothing is typed on it.
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

// ---------------------------------------------------------------- review of 2026-09-26
//
// Each test below was first a reproduction of a review finding against the
// shipped lane (the throwaway lab's live runs, and the review's own unit
// repros); each now pins the fix, with a negative control beside it.

/// The daemon home of [`daemon_home`], serving one daemon-mode TUI — the
/// stand-in tab of a [`Rig`], attached to the daemon — and `world` for that
/// tab. The Rig goes with the returned tuple (its socket is the tab's).
fn served(
    name: &str,
    last: &str,
    world: impl FnOnce(&mut World),
) -> (Rig, PathBuf, Opts, DaemonScript, Target, Tui) {
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
fn attended(w: &mut World) {
    w.before = codex_idle(10).replace(r#""human_ms":null"#, r#""human_ms":1000"#);
}

#[test]
fn the_owners_skip_or_defer_on_a_codex_tab_holds_its_daemon_too() {
    // The review, live: `--skip` on the tab, and 28 s later the daemon was
    // restarted onto exactly the skipped build under it.
    for (request, held) in [
        (Request::Skip("0.157.1".into()), true),
        (Request::DeferUntil(now_s() + 3_600), true),
        // A skip of ANOTHER build, and a deferral that ran out, hold nothing.
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
    // The review's repro: the window's roster holds no Codex tab, and a Codex
    // in another window (or a pane, or one whose build could not be read) is
    // attached to the shared daemon. Nothing of its tab was asked.
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
    // NEGATIVE CONTROL: a daemon nobody is attached to moves.
    assert_eq!(run(Some(Vec::new())), (daemon("daemon-updated"), true));
}

#[test]
fn the_owners_now_on_a_daemon_mode_tab_waives_its_own_attended_guard_at_the_daemon() {
    // CONTROL: a person at the tab and no word — the daemon waits, and the
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
    // The owner's `--now` on that tab: the person asking. The daemon moves,
    // and the client after it, in one sweep (the review: nothing moved).
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
    let c = visit(&opts, &tui, &target, Some(view), &rig.kernel);
    assert_eq!(c.step, "done", "{:#?}", rig.asked());
    drop(rig);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_the_daemon_waits_on_reaches_its_clients_row_and_words() {
    // A detached thread's turn runs in the daemon (the rollout ends on
    // `task_started`): the daemon waits `busy-thread`, and the tab's row now
    // says so — it read a bare `daemon-first` for 7 h in the review.
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
    // The review, live: `1 background terminal running`, its thread idle
    // (`task_complete`), and the update restarted the daemon and ended it.
    let (dir, opts, mut k, target) = daemon_home("term", IDLE_LAST, 600);
    k.terminals = vec!["sleep".into()];
    let home = k.home.clone();
    let (r, view) = daemon_pass(&opts, &home, &target, &[], &k).expect("a daemon");
    assert_eq!(r.step, "wait:background-terminal");
    assert_eq!(view.wait, "background-terminal");
    assert!(!dir.join("verb.args").exists(), "the verb never ran");
    // NEGATIVE CONTROL: once it ends, the daemon moves.
    k.terminals.clear();
    let (r, _) = daemon_pass(&opts, &home, &target, &[], &k).expect("a daemon");
    assert_eq!(r.step, "daemon-updated");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_served_tabs_status_line_backs_the_kernels_count_at_the_daemon() {
    // The kernel reads no session leader (the count is the proof, this the
    // backstop): the tab's own status line says a background terminal of its
    // thread runs — in this daemon.
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
    // The review, live: `--no-daemon`, `sleep 7772` directly under the TUI
    // (no shell between, so `background` never saw it); READY answered, and
    // the `/exit` ended it.
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
    // NEGATIVE CONTROL: nothing runs under it — it moves, and is carried on.
    rig.kernel.terminals.clear();
    rig.settle_keeping(&rig.state());
    assert_eq!(rig.visit(None).step, "continued");
}

/// A TERMINAL THAT NEVER ENDS DOES NOT HOLD THE UPGRADE FOR GOOD (the Codex
/// arm of the 2026-09-26 four-day tab): a READY answer outlived by a
/// background terminal for a whole re-ask window is asked about again, and
/// past the last ask the upgrade gives up — the Claude lane's one rule.
/// Before, the embedded drive swapped the reducer's Terminate for a bare
/// `wait:background-terminal` it never re-asked.
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

/// NO STOP IS FOR GOOD IN THE CODEX LANE EITHER (the owner, 2026-09-27: "you
/// should NEVER have upgrades stalled"). An embedded conversation's round
/// gives up over a background terminal that outlives its READY; it RESTS —
/// `wait:failed:unanswered`, looked at again, nothing typed — and once it has
/// rested `RETRY_S` a new round starts (`rearmed:unanswered`, on the ledger,
/// nothing typed at that look), whose first notice the next look types, with
/// a READY marker of its own. NEGATIVE CONTROL: the owner's `--skip` of the
/// build holds the rested round where it is, however long it rests, and says
/// `wait:skipped` — the host's last word, as in the Claude lane — and a
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
    // The review, live: an inline Codex (`--no-alt-screen`), a `%~` / `%#`
    // prompt, and a daemon update whose reconnect wiped the tab's blocks
    // (measured: `{"blocks":[]}`): after `/exit` only the entering prompt is
    // marked — two rows (`prompt:98, cmd:99`) — and the hint above its first
    // row was read as none: exited, never relaunched.
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

#[test]
fn a_missed_guard_on_exit_is_said_and_the_exit_cleared_from_the_composer() {
    // The review's repro: the guarded Enter missed, `/exit` sat in the
    // composer, and every later visit read it as the person's draft.
    let rig = Rig::new("gm-exit", T1, false, |w| {
        w.miss = Some("OK 0 turn skipped reason=guard submitted=0 seq=11 id=1");
    });
    rig.settle(10);
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "left-typed:exit");
    let st = rig.state();
    assert_eq!(st.phase, Phase::Pending, "nothing is in flight");
    assert_eq!(st.left_typed, "", "cleared, so no record is owed");
    assert!(
        rig.left.lock().expect("left").is_none(),
        "the composer is empty again"
    );
    assert_eq!(
        rig.ledger_steps(),
        vec!["left-typed:exit", "left-typed-cleared"]
    );
    // One `ctrl+u`, fenced on the read that judged it and guarded on the
    // composer's row reading exactly `/exit`.
    let k = keys(&rig);
    assert_eq!(k.len(), 1, "{k:#?}");
    assert!(
        k[0].contains("if-gen=1.30 ")
            && k[0].contains("if=^›\\x20/exit\\s*$ ")
            && k[0].ends_with(" ctrl+u"),
        "{}",
        k[0]
    );
    // The next visit neither reads a draft nor types the act again at once.
    assert_eq!(rig.visit(daemon_current()).step, "wait:left-typed-backoff");
    assert_eq!(rig.typed().len(), 1);
}

#[test]
fn an_enter_that_did_not_submit_the_notice_is_said_and_cleared_too() {
    // The embedded notice: the review found the miss said NOWHERE (no ledger
    // row) and ~600 characters left as the person's "draft". Here the Enter
    // was written and not seen (`submitted=0 pressed=1`), the notice still in
    // the composer.
    let rig = Rig::new("gm-notice", T2, true, |w| {
        w.miss = Some("OK 0 turn submitted=0 pressed=1 status=timeout seq=11 id=1");
    });
    rig.settle(10);
    let r = rig.visit(None);
    assert_eq!(r.step, "left-typed:notice");
    assert_eq!(rig.state().phase, Phase::Pending, "never announced");
    assert!(rig.left.lock().expect("left").is_none());
    assert_eq!(
        rig.ledger_steps(),
        vec!["left-typed:notice", "left-typed-cleared"]
    );
    // NEGATIVE CONTROL: a person's text in the composer is never cleared.
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

#[test]
fn an_exit_the_tui_outlived_is_never_waited_on_again() {
    // The review's repro: an hour-old `/exit` the TUI outlived was waited on
    // for 30 s under the sweep lock, every sweep, and never expired.
    let rig = Rig::new("outlived", T1, false, |_| {});
    let mut st = exiting(&rig, "");
    st.phase = Phase::Exiting {
        at_s: now_s() - 3_600,
    };
    save(&rig.opts, &key(TAB), &st);
    let t0 = std::time::Instant::now();
    let r = rig.visit(daemon_current());
    assert_eq!(r.step, "failed:stale-exit");
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    assert_eq!(rig.state().phase, Phase::Failed("stale-exit".into()));
    // Two minutes old: not waited on either, and a `/exit` left in its
    // composer is cleared while it alone is there.
    let fresh = Rig::new("outlived-2m", T1, false, |_| {});
    if let Ok(mut l) = fresh.left.lock() {
        *l = Some("/exit".to_string());
    }
    let mut st = exiting(&fresh, "");
    st.phase = Phase::Exiting {
        at_s: now_s() - 120,
    };
    save(&fresh.opts, &key(TAB), &st);
    let t0 = std::time::Instant::now();
    assert_eq!(fresh.visit(daemon_current()).step, "wait:exit-not-taken");
    assert!(t0.elapsed() < Duration::from_secs(10));
    assert!(fresh.left.lock().expect("left").is_none());
    assert!(matches!(fresh.state().phase, Phase::Exiting { .. }));
    assert_eq!(fresh.ledger_steps(), vec!["left-typed-cleared"]);
}

/// A kernel whose shell never takes its terminal back: a relaunch waits.
struct NoPrompt<'a>(&'a Script);

impl Kernel for NoPrompt<'_> {
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

impl CodexKernel for NoPrompt<'_> {
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
    fn shell_has_terminal(&self, _: u32) -> bool {
        false
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
fn a_daemon_clients_exit_in_flight_records_no_line_before_its_thread_is_read() {
    // The review's repro: the state in flight carried a line planned with
    // the placeholder thread, which an older build's orphan pass types
    // verbatim — a resume of `00000000-…`.
    let rig = Rig::new("no-line", T1, false, |_| {});
    rig.settle(10);
    let r = visit(
        &rig.opts,
        &rig.tui,
        &rig.target,
        daemon_current(),
        &NoPrompt(&rig.kernel),
    );
    assert_eq!(r.step, "wait:shell-prompt");
    let st = rig.state();
    assert!(matches!(st.phase, Phase::Exiting { .. }));
    assert_eq!(st.line, "", "no line until the exit names the thread");
    assert!(st.exited_at != 0, "the TUI was seen gone");
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
    // NEGATIVE CONTROL: a failure before any exit (the TUI never went) is
    // left alone here — the owner's view vets it against its holder.
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
    // restart's (`relaunch::restarted`, the review of 2026-09-27).
    assert_eq!(rig.state().resumed_pid, NEW);
    assert_eq!(
        rig.typed().len(),
        3,
        "the notice, `/exit` and the relaunch line — the carry-on is the loop's: {:#?}",
        rig.typed()
    );
    // The record stays in flight: what the relaunch primitive's continuation
    // step ([`super::super::super::relaunch::owed`]) finds for the tab.
    assert!(
        matches!(rig.state().phase, Phase::Relaunched { .. }),
        "{:?}",
        rig.state().phase
    );
    // A PERSON AT THE KEYS where the loop parks next: it stays owed, and
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

/// A RELAUNCHED CODEX THE RESTART FOUND IS YOURS ONCE IT IS GONE, ADOPTED OR
/// NOT (the review of 2026-09-27). Only an adoption stamped the TUI on the
/// record ([`St::resumed_pid`]), and two paths leave it up in the tab
/// unstamped: an adopt whose [`ADOPT_WAIT`] ran out (`wait:resume`), and a
/// carry-on that found a person at the keys (`wait:held`). A person's `/exit`
/// of that TUI was then read as the restart's: every carry held the
/// harness's hand on the tab for a TUI that was gone, until the record
/// expired and the tab was badged as an agent the harness had ended. The
/// TUI the relaunch brought up is on the record from the look that finds it,
/// and once it is gone a Codex leaving the tab is that TUI — its own exit.
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
    let restarted = || super::super::super::relaunch::restarted(&tab, true, None);
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
    // A PERSON AT THE KEYS where the carry-on looks: `wait:held`, the record
    // still relaunched and unadopted — and that TUI, once gone, still its own.
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
    // FOUND BY A VISIT: a relaunched TUI no earlier look recorded (the
    // relaunch's own wait ran out before it came up) is on the record from
    // the visit that finds it leading the tab — held at the keys as here, or
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
    assert_eq!(
        super::super::super::relaunch::carry_restart(&tab, true, None).step,
        "refused:no-restart-in-flight",
        "no hand held on the tab for a TUI that is gone"
    );
}

#[test]
fn a_daemon_reached_from_one_tabs_step_asks_every_codex_tab_it_serves() {
    // The window's step is ONE tab's, taken at its idle point; the daemon it
    // reaches serves every Codex tab of the window. Each of those is asked —
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
    // NEGATIVE CONTROL: the other tab's Codex attached and NOT listed — the
    // daemon waits for a client it did not ask.
    let (reports, ran) = run(false);
    let daemon = reports
        .iter()
        .find(|r| r.session == "codex-daemon")
        .expect("the daemon's report");
    assert_eq!(daemon.step, "wait:unseen-client", "{reports:#?}");
    assert!(!ran);
}

#[test]
fn a_codex_daemon_behind_or_still_on_the_vendors_updater_is_due() {
    let (dir, _opts, k, target) = daemon_home("due", IDLE_LAST, 600);
    let home = k.home.clone();
    // Older than the managed Codex.
    assert!(daemon_moves(&home, &target, &k));
    // The managed build, the vendor's updater still armed: due too.
    let same = Target {
        version: v("0.157.0"),
        ..target.clone()
    };
    assert!(daemon_moves(&home, &same, &k));
    // NEGATIVE CONTROLS: pinned on the managed build, ahead of it, or no
    // daemon at all — nothing to move.
    std::fs::remove_file(home.join("packages/app-server-daemon/auto-update-version"))
        .expect("unarm");
    assert!(!daemon_moves(&home, &same, &k));
    let behind = Target {
        version: v("0.156.1"),
        ..target.clone()
    };
    assert!(!daemon_moves(&home, &behind, &k));
    std::fs::remove_file(home.join("app-server-daemon/daemon.pid")).expect("no daemon");
    assert!(!daemon_moves(&home, &target, &k));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_terminals_break_takes_the_notice_and_ends_nothing() {
    // THE OWNER'S ANSWER OF 2026-09-26, the Codex branch: an embedded
    // session whose finished turn left a background terminal (its status
    // line under the turn's end, so its loop reads the turn running) is told
    // of its upgrade at that break — the notice alone; its READY answer
    // waits for an idle point, where the `/exit` still waits out the
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
    // NEGATIVE CONTROL: a daemon-mode client at the break types nothing.
    let mut daemon = Rig::new("daemon-break", T1, false, with_terminal);
    daemon.opts.background = true;
    daemon.settle(9);
    assert_eq!(daemon.visit(daemon_current()).step, "wait:background");
    assert!(daemon.typed().is_empty());
    assert!(!daemon.asked().iter().any(|l| l.contains("signal")));
}

#[test]
fn at_a_break_a_daemon_mode_client_waits_background_and_its_daemon_is_only_read() {
    // Live, 2026-09-26 (the host-driven lab, scenario A): at a break the
    // daemon pass is skipped, and a daemon-mode client whose daemon was never
    // read recorded `wait:no-daemon` — a wait that named the wrong thing. The
    // daemon is READ at a break (its build, from its files), never stepped:
    // the client waits `background`, and the vendor's verb never runs.
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
    };
    let r = visit(&opts, &tui, &target, Some(read), &rig.kernel);
    assert_eq!(r.step, "wait:background", "{r:?}");
    assert!(rig.typed().is_empty());
    // NEGATIVE CONTROL: with no daemon read, the same client reads none —
    // what the break recorded before the read.
    let r = visit(&opts, &tui, &target, None, &rig.kernel);
    assert_eq!(r.step, "wait:no-daemon");
    drop(rig);
    let _ = std::fs::remove_dir_all(&dir);
}
