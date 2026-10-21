// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! agy's live upgrade, its quiet flow, driven: a stand-in aterm instance on a
//! real socket (every request recorded) whose screen is agy 1.4.0's until the
//! `signal`, the shell's prompt after it, and the relaunched agy once the
//! relaunch line is typed; a scripted kernel whose agy lives until the signal,
//! holding its conversation's presence lock, and whose relaunch appears under
//! the shell when its line is, taking that lock; a store with the running
//! build and a newer one the twin execs, each with its record; and the
//! conversation's transcript, messages and summary answer as 1.4.0 wrote
//! them in the lab. Each guard of the gate positive and negative, the claim
//! on disk before the signal, nothing typed after, the live database read in
//! both of its states, and the Tier-1 bind of `HarnessAgyQuietUpgrade`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::super::live::{self, Move, claim_record, upgrade_visit};
use super::*;
use crate::harness::relaunch::ended_by_restart;
use crate::harness::upgrade_agy_live::{self as pure, Look, Message, STATUS_IDLE};
use crate::harness::upgrade_drive::build_start::{NamedProbe, PROBE, Probe};
use crate::harness::upgrade_drive::{Due, LiveTab, descendants_in};

/// The newer build atpkg lands: 1.4.1 at its build id; an older one, 1.3.9.
const NEW_BUILD: &str = "1000001000004000001";
const NEW_VERSION: &str = "1.4.1";
const OLD_BUILD: &str = "1000001000003000009";
const OLD_VERSION: &str = "1.3.9";
/// When the agy started (unix ms), and a row time after it and before it.
const START_MS: u64 = 1_792_349_000_000;
const AT: &str = "2026-10-18T18:43:30Z";
const BEFORE: &str = "2026-10-18T18:40:00Z";
/// A child of the agy (a command task's shell, a daemon's bare command).
const CHILD: u32 = 4_200_009;
/// A subagent of the conversation.
const SUB: &str = "f9935f58-a2d9-4f58-bb63-1f8d3ffa0d23";

/// Which build the twin execs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Twin {
    Newer,
    Same,
    Older,
}

/// One message of the conversation: its id, its priority, how long ago it
/// was written (ms), and whether `read.json` marks it read.
#[derive(Clone, Debug)]
struct Msg {
    id: &'static str,
    priority: &'static str,
    ago_ms: u64,
    read: bool,
}

/// Another agy with the conversation open, as the kernel answers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shared {
    /// Its argv names the conversation (`--conversation=<uuid>`).
    Resuming,
    /// It holds the conversation's database with no presence lock (`-c`, an
    /// in-TUI `/resume`).
    Lockless,
    /// The process table cannot be read.
    Unread,
}

/// One witness of the gate that changes between the first look and the last
/// (from the hand's `lease` on): what a cron firing or a task ending in that
/// window writes, or what a person starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Between {
    /// A background task's `RUNNING` row lands in the transcript.
    Transcript,
    /// An unread HIGH message lands (a task's end, a timer's firing).
    Message,
    /// The summary row reads `RUNNING` (agy took its wake turn).
    Summary,
    /// A child process appears under the agy.
    Child,
    /// The screen reads busy (a turn's spinner).
    Screen,
    /// Another agy opens the conversation.
    Shared,
}

/// What a test varies, each default as an idle agy behind a newer build.
#[derive(Clone, Debug, Default)]
struct Knobs {
    /// The screen before the signal (a lab fixture): `None` idle at a turn's
    /// end.
    screen: Option<&'static str>,
    /// The tab is held (`hold=1`).
    held: bool,
    /// Held from the Nth `status` read on (the last look's).
    held_from: Option<usize>,
    /// The transcript's rows (default: a finished turn of this process);
    /// `Err(())` for no transcript at all.
    transcript: Option<Result<String, ()>>,
    /// The summary query's answer (default: the conversation's row idle);
    /// `Err(())` for a database that cannot be read.
    summary: Option<Result<String, ()>>,
    /// The conversation's messages.
    messages: Vec<Msg>,
    /// `read.json`'s text in place of the one the messages make.
    read_json: Option<&'static str>,
    /// A child process under the agy, by its basename.
    child: Option<&'static str>,
    /// The process table cannot be read.
    table_unread: bool,
    /// The build's start probe's answer (default: it starts).
    probe: Option<Probe>,
    /// The twin's build (default newer).
    twin: Option<Twin>,
    /// The signal is refused by the server.
    signal_refused: bool,
    /// The launch's argv beyond the program (default `--effort high`).
    argv: Option<Vec<String>>,
    /// The shell takes its terminal back only once this is set.
    shell_slow: Option<Arc<AtomicBool>>,
    /// The relaunched agy never holds the conversation's lock.
    lockless: bool,
    /// The step is a background break's.
    background: bool,
    /// The host's step (hand the relaunch back to the loop).
    hand_back: bool,
    /// Another process holds the conversation's lock from the hand's take on
    /// (between the first look and the last).
    lock_moves: bool,
    /// The agy holds no presence lock (a fresh agy that took no turn).
    no_conversation: bool,
    /// Another agy has the conversation open.
    shared: Option<Shared>,
    /// One witness changes between the first look and the last.
    between: Option<Between>,
    /// The command line the shell's integration recorded for the command the
    /// tab runs (default `sh agy.sh`, a launcher naming no agy).
    cmdline: Option<&'static str>,
}

/// The scripted kernel: [`TUI`] runs under [`SHELL`] until the signal,
/// holding the conversation's lock; its relaunch [`NEW`] appears when the
/// line is typed and takes it.
struct LiveKernel {
    exited: Arc<AtomicBool>,
    relaunched: Arc<AtomicBool>,
    moved: Arc<AtomicBool>,
    /// The hand was taken: a [`Between`] change stands from here on.
    changed: Arc<AtomicBool>,
    lock: PathBuf,
    crash: PathBuf,
    exe: PathBuf,
    new_exe: PathBuf,
    home: PathBuf,
    argv: Vec<String>,
    knobs: Knobs,
}

impl Kernel for LiveKernel {
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

impl AgyKernel for LiveKernel {
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
                env: vec![format!("HOME={}", self.home.display())],
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
            let mut files = vec![self.crash.clone()];
            if !self.knobs.no_conversation {
                files.push(self.lock.clone());
            }
            files
        } else {
            Vec::new()
        })
    }

    fn holders_of(&self, file: &Path) -> Option<Vec<u32>> {
        if file != self.lock {
            return Some(Vec::new());
        }
        Some(if self.relaunched.load(Ordering::SeqCst) {
            if self.knobs.lockless {
                Vec::new()
            } else {
                vec![NEW]
            }
        } else if self.exited.load(Ordering::SeqCst) || self.knobs.no_conversation {
            Vec::new()
        } else if self.moved.load(Ordering::SeqCst) {
            vec![OTHER]
        } else {
            vec![TUI]
        })
    }

    fn agy_resuming(&self, conversation: &str) -> Option<Vec<u32>> {
        if self.knobs.shared == Some(Shared::Unread) {
            return None;
        }
        let mut out = Vec::new();
        // The agy itself, launched on the conversation, names it while it
        // lives: never a sharer of its own conversation.
        if AgyKernel::alive(self, TUI)
            && ag::resumed_conversation(&self.argv).as_deref() == Some(conversation)
        {
            out.push(TUI);
        }
        if self.knobs.shared == Some(Shared::Resuming) || self.between(Between::Shared) {
            out.push(OTHER);
        }
        Some(out)
    }

    fn lockless_openers(&self, _: &Path, _: &str) -> Option<Vec<u32>> {
        match self.knobs.shared {
            Some(Shared::Unread) => None,
            Some(Shared::Lockless) => Some(vec![OTHER]),
            _ => Some(Vec::new()),
        }
    }

    fn shell_has_terminal(&self, _: u32) -> bool {
        self.exited.load(Ordering::SeqCst)
            && self
                .knobs
                .shell_slow
                .as_ref()
                .is_none_or(|ready| ready.load(Ordering::SeqCst))
    }

    fn agy_children(&self, _: u32) -> Vec<(u32, Vec<String>)> {
        if self.relaunched.load(Ordering::SeqCst) {
            vec![(
                NEW,
                vec![
                    self.new_exe.to_string_lossy().into_owned(),
                    "--effort".to_string(),
                    "high".to_string(),
                    format!("--conversation={C1}"),
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
        AgyKernel::alive(self, pid)
    }

    fn descendants(&self, pid: u32) -> Option<Vec<(u32, String)>> {
        if self.knobs.table_unread {
            return None;
        }
        let mut table = vec![(TUI, SHELL, "agy".into())];
        if let Some(name) = self.knobs.child
            && AgyKernel::alive(self, TUI)
        {
            table.push((CHILD, TUI, name.into()));
        }
        if self.between(Between::Child) && AgyKernel::alive(self, TUI) {
            table.push((CHILD, TUI, "zsh".into()));
        }
        descendants_in(&table, pid)
    }

    fn started_ms(&self, pid: u32) -> Option<u64> {
        (pid == TUI).then_some(START_MS)
    }

    fn summaries(&self, _: &Path, _: &str) -> Result<String, ()> {
        if self.between(Between::Summary) {
            return Ok(format!("{C1}|CASCADE_RUN_STATUS_RUNNING|1|\n"));
        }
        self.knobs
            .summary
            .clone()
            .unwrap_or_else(|| Ok(idle_summary()))
    }
}

impl LiveKernel {
    /// Whether the change `what` stands now (the knob's, and the hand taken).
    fn between(&self, what: Between) -> bool {
        self.knobs.between == Some(what) && self.changed.load(Ordering::SeqCst)
    }
}

/// The conversation's summary row between turns, as the query answers it.
fn idle_summary() -> String {
    format!("{C1}|{STATUS_IDLE}|0|\n")
}

/// One transcript row as agy writes it.
fn row(kind: &str, status: &str, at: &str, content: &str) -> String {
    let source = match kind {
        "SYSTEM_MESSAGE" => "SYSTEM",
        "USER_INPUT" => "USER_EXPLICIT",
        _ => "MODEL",
    };
    format!(
        r#"{{"step_index":1,"source":"{source}","type":"{kind}","status":"{status}","created_at":"{at}","content":{}}}"#,
        aterm_json::to_string(&aterm_json::Value::from(content)).expect("content")
    )
}

/// A finished turn: the person's message and agy's answer.
fn finished_turn(at: &str) -> String {
    format!(
        "{}\n{}\n",
        row(
            "USER_INPUT",
            "DONE",
            at,
            "<USER_REQUEST>\nremember the word\n</USER_REQUEST>"
        ),
        row("PLANNER_RESPONSE", "DONE", at, "Hi! (reply 1)")
    )
}

/// A command task started (its `RUNNING` row), task-`n`.
fn task_started(n: u32, what: &str, at: &str) -> String {
    row(
        "GENERIC",
        "RUNNING",
        at,
        &format!(
            "Created At: 2026-10-18T11:43:30-07:00\nTool is running as a background task with task id: {C1}/task-{n}\nTask Description: {what}\nTask logs are available at: file:///h/brain/{C1}/.system_generated/tasks/task-{n}.log"
        ),
    ) + "\n"
}

/// Its end: the message its `sender=` names.
fn task_ended(n: u32, at: &str) -> String {
    row(
        "SYSTEM_MESSAGE",
        "DONE",
        at,
        &format!(
            "<SYSTEM_MESSAGE>\n[Message] timestamp={at} sender={C1}/task-{n} priority=MESSAGE_PRIORITY_HIGH content=Task id \"{C1}/task-{n}\" finished with result:\n</SYSTEM_MESSAGE>"
        ),
    ) + "\n"
}

/// A subagent spawned.
fn subagent_spawned(at: &str) -> String {
    row(
        "GENERIC",
        "DONE",
        at,
        &format!(
            "Created the following subagents:\n{{\n  \"conversationId\":  \"{SUB}\",\n  \"workspaceUris\":  [\n    \"file:///p\"\n  ]\n}}"
        ),
    ) + "\n"
}

/// The subagent gone idle.
fn subagent_idle(at: &str) -> String {
    row(
        "SYSTEM_MESSAGE",
        "DONE",
        at,
        &format!(
            "<SYSTEM_MESSAGE>\n[Message] timestamp={at} sender=system priority=MESSAGE_PRIORITY_HIGH content=Subagent {SUB} has gone idle. This was the subagent's most recent response: DONE\n</SYSTEM_MESSAGE>"
        ),
    ) + "\n"
}

/// One message file's text.
fn message_body(id: &str, priority: &str) -> String {
    format!(
        r#"{{"id":"{id}", "recipient":"{C1}", "sender":"{C1}/task-4", "priority":"{priority}", "timestamp":"2026-10-18T18:53:20.270650Z", "content":"Task id finished"}}"#
    )
}

/// One idle agy behind a build: its config with the conversation's files,
/// the store's builds and the twin, the stand-in instance, the kernel.
struct LiveRig {
    dir: ScratchDir,
    opts: Opts,
    kernel: LiveKernel,
    asked: crate::harness::FakeInstanceLog,
    config: PathBuf,
    twin: PathBuf,
    /// The tab's record as it stood on disk when the `signal` came.
    at_signal: Arc<Mutex<Option<String>>>,
}

/// Lays a store build `build` of `version` under `store`: its program and
/// its record.
fn lay(store: &Path, build: &str, version: &str) -> PathBuf {
    let exe = store.join(build).join("bin/agy");
    std::fs::create_dir_all(exe.parent().expect("bin")).expect("store");
    std::fs::write(&exe, "#!/bin/sh\n").expect("build");
    std::fs::write(
        store.join(format!("{build}.vendor")),
        format!("schema = 1\nprogram = \"agy\"\nversion = \"{version}\"\nvendor = \"Google\"\n"),
    )
    .expect("vendor record");
    exe
}

/// A lab fixture's rows, served with the cursor on its caret row.
fn served(fixture: &str) -> String {
    at_caret(fixture)
}

/// The conversation's files a [`Between`] change writes under `generated`
/// (`<config>/brain/<uuid>/.system_generated`) as the hand is taken: a
/// background task's `RUNNING` row appended to the transcript, or a HIGH
/// message unread. The other changes are the kernel's or the screen's.
fn change_on_disk(generated: &Path, what: Between) {
    match what {
        Between::Transcript => {
            let path = generated.join("logs/transcript.jsonl");
            let mut text = std::fs::read_to_string(&path).expect("the transcript");
            text.push_str(&task_started(9, "sleep 60", AT));
            std::fs::write(&path, text).expect("a task row");
        }
        Between::Message => {
            let dir = generated.join("messages");
            std::fs::create_dir_all(&dir).expect("messages");
            let id = "b58339bf-f840-45a7-b92f-12922cc8a8cb";
            std::fs::write(
                dir.join(format!("{id}.json")),
                message_body(id, "MESSAGE_PRIORITY_HIGH"),
            )
            .expect("a message");
            std::fs::write(dir.join("read.json"), "{}").expect("read.json");
        }
        Between::Summary | Between::Child | Between::Screen | Between::Shared => {}
    }
}

impl LiveRig {
    #[allow(clippy::too_many_lines)]
    fn new(name: &str, knobs: Knobs) -> LiveRig {
        let dir = scratch(name);
        let base = std::fs::canonicalize(&*dir).expect("canonical scratch");
        let home = base.join("agy-home");
        let config = home.join(".gemini/antigravity-cli");
        std::fs::create_dir_all(config.join(ag::PRESENCE)).expect("presence");
        std::fs::create_dir_all(config.join(ag::CRASHES)).expect("crashes");
        let lock = ag::presence_lock(&config, C1);
        std::fs::write(&lock, "").expect("lock");
        let crash = config
            .join(ag::CRASHES)
            .join(format!("crash_{TUI}_{CRASH_UUID}.log"));
        std::fs::write(&crash, "").expect("crash file");
        let generated = config.join("brain").join(C1).join(".system_generated");
        std::fs::create_dir_all(generated.join("logs")).expect("logs");
        // `Err(())`: no transcript at all.
        if let Ok(text) = knobs
            .transcript
            .clone()
            .unwrap_or_else(|| Ok(finished_turn(AT)))
        {
            std::fs::write(generated.join("logs/transcript.jsonl"), text).expect("transcript");
        }
        if !knobs.messages.is_empty() || knobs.read_json.is_some() {
            let dir = generated.join("messages");
            std::fs::create_dir_all(dir.join("undelivered")).expect("messages");
            let now = std::time::SystemTime::now();
            let mut read = Vec::new();
            for m in &knobs.messages {
                let path = dir.join(format!("{}.json", m.id));
                std::fs::write(&path, message_body(m.id, m.priority)).expect("message");
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .and_then(|f| f.set_modified(now - Duration::from_millis(m.ago_ms)))
                    .expect("its time");
                if m.read {
                    read.push(format!("\"{}\":true", m.id));
                }
            }
            let read_json = knobs
                .read_json
                .map_or_else(|| format!("{{{}}}", read.join(",")), str::to_string);
            std::fs::write(dir.join("read.json"), read_json).expect("read.json");
        }
        let store = base.join("prefix/store/agy");
        let exe = lay(&store, BUILD, VERSION);
        let new_exe = lay(&store, NEW_BUILD, NEW_VERSION);
        let old_exe = lay(&store, OLD_BUILD, OLD_VERSION);
        let twin = base.join("prefix/agents/agy");
        std::fs::create_dir_all(twin.parent().expect("agents")).expect("agents");
        let target = match knobs.twin.unwrap_or(Twin::Newer) {
            Twin::Newer => &new_exe,
            Twin::Same => &exe,
            Twin::Older => &old_exe,
        };
        std::fs::write(
            &twin,
            format!(
                "#!/bin/sh\n# atpkg shim — exec so the tool authenticates at its real path.\nexport AGY_CLI_DISABLE_AUTO_UPDATE='true'\nexec '{}' \"$@\"\n",
                target.display()
            ),
        )
        .expect("twin");
        let exited = Arc::new(AtomicBool::new(false));
        let relaunched = Arc::new(AtomicBool::new(false));
        let moved = Arc::new(AtomicBool::new(false));
        let changed = Arc::new(AtomicBool::new(false));
        let at_signal = Arc::new(Mutex::new(None));
        let state = base.join("state");
        let before = knobs.screen.map_or_else(
            || served(aterm_phase::agy::fixtures::lab::END_OF_TURN),
            served,
        );
        let statuses = std::sync::atomic::AtomicUsize::new(0);
        let busy = served(aterm_phase::agy::fixtures::lab::BUSY_THINKING);
        let (sock, asked) = {
            let (exited, relaunched, moved, changed, at_signal) = (
                Arc::clone(&exited),
                Arc::clone(&relaunched),
                Arc::clone(&moved),
                Arc::clone(&changed),
                Arc::clone(&at_signal),
            );
            let generated = generated.clone();
            let record = state.join("upgrade").join(format!("agy-{TAB}.json"));
            let knobs = knobs.clone();
            crate::harness::fake_instance(&base.join("t.sock"), TOKEN, move |line| {
                let verb = line
                    .split_whitespace()
                    .find(|w| !w.starts_with('@'))
                    .unwrap_or("");
                let gone = exited.load(Ordering::SeqCst);
                let back = relaunched.load(Ordering::SeqCst);
                match verb {
                    "sessions" => format!("OK 1\nlocal {TAB} 0 idle agy"),
                    "text" => format!(
                        "OK {}",
                        if back {
                            agy_idle()
                        } else if gone {
                            shell_after()
                        } else if knobs.between == Some(Between::Screen)
                            && changed.load(Ordering::SeqCst)
                        {
                            busy.clone()
                        } else {
                            before.clone()
                        }
                    ),
                    "cell" => "OK > ccd3db 1e1e1e none".to_string(),
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
                        } else if gone {
                            blocks(0, true, false)
                        } else if let Some(cmdline) = knobs.cmdline {
                            format!(
                                r#"{{"blocks":[{{"id":7,"state":"executing","exit":null,"prompt":2,"cmd":2,"cmdcol":2,"out":3,"end":null,"cmdline":{}}}]}}"#,
                                aterm_json::to_string(&aterm_json::Value::from(cmdline))
                                    .expect("cmdline")
                            )
                        } else {
                            running()
                        }
                    ),
                    "blocktext" => "OK 0".to_string(),
                    "line" => "OK % ".to_string(),
                    "lease" => {
                        if knobs.lock_moves {
                            moved.store(true, Ordering::SeqCst);
                        }
                        if let Some(what) = knobs.between
                            && !changed.swap(true, Ordering::SeqCst)
                        {
                            change_on_disk(&generated, what);
                        }
                        "ERR unscripted".to_string()
                    }
                    "turn" => {
                        if line.contains("'--conversation=") {
                            relaunched.store(true, Ordering::SeqCst);
                        }
                        "OK 0 turn submitted=1 status=settled seq=1 id=1".to_string()
                    }
                    _ => "ERR unscripted".to_string(),
                }
            })
        };
        let mut argv = vec![exe.to_string_lossy().into_owned()];
        argv.extend(
            knobs
                .argv
                .clone()
                .unwrap_or_else(|| vec!["--effort".to_string(), "high".to_string()]),
        );
        let opts = Opts {
            home: base.join("user"),
            state,
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            dry_run: false,
            human_grace_s: 120,
            hand_back: knobs.hand_back,
            background: knobs.background,
            aterm_state: None,
            // `[harness] upgrade_ask` on: agy's flow is quiet whatever it says.
            ask: true,
            several_logins: false,
            usage_credits: false,
            usage_tap: None,
            instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
            agents_off: Vec::new(),
            land: false,
            timings: crate::harness::Timings::FAST,
        };
        LiveRig {
            kernel: LiveKernel {
                exited,
                relaunched,
                moved,
                changed,
                lock,
                crash,
                exe,
                new_exe,
                home,
                argv,
                knobs,
            },
            asked,
            config,
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
        .expect("an agy leads the tab");
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
    /// exit watch and its idle point reach, `agy_carry_in_flight`), over this
    /// rig's kernel, as a loop's carry takes it (`hand_back` off).
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
}

impl Drop for LiveRig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&*self.dir);
    }
}

/// AN IDLE AGY BEHIND A NEWER BUILD IS RESTARTED ONTO IT, ITS CONVERSATION
/// KEPT, AND NOTHING IS TYPED AFTER: one SIGTERM to that very process; the
/// claim on disk before it (in flight, the upgrade's cause, the process, the
/// conversation, the twin, quiet), which makes its exit `0` the harness's own
/// restart; one line typed — the managed twin with the launch's `--effort`
/// and `--conversation=<uuid>` — and no continuation; the record done once the
/// new process holds the conversation's presence lock (`done:resumed`).
/// `[harness] upgrade_ask` is on in this rig: the flow is quiet whatever it
/// says. NEGATIVE CONTROL: a crash relaunch's record of the same process (its
/// cause) is no claim.
#[test]
fn an_idle_agy_behind_is_restarted_onto_the_new_build_and_nothing_is_typed() {
    let rig = LiveRig::new("idle", Knobs::default());
    let r = rig.visit();
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert_eq!(
        (r.from.as_str(), r.to.as_str()),
        (VERSION, "1.4.1(managed)")
    );
    assert_eq!(
        rig.asked("signal"),
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
        (Agent::Agy, TUI, TAB, C1)
    );
    assert!(
        matches!(claim.phase, Phase::Exiting { .. }),
        "{:?}",
        claim.phase
    );
    assert!(claim.cause.is_empty() && claim.quiet && claim.in_flight());
    assert_eq!(claim.twin, rig.twin.to_string_lossy());
    assert_eq!(claim.agy_config, rig.config.to_string_lossy());
    assert!(
        ended_by_restart(&claim, Agent::Agy, TAB, TUI, now_s()),
        "its exit is the upgrade's own restart"
    );
    let crash = St {
        cause: crate::harness::relaunch::CAUSE_EXIT.to_string(),
        ..claim.clone()
    };
    assert!(
        !ended_by_restart(&crash, Agent::Agy, TAB, TUI, now_s()),
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
        line.contains(&format!("'--effort' 'high' '--conversation={C1}'")),
        "{line}"
    );
    assert!(!line.contains("/store/agy/"), "never a store path: {line}");
    let st = rig.record().expect("the record");
    assert_eq!(st.phase, Phase::Done);
    assert_eq!((st.resumed_pid, st.resumed_on.as_str()), (NEW, NEW_VERSION));
    assert_eq!(
        st.outcome,
        format!(
            "agy upgraded to {NEW_VERSION} (from {VERSION}) · the same conversation resumed, nothing typed"
        )
    );
}

/// THE HOST'S STEP hands the relaunched agy back to its loop (`adopted`: it
/// leads the tab and holds the conversation's lock), whose next idle point
/// carries it on QUIETLY: the lock held by the new process, nothing typed
/// (`done:resumed`).
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
    let st = rig.record().expect("in flight");
    assert!(st.in_flight() && st.quiet, "{st:?}");
    let r = rig.carry();
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert_eq!(rig.asked("turn").len(), 1, "the line alone");
}

/// THE EXIT CARRIED BY THE HOST'S EXIT WATCH: a shell slow to take its
/// terminal back leaves the step `wait:shell-prompt` with the claim in
/// flight. The host reads that exit as the harness's own restart
/// (`relaunch::restarted`), never `ended:graceful-exit`, and the lane's own
/// carry types the line once the prompt is back.
#[test]
fn a_restart_left_in_flight_is_carried_as_the_harnesss_own() {
    let ready = Arc::new(AtomicBool::new(false));
    let rig = LiveRig::new(
        "slow-shell",
        Knobs {
            shell_slow: Some(Arc::clone(&ready)),
            ..Knobs::default()
        },
    );
    let r = rig.visit();
    assert_eq!(r.step, "wait:shell-prompt", "{r:?}");
    assert!(
        crate::harness::relaunch::restarted(&rig.opts, Agent::Agy, Some(TUI)),
        "the exit is the harness's own restart"
    );
    assert!(rig.asked("turn").is_empty(), "nothing typed yet");
    ready.store(true, Ordering::SeqCst);
    let r = rig.carry();
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert_eq!(rig.asked("turn").len(), 1);
}

/// THE QUIET ADOPTION NEEDS THE LOCK: a relaunched agy that never takes the
/// conversation's presence lock started a fresh one (agy says `conversation
/// not found`): `refused:started-fresh`, nothing typed but the line.
#[test]
fn a_relaunch_that_never_holds_the_lock_started_fresh() {
    let mut rig = LiveRig::new(
        "lockless",
        Knobs {
            lockless: true,
            ..Knobs::default()
        },
    );
    // A lock that is never taken is waited out to the bound: 50 ms of it
    // decides as the test clock's second did.
    rig.opts.timings.agy_lock = Duration::from_millis(50);
    let r = rig.visit();
    assert_eq!(r.step, "refused:started-fresh", "{r:?}");
    assert_eq!(rig.asked("turn").len(), 1);
}

/// One knob, the word the step waits on, and nothing signalled or typed: no
/// record is left where none stood.
fn holds(name: &str, knobs: Knobs, want: &str) {
    let rig = LiveRig::new(name, knobs);
    let r = rig.visit();
    assert_eq!(r.step, want, "{name}: {r:?}");
    assert!(rig.asked("signal").is_empty(), "{name}: signalled");
    assert!(rig.asked("turn").is_empty(), "{name}: typed");
    assert!(rig.record().is_none(), "{name}: a record was left");
}

/// One knob, and the restart goes ahead.
fn restarts(name: &str, knobs: Knobs) {
    let rig = LiveRig::new(name, knobs);
    let r = rig.visit();
    assert_eq!(r.step, "done:resumed", "{name}: {r:?}");
}

/// G-A2(b), THE TRANSCRIPT: a command task running, a daemon task, a
/// subagent working — each with no end row in this process's rows — hold the
/// restart; a torn transcript, and none at all, fail closed. POSITIVE: the
/// task ended and the subagent gone idle hold nothing, nor do rows of an
/// earlier process (a task its SIGTERM killed keeps its `RUNNING` row).
#[test]
fn the_transcript_holds_running_work() {
    let with = |rows: String| Knobs {
        transcript: Some(Ok(finished_turn(AT) + &rows)),
        ..Knobs::default()
    };
    holds(
        "tr-task",
        with(task_started(4, "sleep 150; echo LAB-BG-DONE", AT)),
        "wait:background-work",
    );
    holds(
        "tr-daemon",
        with(task_started(35, "sleep 300", AT)),
        "wait:background-work",
    );
    holds(
        "tr-sub",
        with(subagent_spawned(AT)),
        "wait:subagent-working",
    );
    holds(
        "tr-torn",
        with("{\"step_index\":9,\"sou".to_string()),
        "wait:log-unreadable",
    );
    holds(
        "tr-none",
        Knobs {
            transcript: Some(Err(())),
            ..Knobs::default()
        },
        "wait:log-unreadable",
    );
    restarts(
        "tr-ended",
        with(task_started(4, "sleep 150", AT) + &task_ended(4, AT)),
    );
    restarts("tr-idled", with(subagent_spawned(AT) + &subagent_idle(AT)));
    restarts(
        "tr-earlier",
        Knobs {
            transcript: Some(Ok(
                task_started(23, "sleep 400", BEFORE) + &finished_turn(AT)
            )),
            ..Knobs::default()
        },
    );
}

/// G-A1 AND G-A2(c), THE SUMMARY ROW: `RUNNING` (a turn, or a turn's end a
/// moment after the screen reads it over) holds the restart as `turn-open`,
/// `not_fully_idle` as background work, a subagent's own row `RUNNING` as a
/// subagent; a database that cannot be read, and no row of the
/// conversation, fail closed.
#[test]
fn the_summary_row_holds_the_turn_and_the_work() {
    let answer = |text: String| Knobs {
        summary: Some(Ok(text)),
        ..Knobs::default()
    };
    holds(
        "db-running",
        answer(format!("{C1}|CASCADE_RUN_STATUS_RUNNING|1|\n")),
        "wait:turn-open",
    );
    holds(
        "db-nfi",
        answer(format!("{C1}|{STATUS_IDLE}|1|\n")),
        "wait:background-work",
    );
    holds(
        "db-sub",
        answer(format!(
            "{C1}|{STATUS_IDLE}|1|\n{SUB}|CASCADE_RUN_STATUS_RUNNING|1|{C1}\n"
        )),
        "wait:background-work",
    );
    holds(
        "db-sub-only",
        answer(format!(
            "{C1}|{STATUS_IDLE}|0|\n{SUB}|CASCADE_RUN_STATUS_RUNNING|1|{C1}\n"
        )),
        "wait:subagent-working",
    );
    holds(
        "db-unread",
        Knobs {
            summary: Some(Err(())),
            ..Knobs::default()
        },
        "wait:summary-unreadable",
    );
    holds("db-norow", answer(String::new()), "wait:summary-unproved");
    restarts(
        "db-killed-sub",
        answer(format!(
            "{C1}|{STATUS_IDLE}|0|\n{SUB}|{STATUS_IDLE}|0|{C1}\n"
        )),
    );
}

/// G-A2(e), THE WAKES: a waking message unread, or read but just written,
/// holds the restart (`queued-work`); one `read.json` cannot parse fails
/// closed. POSITIVE: an old message read, and a LOW one unread (a cancelled
/// task's end waits for the person's next message), hold nothing.
#[test]
fn a_pending_wake_holds_the_restart() {
    let msg = |priority, ago_ms, read| Msg {
        id: "a48339bf-f840-45a7-b92f-12922cc8a8ca",
        priority,
        ago_ms,
        read,
    };
    let with = |m: Msg| Knobs {
        messages: vec![m],
        ..Knobs::default()
    };
    holds(
        "wake-unread",
        with(msg("MESSAGE_PRIORITY_HIGH", 60_000, false)),
        "wait:queued-work",
    );
    holds(
        "wake-fresh",
        with(msg("MESSAGE_PRIORITY_HIGH", 0, true)),
        "wait:queued-work",
    );
    holds(
        "wake-torn",
        Knobs {
            messages: vec![msg("MESSAGE_PRIORITY_HIGH", 60_000, true)],
            read_json: Some("{"),
            ..Knobs::default()
        },
        "wait:messages-unreadable",
    );
    restarts(
        "wake-read",
        with(msg("MESSAGE_PRIORITY_HIGH", 60_000, true)),
    );
    restarts("wake-low", with(msg("MESSAGE_PRIORITY_LOW", 0, false)));
}

/// G-A1'S SCREEN HALF AND G-A2(a)/(d), AND THE PERSON: agy's own background
/// work drawn under the composer (a task's panel row; with the panel turned
/// off, a subagent's count alone) holds the restart as background work; a
/// turn's spinner, an exit pending and a turn that ended on a wall as a
/// screen not idle; a draft, a hold, a child process of any name (a daemon
/// task's bare `sleep`, which Claude Code's shell-name list misses) and an
/// unreadable process table each by its own word.
#[test]
fn the_screen_the_person_and_any_child_hold_the_restart() {
    use aterm_phase::agy::fixtures::lab;
    let screen = |fixture| Knobs {
        screen: Some(fixture),
        ..Knobs::default()
    };
    holds("bg-panel", screen(lab::BG_TASK), "wait:background-work");
    holds(
        "bg-off",
        screen(lab::BG_PANEL_OFF_SUBAGENT),
        "wait:background-work",
    );
    holds("busy", screen(lab::BUSY_THINKING), "wait:screen-not-idle");
    holds(
        "exit-pending",
        screen(lab::EXIT_CONFIRM_CTRL_D),
        "wait:screen-not-idle",
    );
    holds("wall", screen(lab::ERROR_401), "wait:screen-not-idle");
    holds("draft", screen(lab::DRAFT), "wait:draft");
    holds(
        "held",
        Knobs {
            held: true,
            ..Knobs::default()
        },
        "wait:held",
    );
    holds(
        "child",
        Knobs {
            child: Some("sleep"),
            ..Knobs::default()
        },
        "wait:child-process",
    );
    holds(
        "table",
        Knobs {
            table_unread: true,
            ..Knobs::default()
        },
        "wait:process-table",
    );
}

/// THE BUILD AND THE ORDER: a probe of the new build out of time or exiting
/// is no proof (`wait:build-fails`); a twin naming the running build or an
/// older one moves nothing (`current`); a launch no relaunch can carry is
/// never restarted (`skip:…`); a fresh agy that holds no conversation has
/// nothing to resume (`wait:no-session`); and at a background break nothing
/// acts (`wait:break`).
#[test]
fn the_build_the_order_and_the_launch_hold_the_restart() {
    holds(
        "probe-out",
        Knobs {
            probe: Some(Probe::TimedOut),
            ..Knobs::default()
        },
        "wait:build-fails",
    );
    holds(
        "probe-exit",
        Knobs {
            probe: Some(Probe::Exited),
            ..Knobs::default()
        },
        "wait:build-fails",
    );
    for twin in [Twin::Same, Twin::Older] {
        holds(
            "current",
            Knobs {
                twin: Some(twin),
                ..Knobs::default()
            },
            "current",
        );
    }
    let rig = LiveRig::new(
        "remote",
        Knobs {
            argv: Some(vec!["--remote-control".to_string()]),
            ..Knobs::default()
        },
    );
    let r = rig.visit();
    assert!(r.step.starts_with("skip:argv:"), "{r:?}");
    assert!(rig.asked("signal").is_empty() && rig.record().is_none());
    holds(
        "fresh",
        Knobs {
            no_conversation: true,
            ..Knobs::default()
        },
        "wait:no-session",
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

/// THE LAST LOOK AND THE SIGNAL put the claim back: a hold landing between
/// the claim and the signal, the conversation's lock taken by another
/// process meanwhile, or ANY witness of the gate turning in that window — a
/// background task's row in the transcript, a waking message unread, the
/// summary row `RUNNING`, a child process, a busy screen, another agy on the
/// conversation (`wait:changed-before-signal`: the last look is the whole
/// gate again, never its hold half alone) — and a signal the server refuses
/// (`wait:signal-held`) leave no record, nothing typed.
#[test]
fn a_change_before_the_signal_or_its_refusal_takes_the_claim_back() {
    holds(
        "late-hold",
        Knobs {
            held_from: Some(2),
            ..Knobs::default()
        },
        "wait:changed-before-signal",
    );
    for (name, what) in [
        ("late-task", Between::Transcript),
        ("late-wake", Between::Message),
        ("late-turn", Between::Summary),
        ("late-child", Between::Child),
        ("late-screen", Between::Screen),
        ("late-share", Between::Shared),
    ] {
        holds(
            name,
            Knobs {
                between: Some(what),
                ..Knobs::default()
            },
            "wait:changed-before-signal",
        );
    }
    holds(
        "lock-moved",
        Knobs {
            lock_moves: true,
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
    assert!(rig.asked("turn").is_empty() && rig.record().is_none());
}

/// G-A3, THE CLAIM ON DISK BEFORE THE SIGNAL: a claim whose write does not
/// complete (a directory where the tab's record goes) waits
/// (`wait:claim-unwritten`), signals nothing and types nothing, and leaves
/// what stood there as it was — an exit with no claim on disk would be read
/// as someone's graceful end, and the agy it ended never relaunched.
#[test]
fn a_claim_that_cannot_be_written_signals_nothing() {
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
    assert_eq!(refused.step, "wait:claim-unwritten", "{refused:?}");
    assert!(
        blocked.asked("signal").is_empty() && blocked.asked("turn").is_empty(),
        "AGY_CLAIM_MUST_BE_WRITTEN_BEFORE_SIGNAL: {refused:?}"
    );
}

/// G-A4, ANOTHER AGY ON THE CONVERSATION: one whose argv names it, or one
/// holding its database with no lock (`agy -c` in another tab), holds the
/// restart (`wait:conversation-shared`) — its relaunch could never type while
/// that agy has it open — and so does a process table that cannot be read.
/// POSITIVE: the agy's own argv naming its conversation is no other agy.
#[test]
fn another_agy_on_the_conversation_holds_the_restart() {
    for (name, shared) in [
        ("shared-argv", Shared::Resuming),
        ("shared-lockless", Shared::Lockless),
        ("shared-unread", Shared::Unread),
    ] {
        holds(
            name,
            Knobs {
                shared: Some(shared),
                ..Knobs::default()
            },
            "wait:conversation-shared",
        );
    }
    restarts(
        "own-argv",
        Knobs {
            argv: Some(vec![format!("--conversation={C1}")]),
            ..Knobs::default()
        },
    );
}

/// G-A4, A LAUNCH THAT SET ITS OWN ENVIRONMENT on its typed line (the shell's
/// record of the command the tab runs) is never restarted
/// (`skip:env-not-carried`): the relaunch line runs in the shell's
/// environment, and the key and endpoint would be gone. POSITIVE: a line
/// that sets nothing ahead of agy, and one setting `HOME` alone (which the
/// relaunch carries), restart.
#[test]
fn a_launch_that_set_its_own_environment_is_not_restarted() {
    holds(
        "inline-env",
        Knobs {
            cmdline: Some(
                "GEMINI_API_KEY=k GOOGLE_GEMINI_BASE_URL=https://proxy agy --effort high",
            ),
            ..Knobs::default()
        },
        "skip:env-not-carried",
    );
    restarts(
        "typed-plain",
        Knobs {
            cmdline: Some("cd ~/p && agy --effort high"),
            ..Knobs::default()
        },
    );
    restarts(
        "typed-home",
        Knobs {
            cmdline: Some("HOME=/s/agy-home agy --effort high"),
            ..Knobs::default()
        },
    );
}

/// [`live::upgrade_due`]: due while behind, not due when current, never
/// another lane's tab (a leader that is no `agy`).
#[test]
fn the_tab_is_due_while_its_build_is_behind() {
    let behind = LiveRig::new("due", Knobs::default());
    let tabs = LiveRig::tabs();
    let due = |rig: &LiveRig, tabs: &[LiveTab]| {
        live::upgrade_due_with(&rig.opts, tabs, Some(rig.twin.clone()), &rig.kernel)
    };
    assert_eq!(due(&behind, &tabs), Some(Due::Yes));
    let current = LiveRig::new(
        "due-current",
        Knobs {
            twin: Some(Twin::Same),
            ..Knobs::default()
        },
    );
    assert_eq!(due(&current, &tabs), Some(Due::No));
    let other = vec![LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(SHELL)),
    }];
    assert_eq!(
        due(&behind, &other),
        None,
        "a shell leading the tab is not the agy lane's"
    );
}

// ---------------------------------------------------------------- the live database

/// THE SUMMARY DATABASE IS READ LIVE, WITH NO LOCK, IN BOTH OF ITS STATES
/// (macOS: Apple's `/usr/bin/sqlite3`), over a database laid as agy lays its
/// own (WAL mode, the table's columns): CLOSED — no `-wal`, no `-shm`, as agy
/// leaves it some 30 s after its last write — it is read as an immutable
/// file, and the read leaves neither file behind; OPEN — another connection
/// holding it with rows still in its `-wal` — from a private copy of the file
/// and its `-wal`, which holds them once the holder has committed. NEGATIVE
/// CONTROLS: a rollback journal with something in it beside the file (a write
/// under way) is no answer, and neither is a conversation that is no UUIDv4.
#[cfg(target_os = "macos")]
#[test]
fn the_summary_database_is_read_only_open_or_closed() {
    use std::io::Write as _;
    let dir = scratch("summaries-db");
    let config = std::fs::canonicalize(&*dir).expect("canonical");
    let db = config.join(live::SUMMARIES);
    let side = |ext: &str| config.join(format!("{}-{ext}", live::SUMMARIES));
    let schema = "PRAGMA journal_mode=WAL; CREATE TABLE conversation_summaries \
                  (conversation_id text PRIMARY KEY, status text NOT NULL DEFAULT '', \
                  not_fully_idle numeric NOT NULL DEFAULT false, parent_conversation_id text \
                  NOT NULL DEFAULT '');";
    let row = |id: &str, status: &str, nfi: u8, parent: &str| {
        format!(
            "INSERT OR REPLACE INTO conversation_summaries VALUES ('{id}','{status}',{nfi},'{parent}');"
        )
    };
    let made = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .arg(format!(
                "{schema}{} PRAGMA wal_checkpoint(TRUNCATE);",
                row(C1, "CASCADE_RUN_STATUS_RUNNING", 1, "")
            )),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("sqlite3 within the hang detector: {e}"));
    assert!(made.status.success(), "{made:?}");
    // Closed as agy closes it: every write checkpointed into the file, and
    // the `-wal` and `-shm` gone (Apple's `sqlite3` keeps them; agy's own
    // SQLite deletes both at its close, measured).
    let _ = std::fs::remove_file(side("wal"));
    let _ = std::fs::remove_file(side("shm"));
    let out = live::read_summaries(&config, C1).expect("read closed");
    assert_eq!(out.trim(), format!("{C1}|CASCADE_RUN_STATUS_RUNNING|1|"));
    assert!(
        !side("wal").exists() && !side("shm").exists(),
        "the read writes nothing"
    );
    // Open: a connection holds it, its last write still in the `-wal`. The
    // holder reads its row back once its INSERT is done, and the database is
    // read only then. (Read through SQLite's own open while the holder's first
    // transaction ran, a reader that found the WAL index not yet built took
    // its write lock to build it, and the shell, which sets no busy timeout,
    // failed the INSERT `database is locked` and lost it: fc50504bd3, and the
    // reason the read now takes no lock.)
    let mut holder = aterm_uds::spawnfd::command("/usr/bin/sqlite3")
        .arg(&db)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("holder");
    let mut stdin = holder.stdin.take().expect("its stdin");
    let stdout = holder.stdout.take().expect("its stdout");
    // What it printed, up to the row it read back or its end.
    let (tell, heard) = std::sync::mpsc::channel();
    let listener = std::thread::spawn(move || {
        let mut said = String::new();
        let mut lines = std::io::BufRead::lines(std::io::BufReader::new(stdout));
        while let Some(Ok(line)) = lines.next() {
            said.push_str(&line);
            said.push('\n');
            if line.starts_with("holder reads ") {
                break;
            }
        }
        let _ = tell.send(said);
    });
    writeln!(
        stdin,
        "PRAGMA wal_autocheckpoint=0; {} SELECT 'holder reads ' || status FROM \
         conversation_summaries WHERE conversation_id = '{C1}';",
        row(C1, STATUS_IDLE, 0, "")
    )
    .expect("write");
    stdin.flush().expect("flush");
    // A hang detector: the holder reads its row back within a minute.
    let said = heard
        .recv_timeout(Duration::from_secs(60))
        .unwrap_or_default();
    if !said.contains("holder reads ") {
        let _ = holder.kill();
    }
    let _ = listener.join();
    assert_eq!(
        said,
        format!("0\nholder reads {STATUS_IDLE}\n"),
        "the holder's write is committed"
    );
    assert!(
        side("wal").exists() && side("shm").exists(),
        "the holder keeps the database open"
    );
    assert_eq!(
        live::read_summaries(&config, C1).as_deref().map(str::trim),
        Ok(format!("{C1}|{STATUS_IDLE}|0|").as_str()),
        "the open database's own rows"
    );
    drop(stdin);
    let _ = holder.wait();
    // A rollback journal with something in it: a write under way, no answer.
    std::fs::write(side("journal"), b"\xd9\xd5\x05\xf9").expect("journal");
    assert_eq!(live::read_summaries(&config, C1), Err(()));
    std::fs::remove_file(side("journal")).expect("no journal");
    assert!(
        live::read_summaries(&config, C1).is_ok(),
        "and with none, an answer"
    );
    assert_eq!(live::read_summaries(&config, "not-a-uuid"), Err(()));
    let _ = std::fs::remove_dir_all(&*dir);
}

/// THE SUMMARY READ NEVER LOCKS OUT A WRITER (macOS: Apple's `/usr/bin/sqlite3`),
/// over the database as a holder that died leaves it — rows still in a large
/// `-wal`, the `-shm` beside it, no connection: the state in which a reader
/// through SQLite, the first to open it, empties the WAL index and rebuilds it
/// holding the index's write and recover locks (agy's own open makes the same
/// unbuilt index for a moment). The witness (`aterm_uds::foreign_db::witness`)
/// watches the recover lock for the whole read; had the read held it, it froze
/// the reader there and ran a writer with NO busy timeout (the shell, as
/// fc50504bd3's holder): measured with the read through SQLite's read-only
/// open, its INSERT failed `database is locked` and the row was lost. The read
/// holds none, answers the `-wal`'s row (from its private copy), and the
/// writer's row lands.
#[cfg(all(target_os = "macos", debug_assertions))]
#[test]
fn the_summary_read_never_locks_out_a_writer_with_no_busy_timeout() {
    use std::io::Write as _;
    let dir = scratch("summaries-writer");
    let config = std::fs::canonicalize(&*dir).expect("canonical");
    let db = config.join(live::SUMMARIES);
    let side = |ext: &str| config.join(format!("{}-{ext}", live::SUMMARIES));
    let row = |status: &str, nfi: u8| {
        format!(
            "INSERT OR REPLACE INTO conversation_summaries VALUES ('{C1}','{status}',{nfi},'');"
        )
    };
    // The holder: the table, eight thousand pages more (their recovery is
    // what a reader would hold the lock through), the conversation's row; and
    // then it is killed, as a crash ends agy, its writes still in the `-wal`.
    let mut holder = aterm_uds::spawnfd::command("/usr/bin/sqlite3")
        .arg(&db)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("holder");
    let mut stdin = holder.stdin.take().expect("its stdin");
    writeln!(
        stdin,
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE \
         conversation_summaries (conversation_id text PRIMARY KEY, status text NOT NULL \
         DEFAULT '', not_fully_idle numeric NOT NULL DEFAULT false, parent_conversation_id \
         text NOT NULL DEFAULT ''); CREATE TABLE pad (b); WITH RECURSIVE n(i) AS (SELECT 1 \
         UNION ALL SELECT i + 1 FROM n WHERE i < 8000) INSERT INTO pad SELECT randomblob(4000) \
         FROM n; {} SELECT 'holder wrote';",
        row("CASCADE_RUN_STATUS_RUNNING", 1)
    )
    .expect("write");
    stdin.flush().expect("flush");
    // A hang detector: the holder says it wrote, or ends.
    let said: Vec<String> =
        std::io::BufRead::lines(std::io::BufReader::new(holder.stdout.take().expect("out")))
            .map_while(Result::ok)
            .take_while(|l| l != "holder wrote")
            .collect();
    assert_eq!(said, ["wal", "0"], "the holder wrote");
    holder.kill().expect("killed, as a crash ends it");
    let _ = holder.wait();
    drop(stdin);
    assert!(side("wal").exists() && side("shm").exists(), "left behind");
    let writer = || {
        aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command("/usr/bin/sqlite3")
                .arg(&db)
                .arg(row(STATUS_IDLE, 0)),
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap_or_else(|e| panic!("the writer within the hang detector: {e}"))
    };
    // The index's recover lock (byte 122 of the `-shm`): held, with its write
    // lock, from the moment a reader has the index's first page laid until the
    // index is rebuilt, when a writer that opens meets it at once (a writer
    // that came sooner retries for some 10 s by itself).
    let (read, caught) = aterm_uds::foreign_db::witness::while_read(
        &side("shm"),
        122,
        1,
        || live::read_summaries(&config, C1),
        writer,
    );
    assert!(
        caught.is_none(),
        "the read held the WAL index's lock; a writer with no busy timeout met it: {:?}",
        caught.map(|(pid, out)| (
            pid,
            out.status,
            String::from_utf8_lossy(&out.stderr).into_owned()
        ))
    );
    assert_eq!(
        read.as_deref().map(str::trim),
        Ok(format!("{C1}|CASCADE_RUN_STATUS_RUNNING|1|").as_str()),
        "the `-wal`'s row"
    );
    let wrote = writer();
    assert!(wrote.status.success(), "{wrote:?}");
    let back = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .arg(format!(
                "SELECT status FROM conversation_summaries WHERE conversation_id = '{C1}';"
            )),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("the read back within the hang detector: {e}"));
    assert_eq!(
        String::from_utf8_lossy(&back.stdout).trim(),
        STATUS_IDLE,
        "the writer's row landed"
    );
    let _ = std::fs::remove_dir_all(&*dir);
}

/// The messages look: none, a directory that cannot be read, a message that
/// cannot be read.
#[test]
fn the_messages_look_reads_the_directory_and_fails_closed() {
    let dir = scratch("messages-look");
    let config = std::fs::canonicalize(&*dir).expect("canonical");
    assert_eq!(live::messages_look(&config, C1, 1), Some(0), "no directory");
    let messages = config
        .join("brain")
        .join(C1)
        .join(".system_generated/messages");
    std::fs::create_dir_all(&messages).expect("messages");
    std::fs::write(
        messages.join("m1.json"),
        message_body("m1", "MESSAGE_PRIORITY_HIGH"),
    )
    .expect("message");
    assert_eq!(
        live::messages_look(&config, C1, u64::MAX),
        Some(1),
        "unread"
    );
    std::fs::write(messages.join("read.json"), r#"{"m1":true}"#).expect("read");
    assert_eq!(
        live::messages_look(&config, C1, u64::MAX),
        Some(0),
        "read, long ago"
    );
    std::fs::write(messages.join("m2.json"), "{").expect("torn");
    assert_eq!(live::messages_look(&config, C1, u64::MAX), None, "torn");
    let _ = std::fs::remove_dir_all(&*dir);
}

// ---------------------------------------------------------------- Tier-1

type State = BTreeMap<&'static str, i64>;

/// The running build's version, and the twin's for each `landed`.
fn versions(landed: i64) -> (&'static str, &'static str) {
    match landed {
        1 => (VERSION, NEW_VERSION),
        2 => (VERSION, OLD_VERSION),
        _ => (VERSION, VERSION),
    }
}

/// The screen a state stands for, as agy 1.4.0 draws it: a draft (over the
/// panel where work runs and it is drawn); a screen not idle (a turn's
/// spinner over the panel, a turn's spinner, an exit pending); else the work
/// drawn under an ended turn (its panel row, or its count alone with the
/// panel off) or the end of a turn.
fn state_rows(s: &State) -> Vec<String> {
    use aterm_phase::agy::fixtures::lab;
    let work = s["task"] == 1 || s["daemon"] == 1 || s["subagent"] == 1;
    let panel = s["panel"] == 1;
    let fixture = if s["draft"] == 1 {
        if work && panel {
            lab::BG_DRAFT
        } else {
            lab::DRAFT
        }
    } else if s["shown"] == 1 {
        match (work && panel, s["turn"] == 1) {
            (true, _) => lab::BG_BUSY,
            (false, true) => lab::BUSY_THINKING,
            (false, false) => lab::EXIT_CONFIRM_CTRL_D,
        }
    } else if s["subagent"] == 1 {
        if panel {
            lab::BG_SUBAGENT
        } else {
            lab::BG_PANEL_OFF_SUBAGENT
        }
    } else if s["daemon"] == 1 {
        if panel {
            lab::BG_DAEMON
        } else {
            lab::BG_PANEL_OFF_DAEMON
        }
    } else if s["task"] == 1 {
        if panel {
            lab::BG_TASK
        } else {
            lab::BG_PANEL_OFF_TASK
        }
    } else {
        lab::END_OF_TURN
    };
    fixture_rows(fixture)
}

/// The summary query's answer a state stands for: the row `RUNNING` through
/// a turn (and a moment past its end), not fully idle while a task, a timer
/// or a subagent runs — never for a daemon task (measured) — and the
/// subagent's own row `RUNNING` while it works.
fn state_summary(s: &State) -> String {
    let status = if s["turn"] == 1 {
        "CASCADE_RUN_STATUS_RUNNING"
    } else {
        STATUS_IDLE
    };
    let nfi = u8::from(s["turn"] == 1 || s["task"] == 1 || s["subagent"] == 1);
    let mut out = format!("{C1}|{status}|{nfi}|\n");
    if s["subagent"] == 1 {
        out.push_str(&format!("{SUB}|CASCADE_RUN_STATUS_RUNNING|1|{C1}\n"));
    }
    out
}

/// The transcript a state stands for: a finished turn, a command task's
/// `RUNNING` row, a daemon task's, a subagent's spawn — each still open — and
/// a torn tail where it cannot be read.
fn state_transcript(s: &State) -> String {
    let mut text = finished_turn(AT);
    if s["task"] == 1 {
        text.push_str(&task_started(4, "sleep 150; echo LAB-BG-DONE", AT));
    }
    if s["daemon"] == 1 {
        text.push_str(&task_started(35, "sleep 300", AT));
    }
    if s["subagent"] == 1 {
        text.push_str(&subagent_spawned(AT));
    }
    if s["unread"] == 1 {
        text.push_str("{\"step_index\":9,\"sou");
    }
    text
}

/// THE REAL DECISION AT STATE `s`: the order of its builds
/// ([`pure::moves_onto`]) and the gate ([`pure::gate`]) over the look the
/// real readers make of its world, composed by the drive's own
/// [`live::look_of`] — the person by the `status` line's own rule
/// (`held_by_status`), the screen by agy's reader and the background-work
/// witness, the summary by its rule, the transcript by
/// [`pure::quiet_transcript`], the children by the process rule
/// (`descendants_in`: a command task's shell, a daemon's bare command; a
/// timer and a subagent are no process), the wakes by [`pure::wakes`], the
/// other agys on the conversation by [`live::sharers`] over a kernel that
/// answers one (`agy -c` in another tab) where the state says so: `rigs`,
/// alone and shared.
fn real_restarts(s: &State, rigs: &(LiveRig, LiveRig)) -> bool {
    let (running, target) = versions(s["landed"]);
    let rows = state_rows(s);
    let status = format!("OK schema=1 hold={} hand=- integration=on", s["held"]);
    let mut table = vec![(TUI, SHELL, "agy".into())];
    if s["task"] == 1 {
        table.push((CHILD, TUI, "zsh".into()));
    }
    if s["daemon"] == 1 {
        table.push((CHILD + 1, TUI, "sleep".into()));
    }
    let now = 1_792_349_400_000;
    let messages = if s["wake"] == 1 {
        vec![Message {
            id: "a48339bf".to_string(),
            body: message_body("a48339bf", "MESSAGE_PRIORITY_HIGH"),
            written_ms: now - 60_000,
        }]
    } else {
        Vec::new()
    };
    let rig = if s["shared"] == 1 { &rigs.1 } else { &rigs.0 };
    let shared = live::sharers(&rig.kernel, &rig.config, C1, TUI);
    // The look composed as the drive's own `look_at` composes it.
    let look: Look = live::look_of(
        crate::harness::upgrade_drive::held_by_status(&status, 120),
        Some(&rows),
        C1,
        Ok(state_summary(s)),
        pure::quiet_transcript(&state_transcript(s), false, START_MS),
        descendants_in(&table, TUI).map(|d| d.len()),
        pure::wakes(Some("{}"), &messages, now).ok(),
        shared,
    );
    pure::moves_onto(running, target) && pure::gate(&look).is_ok()
}

/// THE REAL CLAIM: the record the step writes before its signal, from a
/// snapshot of the agy it ends, read by the one rule the host's exit watch
/// reads a leaving agent by (`relaunch::ended_by_restart`): `true` is its exit
/// relaunched as the harness's own restart.
fn real_claim_relaunches() -> bool {
    let rig = LiveRig::new("tier1-claim", Knobs::default());
    let args = rig.kernel.args(TUI).expect("argv");
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
    assert_eq!(snap.session.as_deref(), Some(C1));
    let mv = Move {
        running: VERSION.to_string(),
        target: NEW_VERSION.to_string(),
        twin: rig.twin.clone(),
    };
    let now = now_s();
    let st = claim_record(&snap, &mv, now).expect("a claim");
    ended_by_restart(&st, Agent::Agy, TAB, TUI, now)
}

/// Tier-1 of `HarnessAgyQuietUpgrade` (aterm-models-harness
/// `harness_agy_quiet_upgrade_model`): every state the model reaches before
/// the move is replayed through the real order, screen reader, summary rule,
/// transcript reader, person rule, process rule, wake rule, sharers and gate:
/// the model's first `Look` is enabled EXACTLY where the real quiet flow's
/// first look opens, and once the claim is on disk its `Restart` exactly
/// where the real last look still opens and its `TakeBack` exactly where it
/// does not (the last look is the whole gate again). Its `ReadExit`
/// relaunches exactly as the real claim makes the exit the harness's own
/// (`ended_by_restart`), where the unclaimed exit, agy's `0`, is read by its
/// own table as someone's end (`exit_kind`: clean, never relaunched); the
/// drive test `a_claim_that_cannot_be_written_signals_nothing` binds
/// `ClaimUnwritten`. Negative controls, each mutant caught alone in a
/// reachable state: a restart over open work, over a daemon task, mid-turn,
/// over a pending wake, over another agy on the conversation, or on a stale
/// first look is where the real gate holds; `BuildReadAsOlder` where the real
/// order moves; `BuildReadAsNewer` where it does not; `RestartUnclaimed`
/// where the real claim relaunches.
#[test]
fn tier1_the_real_agy_quiet_upgrade_conforms_to_the_model() {
    let model = aterm_models_harness::harness_agy_quiet_upgrade_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    const MUTANTS: [&str; 9] = [
        "RestartWithOpenWork",
        "RestartWithDaemonTask",
        "RestartMidTurn",
        "RestartWithUnreadWake",
        "RestartWhileShared",
        "RestartUnclaimed",
        "RestartOnStaleLook",
        "BuildReadAsOlder",
        "BuildReadAsNewer",
    ];
    // The model's Buggy members are exactly these: the actions no state of
    // the faithful model fires.
    let fired = aterm_spec::interp::fired_actions(&model);
    let dead: BTreeSet<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    assert_eq!(dead, BTreeSet::from(MUTANTS), "the model's whole Buggy set");
    let rigs = (
        LiveRig::new("tier1-alone", Knobs::default()),
        LiveRig::new(
            "tier1-shared",
            Knobs {
                shared: Some(Shared::Lockless),
                ..Knobs::default()
            },
        ),
    );
    let claim_relaunches = real_claim_relaunches();
    assert!(
        claim_relaunches,
        "the real claim makes the exit the restart's"
    );
    assert_eq!(
        ag::exit_kind(0, false),
        ag::ExitKind::Clean,
        "an unclaimed SIGTERM is read as someone's end"
    );
    let mut seen: BTreeSet<State> = BTreeSet::new();
    let mut queue = VecDeque::from([model.init_state()]);
    let mut caught: BTreeSet<&str> = BTreeSet::new();
    let mut real_cache: BTreeMap<Vec<i64>, bool> = BTreeMap::new();
    let (mut restarts, mut waits, mut exits, mut lasts) = (0, 0, 0, 0);
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
            "landed", "turn", "task", "daemon", "subagent", "wake", "shown", "panel", "draft",
            "held", "unread", "shared",
        ]
        .iter()
        .map(|k| s[k])
        .collect();
        let real = *real_cache
            .entry(key)
            .or_insert_with(|| real_restarts(&s, &rigs));
        match (s["looked"], s["claimed"]) {
            // The first look.
            (0, _) => {
                assert_eq!(model.action_enabled("Look", &s), real, "Look at {s:?}");
                assert!(
                    !model.action_enabled("Restart", &s),
                    "Restart unlooked {s:?}"
                );
            }
            // The last look, the claim on disk.
            (1, 1) => {
                assert_eq!(
                    model.action_enabled("Restart", &s),
                    real,
                    "Restart at {s:?}"
                );
                assert_eq!(
                    model.action_enabled("TakeBack", &s),
                    !real,
                    "TakeBack at {s:?}"
                );
                lasts += 1;
            }
            // Looked, the claim not yet on disk: nothing signals.
            _ => assert!(
                !model.action_enabled("Restart", &s),
                "Restart unclaimed {s:?}"
            ),
        }
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
                "BuildReadAsOlder" => real,
                "RestartUnclaimed" => {
                    // The mutant's exit is left as someone's; the real
                    // restart claims it first and it is relaunched.
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
        restarts > 0 && waits > 0 && exits > 0 && lasts > 0,
        "{restarts} {waits} {exits} {lasts}"
    );
    assert_eq!(caught, BTreeSet::from(MUTANTS), "every mutant caught alone");
}
