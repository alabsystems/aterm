// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CODEX-ALAB SESSION RECORD AND THE CRASH RULE (the codex-alab native
//! harness contract, §4), for a session a PROVEN self-claim supervises
//! (aterm-gui's `self_claim`): aterm's own Codex lane stands down for it, and
//! the exit's one decider is the window host's watch-only worker, which
//! relaunches a crash on the line codex-alab's own record names — once that
//! line has been proven to be the live process's.
//!
//! * **The record.** codex-alab writes `$CODEX_HOME/alab/sessions/<pid>.json`
//!   (schema v1, [`Record`]): its conversation (`sessionId`), its resume line
//!   (`resumeArgv`), its environment (`env`, at most `CODEX_HOME`) and how it
//!   last stood (`status`, `exit`). aterm reads it ONLY WHILE THE IDENTITY IS
//!   LIVE — the pid running under the kernel start the proof named, before
//!   and after the read ([`read_copy`]) — and keeps its own copy ([`Copy`]):
//!   at the exit it reads nothing of the file, which anything running as the
//!   user (a model's tool process among them) could have rewritten after the
//!   agent died. aterm never writes into `CODEX_HOME`.
//! * **The validation** ([`validate`], made against the live process at the
//!   same read): `resumeArgv[0]` is the path the live process was exec'd from
//!   (`KERN_PROCARGS2`'s exec path), its image (`proc_pidpath`), or atpkg's
//!   managed twin (`<prefix>/agents/codex`, the launch path `PATH` resolves
//!   inside aterm) while that twin execs an ALab build; the line ends
//!   `resume <sessionId>`, `sessionId` a UUID; the elements between are,
//!   in order and all of them, the root flags the live process ran with as
//!   the Codex lane reads them ([`upgrade_codex::rewrite_argv`]: its prompt,
//!   images, subcommand and session dropped, every flag kept with its value
//!   — a flag that lane does not know relaunches nothing); `env` names
//!   `CODEX_HOME` alone, equal to the live process's own; a `cwd` it names is
//!   the live process's own, and the relaunch runs in the live process's
//!   directory, never one the record chose. Anything else relaunches
//!   nothing: a line read from a writable file is never typed into a shell
//!   unless the agent itself ran it.
//! * **The verdict** ([`verdict`]): an exit whose copy saw the record removed
//!   is orderly (`/exit`, `/quit`, archive, the tab's hangup) and is left
//!   alone; one whose record stood with no exit word, or standing as
//!   `upgrading` (an `execve` that failed), is a CRASH; one the agent wrote
//!   down as `exit: "signal:<n>"` is a crash only where aterm sent that
//!   signal itself (its explicit restart's stamp: the stall's remedy).
//! * **The witness** ([`Witnesses`]): each `(pid, identity)` relaunched is
//!   written to aterm's own ledger (`<state>/drive/self-claim-witnesses`), and
//!   a witness found there is never relaunched again — a successor after a
//!   seamless update, or a second reader, sees it.

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::upgrade_codex;

/// The record's schema version this reader takes.
pub const RECORD_VERSION: u64 = 1;

/// Maximum serialized record size. Reads admit only regular files and never
/// wait for a Unix FIFO's writer.
const RECORD_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// The ledger file a handled witness is written to, under `<state>/drive/`.
pub const WITNESS_FILE: &str = "self-claim-witnesses";

/// What codex-alab's record says (schema v1), as far as the crash rule reads
/// it. Unknown fields are ignored; a field of the wrong type is a refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The process the record is of.
    pub pid: u32,
    /// The primary thread (`sessionId`), when the TUI had registered one.
    pub session_id: Option<String>,
    /// The resume line (`resumeArgv`), `None` until the primary thread's
    /// rollout was materialized.
    pub resume_argv: Option<Vec<String>>,
    /// The launch environment the line needs (`env`), in the record's order.
    pub env: Vec<(String, String)>,
    /// `busy|idle|waiting|upgrading|exited`, when named.
    pub status: Option<String>,
    /// How it ended, when it wrote that down (`signal:<n>`, …): `None` while
    /// it ran.
    pub exit: Option<String>,
    /// The directory it ran in.
    pub cwd: Option<String>,
}

/// Why a record or its line is refused (one word, said in the ledger and
/// on the Messages record).
pub type Refusal = &'static str;

/// Parse one record (schema v1, [`Record`]).
///
/// # Errors
/// `not-json`, `version` (another schema), `pid`, or the field whose type is
/// wrong.
pub fn parse(text: &str) -> Result<Record, Refusal> {
    let v: Value = aterm_json::from_str(text).map_err(|_| "not-json")?;
    if v.get("v").and_then(Value::as_u64) != Some(RECORD_VERSION) {
        return Err("version");
    }
    let pid = v
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .ok_or("pid")?;
    let opt_str = |k: &'static str| -> Result<Option<String>, Refusal> {
        match v.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(k),
        }
    };
    let resume_argv = match v.get("resumeArgv") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|i| i.as_str().map(str::to_owned))
                .collect::<Option<Vec<String>>>()
                .ok_or("resumeArgv")?,
        ),
        Some(_) => return Err("resumeArgv"),
    };
    let env = match v.get("env") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
            .collect::<Option<Vec<_>>>()
            .ok_or("env")?,
        Some(_) => return Err("env"),
    };
    Ok(Record {
        pid,
        session_id: opt_str("sessionId")?,
        resume_argv,
        env,
        status: opt_str("status")?,
        exit: opt_str("exit")?,
        cwd: opt_str("cwd")?,
    })
}

/// Whether `id` is a UUID in its one textual form (8-4-4-4-12 hex digits).
#[must_use]
pub fn is_uuid(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The live process's own facts, read while it ran (the same read as the
/// record's: [`read_copy`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Live {
    /// The path it was exec'd from (`KERN_PROCARGS2`): its launch path as
    /// its shell resolved it.
    pub exec_path: String,
    /// Its image (`proc_pidpath`), where readable.
    pub image: Option<PathBuf>,
    /// Its argv, `argv[0]` first.
    pub argv: Vec<String>,
    /// Its `CODEX_HOME` at exec, where set.
    pub codex_home: Option<String>,
    /// Its working directory, where readable.
    pub cwd: Option<PathBuf>,
    /// atpkg's managed Codex twin (`<prefix>/agents/codex`), where it is
    /// configured and execs an ALab build: the launch path a relaunch may
    /// name, so it runs the CURRENT managed build.
    pub twin: Option<PathBuf>,
}

/// A validated resume line ([`validate`]): what the relaunch types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    /// The line, `argv[0]` the launch path.
    pub argv: Vec<String>,
    /// The conversation it resumes.
    pub session: String,
    /// The `CODEX_HOME` it is relaunched under, when the launch had one.
    pub codex_home: Option<String>,
    /// The directory it ran in, when the record names one.
    pub cwd: Option<String>,
}

/// THE VALIDATION (module header): `record`'s line against the live
/// process's own facts.
///
/// # Errors
/// The word that refused it: `no-resume-line`, `no-session`, `session-not-uuid`,
/// `launch-path`, `no-resume`, `live-argv` (the live argv is not one the
/// Codex lane reads), `foreign-argv`, `env`, `cwd`, `pid`.
pub fn validate(record: &Record, pid: u32, live: &Live) -> Result<Resume, Refusal> {
    if record.pid != pid {
        return Err("pid");
    }
    let argv = record
        .resume_argv
        .as_ref()
        .filter(|a| !a.is_empty())
        .ok_or("no-resume-line")?;
    let session = record.session_id.as_deref().ok_or("no-session")?;
    if !is_uuid(session) {
        return Err("session-not-uuid");
    }
    let launch = &argv[0];
    let image = live.image.as_deref().map(Path::to_string_lossy);
    let twin = live.twin.as_deref().map(Path::to_string_lossy);
    if !(Path::new(launch).is_absolute()
        && (*launch == live.exec_path
            || image.as_deref() == Some(launch.as_str())
            || twin.as_deref() == Some(launch.as_str())))
    {
        return Err("launch-path");
    }
    // `resume <sessionId>` ends the line: nothing after it (no prompt).
    let at = argv
        .len()
        .checked_sub(2)
        .filter(|at| *at >= 1)
        .ok_or("no-resume")?;
    if argv[at] != "resume" || argv[at + 1] != session {
        return Err("no-resume");
    }
    // The root flags between, exactly as the live process ran them: in
    // order, every one with its value, its prompt and images dropped.
    let root = upgrade_codex::rewrite_argv(&live.argv, None).map_err(|_| "live-argv")?;
    if argv[1..at] != root[..] {
        return Err("foreign-argv");
    }
    // The relaunch runs where the live process ran; a directory the record
    // names is only checked against it (as spelled, or resolved).
    let same = |named: &str, running: &Path| {
        Path::new(named) == running || std::fs::canonicalize(named).is_ok_and(|c| c == running)
    };
    let cwd = match (record.cwd.as_deref(), live.cwd.as_deref()) {
        (Some(named), Some(running)) if !same(named, running) => return Err("cwd"),
        (Some(_), None) => return Err("cwd"),
        (_, running) => running.map(|p| p.to_string_lossy().into_owned()),
    };
    for (name, value) in &record.env {
        if name != "CODEX_HOME" || live.codex_home.as_deref() != Some(value.as_str()) {
            return Err("env");
        }
    }
    Ok(Resume {
        argv: argv.clone(),
        session: session.to_string(),
        // A record may omit its environment, but a per-command CODEX_HOME
        // belongs to the live process, not necessarily its parent shell.
        codex_home: live.codex_home.clone(),
        cwd: cwd.filter(|c| Path::new(c).is_absolute() && !c.chars().any(char::is_control)),
    })
}

/// What one read of the record found while the identity was live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// It stood, as parsed.
    Present(Record),
    /// It was not there: codex-alab removed it (an orderly end), or had not
    /// written it yet.
    Removed,
    /// It stood and could not be read as a record (the word why).
    Unreadable(Refusal),
}

/// ATERM'S OWN COPY of what the record said, from the last read made while
/// the identity was live ([`read_copy`]): the only thing the exit is decided
/// and relaunched from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copy {
    /// The agent's pid and kernel start: the identity read under.
    pub pid: u32,
    pub start: String,
    /// The `$CODEX_HOME` the record was read under (canonical).
    pub home: PathBuf,
    /// What the read found.
    pub seen: Seen,
    /// The record's line, validated against the live process at that read
    /// ([`validate`]); `Err` names why it relaunches nothing. A record not
    /// there or not readable is `Err("no-record")`.
    pub resume: Result<Resume, Refusal>,
}

/// Where codex-alab files its record: `<home>/alab/sessions/<pid>.json`.
#[must_use]
pub fn record_path(home: &Path, pid: u32) -> PathBuf {
    home.join("alab")
        .join("sessions")
        .join(format!("{pid}.json"))
}

/// The `$CODEX_HOME` a process started with `codex_home`/`home` uses:
/// `CODEX_HOME`, else `$HOME/.codex`; canonical where it exists.
fn codex_home_of(codex_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let dir = codex_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".codex"))
        })?;
    Some(std::fs::canonicalize(&dir).unwrap_or(dir))
}

/// atpkg's managed Codex twin, where one is configured and the build it execs
/// is ALab's ([`upgrade_codex::alab_bytes`]).
fn managed_alab_twin() -> Option<PathBuf> {
    let twin = atpkg::store::resolve_configured()?
        .agents_dir()
        .join("codex");
    let exe = super::upgrade_catalog::twin_binary(&std::fs::read_to_string(&twin).ok()?)?;
    upgrade_codex::alab_bytes(&exe).then_some(twin)
}

/// Make the copy from what was read: the record's text (`None`: no such
/// file) and the live facts (pure).
#[must_use]
pub fn copy_of(pid: u32, start: &str, home: PathBuf, text: Option<&str>, live: &Live) -> Copy {
    let seen = match text.map(parse) {
        None => Seen::Removed,
        Some(Ok(record)) => Seen::Present(record),
        Some(Err(why)) => Seen::Unreadable(why),
    };
    copy_seen(pid, start, home, seen, live)
}

fn copy_seen(pid: u32, start: &str, home: PathBuf, seen: Seen, live: &Live) -> Copy {
    let resume = match &seen {
        Seen::Present(record) => validate(record, pid, live),
        Seen::Removed | Seen::Unreadable(_) => Err("no-record"),
    };
    Copy {
        pid,
        start: start.to_string(),
        home,
        seen,
        resume,
    }
}

fn read_record(path: &Path) -> Seen {
    match super::accounts::read_bounded(path, RECORD_MAX_BYTES) {
        Ok(Some(text)) => match parse(&text) {
            Ok(record) => Seen::Present(record),
            Err(why) => Seen::Unreadable(why),
        },
        Ok(None) => Seen::Removed,
        Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
            Seen::Unreadable("record-too-large")
        }
        Err(_) => Seen::Unreadable("record-unreadable"),
    }
}

/// THE LIVE READ (module header): the record of the process `pid` started at
/// `start`, with the live process's own facts — read only while that very
/// process runs, looked at before the read and again after it. `None` when it
/// does not run (before or after), or its exec record cannot be read: nothing
/// is kept from a read that cannot be vouched for.
#[must_use]
pub fn read_copy(pid: u32, start: &str) -> Option<Copy> {
    use super::restart_stamp::alive_as;
    if !alive_as(pid, start) {
        return None;
    }
    let args = atpkg::caller_shell::process_args(pid)?;
    let live = Live {
        exec_path: args.exec_path.clone(),
        image: aterm_sysprobe::process_path(pid),
        argv: args.argv.clone(),
        codex_home: args.env_var("CODEX_HOME").map(str::to_owned),
        cwd: aterm_sysprobe::process_cwd(pid),
        twin: managed_alab_twin(),
    };
    let home = codex_home_of(live.codex_home.as_deref(), args.env_var("HOME"))?;
    let seen = read_record(&record_path(&home, pid));
    if !alive_as(pid, start) {
        return None;
    }
    Some(copy_seen(pid, start, home, seen, &live))
}

/// How an exit is decided from the copy ([`verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// An orderly end the copy saw (its record removed, or an exit word that
    /// is not aterm's signal): left alone.
    Orderly(Refusal),
    /// A crash: relaunched, where its line validated.
    Crash,
}

/// THE VERDICT (module header): `copy` the last live read's, `asked` whether
/// aterm's own explicit restart ended the process (its stamp).
#[must_use]
pub fn verdict(copy: &Copy, asked: bool) -> Verdict {
    let record = match &copy.seen {
        Seen::Removed => return Verdict::Orderly("removed"),
        Seen::Unreadable(why) => return Verdict::Orderly(why),
        Seen::Present(record) => record,
    };
    match record.exit.as_deref() {
        // A signal it wrote down: a crash only where aterm sent it.
        Some(word) if word.starts_with("signal:") => {
            if asked {
                Verdict::Crash
            } else {
                Verdict::Orderly("signal-not-aterms")
            }
        }
        // An upgrade's `execve` that never landed leaves `upgrading` standing.
        _ if record.status.as_deref() == Some("upgrading") => Verdict::Crash,
        Some(_) => Verdict::Orderly("exited"),
        None if record.status.as_deref() == Some("exited") => Verdict::Orderly("exited"),
        // The record stood with no word of an end: a panic, a SIGKILL, a
        // power loss — or aterm's own restart, which a SIGTERM handler that
        // never ran left unwritten.
        None => Verdict::Crash,
    }
}

/// Whether `copy` shows the agent's END coming: its record removed (an
/// orderly end), or standing with an exit word or as `exited`. A record that
/// stands with none (running, idle, or `upgrading` into its next image) is an
/// agent that goes on.
#[must_use]
pub fn shows_end(copy: &Copy) -> bool {
    match &copy.seen {
        Seen::Removed => true,
        Seen::Unreadable(_) => false,
        Seen::Present(record) => {
            record.exit.is_some() || record.status.as_deref() == Some("exited")
        }
    }
}

/// ATERM'S LEDGER OF HANDLED WITNESSES (module header): one `<pid>
/// <identity>` line per witness relaunched, under `<state>/drive/`.
pub struct Witnesses {
    path: PathBuf,
}

impl Witnesses {
    /// The ledger under the harness state root `state`.
    #[must_use]
    pub fn under(state: &Path) -> Self {
        Self {
            path: state.join("drive").join(WITNESS_FILE),
        }
    }

    fn line(pid: u32, start: &str) -> String {
        format!(
            "{pid} {}",
            start.split_whitespace().collect::<Vec<_>>().join("_")
        )
    }

    /// Whether the witness `(pid, start)` was handled already.
    #[must_use]
    pub fn handled(&self, pid: u32, start: &str) -> bool {
        let want = Self::line(pid, start);
        std::fs::read_to_string(&self.path).is_ok_and(|text| text.lines().any(|l| l == want))
    }

    /// Record the witness `(pid, start)` handled (appended; the directory
    /// made 0700 where it is not there).
    ///
    /// # Errors
    /// The filesystem's.
    pub fn record(&self, pid: u32, start: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", Self::line(pid, start))
    }
}

#[cfg(test)]
#[path = "self_claimed_tests.rs"]
mod tests;
