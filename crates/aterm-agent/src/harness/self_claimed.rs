// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-alab` self-claimed session record parser, live validator, and crash-relaunch rules.

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::codex_alab;

/// Supported `codex-alab` session record schema version.
pub const RECORD_VERSION: u64 = 1;

/// Maximum serialized record size. Reads admit only regular files and never
/// wait for a Unix FIFO's writer.
const RECORD_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Ledger filename for handled crash witnesses under `<state>/drive/`.
pub const WITNESS_FILE: &str = "self-claim-witnesses";

/// Parsed `codex-alab` schema-v1 session record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Process ID recorded by the agent.
    pub pid: u32,
    /// Primary conversation UUID (`sessionId`), once registered.
    pub session_id: Option<String>,
    /// Resume command line (`resumeArgv`), once the rollout is materialized.
    pub resume_argv: Option<Vec<String>>,
    /// Launch environment pairs (`env`) in record order.
    pub env: Vec<(String, String)>,
    /// Agent status word (`busy|idle|waiting|upgrading|exited`).
    pub status: Option<String>,
    /// Exit reason token (`signal:<n>`, etc.), or `None` while running.
    pub exit: Option<String>,
    /// Working directory recorded by the agent.
    pub cwd: Option<String>,
    /// Whether the TUI composer held no user draft (`draftEmpty`).
    pub draft_empty: Option<bool>,
    /// TUI frame state (`working|idle|blocked|done|error`).
    pub frame: Option<String>,
    /// Active goal published by `codex-alab` for the primary thread, sanitized for one-line display.
    pub goal: Option<String>,
    /// Whether the record is in its pre-registration initializing frame.
    pub initializing: bool,
}

/// Short refusal code returned when a record or resume line is rejected.
pub type Refusal = &'static str;

/// Parse a `codex-alab` schema-v1 [`Record`] from JSON `text`.
///
/// # Errors
/// Returns a [`Refusal`] code if `text` is not valid schema-v1 JSON.
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
        draft_empty: v.get("draftEmpty").and_then(Value::as_bool),
        frame: v.get("state").and_then(Value::as_str).map(str::to_owned),
        goal: opt_str("goal")?
            .as_deref()
            .and_then(super::goal::objective_of),
        initializing: ["sessionId", "resumeArgv", "cwd", "state", "exit"]
            .iter()
            .all(|key| matches!(v.get(key), Some(Value::Null)))
            && v.get("status").and_then(Value::as_str) == Some("busy")
            && v.get("draftEmpty").and_then(Value::as_bool) == Some(true),
    })
}

/// Whether `record` reports the agent at a watch-only idle point.
#[must_use]
pub fn idle_point(record: &Record) -> bool {
    record.status.as_deref() == Some("idle")
        && record.draft_empty == Some(true)
        && record.exit.is_none()
}

/// The live process `pid`'s `$CODEX_HOME` (canonical) and its own facts,
/// from its exec record; `None` where that cannot be read.
fn live_of(pid: u32) -> Option<(PathBuf, Live)> {
    let args = atpkg::caller_shell::process_args(pid)?;
    let live = Live {
        exec_path: args.exec_path.clone(),
        image: aterm_sysprobe::process_path(pid),
        argv: args.argv.clone(),
        codex_home: args.env_var(codex_alab::HOME_ENV).map(str::to_owned),
        codex_auth_path: args.env_var(codex_alab::AUTH_PATH_ENV).map(str::to_owned),
        cwd: aterm_sysprobe::process_cwd(pid),
        twin: None,
        managed_launch: atpkg::store::resolve_configured()
            .and_then(|layout| atpkg::managed_launch::Hints::capture(&layout, &args)),
    };
    let home = codex_home_of(
        live.codex_home.as_deref(),
        live.cwd.as_deref(),
        args.env_var("HOME"),
    )?;
    Some((home, live))
}

/// Whether live process `(pid, start)` is currently at its idle point according to its record.
#[must_use]
pub fn read_idle(pid: u32, start: &str) -> bool {
    read_live(pid, start, |record| idle_point(record).then_some(())).is_some()
}

/// Initial TUI frame classification of a relaunched `codex-alab` process ([`first_frame`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstFrame {
    /// Process has reached its first idle frame.
    Idle,
    /// Process is actively working or blocked on an approval/question box.
    Turn,
}

/// Classify the initial TUI frame of a relaunched `codex-alab` from `record`.
#[must_use]
pub fn first_frame(record: &Record) -> Option<FirstFrame> {
    if record.exit.is_some() {
        return None;
    }
    match record.frame.as_deref() {
        _ if idle_point(record) => Some(FirstFrame::Idle),
        Some("idle" | "done" | "error") => Some(FirstFrame::Idle),
        Some("working" | "blocked") => Some(FirstFrame::Turn),
        _ => None,
    }
}

/// Read and classify the initial TUI frame of live process `(pid, start)`.
#[must_use]
pub fn read_first_frame(pid: u32, start: &str) -> Option<FirstFrame> {
    read_live(pid, start, first_frame)
}

/// `point` of the process `pid` started at `start`'s own record, read only
/// while that very process runs (looked at before the read and again after
/// it), the record naming that pid.
fn read_live<T>(pid: u32, start: &str, point: fn(&Record) -> Option<T>) -> Option<T> {
    use super::restart_stamp::alive_as;
    if !alive_as(pid, start) {
        return None;
    }
    let (home, _) = live_of(pid)?;
    let at = match read_record(&record_path(&home, pid)) {
        Seen::Present(record) if record.pid == pid => point(&record),
        Seen::Present(_) | Seen::Removed | Seen::Never | Seen::Unreadable(_) => None,
    };
    at.filter(|_| alive_as(pid, start))
}

/// Extract the proven `codex-alab` PID from a `status` reply string.
#[must_use]
pub fn proven_pid(status: &str) -> Option<u32> {
    use crate::supervise::screen::status_field;
    if status_field(status, "supervisor_proven") != Some("1") {
        return None;
    }
    let holder = status_field(status, "supervisor")?;
    holder
        .strip_prefix("codex-alab%40")
        .or_else(|| holder.strip_prefix("codex-alab@"))?
        .parse()
        .ok()
}

/// Whether `id` is a canonical 8-4-4-4-12 lowercase/uppercase hex UUID.
#[must_use]
pub fn is_uuid(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Live kernel facts for a running `codex-alab` process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Live {
    /// Executable path from `KERN_PROCARGS2`.
    pub exec_path: String,
    /// Resolved executable image path from `proc_pidpath`.
    pub image: Option<PathBuf>,
    /// Process argument vector (`argv[0]` first).
    pub argv: Vec<String>,
    /// `CODEX_HOME` environment variable at exec, if set.
    pub codex_home: Option<String>,
    /// `CODEX_AUTH_PATH` environment variable at exec, if set.
    pub codex_auth_path: Option<String>,
    /// Current working directory of the process.
    pub cwd: Option<PathBuf>,
    /// Managed `atpkg` Codex twin path (`<prefix>/agents/codex`), if configured.
    pub twin: Option<PathBuf>,
    /// Untrusted managed-launch fields from that same kernel snapshot and configured prefix.
    pub managed_launch: Option<atpkg::managed_launch::Hints>,
}

/// Validated resume command and environment for relaunching a crashed session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resume {
    /// Command vector with the launch path at `argv[0]`.
    pub argv: Vec<String>,
    /// Conversation UUID to resume.
    pub session: String,
    /// `CODEX_HOME` directory for the relaunch, if non-default.
    pub codex_home: Option<String>,
    /// `CODEX_AUTH_PATH` login file for the relaunch, if set.
    pub codex_auth_path: Option<String>,
    /// Working directory for the relaunch.
    pub cwd: Option<String>,
}

fn has_unsafe_line_char(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

fn is_structural_abs_path(s: &str) -> bool {
    Path::new(s).is_absolute() || s.starts_with('/')
}

impl Resume {
    /// Validate that this resume command and environment satisfy every structural invariant of a
    /// live-validated `codex-alab` resume record (non-empty absolute executable, canonical UUID
    /// session, kept-flag-only `resume <uuid>` tail, safe paths, and no control or Unicode line
    /// separator characters).
    ///
    /// # Errors
    /// Returns the [`Refusal`] code explaining why this [`Resume`] cannot be executed or restored.
    pub fn validate_structural(&self) -> Result<(), Refusal> {
        let Some((launch, rest)) = self.argv.split_first() else {
            return Err("no-resume-line");
        };
        if !is_uuid(&self.session) {
            return Err("session-not-uuid");
        }
        if launch.is_empty() || !is_structural_abs_path(launch) || has_unsafe_line_char(launch) {
            return Err("launch-path");
        }
        if rest.last().map(String::as_str) != Some(self.session.as_str())
            || !rest.iter().any(|arg| arg == "resume")
        {
            return Err("no-resume");
        }
        if rest.iter().any(|arg| has_unsafe_line_char(arg)) {
            return Err("foreign-argv");
        }
        let expected =
            codex_alab::rewrite_argv(&self.argv, Some(&self.session)).map_err(|_| "live-argv")?;
        if rest != expected.as_slice() {
            return Err("foreign-argv");
        }
        if let Some(c) = self.cwd.as_deref()
            && (c.is_empty() || !is_structural_abs_path(c) || has_unsafe_line_char(c))
        {
            return Err("cwd");
        }
        if let Some(h) = self.codex_home.as_deref().filter(|s| !s.is_empty())
            && (has_unsafe_line_char(h) || (!is_structural_abs_path(h) && self.cwd.is_none()))
        {
            return Err("home-path");
        }
        if let Some(auth) = self.codex_auth_path.as_deref().filter(|s| !s.is_empty()) {
            let last_seg = auth
                .rsplit(['/', std::path::MAIN_SEPARATOR])
                .next()
                .unwrap_or("");
            let auth_path = Path::new(auth);
            if !is_structural_abs_path(auth)
                || matches!(last_seg, "" | "." | "..")
                || auth_path.is_dir()
                || has_unsafe_line_char(auth)
            {
                return Err("auth-path");
            }
        }
        Ok(())
    }
}

/// Validate `record`'s resume command against `pid` and `live` process facts.
///
/// # Errors
/// Returns the [`Refusal`] code explaining why the record cannot be relaunched.
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
        && !has_unsafe_line_char(launch)
        && (*launch == live.exec_path
            || image.as_deref() == Some(launch.as_str())
            || twin.as_deref() == Some(launch.as_str())
            || live
                .managed_launch
                .as_ref()
                .zip(live.image.as_deref())
                .is_some_and(|(hints, image)| hints.admits(pid, image, Path::new(launch)))))
    {
        return Err("launch-path");
    }
    if argv.last().map(String::as_str) != Some(session)
        || !argv.iter().skip(1).any(|arg| arg == "resume")
    {
        return Err("no-resume");
    }
    let expected = codex_alab::rewrite_argv(&live.argv, Some(session)).map_err(|_| "live-argv")?;
    if argv[1..] != expected[..] || argv[1..].iter().any(|arg| has_unsafe_line_char(arg)) {
        return Err("foreign-argv");
    }
    let same = |named: &str, running: &Path| {
        Path::new(named) == running || std::fs::canonicalize(named).is_ok_and(|c| c == running)
    };
    let cwd = match (record.cwd.as_deref(), live.cwd.as_deref()) {
        (Some(named), Some(running)) if !same(named, running) => return Err("cwd"),
        (Some(_), None) => return Err("cwd"),
        (_, running) => running.map(|p| p.to_string_lossy().into_owned()),
    };
    let cwd = cwd.filter(|c| Path::new(c).is_absolute() && !has_unsafe_line_char(c));
    for (name, value) in &record.env {
        let matches_live = match name.as_str() {
            codex_alab::HOME_ENV => live.codex_home.as_deref() == Some(value.as_str()),
            codex_alab::AUTH_PATH_ENV => live.codex_auth_path.as_deref() == Some(value.as_str()),
            _ => false,
        };
        if !matches_live {
            return Err("env");
        }
    }
    if let Some(h) = live.codex_home.as_deref().filter(|s| !s.is_empty())
        && !Path::new(h).is_absolute()
        && cwd.is_none()
    {
        return Err("home-path");
    }
    if let Some(auth) = live.codex_auth_path.as_deref().filter(|s| !s.is_empty())
        && !Path::new(auth).is_absolute()
    {
        return Err("auth-path");
    }
    let resume = Resume {
        argv: argv.clone(),
        session: session.to_string(),
        codex_home: live.codex_home.clone(),
        codex_auth_path: live.codex_auth_path.clone(),
        cwd,
    };
    resume.validate_structural()?;
    Ok(resume)
}

/// Outcome of reading a live process's session record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// Record file existed and parsed.
    Present(Record),
    /// Record file was previously present for this identity and has been removed.
    Removed,
    /// Record file has not yet been observed for this identity.
    Never,
    /// Record file existed but failed to parse.
    Unreadable(Refusal),
}

/// Host-side snapshot of a live process's session record ([`read_copy`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copy {
    /// Process ID and kernel start timestamp.
    pub pid: u32,
    pub start: String,
    /// Canonical `CODEX_HOME` directory where the record was read.
    pub home: PathBuf,
    /// Observed state of the record file.
    pub seen: Seen,
    /// Validated [`Resume`] or [`Refusal`] from the read.
    pub resume: Result<Resume, Refusal>,
}

/// Path to `<home>/alab/sessions/<pid>.json`.
#[must_use]
pub fn record_path(home: &Path, pid: u32) -> PathBuf {
    home.join("alab")
        .join("sessions")
        .join(format!("{pid}.json"))
}

/// The `$CODEX_HOME` a process started with `codex_home`/`home` in `cwd` uses:
/// `CODEX_HOME` (resolved against `cwd` when relative), else `$HOME/.codex`; canonical where it exists.
fn codex_home_of(
    codex_home: Option<&str>,
    cwd: Option<&Path>,
    home: Option<&str>,
) -> Option<PathBuf> {
    let dir = match codex_home.filter(|v| !v.is_empty()) {
        Some(set) if Path::new(set).is_absolute() => PathBuf::from(set),
        Some(relative) => cwd.filter(|c| c.is_absolute())?.join(relative),
        None => home
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|h| h.join(".codex"))?,
    };
    Some(std::fs::canonicalize(&dir).unwrap_or(dir))
}

/// atpkg's managed Codex twin, where one is configured and the build it execs
/// is ALab's ([`codex_alab::alab_bytes`]).
fn managed_alab_twin() -> Option<PathBuf> {
    alab_twin(
        atpkg::store::resolve_configured()?
            .agents_dir()
            .join("codex"),
    )
}

/// The atpkg twin `twin`, where the build it execs is ALab's ([`codex_alab::alab_bytes`]).
pub(crate) fn alab_twin(twin: PathBuf) -> Option<PathBuf> {
    let exe = super::upgrade_catalog::twin_binary(&std::fs::read_to_string(&twin).ok()?)?;
    codex_alab::alab_bytes(&exe).then_some(twin)
}

/// Build a [`struct@Copy`] from raw record file text `raw` and `live` process facts.
#[must_use]
pub fn copy_of(
    pid: u32,
    start: &str,
    home: PathBuf,
    text: Option<&str>,
    live: &Live,
    typed: Option<&Resume>,
) -> Copy {
    let seen = match text.map(parse) {
        None => Seen::Never,
        Some(Ok(record)) => Seen::Present(record),
        Some(Err(why)) => Seen::Unreadable(why),
    };
    copy_seen(pid, start, home, seen, live, typed)
}

/// Inherit the previously typed [`Resume`] when a relaunched process has not yet materialized its rollout.
///
/// # Errors
/// Returns `no-resume` if `typed` does not match `live`.
pub fn inherited(typed: Option<&Resume>, pid: u32, live: &Live) -> Result<Resume, Refusal> {
    let typed = typed.ok_or("no-record-yet")?;
    if codex_alab::resumed_thread(&live.argv).as_deref() != Some(typed.session.as_str()) {
        return Err("not-the-typed-line");
    }
    let record = Record {
        pid,
        session_id: Some(typed.session.clone()),
        resume_argv: Some(typed.argv.clone()),
        env: typed
            .codex_home
            .iter()
            .map(|h| (codex_alab::HOME_ENV.to_string(), h.clone()))
            .chain(
                typed
                    .codex_auth_path
                    .iter()
                    .map(|p| (codex_alab::AUTH_PATH_ENV.to_string(), p.clone())),
            )
            .collect(),
        status: None,
        exit: None,
        cwd: typed.cwd.clone(),
        draft_empty: None,
        frame: None,
        goal: None,
        initializing: false,
    };
    validate(&record, pid, live)
}

/// Fold a new live `read` over the previously retained `before` copy.
#[must_use]
pub fn fold(before: Option<&Copy>, mut read: Copy) -> Copy {
    let seen_before = before.is_some_and(|b| {
        (b.pid, b.start.as_str()) == (read.pid, read.start.as_str()) && b.seen != Seen::Never
    });
    if read.seen == Seen::Never && seen_before {
        read.seen = Seen::Removed;
        read.resume = Err("no-record");
    }
    if matches!(&read.seen, Seen::Present(record) if record.initializing)
        && before.is_some_and(|b| {
            (b.pid, b.start.as_str()) == (read.pid, read.start.as_str())
                && !matches!(&b.seen, Seen::Never)
                && !matches!(&b.seen, Seen::Present(record) if record.initializing)
        })
    {
        read.seen = Seen::Unreadable("record-regressed");
        read.resume = Err("record-regressed");
    }
    read
}

fn copy_seen(
    pid: u32,
    start: &str,
    home: PathBuf,
    seen: Seen,
    live: &Live,
    typed: Option<&Resume>,
) -> Copy {
    let resume = match &seen {
        Seen::Present(record) if record.initializing => {
            inherited(typed, pid, live).and_then(|resume| {
                let record = Record {
                    session_id: Some(resume.session),
                    resume_argv: Some(resume.argv),
                    ..record.clone()
                };
                validate(&record, pid, live)
            })
        }
        Seen::Present(record) => validate(record, pid, live),
        Seen::Never => inherited(typed, pid, live),
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
        Ok(None) => Seen::Never,
        Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => {
            Seen::Unreadable("record-too-large")
        }
        Err(_) => Seen::Unreadable("record-unreadable"),
    }
}

/// Read the active goal published in `<home>/alab/sessions/<pid>.json` for live `pid` and `thread`.
#[must_use]
pub fn read_record_goal(home: &Path, pid: u32, thread: &str) -> Option<String> {
    match read_record(&record_path(home, pid)) {
        Seen::Present(record)
            if record.pid == pid
                && record.exit.is_none()
                && record.session_id.as_deref() == Some(thread) =>
        {
            record.goal
        }
        _ => None,
    }
}

/// Read and validate the live session record for `(pid, start)` while verifying process identity.
#[must_use]
pub fn read_copy(pid: u32, start: &str, typed: Option<&Resume>) -> Option<Copy> {
    use super::restart_stamp::alive_as;
    if !alive_as(pid, start) {
        return None;
    }
    let (home, mut live) = live_of(pid)?;
    live.twin = managed_alab_twin();
    let seen = read_record(&record_path(&home, pid));
    if !alive_as(pid, start) {
        return None;
    }
    Some(copy_seen(pid, start, home, seen, &live, typed))
}

/// Read the record of live process `(pid, start)` under `home` now, with no resume line
/// validated: what a READ-ONLY copy takes (aterm's rollout tap, with no claim standing). The
/// identity is looked at before and after the read, as [`read_copy`] does: `None` when it was
/// not live across it.
#[must_use]
pub fn read_seen(home: &Path, pid: u32, start: &str) -> Option<Seen> {
    use super::restart_stamp::alive_as;
    if !alive_as(pid, start) {
        return None;
    }
    let seen = read_record(&record_path(home, pid));
    alive_as(pid, start).then_some(seen)
}

/// Exit classification derived from the last live [`struct@Copy`] ([`verdict`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Orderly exit that should not be relaunched.
    Orderly(Refusal),
    /// Abnormal termination eligible for relaunch.
    Crash,
}

/// Decide whether an exited process with last live `copy` crashed or ended orderly.
#[must_use]
pub fn verdict(copy: &Copy, asked: bool) -> Verdict {
    let record = match &copy.seen {
        Seen::Removed => return Verdict::Orderly("removed"),
        Seen::Never => return Verdict::Crash,
        Seen::Unreadable(why) => return Verdict::Orderly(why),
        Seen::Present(record) => record,
    };
    match record.exit.as_deref() {
        Some(word) if word.starts_with("signal:") => {
            if asked {
                Verdict::Crash
            } else {
                Verdict::Orderly("signal-not-aterms")
            }
        }
        _ if record.status.as_deref() == Some("upgrading") => Verdict::Crash,
        Some(_) => Verdict::Orderly("exited"),
        None if record.status.as_deref() == Some("exited") => Verdict::Orderly("exited"),
        None => Verdict::Crash,
    }
}

/// Whether `copy` indicates the agent is already shutting down or has exited.
#[must_use]
pub fn shows_end(copy: &Copy) -> bool {
    match &copy.seen {
        Seen::Removed => true,
        Seen::Never | Seen::Unreadable(_) => false,
        Seen::Present(record) => {
            record.exit.is_some() || record.status.as_deref() == Some("exited")
        }
    }
}

/// Persistent ledger of `(pid, start)` witnesses already relaunched.
pub struct Witnesses {
    path: PathBuf,
}

impl Witnesses {
    /// Open the witnesses ledger under harness state root `state`.
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

    /// Whether witness `(pid, start)` has already been recorded.
    #[must_use]
    pub fn handled(&self, pid: u32, start: &str) -> bool {
        let want = Self::line(pid, start);
        std::fs::read_to_string(&self.path).is_ok_and(|text| text.lines().any(|l| l == want))
    }

    /// Append witness `(pid, start)` to the ledger file.
    ///
    /// # Errors
    /// Returns an I/O error if creating the directory or appending fails.
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
