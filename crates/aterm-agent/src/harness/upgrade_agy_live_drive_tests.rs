// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Driven tests for `agy` live upgrade quiet flow.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::super::live::{self, Move, claim_record, upgrade_visit};
use super::*;
use crate::harness::relaunch::ended_by_restart;
use crate::harness::upgrade::Request;
use crate::harness::upgrade_agy_live::{self as pure, Look, Message, STATUS_IDLE};
use crate::harness::upgrade_drive::build_start::{NamedProbe, PROBE, Probe};
use crate::harness::upgrade_drive::{Ask, ask_for};
use crate::harness::upgrade_drive::{Due, LiveTab, descendants_in};

const NEW_BUILD: &str = "1000001000004000001";
const NEW_VERSION: &str = "1.4.1";
const OLD_BUILD: &str = "1000001000003000009";
const OLD_VERSION: &str = "1.3.9";
const START_MS: u64 = 1_792_349_000_000;
const AT: &str = "2026-10-18T18:43:30Z";
const BEFORE: &str = "2026-10-18T18:40:00Z";
const CHILD: u32 = 4_200_009;
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
    /// agy's quota rotates the launch onto this model (default: it keeps its own).
    quota_to: Option<&'static str>,
    /// Actual vendor-record versions, with build ids derived by the same typed parser.
    versions: Option<(&'static str, &'static str)>,
}

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

    fn quota_pick(
        &self,
        model: Option<&str>,
        _exe: &Path,
        _env: &[(String, String)],
        _flags: &[String],
    ) -> Option<crate::harness::agy_quota::Pick> {
        use crate::harness::agy_quota::{Pick, Standing};
        let from = model.unwrap_or("gemini-4-argon-high").to_owned();
        Some(match self.knobs.quota_to {
            Some(to) if to != from => Pick::Rotate {
                from,
                from_standing: Standing::Spent,
                to: to.to_owned(),
                to_standing: Standing::Open,
            },
            _ => Pick::Keep {
                model: from,
                standing: Standing::Open,
            },
        })
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
    fn between(&self, what: Between) -> bool {
        self.knobs.between == Some(what) && self.changed.load(Ordering::SeqCst)
    }
}

fn idle_summary() -> String {
    format!("{C1}|{STATUS_IDLE}|0|\n")
}

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

fn message_body(id: &str, priority: &str) -> String {
    format!(
        r#"{{"id":"{id}", "recipient":"{C1}", "sender":"{C1}/task-4", "priority":"{priority}", "timestamp":"2026-10-18T18:53:20.270650Z", "content":"Task id finished"}}"#
    )
}

struct LiveRig {
    dir: aterm_tempfile::TempDir,
    opts: Opts,
    kernel: LiveKernel,
    asked: crate::harness::FakeInstanceLog,
    config: PathBuf,
    twin: PathBuf,
    /// The tab's record as it stood on disk when the `signal` came.
    at_signal: Arc<Mutex<Option<String>>>,
}

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

fn served(fixture: &str) -> String {
    at_caret(fixture)
}

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
    fn new(knobs: Knobs) -> LiveRig {
        let dir = aterm_tempfile::Builder::new()
            .prefix("agyl-")
            .tempdir()
            .expect("private live fixture");
        let base = std::fs::canonicalize(dir.path()).expect("canonical scratch");
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
        let (build, version, new_build, new_version) = match knobs.versions {
            Some((from, to)) => (
                pure::build_version(from, None)
                    .expect("running version")
                    .build_id()
                    .to_string(),
                from,
                pure::build_version(to, None)
                    .expect("target version")
                    .build_id()
                    .to_string(),
                to,
            ),
            None => (BUILD.to_owned(), VERSION, NEW_BUILD.to_owned(), NEW_VERSION),
        };
        let exe = lay(&store, &build, version);
        let new_exe = lay(&store, &new_build, new_version);
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
            crate::harness::fake_instance(&dir.path().join("t.sock"), TOKEN, move |line| {
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
            press: None,
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

    fn asked(&self, verb: &str) -> Vec<String> {
        let head = format!("@{TAB} {verb} ");
        self.asked
            .lock()
            .map(|a| a.iter().filter(|l| l.starts_with(&head)).cloned().collect())
            .unwrap_or_default()
    }

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

    fn snapshot(&self) -> crate::harness::relaunch::Snapshot {
        let args = self.kernel.args(TUI).expect("argv");
        let snap = snapshot(
            &self.opts,
            TAB,
            TUI,
            "tui-birth".into(),
            SHELL,
            &args,
            &self.kernel,
        )
        .expect("actual snapshot path");
        assert_eq!(snap.session.as_deref(), Some(C1));
        snap
    }

    /// The production claim/failure record of an earlier refused signal, built from actual
    /// validated fixture store records. Preparing it signals no real process.
    fn failed_claim(&self) -> St {
        let mv = live::behind(&self.kernel.exe, Some(self.twin.clone()))
            .expect("validated records")
            .expect("newer build");
        let mut st =
            claim_record(&self.snapshot(), &mv, self.opts.human_grace_s, now_s()).expect("claim");
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
        let _ = std::fs::remove_dir_all(self.dir.path());
    }
}

#[test]
fn a_persisted_agy_numeric_or_revision_word_holds_the_real_worker() {
    for versions in [None, Some(("1.4.0-R6427.1", "1.4.0-R6500.1"))] {
        for (ask, step) in [
            (Ask::Skip, "wait:skipped"),
            (Ask::Defer(3600), "wait:deferred"),
        ] {
            let rig = LiveRig::new(Knobs {
                versions,
                ..Knobs::default()
            });
            let claim = rig.failed_claim();
            let target = claim.to.clone();
            rig.store(&claim);
            let row = ask_for(&rig.opts, TAB, ask, &target, &key(TAB)).expect("public request");
            assert!(row.word_in_force(now_s()));
            let stored = rig.record().expect("request reloaded");
            assert_eq!(stored.request, row.request);
            let report = rig.visit();
            assert_eq!(report.step, step, "{versions:?}: {report:?}");
            assert!(rig.asked("signal").is_empty() && rig.asked("turn").is_empty());
            assert_eq!(rig.record(), Some(stored), "the request is not overwritten");
        }
    }
    let rig = LiveRig::new(Knobs::default());
    let mut st = rig.failed_claim();
    st.request = Request::Skip(format!("v{NEW_VERSION}.0"));
    assert!(st.request_tab.is_empty(), "legacy tab scope");
    rig.store(&st);
    assert_eq!(
        rig.visit().step,
        "wait:skipped",
        "numeric aliases still hold"
    );
    assert!(rig.asked("signal").is_empty());
}

#[test]
fn stale_foreign_or_invalid_agy_words_cannot_hold_another_move() {
    type Change = fn(&mut St);
    let cases: [(&str, Change); 11] = [
        ("other-agent", |s| s.agent = Agent::Muse),
        ("other-tab", |s| s.tab = "s-foreign".into()),
        ("other-request-tab", |s| s.request_tab = "s-foreign".into()),
        ("other-thread", |s| s.thread = SUB.into()),
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
        let rig = LiveRig::new(Knobs::default());
        rig.store(&rig.failed_claim());
        ask_for(&rig.opts, TAB, Ask::Skip, NEW_VERSION, &key(TAB)).expect("request producer");
        let mut st = rig.record().expect("request");
        change(&mut st);
        rig.store(&st);
        let report = rig.visit();
        assert_eq!(report.step, "done:resumed", "{name}: {report:?}");
        assert_eq!(rig.asked("signal").len(), 1, "{name}");
        assert_eq!(rig.asked("turn").len(), 1, "{name}");
    }
}

#[test]
fn a_build_word_does_not_block_an_agy_quota_model_rotation() {
    for request in [Request::Skip(VERSION.into()), Request::DeferUntil(u64::MAX)] {
        let rig = LiveRig::new(Knobs {
            twin: Some(Twin::Same),
            quota_to: Some("claude-opus-5-5-medium"),
            ..Knobs::default()
        });
        let mv = Move {
            running: VERSION.into(),
            target: VERSION.into(),
            twin: rig.twin.clone(),
        };
        let mut st = claim_record(&rig.snapshot(), &mv, rig.opts.human_grace_s, now_s())
            .expect("claim shape");
        st.fail("signal-refused", now_s());
        st.request = request;
        st.request_tab = TAB.into();
        rig.store(&st);
        let report = rig.visit();
        assert_eq!(report.step, "done:resumed", "{report:?}");
        assert_eq!(rig.asked("signal").len(), 1);
        assert_eq!(
            rig.record().expect("rotation").cause,
            "model:claude-opus-5-5-medium"
        );
    }
}

#[test]
fn a_saved_word_does_not_strand_an_agy_restart_already_in_flight() {
    let ready = Arc::new(AtomicBool::new(false));
    let rig = LiveRig::new(Knobs {
        shell_slow: Some(Arc::clone(&ready)),
        ..Knobs::default()
    });
    assert_eq!(rig.visit().step, "wait:shell-prompt");
    let mut st = rig.record().expect("actual in-flight claim");
    st.request = Request::Skip(NEW_VERSION.into());
    st.request_tab = TAB.into();
    rig.store(&st);
    ready.store(true, Ordering::SeqCst);
    assert_eq!(rig.visit().step, "done:resumed");
    assert_eq!(rig.asked("signal").len(), 1);
    assert_eq!(rig.asked("turn").len(), 1);
}

#[test]
fn an_idle_agy_behind_is_restarted_onto_the_new_build_and_nothing_is_typed() {
    let rig = LiveRig::new(Knobs::default());
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

#[test]
fn the_hosts_step_adopts_and_its_carry_types_nothing() {
    let rig = LiveRig::new(Knobs {
        hand_back: true,
        ..Knobs::default()
    });
    let r = rig.visit();
    assert_eq!(r.step, "adopted", "{r:?}");
    assert_eq!(r.pid, NEW);
    let st = rig.record().expect("in flight");
    assert!(st.in_flight() && st.quiet, "{st:?}");
    let r = rig.carry();
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert_eq!(rig.asked("turn").len(), 1, "the line alone");
}

#[test]
fn a_restart_left_in_flight_is_carried_as_the_harnesss_own() {
    let ready = Arc::new(AtomicBool::new(false));
    let rig = LiveRig::new(Knobs {
        shell_slow: Some(Arc::clone(&ready)),
        ..Knobs::default()
    });
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

#[test]
fn a_relaunch_that_never_holds_the_lock_started_fresh() {
    let mut rig = LiveRig::new(Knobs {
        lockless: true,
        ..Knobs::default()
    });
    rig.opts.timings.agy_lock = Duration::from_millis(50);
    let r = rig.visit();
    assert_eq!(r.step, "refused:started-fresh", "{r:?}");
    assert_eq!(rig.asked("turn").len(), 1);
}

fn holds(name: &str, knobs: Knobs, want: &str) {
    let rig = LiveRig::new(knobs);
    let r = rig.visit();
    assert_eq!(r.step, want, "{name}: {r:?}");
    assert!(rig.asked("signal").is_empty(), "{name}: signalled");
    assert!(rig.asked("turn").is_empty(), "{name}: typed");
    assert!(rig.record().is_none(), "{name}: a record was left");
}

fn restarts(name: &str, knobs: Knobs) {
    let rig = LiveRig::new(knobs);
    let r = rig.visit();
    assert_eq!(r.step, "done:resumed", "{name}: {r:?}");
}

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
    let rig = LiveRig::new(Knobs {
        argv: Some(vec!["--remote-control".to_string()]),
        ..Knobs::default()
    });
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
    let rig = LiveRig::new(Knobs {
        signal_refused: true,
        ..Knobs::default()
    });
    let r = rig.visit();
    assert_eq!(r.step, "wait:signal-held", "{r:?}");
    assert!(rig.asked("turn").is_empty() && rig.record().is_none());
}

#[test]
fn a_claim_that_cannot_be_written_signals_nothing() {
    let blocked = LiveRig::new(Knobs::default());
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

#[test]
fn the_tab_is_due_while_its_build_is_behind() {
    let behind = LiveRig::new(Knobs::default());
    let tabs = LiveRig::tabs();
    let due = |rig: &LiveRig, tabs: &[LiveTab]| {
        live::upgrade_due_with(&rig.opts, tabs, Some(rig.twin.clone()), &rig.kernel)
    };
    assert_eq!(due(&behind, &tabs), Some(Due::Yes));
    let current = LiveRig::new(Knobs {
        twin: Some(Twin::Same),
        ..Knobs::default()
    });
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

/// THE SUMMARY DATABASE IS READ LIVE, WITH NO LOCK, IN BOTH OF ITS STATES
/// (macOS: Apple's `/usr/bin/sqlite3`), over a database laid as agy lays its
/// own (WAL mode, the table's columns): CLOSED — no `-wal`, no `-shm`, as agy
/// leaves it some 30 s after its last write — it is read as an immutable
/// file, and the read leaves neither file behind; OPEN — another connection
/// holding it with rows still in its `-wal` — from a private copy of the file
/// and its `-wal`, which holds them once the holder has committed.
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
    let _ = std::fs::remove_file(side("wal"));
    let _ = std::fs::remove_file(side("shm"));
    let out = live::read_summaries(&config, C1).expect("read closed");
    assert_eq!(out.trim(), format!("{C1}|CASCADE_RUN_STATUS_RUNNING|1|"));
    assert!(
        !side("wal").exists() && !side("shm").exists(),
        "the read writes nothing"
    );
    let mut holder = aterm_uds::spawnfd::command("/usr/bin/sqlite3")
        .arg(&db)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("holder");
    let mut stdin = holder.stdin.take().expect("its stdin");
    let stdout = holder.stdout.take().expect("its stdout");
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
    std::fs::write(side("journal"), b"\xd9\xd5\x05\xf9").expect("journal");
    assert_eq!(live::read_summaries(&config, C1), Err(()));
    std::fs::remove_file(side("journal")).expect("no journal");
    assert!(
        live::read_summaries(&config, C1).is_ok(),
        "and with none, an answer"
    );
    assert_eq!(live::read_summaries(&config, "not-a-uuid"), Err(()));

    let bad_dir = scratch("summaries-empty-bad-schema");
    let bad_config = std::fs::canonicalize(&*bad_dir).expect("canonical");
    let bad_db = bad_config.join(live::SUMMARIES);
    let bad_made = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&bad_db)
            .arg(
                "PRAGMA journal_mode=WAL; \
                 CREATE TABLE conversation_summaries (conversation_id text PRIMARY KEY, status text); \
                 PRAGMA wal_checkpoint(TRUNCATE);",
            ),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("sqlite3 within the hang detector: {e}"));
    assert!(bad_made.status.success(), "{bad_made:?}");
    assert_eq!(
        live::read_summaries(&bad_config, C1),
        Err(()),
        "empty table missing required summary columns must fail"
    );

    let injected = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .arg(format!(
                "INSERT INTO conversation_summaries (conversation_id,status,not_fully_idle,parent_conversation_id) \
                 VALUES ('child-1', 'CASCADE_RUN_STATUS_IDLE|0|' || char(10) || '{C1}|{STATUS_IDLE}|0|', 0, '{C1}'); \
                 PRAGMA wal_checkpoint(TRUNCATE);"
            )),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("sqlite3 within the hang detector: {e}"));
    assert!(injected.status.success(), "{injected:?}");
    assert_eq!(
        live::read_summaries(&config, C1),
        Err(()),
        "embedded pipe/newline delimiters in summary cells must fail closed"
    );
    let _ = std::fs::remove_dir_all(&*bad_dir);
    let _ = std::fs::remove_dir_all(&*dir);
}

/// THE SUMMARY READ NEVER LOCKS OUT A WRITER (macOS: Apple's `/usr/bin/sqlite3`),
/// over the database as a holder that died leaves it — rows still in a large
/// `-wal`, the `-shm` beside it, no connection: the state in which a reader
/// through SQLite, the first to open it, empties the WAL index and rebuilds it
/// holding the index's write and recover locks (agy's own open makes the same
/// unbuilt index for a moment).
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

type State = BTreeMap<&'static str, i64>;

fn versions(landed: i64) -> (&'static str, &'static str) {
    match landed {
        1 => (VERSION, NEW_VERSION),
        2 => (VERSION, OLD_VERSION),
        _ => (VERSION, VERSION),
    }
}

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

fn real_claim_relaunches() -> bool {
    let rig = LiveRig::new(Knobs::default());
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
    let st = claim_record(&snap, &mv, rig.opts.human_grace_s, now).expect("a claim");
    assert_eq!(
        st.proc_start, snap.start,
        "the claim preserves its snapshot's run identity"
    );
    ended_by_restart(&st, Agent::Agy, TAB, TUI, now)
}

/// 2026-10-26 review: the agy quiet upgrade's SIGTERM waits out the whole grace, so its claim names
/// that grace, and a key typed at the prompt after it holds the relaunch line it owes exactly as
/// long as it would have held the SIGTERM, for the upgrade's claim and a quota rotation's (`model:`).
/// The control: a claim naming the land rung's keys gap (what the line read an agy claim that named
/// none as) lets go a key the SIGTERM waited on.
#[test]
fn an_agy_claim_holds_its_line_on_the_grace_its_sigterm_waited_out() {
    let rig = LiveRig::new(Knobs::default());
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
    let mv = Move {
        running: VERSION.to_string(),
        target: NEW_VERSION.to_string(),
        twin: rig.twin.clone(),
    };
    use crate::harness::relaunch::{PromptLook, line_grace, line_held_by_status};
    let grace_s = rig.opts.human_grace_s;
    let key_status =
        |age_s: u64| format!("OK schema=1 hold=0 hand=- input=clear human_ms={age_s}000");
    let owed_line_held = |claim: &St, grace_s: u32, age_s: u64| {
        let mut claim = claim.clone();
        let look = PromptLook::of(&mut claim);
        let grace = line_grace(grace_s, look.ended_at_s, look.line_grace_s);
        line_held_by_status(Some(&key_status(age_s)), grace, "aterm-harness@1").is_some()
    };
    let upgrade = claim_record(&snap, &mv, grace_s, now_s()).expect("a claim");
    assert_eq!(
        upgrade.line_grace_s,
        Some(grace_s),
        "the claim names the grace its SIGTERM waited out"
    );
    let rotation = St {
        cause: format!("{}gemini-3-flash", crate::harness::relaunch::CAUSE_MODEL),
        ..upgrade.clone()
    };
    let sigterm_held =
        |age_s: u64| crate::harness::upgrade_drive::held_by_status(&key_status(age_s), grace_s);
    for claim in [&upgrade, &rotation] {
        for age_s in [0, 1, 19, 20, 21, 30, 60, 119, 120, 121, 300] {
            assert_eq!(
                owed_line_held(claim, grace_s, age_s),
                sigterm_held(age_s),
                "{:?}: a key {age_s} s old",
                claim.cause
            );
        }
    }
    let gap = St {
        line_grace_s: Some(crate::harness::upgrade::KEYS_GAP_S),
        ..upgrade.clone()
    };
    assert!(
        (0..=300).any(|age_s| sigterm_held(age_s) && !owed_line_held(&gap, grace_s, age_s)),
        "control: the keys gap lets go a key the SIGTERM waited on"
    );
}

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
    let fired = aterm_spec::interp::fired_actions(&model);
    let dead: BTreeSet<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !fired.contains(a))
        .collect();
    assert_eq!(dead, BTreeSet::from(MUTANTS), "the model's whole Buggy set");
    let rigs = (
        LiveRig::new(Knobs::default()),
        LiveRig::new(Knobs {
            shared: Some(Shared::Lockless),
            ..Knobs::default()
        }),
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
            (0, _) => {
                assert_eq!(model.action_enabled("Look", &s), real, "Look at {s:?}");
                assert!(
                    !model.action_enabled("Restart", &s),
                    "Restart unlooked {s:?}"
                );
            }
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

#[test]
fn an_idle_agy_on_a_spent_pool_is_relaunched_on_the_quota_pick() {
    let knobs = Knobs {
        twin: Some(Twin::Same),
        quota_to: Some("claude-opus-5-5-medium"),
        ..Knobs::default()
    };
    let rig = LiveRig::new(knobs.clone());
    let tabs = LiveRig::tabs();
    assert_eq!(
        live::upgrade_due_with(&rig.opts, &tabs, Some(rig.twin.clone()), &rig.kernel),
        Some(Due::Yes),
        "a rotation owed is due though the build is current"
    );
    let rotation = live::rotation_due(
        &rig.kernel.exe,
        &rig.kernel.args(TUI).expect("running launch"),
        Some(rig.twin.clone()),
        &rig.opts.home,
        &rig.kernel,
    )
    .expect("the exact current twin can rotate");
    assert_eq!(rotation.mv.target, VERSION);
    assert_eq!(rotation.mv.twin, rig.twin);
    let r = rig.visit();
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert_eq!(
        (r.from.as_str(), r.to.as_str()),
        ("gemini-4-argon-high", "claude-opus-5-5-medium(quota)")
    );
    let typed = rig.asked("turn");
    assert_eq!(typed.len(), 1, "the relaunch line alone: {typed:#?}");
    assert!(
        typed[0].contains(&format!(
            "'--model=claude-opus-5-5-medium' '--effort' 'high' '--conversation={C1}'"
        )),
        "{}",
        typed[0]
    );
    let st = rig.record().expect("the record");
    assert_eq!(st.cause, "model:claude-opus-5-5-medium");
    assert_eq!(
        ag::argv_model(&st.argv).as_deref(),
        Some("claude-opus-5-5-medium")
    );
    assert!(
        st.outcome
            .starts_with("agy moved onto claude-opus-5-5-medium by its quota"),
        "{}",
        st.outcome
    );
    let kept = LiveRig::new(Knobs {
        twin: Some(Twin::Same),
        ..Knobs::default()
    });
    assert_eq!(
        kept.visit().step,
        "current",
        "a pool that keeps its model moves nothing"
    );
    assert_eq!(
        live::upgrade_due_with(&kept.opts, &tabs, Some(kept.twin.clone()), &kept.kernel),
        Some(Due::No)
    );
    let older = LiveRig::new(Knobs {
        twin: Some(Twin::Older),
        quota_to: Some("claude-opus-5-5-medium"),
        ..Knobs::default()
    });
    assert_eq!(
        live::rotation_due(
            &older.kernel.exe,
            &older.kernel.args(TUI).expect("running launch"),
            Some(older.twin.clone()),
            &older.opts.home,
            &older.kernel,
        ),
        None,
        "a quota move must not disguise an older executable as the current build"
    );
    assert_eq!(
        live::upgrade_due_with(&older.opts, &tabs, Some(older.twin.clone()), &older.kernel),
        Some(Due::No)
    );
    assert_eq!(older.visit().step, "current");
    assert!(older.asked("signal").is_empty());
    assert!(older.asked("turn").is_empty());
    assert!(older.record().is_none());
}
