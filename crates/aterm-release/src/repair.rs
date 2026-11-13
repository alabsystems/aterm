// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The repair worker a failed release cut opens.
//!
//! When `targo --unverified ship release` refuses at a failing gate or build, a red it decided
//! about R's code, its failure exit hands [`open`] the failure and the cut's inputs
//! ([`Failure`]); a COULD NOT RUN, a dry run and an operational stop (a dirty clone, another
//! run's claim, a flag) open nothing. `open` starts one Claude Code worker under Autopilot Land
//! on it, `aterm ctl @self spawn agent=claude cwd=<the release clone> -` with [`task`] on stdin,
//! once per failing input hash ([`input_hash`]: the version, R and the prover), and keeps the
//! worker's sid in the clone's repair record ([`RECORD_FILE`], beside the release journal). The
//! record outlives the run, so a cut that fails again on the same inputs names the worker
//! already on them and opens none, while a new hop, version or prover is a new hash and opens
//! one. Outside an aterm session `open` starts nothing and gives the command to run instead
//! ([`command`]). A spawn that fails, or a record that cannot be read or kept, is said
//! ([`Opened::lines`]) and changes nothing else: the cut still ends with its own refusal.
//!
//! The spawn is injected ([`Spawner`]): [`Ctl`] is the real one, and a test's records its tasks.

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

/// The word that ends the task in [`command`]'s here-document. No task line is this word: every
/// field the failure supplies is one line after a label, and every excerpt line is indented.
const TASK_END: &str = "ATERM_REPAIR_TASK";

const HASH_DOMAIN: &[u8] = b"aterm-release-repair-v1";

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
}

/// The cut's input hash: SHA-256 over the version, R and the prover's `trustc` commit, which
/// decide what the cut judges. What failed, its log and the time are not inputs: a cut that fails
/// again on the same inputs is the same failure, whichever gate says it first.
#[must_use]
pub fn input_hash(failure: &Failure) -> String {
    let mut hash = aterm_digest::Sha256::new();
    for part in [
        HASH_DOMAIN,
        failure.version.as_bytes(),
        failure.commit.as_bytes(),
        failure.trustc.as_bytes(),
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    aterm_codec::hex::encode_lower(&hash.finalize())
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
    /// Whether this cut runs in an aterm session, where `@self` names it.
    fn in_session(&self) -> bool;
    /// Start one worker in `cwd` on `task`: its sid.
    /// # Errors
    /// Why no worker started.
    fn spawn(&mut self, cwd: &Path, task: &str) -> Result<String, String>;
}

/// The real spawner: in a session when `ATERM_SESSION_ID` names one, and `aterm ctl`
/// ([`spawn_args`]) with the task on stdin, each socket read and write bounded by a minute.
pub struct Ctl;

impl Spawner for Ctl {
    fn in_session(&self) -> bool {
        std::env::var_os(aterm_types::domain::ENV_SESSION_ID).is_some_and(|sid| !sid.is_empty())
    }

    fn spawn(&mut self, cwd: &Path, task: &str) -> Result<String, String> {
        use aterm_uds::spawnfd::Stdio;
        let mut child = aterm_uds::spawnfd::command("aterm")
            .args(["ctl", "--timeout", SPAWN_TIMEOUT_SECS])
            .args(spawn_args(cwd))
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

/// The command that starts the worker from an aterm tab: [`spawn_args`], the task in a
/// here-document.
#[must_use]
pub fn command(cwd: &Path, task: &str) -> String {
    let words: Vec<String> = spawn_args(cwd).iter().map(|w| shell_word(w)).collect();
    format!(
        "aterm ctl {} <<'{TASK_END}'\n{task}{TASK_END}",
        words.join(" ")
    )
}

/// `word` as a POSIX shell reads it back: itself when it holds nothing the shell reads, else
/// single-quoted.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@%+=:,./_-".contains(&b));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
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
    let mut head = String::new();
    let _ = writeln!(
        head,
        "A release cut of aterm stopped at a failing gate or build, and you are its repair \
         worker: a failed cut opens one per failing input hash."
    );
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
         that worktree once it has landed.\n\
         4. Do not run `ship release`, publish, or read the release credentials. The cut carries \
         your fix with `targo --unverified ship release --fix <the fix's own non-merge commit>`, \
         so end your report with that commit.\n\
         5. If the cause is not in the source (this machine, a service, a credential), change \
         nothing and say what it is.\n",
    );
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
    /// This cut runs in no aterm session: the command that starts the worker from one. It started
    /// none.
    Command { hash: String, command: String },
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
            Self::Command { hash, command } => {
                let mut lines = vec![format!(
                    "repair: not in an aterm session, so no worker opened (input hash {}); in an \
                     aterm tab this opens it:",
                    short(hash)
                )];
                lines.extend(command.lines().map(str::to_string));
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
/// named and none started; outside an aterm session ([`Spawner::in_session`]) the command is
/// given and none started; else one starts in `clone` on [`task`] and the record keeps its sid. A
/// record that cannot be read starts none, as it cannot say whether one was opened.
pub fn open(record: &Path, clone: &Path, failure: &Failure, spawner: &mut dyn Spawner) -> Opened {
    let hash = input_hash(failure);
    let mut kept = match Record::load(record) {
        Ok(kept) => kept,
        Err(why) => return Opened::Failed { hash, why },
    };
    if let Some(worker) = kept.worker(&hash) {
        let sid = worker.sid.clone();
        return Opened::Already { hash, sid };
    }
    let task = task(clone, failure, &hash);
    if !spawner.in_session() {
        let command = command(clone, &task);
        return Opened::Command { hash, command };
    }
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
