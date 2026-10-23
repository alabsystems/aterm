// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The codex-alab path over a stand-in aterm instance on a real socket (every request recorded, nothing typed anywhere) and a scripted kernel.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::harness::self_claimed::{
    FirstFrame, Live as ClaimedLive, Record, Resume, Witnesses, validate,
};
use crate::harness::upgrade::Request;
use crate::supervise::ScratchDir;

const TAB: &str = "s-c0dec0dec0dec0dec0de";
const TOKEN: &str = "0badf00d0badf00d";
/// The conversation the record names; another one an older exit's hint names.
const T1: &str = "01a0dc36-1dc1-7ee2-be91-11bc323e377c";
const T2: &str = "01a0dc3a-5c71-7143-bcee-58def0a15dfd";
/// Pids no real process of this test has: the kernel is scripted.
const TUI: u32 = 4_000_001;
const SHELL: u32 = 4_000_002;
const NEW: u32 = 4_000_003;

/// A fresh directory per call, removed when it drops.
fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("cxa", name)
}

/// One `text --json` body: `rows`, the cursor at `(row, col)`, `seq` and a generation, and no
/// person's input to the tab.
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

/// An idle Codex with an empty composer, its dim placeholder under the cursor: the screen a
/// continuation would be typed at, which this path never types at.
fn codex_idle(seq: u64) -> String {
    screen_json(
        &[
            "╭─────────────────────────────────────────────╮",
            "│ >_ OpenAI Codex (v0.164.0-dev)              │",
            "╰─────────────────────────────────────────────╯",
            "› say hello",
            "",
            "› Ask Codex to do anything",
            "",
            "  GPT-6-Astra default · /w",
        ],
        (5, 2),
        seq,
    )
}

/// The shell after the crash: `hint` under the command that ran the TUI, then the prompt with
/// the cursor after it.
fn shell_after(hint: &[&str]) -> String {
    let mut rows = vec!["% codex".to_string()];
    rows.extend(hint.iter().map(|r| (*r).to_string()));
    rows.push("% ".to_string());
    let at = rows.len() - 1;
    let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
    screen_json(&refs, (at, 2), 900)
}

/// The shell integration's last two blocks for [`shell_after`] with `out_rows` output rows:
/// the command that ran the TUI, killed (`exit 137`), then the prompt entering.
fn blocks_after(out_rows: usize) -> (String, String) {
    let (cmd, out) = (0, 1);
    let end = out + out_rows;
    let done = format!(
        r#"{{"id":7,"state":"complete","exit":137,"prompt":{cmd},"cmd":{cmd},"cmdcol":2,"out":{out},"end":{end},"cmdline":"codex"}}"#
    );
    let entering = format!(
        r#"{{"id":8,"state":"entering","exit":null,"prompt":{end},"cmd":{end},"cmdcol":2,"out":null,"end":null,"cmdline":""}}"#
    );
    (
        format!(r#"{{"blocks":[{done},{entering}]}}"#),
        format!(r#"{{"blocks":[{entering}]}}"#),
    )
}

/// What the stand-in serves: the shell after the crash, codex-alab once its line was typed.
struct World {
    after_exit: String,
    after_relaunch: String,
    blocks2: String,
    blocks1: String,
    out: Vec<String>,
    status_extra: Arc<Mutex<String>>,
    relaunched: Arc<AtomicBool>,
}

/// A control socket at `<dir>/t.sock` standing in for the aterm instance of [`TAB`], and the
/// requests it was asked, in order.
fn instance(dir: &Path, w: World) -> (String, crate::harness::FakeInstanceLog) {
    crate::harness::fake_instance(&dir.join("t.sock"), TOKEN, move |line| {
        let verb = line
            .split_whitespace()
            .find(|w| !w.starts_with('@'))
            .unwrap_or("");
        let relaunched = w.relaunched.load(Ordering::SeqCst);
        match verb {
            "sessions" => format!("OK 1\nlocal {TAB} 0 idle codex"),
            "text" => format!(
                "OK {}",
                if relaunched {
                    w.after_relaunch.clone()
                } else {
                    w.after_exit.clone()
                }
            ),
            "cell" => "OK A 9b9b9c 2d2f33 dim".to_string(),
            "status" => format!(
                "OK schema=1 hold=0 hand=- integration=on{}",
                w.status_extra.lock().map(|e| e.clone()).unwrap_or_default()
            ),
            "help" if line.contains("help key") => {
                "OK 2\nkey  key [if-gen=<epoch>.<seq>] [if=<re>] <name>\n     FENCES".to_string()
            }
            "help" => "OK 1\nturn [if-gen=<epoch>.<seq>] <text>".to_string(),
            "blocks" if line.contains("blocks 2") => format!("OK {}", w.blocks2),
            "blocks" => format!("OK {}", w.blocks1),
            "blocktext" => std::iter::once(format!("OK {}", w.out.len()))
                .chain(w.out.iter().cloned())
                .collect::<Vec<_>>()
                .join("\n"),
            "line" => "OK % ".to_string(),
            "lease" if line.contains(" acquire ") => "OK lease acquired".to_string(),
            "lease" => "OK lease released".to_string(),
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

/// The scripted kernel: [`TUI`] crashed out of [`SHELL`], whose group has the terminal back;
/// the relaunch [`NEW`] appears under the shell, holding the thread's lock, once its line is
/// typed, and is up at the `up_at`th look with the frame it names (never: `None`), or ends after
/// `ends_at` looks; its kernel start reads `births` read by read, then [`NEW_BIRTH`].
struct Crashed {
    relaunched: Arc<AtomicBool>,
    lock: PathBuf,
    new_argv: Vec<String>,
    tui_alive: bool,
    exe: Option<PathBuf>,
    files: Vec<PathBuf>,
    up_at: Option<(usize, FirstFrame)>,
    ends_at: Option<usize>,
    births: Arc<Mutex<VecDeque<Option<String>>>>,
    looks: Arc<AtomicUsize>,
    marks: crate::harness::FakeInstanceLog,
    owners: Mutex<VecDeque<aterm_uds::ownerlock::Owner>>,
}

/// [`NEW`]'s kernel start, as the record reader spells one.
const NEW_BIRTH: &str = "Thu Oct 16 08:00:09 2026";

impl Kernel for Crashed {
    fn job(&self, _: u32) -> Option<(super::super::Job, u32)> {
        Some((super::super::Job::Foreground, SHELL))
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        (pid == NEW || pid == TUI).then_some(SHELL)
    }

    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        Some((1, "launchd".to_string()))
    }
}

impl AlabKernel for Crashed {
    fn alive(&self, pid: u32) -> bool {
        match pid {
            TUI => self.tui_alive,
            NEW => {
                self.relaunched.load(Ordering::SeqCst)
                    && self
                        .ends_at
                        .is_none_or(|n| self.looks.load(Ordering::SeqCst) < n)
            }
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
        self.exe.clone().filter(|_| pid == TUI)
    }

    fn open_files(&self, pid: u32) -> Option<Vec<PathBuf>> {
        Some(if pid == TUI {
            self.files.clone()
        } else {
            Vec::new()
        })
    }

    fn writer_owner(&self, file: &Path) -> aterm_uds::ownerlock::Owner {
        use aterm_uds::ownerlock::Owner;
        self.owners
            .lock()
            .expect("writer owners")
            .pop_front()
            .unwrap_or_else(|| {
                if file == self.lock && self.relaunched.load(Ordering::SeqCst) {
                    Owner::Live
                } else {
                    Owner::Dead
                }
            })
    }

    fn shell_has_terminal(&self, _: u32) -> bool {
        true
    }

    fn codex_children(&self, _: u32) -> Vec<(u32, Vec<String>)> {
        if self.relaunched.load(Ordering::SeqCst) {
            vec![(NEW, self.new_argv.clone())]
        } else {
            Vec::new()
        }
    }

    fn cwd(&self, _: u32) -> Option<String> {
        Some("/w".to_string())
    }

    fn in_tab(&self, _: &mut Client, _: u32, _: &str) -> bool {
        true
    }

    fn birth(&self, pid: u32) -> Option<String> {
        if pid != NEW || !self.alive(NEW) {
            return None;
        }
        let scripted = self.births.lock().ok().and_then(|mut b| b.pop_front());
        scripted.unwrap_or_else(|| Some(NEW_BIRTH.to_string()))
    }

    fn first_frame(&self, _: &mut Client, pid: u32, birth: &str, _: &str) -> Option<FirstFrame> {
        let n = self.looks.fetch_add(1, Ordering::SeqCst) + 1;
        let up = self
            .up_at
            .filter(|(at, _)| pid == NEW && birth == NEW_BIRTH && n >= *at)
            .map(|(_, frame)| frame);
        let word = match up {
            Some(FirstFrame::Idle) => "idle",
            Some(FirstFrame::Turn) => "turn",
            None => "-",
        };
        if let Ok(mut log) = self.marks.lock() {
            log.push(format!("kernel first-frame {n} {word}"));
        }
        up
    }
}

/// One rig: a Codex home with the thread's rollout, a state directory, the stand-in instance
/// showing `hint` under the crashed command, and the kernel; the snapshot the host read while
/// codex-alab ran, and the line its record validated.
struct Rig {
    dir: ScratchDir,
    opts: Opts,
    home: PathBuf,
    kernel: Crashed,
    asked: crate::harness::FakeInstanceLog,
    status_extra: Arc<Mutex<String>>,
    twin: PathBuf,
    /// The harness's waits on the test's thread run on their own steps
    /// ([`crate::harness::relaunch::virtual_waits`]): every stand-in here answers at once.
    _waits: crate::harness::relaunch::VirtualWaits,
}

impl Rig {
    fn new(name: &str, hint: &[&str]) -> Rig {
        let dir = scratch(name);
        let user = dir.join("home");
        let codex = user.join(".codex");
        let day = codex.join("sessions/2026/10/16");
        std::fs::create_dir_all(&day).expect("sessions");
        std::fs::create_dir_all(codex.join("thread-writer-locks")).expect("locks");
        let home = std::fs::canonicalize(&codex).expect("home");
        std::fs::write(
            day.join(format!("rollout-2026-10-16T08-00-00-{T2}.jsonl")),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\",\"cwd\":\"/w\"}}\n",
        )
        .expect("rollout");
        let relaunched = Arc::new(AtomicBool::new(false));
        let status_extra = Arc::new(Mutex::new(String::new()));
        let (blocks2, blocks1) = blocks_after(hint.len());
        let (sock, asked) = instance(
            &dir,
            World {
                after_exit: shell_after(hint),
                after_relaunch: codex_idle(20),
                blocks2,
                blocks1,
                out: hint.iter().map(|r| (*r).to_string()).collect(),
                status_extra: Arc::clone(&status_extra),
                relaunched: Arc::clone(&relaunched),
            },
        );
        let twin = dir.join("prefix/agents/codex");
        let opts = Opts {
            home: user,
            state: dir.join("state"),
            sock: Some(sock),
            only_sid: None,
            dry_run: false,
            human_grace_s: 120,
            hand_back: false,
            background: false,
            aterm_state: None,
            ask: false,
            several_logins: false,
            usage_credits: false,
            usage_tap: None,
            instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
            timings: crate::harness::Timings::FAST,
            agents_off: Vec::new(),
            land: false,
        };
        let kernel = Crashed {
            relaunched,
            lock: home.join("thread-writer-locks").join(format!("{T2}.lock")),
            new_argv: vec![
                twin.to_string_lossy().into_owned(),
                "resume".to_string(),
                T2.to_string(),
            ],
            tui_alive: false,
            exe: None,
            files: Vec::new(),
            up_at: Some((1, FirstFrame::Idle)),
            ends_at: None,
            births: Arc::new(Mutex::new(VecDeque::new())),
            looks: Arc::new(AtomicUsize::new(0)),
            marks: asked.clone(),
            owners: Mutex::new(VecDeque::new()),
        };
        Rig {
            _waits: crate::harness::relaunch::virtual_waits(),
            dir,
            opts,
            home,
            kernel,
            asked,
            status_extra,
            twin,
        }
    }

    /// The crashed codex-alab as the host's snapshot read it while it ran (embedded, its
    /// thread [`T2`], its `CODEX_HOME` as `home_env`).
    fn snap(&self, home_env: Option<String>) -> crate::harness::relaunch::Snapshot {
        crate::harness::relaunch::Snapshot {
            stored_login: false,
            profile: None,
            tab: TAB.to_string(),
            pid: TUI,
            start: "Thu Oct 16 08:00:00 2026".to_string(),
            shell: SHELL,
            program: self.twin.clone(),
            argv: vec![self.twin.to_string_lossy().into_owned()],
            session: Some(T2.to_string()),
            cwd: "/w".to_string(),
            version: None,
            run: crate::harness::relaunch::AgentRun::Codex(crate::harness::relaunch::CodexRun {
                home: self.home.clone(),
                home_env,
                self_claimed: None,
            }),
            dialect: shell_dialect(SHELL, &self.kernel, Vec::new),
            login: None,
            home: None,
            claude_dir: None,
        }
    }

    /// The line aterm's copy of the record validated: `<twin> resume <T2>`, under `codex_home`.
    fn resume(&self, codex_home: Option<String>) -> Resume {
        Resume {
            argv: vec![
                self.twin.to_string_lossy().into_owned(),
                "resume".to_string(),
                T2.to_string(),
            ],
            session: T2.to_string(),
            codex_home,
            cwd: Some("/w".to_string()),
        }
    }

    fn state(&self) -> St {
        load(&self.opts, &key(TAB)).expect("a state")
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().map(|a| a.clone()).unwrap_or_default()
    }

    fn typed(&self) -> Vec<String> {
        self.asked()
            .into_iter()
            .filter(|l| l.contains(" turn "))
            .collect()
    }

    /// What was asked of the tab after the relaunch line went in: the verbs only.
    fn after_the_line(&self) -> Vec<String> {
        let asked = self.asked();
        let at = asked
            .iter()
            .position(|l| l.contains(" turn ") && l.contains("'resume'"))
            .expect("the line was typed");
        asked[at + 1..]
            .iter()
            .filter_map(|l| {
                l.split_whitespace()
                    .find(|w| !w.starts_with('@'))
                    .map(str::to_owned)
            })
            .collect()
    }
}

/// The verbs that type into a tab or read its screen.
const TYPES_OR_READS: [&str; 7] = ["turn", "key", "send", "paste", "feed", "text", "cell"];

/// A jail that refuses other processes' descriptor tables makes the all-process census
/// unknown.
#[cfg(target_vendor = "apple")]
#[test]
fn a_free_writer_lock_needs_no_read_of_unrelated_process_descriptors() {
    const CHILD: &str = "ATERM_CODEX_WRITER_PROBE_CHILD";
    const NAME: &str = "harness::upgrade_drive::codex_alab_drive::tests::a_free_writer_lock_needs_no_read_of_unrelated_process_descriptors";
    if let Some(path) = std::env::var_os(CHILD) {
        let path = PathBuf::from(path);
        assert_eq!(
            super::super::file_holders(&path),
            None,
            "the jail keeps the unrelated census unknown"
        );
        assert_eq!(Live.writer_owner(&path), aterm_uds::ownerlock::Owner::Dead);
        println!("PROBE: free conversation writer lock, unreadable unrelated census");
        return;
    }
    let dir = scratch("writer-probe-jail");
    let lock = dir.join("writer.lock");
    std::fs::write(&lock, b"").expect("writer lock");
    let profile = "(version 1)(allow default)(deny process-info-pidinfo)";
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/usr/bin/sandbox-exec")
            .args(["-p", profile])
            .arg(std::env::current_exe().expect("the test binary"))
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD, &lock),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("the sandboxed kernel probe, within its hang detector");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("PROBE: free conversation writer lock"),
        "{:?}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A free kernel lock admits exactly the resume line; a held or unknown answer does not.
#[test]
fn the_writer_lock_must_be_proven_free_at_both_relaunch_checks() {
    use aterm_uds::ownerlock::Owner::{Dead, Live, Unknown};
    for (name, owners, step, lines) in [
        ("free", vec![Dead, Dead], "done:resumed", 1),
        ("held", vec![Live], "wait:thread-locked", 0),
        ("unknown", vec![Unknown], "wait:thread-locked", 0),
        (
            "became-held",
            vec![Dead, Live],
            "wait:conversation-owner-ambiguous",
            0,
        ),
        (
            "became-unknown",
            vec![Dead, Unknown],
            "wait:conversation-owner-ambiguous",
            0,
        ),
    ] {
        let rig = Rig::new(&format!("writer-{name}"), &["zsh: killed     codex"]);
        *rig.kernel.owners.lock().expect("writer owners") = owners.into();
        let r = self_claimed_after_exit(
            &rig.opts,
            &rig.snap(None),
            &rig.resume(None),
            ExitCause::Exit,
            &rig.kernel,
        );
        assert_eq!(r.step, step, "{name}: {r:?}\n{:#?}", rig.asked());
        assert_eq!(rig.typed().len(), lines, "{name}: {:#?}", rig.asked());
    }
}

/// A crash relaunch held by a person at the prompt waits with nothing typed and no witness
/// recorded; carried at the next decision, its line goes in under the captured `CODEX_HOME`
/// alias, the witness is recorded once the line landed, and the relaunch is done once the new
/// TUI leads the tab.
#[test]
fn a_self_claimed_relaunch_records_its_witness_after_a_deferred_carry() {
    let rig = Rig::new("self-claim-carry", &["zsh: killed     codex"]);
    let home_alias = rig.dir.join("custom codex");
    std::os::unix::fs::symlink(&rig.home, &home_alias).expect("home alias");
    let alias = home_alias.to_string_lossy().into_owned();
    let snap = rig.snap(Some(alias.clone()));
    let resume = rig.resume(Some(alias.clone()));
    let witnesses = Witnesses::under(&rig.opts.state);
    *rig.status_extra.lock().expect("status") = " human_ms=500".to_string();
    let r = self_claimed_after_exit(&rig.opts, &snap, &resume, ExitCause::Exit, &rig.kernel);
    assert_eq!(r.step, "wait:held", "{r:?}");
    assert!(rig.typed().is_empty());
    assert!(!witnesses.handled(snap.pid, &snap.start));
    let waiting = rig.state();
    assert!(matches!(waiting.phase, Phase::Exiting { .. }));
    assert_eq!(waiting.resume_start, snap.start);

    rig.status_extra.lock().expect("status").clear();
    let r = self_claimed_after_exit(&rig.opts, &snap, &resume, ExitCause::Exit, &rig.kernel);
    assert_eq!(r.step, "done:resumed", "{r:?}\n{:#?}", rig.asked());
    assert!(witnesses.handled(snap.pid, &snap.start));
    let done = rig.state();
    assert_eq!(done.phase, Phase::Done);
    assert_eq!((done.resumed_pid, done.relaunched_pid), (NEW, NEW));
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "the resume line alone: {typed:#?}");
    assert!(
        typed[0].contains(&format!(
            "CODEX_HOME={} ",
            upgrade::quote(Dialect::Zsh, &alias)
        )),
        "{}",
        typed[0]
    );
    let r = self_claimed_after_exit(&rig.opts, &snap, &resume, ExitCause::Exit, &rig.kernel);
    assert_eq!(r.step, "ended:handled", "{r:?}");
    assert_eq!(rig.typed(), typed, "a second decision types nothing");
}

/// The line carries the `CODEX_HOME` the crashed process's own environment had, verbatim:
/// unset, a path, or set empty.
#[test]
fn a_self_claimed_crash_relaunch_keeps_the_captured_home_environment() {
    for case in ["unset", "path", "empty"] {
        let rig = Rig::new(
            &format!("self-claim-home-{case}"),
            &["zsh: killed     codex"],
        );
        let captured = match case {
            "unset" => None,
            "empty" => Some(String::new()),
            _ => Some(rig.home.to_string_lossy().into_owned()),
        };
        let snap = rig.snap(captured.clone());
        let launch = snap.program.to_string_lossy().into_owned();
        let record = Record {
            pid: TUI,
            session_id: Some(T2.to_string()),
            resume_argv: Some(vec![launch.clone(), "resume".to_string(), T2.to_string()]),
            env: captured
                .iter()
                .map(|s| ("CODEX_HOME".to_string(), s.clone()))
                .collect(),
            status: Some("busy".to_string()),
            exit: None,
            cwd: Some(snap.cwd.clone()),
            draft_empty: None,
            frame: None,
            initializing: false,
        };
        let live = ClaimedLive {
            exec_path: launch,
            argv: snap.argv.clone(),
            codex_home: captured.clone(),
            cwd: Some(PathBuf::from(&snap.cwd)),
            ..ClaimedLive::default()
        };
        let resume = validate(&record, TUI, &live).expect("the captured line validates");
        assert_eq!(resume.codex_home, captured);
        let result =
            self_claimed_after_exit(&rig.opts, &snap, &resume, ExitCause::Exit, &rig.kernel);
        assert_eq!(result.step, "done:resumed", "{case}: {result:?}");
        assert_eq!(
            rig.state().codex_home_env,
            captured,
            "{case}: persisted environment"
        );
        let typed = rig.typed();
        let line = typed
            .first()
            .expect("the fake instance received a relaunch");
        match captured {
            Some(value) => assert!(
                line.contains(&format!("CODEX_HOME={value} "))
                    || line.contains(&format!(
                        "CODEX_HOME={} ",
                        upgrade::quote(Dialect::Zsh, &value)
                    )),
                "{case}: {typed:#?}"
            ),
            None => assert!(!line.contains("CODEX_HOME"), "{typed:#?}"),
        }
    }
}

/// CODEX-ALAB CARRIES ON BY ITSELF (the contract §5): once its line is in and the new TUI leads
/// the tab, the relaunch is done, though the tab shows an idle Codex at an empty composer: no
/// continuation is typed, no key pressed, and the screen is not read again.
#[test]
fn the_crash_relaunch_types_nothing_after_the_new_tui_leads_the_tab() {
    for hand_back in [false, true] {
        let mut rig = Rig::new(
            &format!("self-claim-quiet-{hand_back}"),
            &["zsh: killed     codex"],
        );
        rig.opts.hand_back = hand_back;
        let snap = rig.snap(None);
        let resume = rig.resume(None);
        let r = self_claimed_after_exit(&rig.opts, &snap, &resume, ExitCause::Stall, &rig.kernel);
        assert_eq!(r.step, "done:resumed", "hand_back={hand_back}: {r:?}");
        let st = rig.state();
        assert_eq!(st.phase, Phase::Done, "nothing left in flight");
        assert!(!st.in_flight());
        let typed = rig.typed();
        assert_eq!(typed.len(), 1, "the line, and only it: {typed:#?}");
        assert!(typed[0].contains(&format!("'resume' '{T2}'")), "{typed:#?}");
        let after = rig.after_the_line();
        assert!(
            after
                .iter()
                .all(|verb| !TYPES_OR_READS.contains(&verb.as_str())),
            "hand_back={hand_back}: asked after the line: {after:?}"
        );
    }
}

/// Whether `asked` (the stand-in's log, the kernel's looks marked in it) shows the harness's
/// hand given back after the relaunch line and before the relaunched process was up: the
/// 0.106.0 give-back that codex-alab's launch probe read as `hand=-`.
fn released_before_up(asked: &[String]) -> bool {
    let line = asked
        .iter()
        .position(|l| l.contains(" turn ") && l.contains("'resume'"))
        .expect("the line was typed");
    let after = &asked[line..];
    let released = after.iter().position(|l| l.contains(" lease release "));
    let up = after
        .iter()
        .position(|l| l.starts_with("kernel first-frame ") && !l.ends_with(" -"));
    match (released, up) {
        (Some(released), Some(up)) => released < up,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

impl Rig {
    /// The kernel's looks at the relaunched process during the hold.
    fn looks(&self) -> usize {
        self.asked()
            .iter()
            .filter(|l| l.starts_with("kernel first-frame "))
            .count()
    }

    /// How the hold ended, as the ledger's `done:resumed` row says it after the outcome.
    fn held(&self) -> String {
        let ledger =
            std::fs::read_to_string(crate::harness::upgrade_drive::ledger_path(&self.opts))
                .unwrap_or_default();
        ledger
            .lines()
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .filter(|row| {
                row.get("step").and_then(aterm_json::Value::as_str) == Some("done:resumed")
            })
            .filter_map(|row| {
                let detail = row.get("detail")?.as_str()?;
                Some(
                    detail
                        .rsplit_once("; ")
                        .map_or(detail, |(_, held)| held)
                        .to_string(),
                )
            })
            .next_back()
            .unwrap_or_default()
    }

    /// The crash rule's relaunch, run to its end: `done:resumed`, the line alone typed, and
    /// nothing typed or read after it.
    fn relaunch_resumed(&self) {
        let snap = self.snap(None);
        let resume = self.resume(None);
        let r = self_claimed_after_exit(&self.opts, &snap, &resume, ExitCause::Exit, &self.kernel);
        let asked = self.asked();
        assert_eq!(r.step, "done:resumed", "{r:?} {asked:#?}");
        assert!(
            asked.iter().any(|l| l.contains(" lease release ")),
            "given back in the end: {asked:#?}"
        );
        assert_eq!(self.typed().len(), 1, "the line alone: {asked:#?}");
        assert!(
            self.after_the_line()
                .iter()
                .all(|verb| !TYPES_OR_READS.contains(&verb.as_str())),
            "{asked:#?}"
        );
    }
}

/// THE RELAUNCH HAND IS HELD TO THE RELAUNCHED PROCESS'S FIRST IDLE (the contract §5,
/// carry-on): the lease taken before the line goes back only once the relaunched codex-alab
/// reads at its first idle (here its third look), never as its TUI first leads the tab, so
/// its launch probe — its first `status`, read as it starts — sees
/// `hand=lease:aterm-harness@<aterm>`, and the ledger says so; still `done:resumed`, nothing
/// typed and no screen read meanwhile.
#[test]
fn the_relaunch_hand_is_held_to_the_relaunched_processs_first_idle() {
    for (bound, held) in [(None, true), (Some(Duration::ZERO), false)] {
        let mut rig = Rig::new(
            &format!("self-claim-first-idle-{held}"),
            &["zsh: killed     codex"],
        );
        rig.kernel.up_at = Some((3, FirstFrame::Idle));
        if let Some(bound) = bound {
            rig.opts.timings.alab_first_idle = bound;
        }
        rig.relaunch_resumed();
        let asked = rig.asked();
        assert_eq!(
            released_before_up(&asked),
            !held,
            "bound={bound:?}: {asked:#?}"
        );
        let said = if held {
            HELD_TO_FIRST_IDLE
        } else {
            HELD_TO_THE_BOUND
        };
        assert_eq!(rig.held(), said, "bound={bound:?}: {asked:#?}");
    }
}

/// A TURN ALREADY UNDER WAY ENDS THE HOLD AT ONCE: the relaunched codex-alab's claim proven
/// (its launch probe has read) and its frame in a turn or a box — a person's turn, or a carry-on
/// that went before a later step took the hand — is past its first idle, and codex-alab's own
/// acts in that turn wait for this very lease (the contract §5, `hand=-` before every act), so
/// the hand goes back at that look.
#[test]
fn a_turn_already_under_way_ends_the_relaunch_hold_at_once() {
    for (up, looks_once, said) in [
        (Some((1, FirstFrame::Turn)), true, HELD_TO_A_TURN),
        (None, false, HELD_TO_THE_BOUND),
    ] {
        let mut rig = Rig::new(
            &format!("self-claim-turn-{looks_once}"),
            &["zsh: killed     codex"],
        );
        rig.kernel.up_at = up;
        rig.relaunch_resumed();
        let asked = rig.asked();
        assert_eq!(rig.looks() == 1, looks_once, "{asked:#?}");
        assert_eq!(rig.held(), said, "{asked:#?}");
    }
}

/// AN UNREAD KERNEL START IS UNKNOWN, NEVER THE RELAUNCHED PROCESS'S END: off Apple the start
/// is a `ps`, which can fail once; read as an end, the hand went back while codex-alab still
/// ran (or was never held at all, when the first read failed), and its launch probe could read
/// `hand=-`.
#[test]
fn an_unread_kernel_start_never_ends_the_relaunch_hold() {
    let other = "Thu Oct 16 08:00:10 2026".to_string();
    for (births, up, looks, said) in [
        (
            vec![None, None],
            Some((1, FirstFrame::Idle)),
            1,
            HELD_TO_FIRST_IDLE,
        ),
        (
            vec![Some(NEW_BIRTH.to_string()), Some(other.clone())],
            None,
            1,
            HELD_TO_ITS_END,
        ),
    ] {
        let mut rig = Rig::new(
            &format!("self-claim-birth-{}", up.is_some()),
            &["zsh: killed     codex"],
        );
        rig.kernel.up_at = up;
        rig.kernel.births = Arc::new(Mutex::new(births.into_iter().collect()));
        rig.relaunch_resumed();
        let asked = rig.asked();
        assert_eq!(rig.looks(), looks, "{asked:#?}");
        assert_eq!(rig.held(), said, "{asked:#?}");
        assert_eq!(released_before_up(&asked), up.is_none(), "{asked:#?}");
    }
}

/// The hold ends with the relaunched process: one that ends before its first idle (the proof's
/// second crash, killed as soon as its claim is proven) has the hand given back at its end,
/// looked at no more, and the relaunch is still `done:resumed` (its own exit is its own
/// copy's to decide, `relaunch::outcome`).
#[test]
fn the_relaunch_hand_goes_back_when_the_relaunched_process_ends_before_its_first_idle() {
    for ends_at in [Some(2), None] {
        let mut rig = Rig::new(
            &format!("self-claim-ends-{}", ends_at.is_some()),
            &["zsh: killed     codex"],
        );
        rig.kernel.up_at = None;
        rig.kernel.ends_at = ends_at;
        rig.relaunch_resumed();
        let asked = rig.asked();
        let last_look = asked
            .iter()
            .rposition(|l| l.starts_with("kernel first-frame "))
            .expect("looked");
        let release = asked
            .iter()
            .rposition(|l| l.contains(" lease release "))
            .expect("given back");
        assert!(last_look < release, "{asked:#?}");
        match ends_at {
            Some(n) => {
                assert_eq!(rig.looks(), n, "no look after its end: {asked:#?}");
                assert_eq!(rig.held(), HELD_TO_ITS_END, "{asked:#?}");
            }
            None => {
                assert!(rig.looks() > 2, "held to the bound: {asked:#?}");
                assert_eq!(rig.held(), HELD_TO_THE_BOUND, "{asked:#?}");
            }
        }
    }
}

/// THE COPY NAMES THE THREAD, NEVER THE SCREEN: the shell shows an exit hint naming another
/// conversation (as an older Codex's exit in the same tab leaves it), and the relaunch resumes
/// the record's own.
#[test]
fn an_exit_hint_naming_another_thread_is_not_read() {
    let hint = [
        "To continue this session, run:".to_string(),
        format!("  codex resume {T1}"),
    ];
    assert!(
        hint[1].contains(T1) && T1 != T2,
        "the screen names another thread"
    );
    let refs: Vec<&str> = hint.iter().map(String::as_str).collect();
    let rig = Rig::new("self-claim-other-hint", &refs);
    let snap = rig.snap(None);
    let resume = rig.resume(None);
    let r = self_claimed_after_exit(&rig.opts, &snap, &resume, ExitCause::Exit, &rig.kernel);
    assert_eq!(r.step, "done:resumed", "{r:?}");
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(typed[0].contains(&format!("'resume' '{T2}'")), "{typed:#?}");
    assert!(!typed[0].contains(T1), "{typed:#?}");
}

/// A codex-alab's run read while it runs: the thread its writer lock names (embedded), else
/// its launch's `resume <id>`, else none; never the vendor daemon's word.
#[test]
fn a_codex_alab_run_names_its_lock_or_its_resumed_thread() {
    let mut rig = Rig::new("alab-snapshot", &[]);
    rig.opts.sock = None;
    rig.kernel.tui_alive = true;
    let exe = rig.dir.join("store/codex/alab/bin/codex");
    rig.kernel.exe = Some(exe.clone());
    let lock = |t: &str| {
        rig.home
            .join("thread-writer-locks")
            .join(format!("{t}.lock"))
    };
    let args = |argv: &[&str]| atpkg::caller_shell::ProcArgs {
        exec_path: exe.to_string_lossy().into_owned(),
        argv: argv.iter().map(|a| (*a).to_string()).collect(),
        env: vec![format!("CODEX_HOME={}", rig.home.display())],
    };
    let read = |k: &Crashed, argv: &[&str]| {
        snapshot(
            &rig.opts,
            TAB,
            TUI,
            "Thu Oct 16 08:00:00 2026".to_string(),
            SHELL,
            &args(argv),
            k,
        )
    };
    let held = Crashed {
        files: vec![lock(T2), PathBuf::from("/w/notes.txt")],
        ..rig.kernel.clone_world()
    };
    let snap = read(&held, &["codex"]).expect("a run");
    let run = snap.codex().expect("a Codex run");
    assert_eq!(snap.session.as_deref(), Some(T2));
    assert_eq!(run.home, rig.home);
    assert_eq!(run.home_env, Some(rig.home.to_string_lossy().into_owned()));
    let resumed = read(&rig.kernel, &["codex", "resume", T1]).expect("a run");
    assert_eq!(resumed.session.as_deref(), Some(T1));
    let plain = read(&rig.kernel, &["codex"]).expect("a run");
    assert_eq!(plain.session, None, "nothing names a thread");
    let two = Crashed {
        files: vec![lock(T1), lock(T2)],
        ..rig.kernel.clone_world()
    };
    assert_eq!(read(&two, &["codex"]).err(), Some("threads-ambiguous"));
    assert_eq!(
        read(&rig.kernel, &["codex", "app-server", "--listen", "unix://"]).err(),
        Some("not-a-tui")
    );
}

impl Crashed {
    /// The same world, its files to be set.
    fn clone_world(&self) -> Crashed {
        Crashed {
            owners: Mutex::new(VecDeque::new()),
            relaunched: Arc::clone(&self.relaunched),
            lock: self.lock.clone(),
            new_argv: self.new_argv.clone(),
            tui_alive: self.tui_alive,
            exe: self.exe.clone(),
            files: self.files.clone(),
            up_at: self.up_at,
            ends_at: self.ends_at,
            births: Arc::clone(&self.births),
            looks: Arc::clone(&self.looks),
            marks: self.marks.clone(),
        }
    }
}

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
        usage_credits: false,
        usage_tap: None,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
        timings: crate::harness::Timings::FAST,
        agents_off: Vec::new(),
        land: false,
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
/// gone, its round's stage, whether its week has passed.
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
/// fairness (every such pass takes it).
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

/// Tier-1: the real `prune`, reading the real registry over records on disk, bound to the
/// derived `HarnessUpgradeStateCollection`.
#[test]
#[allow(clippy::too_many_lines)]
fn the_prune_collects_a_closed_tabs_settled_codex_record_and_nothing_else() {
    const HERE: &str = "s-0a0a0a0a0a0a";
    const THERE: &str = "s-0b0b0b0b0b0b";
    let old = super::super::STATE_KEEP_S + 3_600;
    let model = aterm_models_harness::harness_upgrade_state_collection_model();
    let dir = scratch("codex-prune");
    let opts = prune_opts(&dir);
    let graph = registry(&dir, &[HERE, THERE]);
    let known_live = vec![HERE.to_string()];

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

    super::super::prune_states(&opts, &[], now);
    let dead_done = collection_state(&model, false, &Phase::Done, &Request::None, true);
    assert!(paths[0].exists(), "the Claude lane passes Codex records by");
    assert!(
        collection_conforms(&model, &dead_done, !paths[0].exists()).is_err(),
        "the bind catches the lane that kept it for ever"
    );
    assert!(!strangers[0].exists() && !strangers[2].exists());
    let claude_under_codex = filed_as(
        &opts,
        &key("s-dead0000000c"),
        &done(Agent::Claude, "s-dead0000000c"),
        old,
    );

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
/// defaults or both private.
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

/// THE RETIRED LANE'S LEFTOVERS GO ONCE A WEEK OLD ([`prune_retired`]): a daemon's pin stamp
/// (its directory too, once empty) and a tab's goal-pause record, neither of which anything
/// writes any more.
#[test]
fn the_retired_lanes_pin_stamps_and_goal_records_go_once_a_week_old() {
    let dir = scratch("retired-leftovers");
    let root = dir.join("aterm-state");
    let opts = Opts {
        aterm_state: Some(root.clone()),
        ..prune_opts(&dir)
    };
    let pins = state_dir(&opts).join("codex-daemons");
    let ledger = crate::supervise::approvals::ledger_under(&root, Some(TAB));
    let drive = ledger.parent().expect("the drive directory").to_path_buf();
    std::fs::create_dir_all(&pins).expect("pins");
    std::fs::create_dir_all(&drive).expect("drive");
    let aged = |path: &Path, ago: u64| {
        std::fs::write(path, "{}").expect("write");
        let when = std::time::SystemTime::now() - Duration::from_secs(ago);
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(when))
            .expect("mtime");
        path.to_path_buf()
    };
    let week = super::super::STATE_KEEP_S + 60;
    let old_pin = aged(&pins.join("default-pin.json"), week);
    let old_goal = aged(&drive.join(format!("{TAB}.goal-hold.json")), week);
    let new_pin = aged(&pins.join("other-pin.json"), 86_400);
    let new_goal = aged(&drive.join("s-0a0a.goal-hold.json"), 86_400);
    let ledger_file = aged(&ledger, week);
    let now = now_s();
    for quiet in [
        Opts {
            dry_run: true,
            ..opts.clone()
        },
        Opts {
            background: true,
            ..opts.clone()
        },
    ] {
        prune_retired(&quiet, now);
    }
    assert!(
        [&old_pin, &old_goal, &new_pin, &new_goal, &ledger_file]
            .iter()
            .all(|p| p.exists()),
        "a dry run and a break remove nothing"
    );
    prune_retired(&opts, now);
    assert!(!old_pin.exists() && !old_goal.exists(), "a week old: gone");
    assert!(new_pin.exists() && new_goal.exists(), "a day old: kept");
    assert!(ledger_file.exists(), "the loop's ledger is no leftover");
    assert!(pins.exists(), "a directory still holding a stamp is kept");
    let when = std::time::SystemTime::now() - Duration::from_secs(week);
    std::fs::File::options()
        .write(true)
        .open(&new_pin)
        .and_then(|f| f.set_modified(when))
        .expect("mtime");
    prune_retired(&opts, now);
    assert!(!pins.exists(), "emptied, the directory goes too");
}
