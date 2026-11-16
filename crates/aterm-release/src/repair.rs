// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Repair ownership when a release cut fails.
//!
//! When `targo --unverified ship release` refuses at a failing gate or build, a red it decided
//! about R's code, its failure exit hands [`open`] the failure and the cut's inputs
//! ([`Failure`]); a COULD NOT RUN, a dry run and an operational stop (a dirty clone, another
//! run's claim, a flag) open nothing. A caller-invoked cut returns its repair briefing for the
//! caller to handle through native subagents or directly, without opening another session. A
//! release pump needs an independent repair that persists after the cutter exits, so [`open`]
//! starts one Claude Code worker under Autopilot Land with [`task`] on stdin,
//! once per failing input hash ([`input_hash`]: the version, R and the prover), and keeps the
//! worker's sid in the clone's repair record ([`RECORD_FILE`], beside the release journal). The
//! record outlives the run, so a cut that fails again on the same inputs names the worker
//! already on them and opens none, while a new hop, version or prover is a new hash and opens
//! one. A red of `--open-next` judges main's tip, which is another commit at every try, so its
//! hash is of the version it opens after alone ([`opening_hash`]): one worker per opened
//! version. A pump in an aterm session starts it there; a pump outside one reaches the newest
//! local aterm. A spawn that fails, or a record that cannot be read or kept, is said
//! ([`Opened::lines`]) and changes nothing else: the cut still ends with its own refusal.
//!
//! The spawn is injected ([`Spawner`]): [`Ctl`] is the real one, and a test's records its tasks.
//! [`spawner`] chooses a route only for a pump; caller-owned repair needs no spawner or record.
//! The worker's report ends with one
//! line the pump reads ([`verdict`]): `FIX <commit>`, or `NO-FIX <why>`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The repair record's file name, in the clone's release directory beside its journal.
pub const RECORD_FILE: &str = "repairs.toml";

/// The most task bytes a worker's spawn takes (`aterm help spawn`: `len=` is 1 to 65536).
pub const MAX_TASK_BYTES: usize = 65_536;

/// The most workers the record keeps, the newest: an older failing input hash names a commit no
/// cut selects again.
const MAX_WORKERS: usize = 64;

/// The largest record read.
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

/// Each socket read and write of the real spawn, in seconds (`aterm ctl --timeout`), so the cut's
/// failure exit ends even when no aterm answers.
const SPAWN_TIMEOUT_SECS: &str = "60";

const HASH_DOMAIN: &[u8] = b"aterm-release-repair-v1";
const OPENING_DOMAIN: &[u8] = b"aterm-release-repair-open-next-v1";

/// A cut's red and its inputs: what its repair worker is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The failing gate or build, as the refusal names it.
    pub gate: String,
    /// The step the cut stopped at, `n/N name`.
    pub step: String,
    /// The step's log, which ends with the refusal's detail.
    pub log: PathBuf,
    /// The refusal's why.
    pub why: String,
    /// The refusal's detail lines.
    pub detail: Vec<String>,
    /// The refusal's remedy.
    pub remedy: String,
    /// The release's version.
    pub version: String,
    /// R: the commit the cut judged, the head of its line.
    pub commit: String,
    /// The prover's bin directory.
    pub prover: String,
    /// The prover's `trustc -vV` commit-hash.
    pub trustc: String,
    /// Epoch seconds the cut stopped.
    pub at: u64,
    /// For a red of `--open-next`: the cut version it opens the next one after. `commit` is
    /// then main's tip as that try read it, and `version` the one it would have opened.
    pub opening: Option<String>,
}

fn hashed(parts: &[&[u8]]) -> String {
    let mut hash = aterm_digest::Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    aterm_codec::hex::encode_lower(&hash.finalize())
}

/// The cut's input hash: SHA-256 over the version, R and the prover's `trustc` commit, which
/// decide what the cut judges. What failed, its log and the time are not inputs: a cut that fails
/// again on the same inputs is the same failure, whichever gate says it first. A red of
/// `--open-next` is [`opening_hash`] of the version it opens after.
#[must_use]
pub fn input_hash(failure: &Failure) -> String {
    match &failure.opening {
        Some(from) => opening_hash(from),
        None => cut_hash(&failure.version, &failure.commit, &failure.trustc),
    }
}

fn cut_hash(version: &str, commit: &str, trustc: &str) -> String {
    hashed(&[
        HASH_DOMAIN,
        version.as_bytes(),
        commit.as_bytes(),
        trustc.as_bytes(),
    ])
}

/// The input hash of every red of opening the version after `from`: of `from` alone, because
/// each try judges whatever commit main's tip then is, and a red that main's movement does not
/// clear is one failure with one worker.
#[must_use]
pub fn opening_hash(from: &str) -> String {
    hashed(&[OPENING_DOMAIN, from.as_bytes()])
}

/// The clone's repair record: the workers its failed cuts opened, one per failing input hash.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    #[serde(default, rename = "worker")]
    pub workers: Vec<Worker>,
}

/// One worker a failed cut opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    /// The failing input hash it was opened for ([`input_hash`]).
    pub hash: String,
    /// Its session.
    pub sid: String,
    /// The failing gate or build it was opened on, the step and the log it was given.
    pub gate: String,
    pub step: String,
    pub log: PathBuf,
    /// The cut's inputs.
    pub version: String,
    pub commit: String,
    pub trustc: String,
    /// Epoch seconds the cut stopped.
    pub at: u64,
}

impl Worker {
    /// Whether it was opened on a red of the commit it names, as a cut of a line judges one:
    /// its hash is of its own version, commit and prover. A worker of an `--open-next` red is
    /// kept under [`opening_hash`] and is no red of any line.
    #[must_use]
    pub fn of_cut(&self) -> bool {
        self.hash == cut_hash(&self.version, &self.commit, &self.trustc)
    }
}

impl Record {
    /// The record at `path`; an empty one where there is none.
    /// # Errors
    /// A record that cannot be read or does not parse.
    pub fn load(path: &Path) -> Result<Self, String> {
        match aterm_uds::atomic_file::read_regular_text(path, MAX_RECORD_BYTES) {
            Ok(text) => aterm_toml::from_str(&text)
                .map_err(|e| format!("{} is not a repair record: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    /// Written whole: a temporary, flushed, renamed, the directory flushed
    /// ([`aterm_uds::atomic_file::write`]).
    /// # Errors
    /// Any write.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text =
            aterm_toml::to_string(self).map_err(|e| format!("serialize the repair record: {e}"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        aterm_uds::atomic_file::write(path, text.as_bytes(), 0o600)
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    /// The worker opened for `hash`, if one was.
    #[must_use]
    pub fn worker(&self, hash: &str) -> Option<&Worker> {
        self.workers.iter().find(|worker| worker.hash == hash)
    }
}

/// Where a repair worker starts.
pub trait Spawner {
    /// Start one worker in `cwd` on `task`: its sid.
    /// # Errors
    /// Why no worker started.
    fn spawn(&mut self, cwd: &Path, task: &str) -> Result<String, String>;
}

/// The environment variable the release pump sets on each cut it runs
/// ([`crate::pump`]): a red of such a cut opens its worker without a session ([`spawner`]).
pub const PUMP_ENV: &str = "ATERM_RELEASE_PUMP";

/// The real spawner: in a session when `ATERM_SESSION_ID` names one, and `aterm ctl`
/// ([`spawn_args`]) with the task on stdin, each socket read and write bounded by a minute.
pub struct Ctl;

impl Ctl {
    /// Whether `@self` can route a pump's repair through this cut's aterm session.
    #[must_use]
    pub fn in_session() -> bool {
        std::env::var_os(aterm_types::domain::ENV_SESSION_ID).is_some_and(|sid| !sid.is_empty())
    }
}

impl Spawner for Ctl {
    fn spawn(&mut self, cwd: &Path, task: &str) -> Result<String, String> {
        ctl_spawn(&spawn_args(cwd), task)
    }
}

/// The spawner of a cut no session runs (the release pump's, under launchd): `aterm ctl`
/// ([`unattended_args`]) names no session, so it reaches the newest aterm of this Mac, which
/// starts the worker in a background tab. With no aterm running, or one that takes no workers,
/// the spawn fails, [`open`] says so and records nothing, and the next red of the same inputs
/// tries again.
pub struct Unattended;

impl Spawner for Unattended {
    fn spawn(&mut self, cwd: &Path, task: &str) -> Result<String, String> {
        ctl_spawn(&unattended_args(cwd), task)
    }
}

/// The route for a pump's repair: [`Ctl`] in a session, [`Unattended`] outside one. A caller's
/// cut returns no spawner regardless of its terminal: its own harness handles the failure.
#[must_use]
pub fn spawner(in_session: bool, pump: bool) -> Option<Box<dyn Spawner>> {
    if !pump {
        None
    } else if unattended(in_session, pump) {
        Some(Box::new(Unattended))
    } else {
        Some(Box::new(Ctl))
    }
}

/// Who handles this cut's failure. Only a pump has an independently supervised repair process.
pub enum RepairMode<'a> {
    /// Return the failure briefing to the caller, with no independent session.
    Caller,
    /// The persistent release pump owns the repair and its worker's eventual report.
    Pump(&'a mut dyn Spawner),
}

/// Whether a cut's worker starts with no session: only a pump's cut outside one.
#[must_use]
pub const fn unattended(in_session: bool, pump: bool) -> bool {
    pump && !in_session
}

fn ctl_spawn(words: &[String], task: &str) -> Result<String, String> {
    use aterm_uds::spawnfd::Stdio;
    eprintln!(
        "repair: the release pump needs an independent repair session that persists after this cut exits"
    );
    let mut child = aterm_uds::spawnfd::command("aterm")
        .args(["ctl", "--timeout", SPAWN_TIMEOUT_SECS])
        .args(words)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run aterm ctl: {e}"))?;
    // aterm ctl reads the whole task before it asks the aterm; a write it cut short is told
    // by its answer.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut stdin, task.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("aterm ctl: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if out.status.success()
        && let Some(sid) = sid_of(&stdout)
    {
        return Ok(sid);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let said = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|said| !said.is_empty())
        .unwrap_or("no answer");
    Err(format!("aterm ctl exited {}: {said}", out.status))
}

/// `aterm ctl`'s words that start the worker in `cwd` from no session, its task read from
/// stdin: the newest local aterm takes it, in a tab it does not bring to the front.
#[must_use]
pub fn unattended_args(cwd: &Path) -> Vec<String> {
    vec![
        "spawn".into(),
        "raise=f".into(),
        "agent=claude".into(),
        format!("cwd={}", cwd.display()),
        "-".into(),
    ]
}

/// `aterm ctl`'s answer for a sid no session of the aterm has.
const GONE: &str = "no such session";

/// One `aterm ctl` read of the worker `sid` (`task`, `result`, `result full`), bounded like
/// the spawn.
fn ctl_read(sid: &str, verb: &[&str]) -> Result<String, String> {
    let out = aterm_uds::spawnfd::command("aterm")
        .args(["ctl", "--timeout", SPAWN_TIMEOUT_SECS])
        .arg(format!("@{sid}"))
        .args(verb)
        .output()
        .map_err(|e| format!("cannot run aterm ctl: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let said = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|said| !said.is_empty())
        .unwrap_or("no answer")
        .to_string();
    Err(format!(
        "aterm ctl @{sid} {} exited {}: {said}",
        verb.join(" "),
        out.status
    ))
}

/// What the worker `sid` reports now, read from the aterm that runs it.
#[must_use]
pub fn report(sid: &str) -> crate::pump::Report {
    report_from(ctl_read(sid, &["task"]), |full| {
        ctl_read(
            sid,
            if full {
                &["result", "full"]
            } else {
                &["result"]
            },
        )
    })
}

/// A worker's report from its task record (`state=<word> …`) and, once it ended, its report:
/// `result(false)` is its last reply (`aterm ctl result`), `result(true)` its whole exchange
/// since the task prompt (`result full`). Its state is one of `aterm ctl`'s
/// `working|done|exited|stalled|failed|-`: `working` (and a word this does not know) is still
/// working; `done`, `exited`, `stalled` and `failed` have ended, with a [`verdict`] and, without
/// one, no fix; `-` is a session that runs no task, which ended without a fix. A session the
/// aterm no longer has (`no such session`: its tab was closed, or the aterm restarted) ended
/// without a fix too. Any other record or last reply that cannot be read is unread, never an
/// ended worker.
///
/// The verdict is the last reply's, else the newest verdict line in the whole exchange: under
/// Autopilot Land a worker's last reply answers the harness's land check (`LANDED`), after the
/// reply that named its fix (0.202.0, 2026-11-14 15:58: `FIX 73c05e2c0d8…` then `LANDED`, read
/// as no fix). The task prompt itself names no verdict line (`FIX` needs 40 hex digits, and its
/// `NO-FIX` sits inside a sentence), so only a worker's own line is found. An exchange that
/// cannot be read leaves the last reply's answer: no fix.
pub fn report_from(
    task: Result<String, String>,
    mut result: impl FnMut(bool) -> Result<String, String>,
) -> crate::pump::Report {
    use crate::pump::Report;
    let task = match task {
        Ok(task) => task,
        Err(why) if why.contains(GONE) => {
            return Report::NoFix(format!("its session is gone ({why})"));
        }
        Err(why) => return Report::Unread(why),
    };
    let Some(state) = task
        .split_whitespace()
        .find_map(|word| word.strip_prefix("state="))
    else {
        return Report::Unread(format!(
            "its task record names no state: {}",
            one_line(&task)
        ));
    };
    if state == "-" {
        return Report::NoFix("its session runs no task".into());
    }
    if !matches!(state, "done" | "exited" | "stalled" | "failed") {
        return Report::Working;
    }
    let last = match result(false) {
        Err(why) if why.contains(GONE) => {
            return Report::NoFix(format!("its session is gone ({why})"));
        }
        Err(why) => return Report::Unread(why),
        Ok(report) => verdict(&report),
    };
    let (found, unread) = match last {
        Some(found) => (Some(found), None),
        None => match result(true) {
            Ok(exchange) => (verdict(&exchange), None),
            Err(why) => (None, Some(why)),
        },
    };
    match found {
        Some(Ok(commit)) => Report::Fix(commit),
        Some(Err(why)) => Report::NoFix(why),
        None => Report::NoFix(format!(
            "its task is {state} and none of its replies holds `FIX <commit>` or `NO-FIX \
             <why>`{}",
            unread.map_or_else(String::new, |why| format!(
                " (its whole exchange could not be read: {why}; its last reply holds neither)"
            ))
        )),
    }
}

/// What a worker's final report says of its fix, read from its last verdict line: `Ok(commit)`
/// for `FIX <40 hex>`, `Err(why)` for `NO-FIX <why>`, `None` where no line is either. A line
/// may be wrapped as a report formats it (backticks, emphasis, a list mark, a closing period).
#[must_use]
pub fn verdict(report: &str) -> Option<Result<String, String>> {
    report.lines().rev().find_map(|line| {
        let line = line.trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '`' | '*' | '_' | '-' | '>' | '.')
        });
        if let Some(why) = line.strip_prefix("NO-FIX") {
            let why = why.trim_start_matches([':', ' ', '`']).trim();
            return Some(Err(if why.is_empty() {
                "no reason given".to_string()
            } else {
                why.to_string()
            }));
        }
        let commit = line
            .strip_prefix("FIX")?
            .trim_start_matches([':', ' ', '`'])
            .trim();
        (commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| Ok(commit.to_ascii_lowercase()))
    })
}

/// The sid of a worker spawn's answer, `OK <sid> …`.
fn sid_of(answer: &str) -> Option<String> {
    let mut words = answer.split_whitespace();
    (words.next() == Some("OK"))
        .then(|| words.next())
        .flatten()
        .map(str::to_string)
}

/// `aterm ctl`'s words that start the worker in `cwd`, its task read from stdin.
#[must_use]
pub fn spawn_args(cwd: &Path) -> Vec<String> {
    vec![
        "@self".into(),
        "spawn".into(),
        "agent=claude".into(),
        format!("cwd={}", cwd.display()),
        "-".into(),
    ]
}

/// `text` on one line: each NUL, carriage return and newline a space.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r', '\0'], " ")
}

/// `text`'s lines, each indented by `lead`, without NULs or carriage returns and at most `width`
/// characters, at most `lines` of them and at most `budget` bytes in all; then, where that left
/// lines out, a line saying how many more the log holds, inside the same `budget`.
fn excerpt(text: &str, lines: usize, width: usize, lead: &str, budget: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let note = |left: usize| format!("{lead}… {left} more line(s), in the log\n");
    // The longest note this text can need stays free while lines are added.
    let room = note(all.len()).len();
    let mut out = String::new();
    let mut kept = 0;
    for line in all.iter().take(lines) {
        let clean: String = line.chars().filter(|c| !matches!(c, '\0' | '\r')).collect();
        let row = match clean.char_indices().nth(width) {
            Some((at, _)) => format!("{lead}{} …\n", &clean[..at]),
            None => format!("{lead}{clean}\n"),
        };
        if out.len() + row.len() + room > budget {
            break;
        }
        out.push_str(&row);
        kept += 1;
    }
    let left = all.len() - kept;
    if left > 0 && out.len() + note(left).len() <= budget {
        out.push_str(&note(left));
    }
    out
}

/// The worker's task: what failed, the refusal's why, detail and remedy, where its log is, the
/// cut's inputs, and what to do. Data only: paths, never what a credential holds. At most
/// [`MAX_TASK_BYTES`]: the refusal's own words take only the bytes the rest leaves, so what
/// failed, the log, the inputs and what to do are written whole; only were those parts alone
/// over the cap (no path, gate or commit makes them so) would its end be cut.
#[must_use]
pub fn task(clone: &Path, failure: &Failure, hash: &str) -> String {
    briefing(clone, failure, hash, true)
}

/// The failure context shared by a caller's handoff and an independent pump worker's task.
fn briefing(clone: &Path, failure: &Failure, hash: &str, worker: bool) -> String {
    let mut head = String::new();
    if worker {
        let _ = writeln!(
            head,
            "A release cut of aterm stopped at a failing gate or build, and you are its repair \
             worker: a failed cut opens one per failing input hash."
        );
    } else {
        head.push_str(
            "A release cut of aterm stopped at a failing gate or build. The caller handles its \
             repair; no new aterm session was opened. Use your harness's native subagents for \
             same-machine delegation and your own shell/background tools for commands. Work \
             directly when native subagents are unavailable. A separate session needs an \
             explicit request or a concrete need native tools cannot meet; state that reason \
             before spawning.\n",
        );
    }
    let _ = writeln!(
        head,
        "\nWhat failed: {}, at step {} of `targo --unverified ship release`.",
        one_line(&failure.gate),
        one_line(&failure.step)
    );
    let mut tail = String::new();
    let _ = writeln!(
        tail,
        "The step's log, which ends with that detail: {}",
        one_line(&failure.log.display().to_string())
    );
    if let Some(logs) = failure.log.parent() {
        let _ = writeln!(
            tail,
            "The run's other logs are beside it, in {}",
            one_line(&logs.display().to_string())
        );
    }
    let _ = writeln!(tail, "\nThe cut's inputs (input hash {}):", one_line(hash));
    let _ = writeln!(tail, "- version: {}", one_line(&failure.version));
    let _ = writeln!(
        tail,
        "- R, the commit the cut judged: {}",
        one_line(&failure.commit)
    );
    let _ = writeln!(
        tail,
        "- prover: {} (trustc {})",
        one_line(&failure.prover),
        one_line(&failure.trustc)
    );
    let _ = writeln!(
        tail,
        "- release clone: {}",
        one_line(&clone.display().to_string())
    );
    tail.push_str(
        "\nWhat to do:\n\
         1. Find the cause in the log.\n\
         2. Leave the release clone as it is: it holds the cut's journal and state, and nothing \
         in it is yours to edit, commit, reset or switch.\n\
         3. Fix the cause forward on main in a worktree of your own outside the release clone \
         (AGENTS.md, Shared checkout), land it with `targo --unverified verify land`, and remove \
         that worktree when its overall goal is done.\n",
    );
    if worker {
        tail.push_str(
            "4. Do not run `ship release`, publish, or read the release credentials. The cut \
             carries your fix with `targo --unverified ship release --fix <the fix's own \
             non-merge commit>`, so end your report with one line that names that commit in \
             full: `FIX <its 40 hex digits>`.\n\
             5. If the cause is not in the source (this machine, a service, a credential), or \
             you could not finish, change nothing and end your report with one line: \
             `NO-FIX <what it is>`.\n",
        );
    } else {
        tail.push_str(
            "4. After landing a source fix, the cut can carry it with `targo --unverified ship \
             release --fix <the fix's own non-merge commit>`.\n\
             5. If a machine, service or credential prevents the operation, record that \
             limitation and continue useful work elsewhere.\n",
        );
    }
    // The why, the detail and the remedy share what is left, a third each.
    let share = MAX_TASK_BYTES.saturating_sub(head.len() + tail.len()) / 3;
    let detail = failure.detail.join("\n");
    let parts = [
        Some(("Why:\n", failure.why.as_str(), 40, 300, "  ")),
        (!detail.is_empty()).then_some(("Detail:\n", detail.as_str(), 30, 300, "  | ")),
        Some((
            "The cut's remedy:\n",
            failure.remedy.as_str(),
            10,
            600,
            "  ",
        )),
    ];
    let mut t = head;
    for (title, text, lines, width, lead) in parts.into_iter().flatten() {
        if let Some(budget) = share.checked_sub(title.len()) {
            t.push_str(title);
            t.push_str(&excerpt(text, lines, width, lead, budget));
        }
    }
    t.push_str(&tail);
    if t.len() > MAX_TASK_BYTES {
        let mut end = MAX_TASK_BYTES - 1;
        while !t.is_char_boundary(end) {
            end -= 1;
        }
        t.truncate(end);
        t.push('\n');
    }
    t
}

/// What [`open`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// It started a worker on the failure and recorded its sid.
    Spawned { hash: String, sid: String },
    /// A worker was already opened for this input hash: its sid. It started none.
    Already { hash: String, sid: String },
    /// The caller owns the repair; its failure context, with no worker started or recorded.
    Caller { hash: String, briefing: String },
    /// It started a worker, and the record could not keep its sid: why. A cut that fails again on
    /// these inputs opens another.
    Unrecorded {
        hash: String,
        sid: String,
        why: String,
    },
    /// It started none: why.
    Failed { hash: String, why: String },
}

impl Opened {
    /// What the cut says of it, `record` being the repair record.
    #[must_use]
    pub fn lines(&self, record: &Path) -> Vec<String> {
        let short = |hash: &str| hash.get(..12).unwrap_or(hash).to_string();
        match self {
            Self::Spawned { hash, sid } => vec![format!(
                "repair: opened worker {sid} on this failure (input hash {}), kept in {}; \
                 `aterm ctl @{sid} result` reads its report",
                short(hash),
                record.display()
            )],
            Self::Already { hash, sid } => vec![format!(
                "repair: worker {sid} was opened on these inputs already (input hash {}, {}); \
                 none opened",
                short(hash),
                record.display()
            )],
            Self::Caller { hash, briefing } => {
                let mut lines = vec![format!(
                    "repair: caller-owned repair (input hash {}); no worker opened",
                    short(hash)
                )];
                lines.extend(briefing.lines().map(str::to_string));
                lines
            }
            Self::Unrecorded { hash, sid, why } => vec![format!(
                "repair: opened worker {sid} on this failure (input hash {}), but {} could not \
                 keep it: {why}",
                short(hash),
                record.display()
            )],
            Self::Failed { hash, why } => vec![format!(
                "repair: no worker opened (input hash {}): {why}",
                short(hash)
            )],
        }
    }
}

/// Open the repair worker of `failure`, a red of the cut in the release clone `clone` whose
/// repair record is `record`, once per [`input_hash`]: a worker the record holds for the hash is
/// named and none started. [`RepairMode::Caller`] instead returns the repair briefing without
/// reading or writing a worker record. Only a pump starts a worker in `clone` on [`task`] and
/// keeps its sid. A pump's unreadable record starts none, as it cannot say whether one opened.
pub fn open(record: &Path, clone: &Path, failure: &Failure, mode: RepairMode<'_>) -> Opened {
    let hash = input_hash(failure);
    let spawner = match mode {
        RepairMode::Caller => {
            let briefing = briefing(clone, failure, &hash, false);
            return Opened::Caller { hash, briefing };
        }
        RepairMode::Pump(spawner) => spawner,
    };
    let mut kept = match Record::load(record) {
        Ok(kept) => kept,
        Err(why) => return Opened::Failed { hash, why },
    };
    if let Some(worker) = kept.worker(&hash) {
        let sid = worker.sid.clone();
        return Opened::Already { hash, sid };
    }
    let task = task(clone, failure, &hash);
    let sid = match spawner.spawn(clone, &task) {
        Ok(sid) => sid,
        Err(why) => return Opened::Failed { hash, why },
    };
    kept.workers.push(Worker {
        hash: hash.clone(),
        sid: sid.clone(),
        gate: failure.gate.clone(),
        step: failure.step.clone(),
        log: failure.log.clone(),
        version: failure.version.clone(),
        commit: failure.commit.clone(),
        trustc: failure.trustc.clone(),
        at: failure.at,
    });
    let over = kept.workers.len().saturating_sub(MAX_WORKERS);
    kept.workers.drain(..over);
    match kept.save(record) {
        Ok(()) => Opened::Spawned { hash, sid },
        Err(why) => Opened::Unrecorded { hash, sid, why },
    }
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod tests;
