// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What Codex's own records say of a session: how much of its usage window is
//! spent, and whether its thread still runs outside the sandbox it was launched
//! out of. Read from the rollouts under the TUI's `$CODEX_HOME`
//! (`sessions/<y>/<m>/<d>/rollout-*.jsonl`), never the screen.
//!
//! Usage ([`rollout_rate_limits`], [`read_limits`]): every `token_count` event
//! carries the account's `rate_limits` per `limit_id` (`{primary, secondary}`,
//! each `{used_percent, window_minutes, resets_at}`), so the newest rollout
//! carries them. A reading is fresh for [`FRESH_S`]; a passed `resets_at` is
//! reset; near is some window at [`NEAR_PERCENT`] or more.
//!
//! Sandbox ([`sandbox_fell`]): every turn writes a `turn_context` with its
//! `sandbox_policy.type`. A thread launched bypassing the sandbox
//! ([`launch_bypasses`]) whose turns now run sandboxed (as after the app-server
//! daemon restarts onto a new build) can neither commit nor push, so nothing is
//! typed into it. The session's own thread decides when known
//! ([`own_thread`]); otherwise every rollout written within [`RECENT_S`] whose
//! last turn ran in the session's folder must agree, and any unreadable
//! candidate is no verdict. The last `turn_context` is found by reading
//! backward in [`CONTEXT_CHUNK`] steps up to [`CONTEXT_REACH`]
//! ([`last_turn_context`]).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aterm_json::Value;

/// The share of a usage window, in percent, at or above which it is near its
/// limit.
pub const NEAR_PERCENT: f64 = 90.0;

/// How old a usage reading may be and still be read: Codex writes one only
/// when some turn runs, so an idle account's last reading ages.
pub const FRESH_S: i64 = 6 * 3600;

/// How recently a rollout must have been written to be a candidate for the
/// session's own thread at a turn end ([`sandbox_fell`]).
pub const RECENT_S: u64 = 180;

/// How much of a rollout's end is read for its usage reading: rollouts reach
/// gigabytes, and a `token_count` is written after every model response.
pub const TAIL_BYTES: u64 = 512 * 1024;

/// The step a rollout is read backward in for its last `turn_context`
/// ([`last_turn_context`]).
pub const CONTEXT_CHUNK: u64 = 1024 * 1024;

/// How far back from a rollout's end its last `turn_context` is looked for
/// ([`last_turn_context`]): past it, the candidate gives no verdict.
pub const CONTEXT_REACH: u64 = 64 * 1024 * 1024;

/// How many of the newest rollouts are read for a usage reading before the
/// search gives up.
const NEWEST_READ: usize = 6;

/// One usage window of the `codex` limit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    /// Epoch seconds.
    pub resets_at: Option<i64>,
}

/// The `codex` limit's windows as one `token_count` recorded them.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexLimits {
    /// When the reading was written, epoch seconds.
    pub at_unix: i64,
    pub windows: Vec<Window>,
}

/// What the usage window says, for the rate-limit nudge and the hold after a
/// save-then-wait switch. `used` is the highest live window's share in whole
/// percent (rounded down), for the words; the verdict itself is taken on the
/// recorded value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitRead {
    /// Some window at [`NEAR_PERCENT`] or more: `back_at` the latest reset
    /// among the near windows (epoch seconds).
    Near { used: u16, back_at: Option<i64> },
    /// Every window below it (a reset window counts as empty).
    Far { used: u16 },
    /// No reading, a stale one, or none could be read — why.
    Unknown(&'static str),
}

impl LimitRead {
    /// Whether the window reads near its limit.
    #[must_use]
    pub fn near(&self) -> bool {
        matches!(self, LimitRead::Near { .. })
    }

    /// The reading in a person's words.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            LimitRead::Near { used, .. } => format!("Codex's usage window reads {used}% used"),
            LimitRead::Far { used } => {
                format!("Codex's usage window reads {used}% used, far from its limit")
            }
            LimitRead::Unknown(why) => format!("Codex's usage window is unread ({why})"),
        }
    }
}

impl Default for LimitRead {
    fn default() -> Self {
        LimitRead::Unknown("not read")
    }
}

fn event(line: &str) -> Option<(i64, Value)> {
    let v: Value = aterm_json::from_str(line).ok()?;
    let at = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(aterm_types::rfc3339::parse_utc_fractional)?;
    Some((at, v))
}

fn window(v: Option<&Value>) -> Option<Window> {
    let v = v?;
    Some(Window {
        used_percent: v.get("used_percent")?.as_f64()?,
        window_minutes: v.get("window_minutes").and_then(Value::as_u64),
        resets_at: v.get("resets_at").and_then(Value::as_i64),
    })
}

/// The last `token_count` in `tail` whose `rate_limits.limit_id` is `codex`,
/// with at least one window (the `premium` rows' windows are null, and
/// `codex_bengalfox` is another model's limit). A line that is not JSON — the
/// tail's cut first line — is skipped.
#[must_use]
pub fn rollout_rate_limits(tail: &str) -> Option<CodexLimits> {
    let mut last = None;
    for line in tail.lines() {
        if !line.contains("\"token_count\"") || !line.contains("\"rate_limits\"") {
            continue;
        }
        let Some((at, v)) = event(line) else {
            continue;
        };
        let Some(limits) = v.get("payload").and_then(|p| p.get("rate_limits")) else {
            continue;
        };
        if limits.get("limit_id").and_then(Value::as_str) != Some("codex") {
            continue;
        }
        let windows: Vec<Window> = [limits.get("primary"), limits.get("secondary")]
            .into_iter()
            .filter_map(window)
            .collect();
        if !windows.is_empty() {
            last = Some(CodexLimits {
                at_unix: at,
                windows,
            });
        }
    }
    last
}

/// The reading's verdict at `now_unix` (module header): stale past
/// [`FRESH_S`] → unknown; a window whose `resets_at` has passed counts as
/// empty; some window at [`NEAR_PERCENT`] or more → near, back at the latest
/// reset among those; else far.
#[must_use]
pub fn read_limits(l: Option<&CodexLimits>, now_unix: i64) -> LimitRead {
    let Some(l) = l else {
        return LimitRead::Unknown("no reading");
    };
    if now_unix.saturating_sub(l.at_unix) > FRESH_S {
        return LimitRead::Unknown("the last reading is more than 6 hours old");
    }
    let live = |w: &&Window| w.resets_at.is_none_or(|r| r > now_unix);
    let used = |w: &Window| w.used_percent;
    let near: Vec<&Window> = l
        .windows
        .iter()
        .filter(live)
        .filter(|w| w.used_percent >= NEAR_PERCENT)
        .collect();
    let highest = l
        .windows
        .iter()
        .filter(live)
        .map(used)
        .fold(0.0_f64, f64::max);
    // A share is 0..=100; out of that range it is read as the range's end.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let highest = highest.clamp(0.0, 100.0).floor() as u16;
    if near.is_empty() {
        return LimitRead::Far { used: highest };
    }
    LimitRead::Near {
        used: highest,
        back_at: near.iter().filter_map(|w| w.resets_at).max(),
    }
}

/// Whether a Codex TUI's argv runs it outside the sandbox: the bypass flag,
/// or `-s` / `--sandbox` `danger-full-access`.
#[must_use]
pub fn launch_bypasses(argv: &[String]) -> bool {
    argv.iter().enumerate().any(|(i, a)| {
        a == "--dangerously-bypass-approvals-and-sandbox"
            || a == "--sandbox=danger-full-access"
            || (matches!(a.as_str(), "-s" | "--sandbox")
                && argv.get(i + 1).is_some_and(|v| v == "danger-full-access"))
    })
}

/// A turn's recorded folder, sandbox and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnContext {
    pub cwd: String,
    /// `sandbox_policy.type`: `danger-full-access`, `workspace-write`,
    /// `read-only`, …
    pub sandbox: String,
    /// `model`: the slug the turn ran on (e.g. `gpt-6-astra`, beside
    /// `effort`), `None` where the line names none.
    pub model: Option<String>,
    /// The line's `timestamp` (epoch seconds): when the turn began, `None`
    /// where it names none.
    pub at_unix: Option<i64>,
}

/// One rollout line read as a `turn_context`, `None` for any other line.
fn turn_context_line(line: &str) -> Option<TurnContext> {
    if !line.contains("\"turn_context\"") {
        return None;
    }
    let v = crate::harness::transcript::codex_metadata(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("turn_context") {
        return None;
    }
    let p = v.get("payload")?;
    Some(TurnContext {
        cwd: p.get("cwd").and_then(Value::as_str)?.to_string(),
        sandbox: p
            .get("sandbox_policy")
            .and_then(|s| s.get("type"))
            .and_then(Value::as_str)?
            .to_string(),
        model: p.get("model").and_then(Value::as_str).map(str::to_string),
        at_unix: v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(aterm_types::rfc3339::parse_utc_fractional),
    })
}

/// The last `turn_context` in `tail`.
#[must_use]
pub fn rollout_turn_context(tail: &str) -> Option<TurnContext> {
    tail.lines().rev().find_map(turn_context_line)
}

/// The last `turn_context` of the rollout at `path`, read backward from its
/// end in [`CONTEXT_CHUNK`] steps until one is found, at most `reach` bytes
/// from the end (module header). A line cut by a step's start is carried to
/// the next step and read whole there; a file that cannot be read, or whose
/// last `turn_context` lies further back than `reach`, gives `None`.
#[must_use]
pub fn last_turn_context(path: &Path, reach: u64) -> Option<TurnContext> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    crate::harness::upgrade_codex::read_back(&mut file, len, CONTEXT_CHUNK, reach, |text| {
        text.lines().rev().find_map(turn_context_line)
    })
}

/// The fall (module header): the sandbox every candidate thread now runs in,
/// when the launch bypassed it and every recent rollout whose last turn ran in
/// `cwd` records a sandbox. `None` when the launch did not bypass, a
/// candidate's `turn_context` could not be read, no candidate ran in `cwd`, or
/// any one of them runs outside the sandbox.
#[must_use]
pub fn sandbox_fell(
    recent: &[Option<TurnContext>],
    cwd: &str,
    launch_bypass: bool,
) -> Option<String> {
    if !launch_bypass || recent.iter().any(Option::is_none) {
        return None;
    }
    let mine: Vec<&TurnContext> = recent
        .iter()
        .flatten()
        .filter(|t| same_dir(&t.cwd, cwd))
        .collect();
    let first = mine.first()?;
    mine.iter()
        .all(|t| t.sandbox != "danger-full-access")
        .then(|| first.sandbox.clone())
}

fn same_dir(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The session's own thread (module header): the one whose writer lock
/// (`<home>/thread-writer-locks/<thread>.lock`) is among the files the Codex
/// TUI holds open (`open`, where readable), else the thread its argv resumes
/// (`codex resume … <thread>`). `None`: neither says (a fresh daemon-mode
/// launch), or the TUI holds more than one lock. The locks' folder and each
/// open file are compared resolved (`lsof` prints resolved paths, `/tmp/…` as
/// `/private/tmp/…`), as the Codex upgrade's reader does.
#[must_use]
pub fn own_thread(argv: &[String], open: Option<&[PathBuf]>, home: &Path) -> Option<String> {
    use crate::harness::upgrade_codex::{is_thread_id, lock_thread};
    let resolved = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let locks = resolved(&home.join("thread-writer-locks"));
    if let Some(files) = open {
        let held: Vec<&str> = files
            .iter()
            .filter(|f| f.parent().is_some_and(|d| resolved(d) == locks))
            .filter_map(|f| lock_thread(f.file_name()?.to_str()?))
            .collect();
        match held.as_slice() {
            [thread] => return Some((*thread).to_string()),
            [] => {}
            _ => return None,
        }
    }
    let resume = argv.iter().position(|a| a == "resume")?;
    argv[resume + 1..].iter().find(|a| is_thread_id(a)).cloned()
}

/// The longest the `lsof` of [`open_files`] may run: it runs on the
/// supervising loop's thread, and a wedged filesystem must not hold the loop.
pub const LSOF_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// The files the processes of process group `pgid` hold open — the
/// session's foreground job: a launcher and the Codex TUI it started are one
/// group, and the TUI is the one holding its thread's lock — by `lsof` (the
/// Codex upgrade's own reader, `-g` for `-p`), bounded by [`LSOF_BOUND`]
/// (killed past it): `None` when they cannot be read.
#[must_use]
pub fn open_files(pgid: u32) -> Option<Vec<PathBuf>> {
    let mut cmd = aterm_uds::spawnfd::command("lsof");
    cmd.args(["-n", "-P", "-w", "-g", &pgid.to_string(), "-Fn"]);
    let (ok, stdout) = output_within(cmd, LSOF_BOUND)?;
    // lsof exits 1 for a group it found nothing for: an unreadable answer.
    if !ok {
        return None;
    }
    Some(
        String::from_utf8_lossy(&stdout)
            .lines()
            .filter_map(|l| l.strip_prefix('n').map(PathBuf::from))
            .collect(),
    )
}

/// `cmd`'s success and standard output, waited for at most `bound`: a child
/// still running then is killed (`SIGKILL`) and reaped, and reads `None`, as
/// does one that cannot be started.
fn output_within(
    mut cmd: aterm_uds::spawnfd::Command,
    bound: std::time::Duration,
) -> Option<(bool, Vec<u8>)> {
    use aterm_uds::spawnfd::Stdio;
    use std::io::Read as _;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("codex-lsof".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            let _ = tx.send(buf);
        })
        .ok()?;
    let deadline = std::time::Instant::now() + bound;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    // Its output drained: the child has exited, so the pipe closes at once.
    let drain = deadline
        .saturating_duration_since(std::time::Instant::now())
        .max(std::time::Duration::from_millis(500));
    let buf = rx.recv_timeout(drain).ok()?;
    Some((status.success(), buf))
}

/// The `$CODEX_HOME` a Codex started with `env` uses: `CODEX_HOME`, else
/// `$HOME/.codex` (the Codex upgrade's own rule). A `CODEX_HOME` that is set
/// but not absolute names no home aterm can place — never `$HOME/.codex`,
/// which may be another Codex's records — so nothing is read (`None`).
#[must_use]
pub fn codex_home(codex_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    match codex_home {
        Some(set) => Some(PathBuf::from(set)).filter(|p| p.is_absolute()),
        None => home
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|h| h.join(".codex")),
    }
}

/// Every rollout under `home`'s `sessions/<y>/<m>/<d>/`, newest write first.
/// A thread's rollout sits in the folder of the day it started, so the one
/// written last may be in an old day's folder.
#[must_use]
pub fn rollouts_by_write(home: &Path) -> Vec<(PathBuf, SystemTime)> {
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for year in dirs(&home.join("sessions")) {
        for month in dirs(&year) {
            for day in dirs(&month) {
                let Ok(entries) = std::fs::read_dir(&day) else {
                    continue;
                };
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    if let Ok(at) = e.metadata().and_then(|m| m.modified()) {
                        out.push((e.path(), at));
                    }
                }
            }
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.1));
    out
}

/// The last `bytes` of `path`, its cut first line dropped.
#[must_use]
pub fn tail_of(path: &Path, bytes: u64) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(bytes);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.take(bytes).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if from > 0 {
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_string())
    } else {
        text
    })
}

/// What a look at Codex's records found for one session.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CodexSeen {
    pub limits: LimitRead,
    /// The sandbox the session's thread fell into from its launch's bypass
    /// ([`sandbox_fell`]).
    pub sandbox_fell: Option<String>,
    /// The model the session's own thread ran its last turn on (its
    /// rollout's last `turn_context`), where the thread is known
    /// ([`own_thread`]).
    pub thread_model: Option<String>,
    /// When that turn began (the `turn_context`'s `timestamp`, epoch
    /// seconds): [`model_since`] counts it only for a turn begun since a
    /// save-then-wait switch was pressed.
    pub thread_model_at: Option<i64>,
}

/// How far before a save-then-wait switch's press a turn may seem to begin
/// and still count as begun since it ([`model_since`]): the `turn_context`'s
/// time is cut to its whole second and the press's rounded to the nearest,
/// so a turn Codex began in the press's own second can read as a second
/// earlier.
pub const MODEL_SINCE_SLACK_S: i64 = 1;

/// The thread's model as a save-then-wait switch pressed at `pressed_unix` may
/// read it: the own thread's last turn's model only when that turn began since
/// the press (within [`MODEL_SINCE_SLACK_S`]). The goal turn Codex runs on the
/// cheaper model under the nudge's box, begun between the press and the box
/// leaving, is the evidence the owed footer gate waits on; a turn begun before
/// the press says nothing of where the thread is now. No press known: the model
/// as read; no time on the turn: none.
#[must_use]
pub fn model_since(seen: &CodexSeen, pressed_unix: Option<i64>) -> Option<String> {
    match pressed_unix {
        None => seen.thread_model.clone(),
        Some(p) => seen.thread_model.clone().filter(|_| {
            seen.thread_model_at
                .is_some_and(|at| at.saturating_add(MODEL_SINCE_SLACK_S) >= p)
        }),
    }
}

/// The session a look is for: its folder, its TUI's argv, its own thread
/// where known ([`own_thread`]), and when it last worked (the loop's last
/// busy read; `now` when none was seen).
#[derive(Debug, Clone, Copy)]
pub struct Whose<'a> {
    pub cwd: Option<&'a str>,
    pub argv: &'a [String],
    pub thread: Option<&'a str>,
    pub active_at: SystemTime,
}

/// One look at `home`'s rollouts for `who` at `now` (module header): the usage
/// window from the newest rollouts carrying a reading; the sandbox verdict and
/// the thread's model from the session's own thread's rollout where known, else
/// over those written within [`RECENT_S`] of its last work, each by its last
/// `turn_context` ([`last_turn_context`], up to [`CONTEXT_REACH`] back).
#[must_use]
pub fn look(home: &Path, who: Whose<'_>, now: SystemTime) -> CodexSeen {
    look_within(home, who, now, CONTEXT_REACH)
}

/// [`look`], each candidate's `turn_context` looked for as far back as
/// `reach` (a test's smaller reach).
fn look_within(home: &Path, who: Whose<'_>, now: SystemTime, reach: u64) -> CodexSeen {
    let now_unix = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let files = rollouts_by_write(home);
    let bypass = launch_bypasses(who.argv);
    // Written within RECENT_S of the session's last work, either side.
    let near_work = |at: &SystemTime| {
        let apart = who
            .active_at
            .duration_since(*at)
            .or_else(|_| at.duration_since(who.active_at))
            .map_or(0, |d| d.as_secs());
        apart <= RECENT_S
    };
    let before_work = |at: &SystemTime| {
        who.active_at
            .duration_since(*at)
            .is_ok_and(|d| d.as_secs() > RECENT_S)
    };
    let mut reading: Option<CodexLimits> = None;
    let mut recent = Vec::new();
    for (i, (path, at)) in files.iter().enumerate() {
        // Newest first: past the session's last work, nothing more is a
        // candidate.
        if i >= NEWEST_READ && (who.thread.is_some() || before_work(at)) {
            break;
        }
        if i < NEWEST_READ
            && let Some(tail) = tail_of(path, TAIL_BYTES)
            && let Some(l) = rollout_rate_limits(&tail)
            && reading.as_ref().is_none_or(|r| l.at_unix > r.at_unix)
        {
            reading = Some(l);
        }
        if who.thread.is_none() && near_work(at) {
            recent.push(last_turn_context(path, reach));
        }
    }
    let (sandbox_fell, thread_model, thread_model_at) = match who.thread {
        // The session's own thread decides alone.
        Some(thread) => {
            let own = files
                .iter()
                .find(|(p, _)| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| crate::harness::upgrade_codex::is_rollout_of(n, thread))
                })
                .and_then(|(p, _)| last_turn_context(p, reach));
            let fell = own
                .as_ref()
                .filter(|t| bypass && t.sandbox != "danger-full-access")
                .map(|t| t.sandbox.clone());
            let at = own.as_ref().and_then(|t| t.at_unix);
            (fell, own.and_then(|t| t.model), at)
        }
        None => (
            who.cwd.and_then(|c| sandbox_fell(&recent, c, bypass)),
            None,
            None,
        ),
    };
    CodexSeen {
        limits: read_limits(reading.as_ref(), now_unix),
        sandbox_fell,
        thread_model,
        thread_model_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `token_count` line (99% of the weekly window), its numbers set.
    fn token_count(at: &str, limit_id: &str, used: f64, resets_at: i64) -> String {
        format!(
            "{{\"timestamp\":\"{at}\",\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\
             \"token_count\",\"info\":null,\"rate_limits\":{{\"limit_id\":\"{limit_id}\",\
             \"limit_name\":null,\"primary\":{{\"used_percent\":{used},\"window_minutes\":10080,\
             \"resets_at\":{resets_at}}},\"secondary\":null,\"credits\":{{\"has_credits\":false,\
             \"unlimited\":false,\"balance\":\"0\"}},\"plan_type\":\"pro\",\
             \"rate_limit_reached_type\":null}}}}}}"
        )
    }

    const PREMIUM: &str = "{\"timestamp\":\"2026-09-27T20:45:24.900Z\",\"type\":\"event_msg\",\
        \"payload\":{\"type\":\"token_count\",\"info\":null,\"rate_limits\":{\"limit_id\":\
        \"premium\",\"primary\":null,\"secondary\":null}}}";

    fn unix(s: &str) -> i64 {
        aterm_types::rfc3339::parse_utc_fractional(s).expect("a stamp")
    }

    /// Recorded readings: 99% of the weekly window reads near, and back at its
    /// reset; 1% twenty minutes after a usage reset reads far. `codex_bengalfox`
    /// and `premium` rows are ignored; the last `codex` row wins. Negative
    /// controls: a reading seven hours old is unknown; one whose reset has
    /// passed is far; no reading is unknown.
    #[test]
    fn the_incidents_readings_read_near_then_far() {
        let first = [
            token_count("2026-09-27T20:30:00.000Z", "codex", 97.0, 1_791_082_750),
            token_count("2026-09-27T20:37:17.251Z", "codex", 99.0, 1_791_082_750),
            token_count(
                "2026-09-27T20:38:00.000Z",
                "codex_bengalfox",
                3.0,
                1_791_082_999,
            ),
            PREMIUM.to_string(),
        ]
        .join("\n");
        let l = rollout_rate_limits(&first).expect("a reading");
        assert_eq!(l.at_unix, unix("2026-09-27T20:37:17.251Z"));
        assert_eq!(l.windows[0].used_percent, 99.0);
        assert_eq!(
            read_limits(Some(&l), unix("2026-09-27T20:39:47Z")),
            LimitRead::Near {
                used: 99,
                back_at: Some(1_791_082_750)
            }
        );
        let second = token_count("2026-09-28T03:29:30.000Z", "codex", 1.0, 1_791_169_817);
        let l2 = rollout_rate_limits(&second).expect("a reading");
        let at_press = unix("2026-09-28T03:30:24Z");
        assert_eq!(read_limits(Some(&l2), at_press), LimitRead::Far { used: 1 });
        // Stale: seven hours on.
        assert!(matches!(
            read_limits(Some(&l2), at_press + 7 * 3600),
            LimitRead::Unknown(_)
        ));
        // The window's reset passed: it counts as empty.
        assert_eq!(
            read_limits(Some(&l), 1_791_082_751),
            LimitRead::Unknown("the last reading is more than 6 hours old"),
            "a reset days on is also stale"
        );
        let just_reset = CodexLimits {
            at_unix: 1_791_082_700,
            windows: l.windows.clone(),
        };
        assert_eq!(
            read_limits(Some(&just_reset), 1_791_082_760),
            LimitRead::Far { used: 0 }
        );
        assert!(matches!(read_limits(None, at_press), LimitRead::Unknown(_)));
        // Exactly the threshold is near; just under is far.
        let at = |used: f64| CodexLimits {
            at_unix: at_press,
            windows: vec![Window {
                used_percent: used,
                window_minutes: Some(10080),
                resets_at: Some(at_press + 3600),
            }],
        };
        assert!(read_limits(Some(&at(90.0)), at_press).near());
        assert!(!read_limits(Some(&at(89.9)), at_press).near());
        assert_eq!(rollout_rate_limits(PREMIUM), None);
        assert_eq!(rollout_rate_limits("not json\n{\"cut"), None);
    }

    /// The session a test's look is for.
    fn whose<'a>(
        cwd: Option<&'a str>,
        argv: &'a [String],
        thread: Option<&'a str>,
        active_at: SystemTime,
    ) -> Whose<'a> {
        Whose {
            cwd,
            argv,
            thread,
            active_at,
        }
    }

    fn turn_context(at: &str, cwd: &str, sandbox: &str) -> String {
        format!(
            "{{\"timestamp\":\"{at}\",\"ordinal\":550412,\"type\":\"turn_context\",\"payload\":{{\
             \"turn_id\":\"t\",\"cwd\":\"{cwd}\",\"approval_policy\":\"never\",\
             \"sandbox_policy\":{{\"type\":\"{sandbox}\",\"network_access\":false}},\
             \"model\":\"gpt-6-luna\",\"effort\":\"medium\"}}}}"
        )
    }

    /// The sandbox fall: a thread launched with the bypass whose last turn ran
    /// `workspace-write` has fallen; resumed with the bypass, its turns run
    /// `danger-full-access` again and it has not. Negative controls: a launch
    /// without the bypass never falls; another folder's thread says nothing of
    /// this one; any one candidate outside the sandbox is no fall.
    #[test]
    fn a_bypassed_launch_whose_turns_run_sandboxed_has_fallen() {
        let fell = turn_context("2026-09-28T06:05:47.331Z", "/w/pj", "workspace-write");
        let back = turn_context("2026-09-28T15:27:29.874Z", "/w/pj", "danger-full-access");
        let t_fell = rollout_turn_context(&format!("{back}\n{fell}")).expect("a context");
        assert_eq!(t_fell.sandbox, "workspace-write");
        let t_back = rollout_turn_context(&format!("{fell}\n{back}")).expect("a context");
        let bypass: Vec<String> = [
            "codex",
            "resume",
            "--dangerously-bypass-approvals-and-sandbox",
        ]
        .map(String::from)
        .to_vec();
        assert!(launch_bypasses(&bypass));
        assert!(launch_bypasses(
            &["codex", "-s", "danger-full-access"].map(String::from)
        ));
        assert!(!launch_bypasses(
            &["codex", "-s", "workspace-write"].map(String::from)
        ));
        let fell_ = Some(t_fell.clone());
        let back_ = Some(t_back.clone());
        assert_eq!(
            sandbox_fell(std::slice::from_ref(&fell_), "/w/pj/", true),
            Some("workspace-write".to_string())
        );
        assert_eq!(
            sandbox_fell(std::slice::from_ref(&back_), "/w/pj", true),
            None
        );
        assert_eq!(
            sandbox_fell(std::slice::from_ref(&fell_), "/w/pj", false),
            None
        );
        assert_eq!(
            sandbox_fell(std::slice::from_ref(&fell_), "/w/other", true),
            None
        );
        assert_eq!(sandbox_fell(&[fell_.clone(), back_], "/w/pj", true), None);
        assert_eq!(sandbox_fell(&[], "/w/pj", true), None);
        // A candidate whose `turn_context` could not be read may be this
        // session's own thread: no verdict.
        assert_eq!(sandbox_fell(&[fell_, None], "/w/pj", true), None);
    }

    /// The session's own thread decides however long its last turn: a rollout
    /// whose last `turn_context` sits more than [`TAIL_BYTES`] from its end is
    /// read backward to it across chunk steps (a line cut by a step's start read
    /// whole), and its `danger-full-access` vetoes a sandboxed neighbour in the
    /// same folder. Negative controls: past the reach the candidate gives no
    /// verdict (never the neighbour's alone); a fixed tail finds nothing there.
    #[test]
    fn a_turn_context_far_from_the_end_is_found_and_decides() {
        let home = std::env::temp_dir().join(format!("codex-usage-far-{}", std::process::id()));
        let day = home.join("sessions/2026/09/28");
        std::fs::create_dir_all(&day).expect("mkdir");
        let now = SystemTime::now();
        let now_unix = i64::try_from(
            now.duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock")
                .as_secs(),
        )
        .expect("fits");
        let stamp =
            aterm_types::rfc3339::format_rfc3339(u64::try_from(now_unix - 60).expect("after 1970"));
        // The session's own thread: its turn ran outside the sandbox, then
        // wrote 3 MB of items (output lines of 1.5 MB and small ones) — the
        // context 3 MB back, over two chunk steps, one line straddling one.
        let own_ctx = turn_context(&stamp, "/w/pj", "danger-full-access");
        let big = format!(
            "{{\"timestamp\":\"{stamp}\",\"type\":\"response_item\",\"payload\":\"{}\"}}",
            "x".repeat(1_500_000)
        );
        let small = format!(
            "{{\"timestamp\":\"{stamp}\",\"type\":\"event_msg\",\"payload\":\"{}\"}}",
            "y".repeat(1000)
        );
        let mut own = format!(
            "{}\n{own_ctx}\n",
            turn_context(&stamp, "/w/pj", "workspace-write")
        );
        own.push_str(&format!("{big}\n"));
        for _ in 0..40 {
            own.push_str(&format!("{small}\n"));
        }
        own.push_str(&format!("{big}\n"));
        let own_path = day.join("rollout-2026-08-25T22-21-24-own.jsonl");
        std::fs::write(&own_path, &own).expect("write");
        assert!(
            own.len() - own.find(&own_ctx).expect("the context") > 2 * TAIL_BYTES as usize,
            "the context is far from the end"
        );
        assert_eq!(
            rollout_turn_context(&tail_of(&own_path, TAIL_BYTES).expect("tail")),
            None,
            "the fixed tail finds nothing"
        );
        assert_eq!(
            last_turn_context(&own_path, CONTEXT_REACH).map(|t| t.sandbox),
            Some("danger-full-access".to_string())
        );
        // A neighbour in the same folder, sandboxed (a subagent, a second
        // tab launched without the bypass).
        std::fs::write(
            day.join("rollout-2026-09-28T08-00-00-sub.jsonl"),
            format!("{}\n", turn_context(&stamp, "/w/pj", "workspace-write")),
        )
        .expect("write");
        let argv = [
            "codex".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];
        let seen = look(&home, whose(Some("/w/pj"), &argv, None, now), now);
        assert_eq!(seen.sandbox_fell, None, "the session's own thread vetoes");
        // Past the reach: no verdict, never the neighbour's alone.
        let short = look_within(
            &home,
            whose(Some("/w/pj"), &argv, None, now),
            now,
            1024 * 1024,
        );
        assert_eq!(short.sandbox_fell, None);
        // The own thread fallen too: the fall is read.
        let mut fell = own.replace(&own_ctx, "");
        fell.insert_str(
            0,
            &format!("{}\n", turn_context(&stamp, "/w/pj", "danger-full-access")),
        );
        std::fs::write(
            &own_path,
            format!(
                "{fell}{}\n{big}\n",
                turn_context(&stamp, "/w/pj", "workspace-write")
            ),
        )
        .expect("write");
        let seen = look(&home, whose(Some("/w/pj"), &argv, None, now), now);
        assert_eq!(seen.sandbox_fell.as_deref(), Some("workspace-write"));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// One look over a home written the way Codex writes it: the newest
    /// reading wins wherever its rollout's folder is, and the fall is read
    /// off the rollouts written lately. Negative control: an empty home reads
    /// nothing.
    #[test]
    fn a_look_reads_the_newest_rollouts() {
        let home = std::env::temp_dir().join(format!("codex-usage-{}", std::process::id()));
        let old = home.join("sessions/2026/08/25");
        let new = home.join("sessions/2026/09/28");
        std::fs::create_dir_all(&old).expect("mkdir");
        std::fs::create_dir_all(&new).expect("mkdir");
        let now = SystemTime::now();
        let now_unix = i64::try_from(
            now.duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock")
                .as_secs(),
        )
        .expect("fits");
        let stamp =
            aterm_types::rfc3339::format_rfc3339(u64::try_from(now_unix - 60).expect("after 1970"));
        let bypass = [
            "codex".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];
        // The August thread, written now: near, sandboxed.
        std::fs::write(
            old.join("rollout-2026-08-25T22-21-24-a.jsonl"),
            format!(
                "{}\n{}\n",
                turn_context(&stamp, "/w/pj", "workspace-write"),
                token_count(&stamp, "codex", 95.0, now_unix + 3600)
            ),
        )
        .expect("write");
        let seen = look(&home, whose(Some("/w/pj"), &bypass, None, now), now);
        assert_eq!(
            seen.limits,
            LimitRead::Near {
                used: 95,
                back_at: Some(now_unix + 3600)
            }
        );
        assert_eq!(seen.sandbox_fell.as_deref(), Some("workspace-write"));
        // A newer thread of today in the same folder, outside the sandbox.
        std::fs::write(
            new.join("rollout-2026-09-28T08-00-00-b.jsonl"),
            format!("{}\n", turn_context(&stamp, "/w/pj", "danger-full-access")),
        )
        .expect("write");
        let seen = look(&home, whose(Some("/w/pj"), &bypass, None, now), now);
        assert_eq!(seen.sandbox_fell, None, "one candidate is outside it");
        assert!(seen.limits.near(), "the reading is still found");
        let _ = std::fs::remove_dir_all(&home);
        let empty = look(&home, whose(Some("/w/pj"), &[], None, now), now);
        assert!(matches!(empty.limits, LimitRead::Unknown(_)));
        assert_eq!(empty.sandbox_fell, None);
        assert_eq!(
            codex_home(None, Some("/Users//x")),
            Some(PathBuf::from("/Users//x/.codex"))
        );
        assert_eq!(
            codex_home(Some("/alt/home"), Some("/Users//x")),
            Some(PathBuf::from("/alt/home"))
        );
        assert_eq!(codex_home(Some("rel"), None), None);
        assert_eq!(
            codex_home(Some("rel"), Some("/Users//x")),
            None,
            "never another home's records"
        );
    }

    /// The session's own thread, named: the writer lock its TUI holds open,
    /// else the thread its argv resumes. Negative controls: two locks held name
    /// none; a lock outside the home's lock folder is none of its; a launch
    /// that resumes nothing names none.
    #[test]
    fn the_sessions_own_thread_is_its_lock_else_its_resume() {
        let home = PathBuf::from("/Users//x/.codex");
        let t1 = "01a0dc36-1dc1-7ee2-be91-11bc323e377c";
        let t2 = "01a0e8a1-4ca6-7d81-8cd1-6b9b60797925";
        let lock = |t: &str| home.join("thread-writer-locks").join(format!("{t}.lock"));
        let argv = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let plain = argv(&["codex", "--dangerously-bypass-approvals-and-sandbox"]);
        let resumed = argv(&[
            "codex",
            "resume",
            "--dangerously-bypass-approvals-and-sandbox",
            t2,
        ]);
        assert_eq!(
            own_thread(&plain, Some(&[lock(t1), PathBuf::from("/dev/null")]), &home).as_deref(),
            Some(t1)
        );
        assert_eq!(
            own_thread(&resumed, Some(&[lock(t1)]), &home).as_deref(),
            Some(t1),
            "the lock the kernel proves wins"
        );
        assert_eq!(own_thread(&resumed, Some(&[]), &home).as_deref(), Some(t2));
        assert_eq!(own_thread(&resumed, None, &home).as_deref(), Some(t2));
        assert_eq!(own_thread(&plain, Some(&[lock(t1), lock(t2)]), &home), None);
        assert_eq!(
            own_thread(
                &plain,
                Some(&[PathBuf::from(format!("/tmp/{t1}.lock"))]),
                &home
            ),
            None
        );
        assert_eq!(own_thread(&plain, None, &home), None);
        assert_eq!(own_thread(&argv(&["codex", t1]), None, &home), None);
    }

    /// A held session's own thread decides: at a hold's end the session's own
    /// rollout was last written days ago and a neighbour in the folder (a tab
    /// launched without the bypass, a sandboxed subagent) wrote a minute ago.
    /// With the own thread known, its last `turn_context` alone decides and
    /// names its model; not known, the candidates are those written around the
    /// session's last work, so the neighbour is none. Negative controls: the own
    /// thread's rollout missing gives no verdict; a daemon-mode session that
    /// just worked takes the neighbour as a candidate.
    #[test]
    fn a_held_sessions_own_thread_decides_never_a_neighbour() {
        let home = std::env::temp_dir().join(format!("codex-usage-own-{}", std::process::id()));
        let day = home.join("sessions/2026/09/28");
        std::fs::create_dir_all(&day).expect("mkdir");
        let now = SystemTime::now();
        let days_ago = now - std::time::Duration::from_secs(3 * 24 * 3600);
        let stamp = "2026-09-25T10:00:00.000Z";
        let own_id = "01a0dc36-1dc1-7ee2-be91-11bc323e377c";
        let own_path = day.join(format!("rollout-2026-09-25T10-00-00-{own_id}.jsonl"));
        let write_at = |path: &Path, body: String, at: SystemTime| {
            std::fs::write(path, body).expect("write");
            std::fs::File::options()
                .write(true)
                .open(path)
                .and_then(|f| f.set_modified(at))
                .expect("mtime");
        };
        let own_ctx = |sandbox: &str| {
            turn_context(stamp, "/w/pj", sandbox).replace("gpt-6-luna", "gpt-6-astra")
        };
        write_at(
            &own_path,
            format!("{}\n", own_ctx("danger-full-access")),
            days_ago,
        );
        write_at(
            &day.join("rollout-2026-09-28T08-00-00-01a0e8a1-4ca6-7d81-8cd1-6b9b60797925.jsonl"),
            format!("{}\n", turn_context(stamp, "/w/pj", "workspace-write")),
            now - std::time::Duration::from_secs(60),
        );
        let bypass = [
            "codex".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];
        // The own thread known: its context alone.
        let seen = look(
            &home,
            whose(Some("/w/pj"), &bypass, Some(own_id), days_ago),
            now,
        );
        assert_eq!(
            seen.sandbox_fell, None,
            "its own thread runs outside the sandbox"
        );
        assert_eq!(seen.thread_model.as_deref(), Some("gpt-6-astra"));
        // Not known: around its last work, days ago — the neighbour is none.
        let seen = look(&home, whose(Some("/w/pj"), &bypass, None, days_ago), now);
        assert_eq!(seen.sandbox_fell, None);
        assert_eq!(seen.thread_model, None);
        // The control: a session that just worked takes it as a candidate.
        let seen = look(&home, whose(Some("/w/pj"), &bypass, None, now), now);
        assert_eq!(seen.sandbox_fell.as_deref(), Some("workspace-write"));
        // Its own thread fallen: the fall, whatever the neighbours.
        write_at(
            &own_path,
            format!("{}\n", own_ctx("workspace-write")),
            days_ago,
        );
        let seen = look(
            &home,
            whose(Some("/w/pj"), &bypass, Some(own_id), days_ago),
            now,
        );
        assert_eq!(seen.sandbox_fell.as_deref(), Some("workspace-write"));
        // Its rollout missing: no verdict.
        let gone = "01a0ffff-1dc1-7ee2-be91-11bc323e377c";
        let seen = look(&home, whose(Some("/w/pj"), &bypass, Some(gone), now), now);
        assert_eq!((seen.sandbox_fell, seen.thread_model), (None, None));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The thread's model counts for a switch only for a turn begun since it
    /// opened: a `turn_context` carries its time, and a switch opened after it
    /// reads no model off it. Negative controls: a turn begun at or after the
    /// opening counts; with no opening known, the model as read; a turn with no
    /// time never counts for a switch.
    #[test]
    fn the_threads_model_counts_only_for_a_turn_since_the_switch_opened() {
        let ctx = rollout_turn_context(&turn_context(
            "2026-09-28T15:55:30.874Z",
            "/w/pj",
            "danger-full-access",
        ))
        .expect("the turn_context");
        let at = unix("2026-09-28T15:55:30Z");
        assert_eq!(ctx.at_unix, Some(at));
        let seen = CodexSeen {
            thread_model: ctx.model.clone(),
            thread_model_at: ctx.at_unix,
            ..CodexSeen::default()
        };
        assert_eq!(model_since(&seen, Some(at + 60)), None, "before the press");
        assert_eq!(model_since(&seen, Some(at)).as_deref(), Some("gpt-6-luna"));
        // The press's own second: the turn's time cut to its second, the
        // press's rounded — a second's slack, and no more.
        assert_eq!(
            model_since(&seen, Some(at + MODEL_SINCE_SLACK_S)).as_deref(),
            Some("gpt-6-luna")
        );
        assert_eq!(
            model_since(&seen, Some(at + MODEL_SINCE_SLACK_S + 1)),
            None,
            "past the slack"
        );
        assert_eq!(
            model_since(&seen, Some(at - 60)).as_deref(),
            Some("gpt-6-luna")
        );
        assert_eq!(model_since(&seen, None).as_deref(), Some("gpt-6-luna"));
        let untimed = CodexSeen {
            thread_model_at: None,
            ..seen
        };
        assert_eq!(model_since(&untimed, Some(at - 60)), None);
    }

    /// The own thread's lock through a linked home: `lsof` prints resolved
    /// paths (`/tmp/…` as `/private/tmp/…`), so a `$CODEX_HOME` reached through
    /// a symlink is resolved before its locks' folder is compared. Negative
    /// control: a lock in another folder is none of it.
    #[cfg(unix)]
    #[test]
    fn the_own_threads_lock_is_found_through_a_linked_home() {
        let base = std::env::temp_dir().join(format!("codex-usage-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(real.join("thread-writer-locks")).expect("mkdir");
        let linked = base.join("linked");
        std::os::unix::fs::symlink(&real, &linked).expect("symlink");
        let t = "01a0dc36-1dc1-7ee2-be91-11bc323e377c";
        let lock = std::fs::canonicalize(real.join("thread-writer-locks"))
            .expect("resolved")
            .join(format!("{t}.lock"));
        assert_ne!(
            lock.parent(),
            Some(linked.join("thread-writer-locks").as_path())
        );
        let plain = ["codex".to_string()];
        assert_eq!(
            own_thread(&plain, Some(std::slice::from_ref(&lock)), &linked).as_deref(),
            Some(t),
            "through the link"
        );
        assert_eq!(own_thread(&plain, Some(&[lock]), &real).as_deref(), Some(t));
        let elsewhere = std::fs::canonicalize(&base)
            .expect("resolved")
            .join(format!("{t}.lock"));
        assert_eq!(own_thread(&plain, Some(&[elsewhere]), &linked), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The `lsof` child is bounded: it runs on the supervising loop's thread,
    /// so a child past its bound is killed (`SIGKILL`, no crash report) and
    /// reads nothing. Negative control: one that answers in time is read.
    #[test]
    fn a_child_past_its_bound_is_killed_and_reads_nothing() {
        // The child sleeps five minutes, so a bound that did not kill it is
        // waited out far past the minute's hang detector (AGENTS.md).
        let start = std::time::Instant::now();
        let mut slow = aterm_uds::spawnfd::command("/bin/sleep");
        slow.arg("300");
        assert_eq!(
            output_within(slow, std::time::Duration::from_millis(200)),
            None
        );
        assert!(
            start.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the child past its bound was waited out: {:?}",
            start.elapsed()
        );
        let mut quick = aterm_uds::spawnfd::command("/bin/echo");
        quick.arg("n/x.lock");
        // The bound on a child that answers is a hang detector, a minute
        // (AGENTS.md): a loaded box can take seconds to start `echo`.
        assert_eq!(
            output_within(quick, std::time::Duration::from_secs(60)),
            Some((true, b"n/x.lock\n".to_vec()))
        );
    }
}
