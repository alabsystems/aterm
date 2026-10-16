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
//!
//! Context ([`rollout_context_left`]): every `token_count` with `info` carries
//! the last response's `last_token_usage.total_tokens` and the model's
//! `model_context_window`, the two numbers Codex's own `N% context left` is
//! drawn from ([`context_left_of`]). Codex 0.163.0's default footer draws no
//! such item (measured 2026-10-16 in the lab, at 6% to 99.6% used: only an
//! empty `tui.status_line` or one naming `context-remaining` draws it), so
//! the supervisor's context watch reads the session's own thread's figure
//! here. A thread not named reads one only where a single rollout written
//! around the session's last work, in its folder and begun since its TUI
//! started, is the candidate.
//!
//! Spend ([`TurnReader`], the account stats' C1,
//! `docs/DESIGN-account-stats-2026-10-15.md` §3.3, §3.5, §4.3): a rollout
//! read from its start yields each served turn once — the `last_token_usage`
//! of every `token_count` whose running total (`total_token_usage`) grew, on
//! the model of the latest `turn_context` before it. Codex writes a
//! `token_count` again with the same total (most often after a reasoning
//! item, and at a turn's start once the thread has a total), and those count
//! nothing. A FORK (`session_meta` naming `forked_from_id`) begins with a
//! copy of its parent's lines, its `token_count`s among them, rewritten
//! under new stamps in one burst; summing them counts the parent's turns
//! again. So the copied prefix — the run of lines from the fork's
//! `session_meta` each stamped within [`FORK_BURST_MS`] of the newest before
//! it — counts for nothing: no tokens, no model. Its totals stand: the
//! fork's own go on from the copy's last, so a count the fork writes again
//! with that total is a repeat, not its parent's last turn. A rollout whose first line
//! names no session is read the same way, since nothing rules a fork out,
//! and a turn needs a model's answer, which no burst holds. Each turn
//! carries a fingerprint of its total and its counts ([`Turn::key`]), one
//! per turn within its rollout (the total grows at every one), so a rollout
//! read again from its start is known by them. Across rollouts it names no
//! turn: two threads served the same counts (one prompt run twice, the lab's
//! stub) share it, so no fold dedupes across rollouts by it.
//!
//! MEASURED on the 133 rollouts of the machine this was built on (Codex
//! 0.145.0 and 0.146.0, July and August 2026; 121 of them forks, each with
//! lines after its copy): the copy's consecutive stamps were at most 26 ms
//! apart and the fork's own first line came at least 1.47 s after the
//! copy's last; the fork's own running total continued from the copy's last;
//! outside the copies no total fell, each growth equalled the line's
//! `last_token_usage.total_tokens`, and every `token_count` had a model.
//! There, 1,063 `token_count`s repeated their thread's total; the first
//! count of a turn after a thread's first did so in 151 of 218 turns, and
//! the first of a thread's first turn, a fork's included, never did.
//! Summing every growth per file, copies included, gave 20.4 times the
//! fork-aware sum. `cache_write_input_tokens` was 0 on all 463,935
//! `token_count` lines, and no `turn_context` named a `service_tier`.
//! MEASURED again on the lab's 34 rollouts of Codex 0.163.0 (stub replies,
//! no forks): the same `token_count` and `turn_context` keys, and 79 turns
//! with only 46 distinct fingerprints among them.

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

/// A turn's recorded folder, permission mode and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnContext {
    pub cwd: String,
    /// `sandbox_policy.type`: `danger-full-access`, `workspace-write`,
    /// `read-only`, …
    pub sandbox: String,
    /// `approval_policy` where it is a word (`on-request`, `never`, …);
    /// `None` for a `granular` policy's object, or none written.
    pub approval: Option<String>,
    /// `approvals_reviewer` (`user`, `auto_review`), where written.
    pub reviewer: Option<String>,
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
        approval: p
            .get("approval_policy")
            .and_then(Value::as_str)
            .map(str::to_string),
        reviewer: p
            .get("approvals_reviewer")
            .and_then(Value::as_str)
            .map(str::to_string),
        model: p.get("model").and_then(Value::as_str).map(str::to_string),
        at_unix: v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(aterm_types::rfc3339::parse_utc_fractional),
    })
}

/// The tokens Codex counts as a conversation's floor (its system prompt and
/// tools), off both what is used and the window before `N% context left` is
/// drawn: every figure measured on 0.163.0 fits `(window − used) / (window −
/// 12000)`, rounded (2026-10-16, the lab, window 258400: 15036 used drew 99%
/// left, 129200 drew 52%, 230400 drew 11%, 245500 drew 5%, 257300 drew 0%,
/// and 8.3K after a compaction drew 100%).
pub const CONTEXT_BASELINE: u64 = 12_000;

/// The share of `window` left with `used` tokens in it, in whole percent, as
/// Codex draws it ([`CONTEXT_BASELINE`]): `(window − used)` over `(window −
/// baseline)`, the baseline off both, rounded and clamped to 0..=100. A
/// window no larger than the baseline has none left.
#[must_use]
pub fn context_left_of(used: u64, window: u64) -> u8 {
    let Some(effective) = window.checked_sub(CONTEXT_BASELINE).filter(|e| *e > 0) else {
        return 0;
    };
    let left = effective.saturating_sub(used.saturating_sub(CONTEXT_BASELINE));
    let pct = (u128::from(left) * 100 + u128::from(effective) / 2) / u128::from(effective);
    u8::try_from(pct.min(100)).unwrap_or(100)
}

/// The context left after the last `token_count` in `tail` that counts one
/// (`info.last_token_usage.total_tokens` and `info.model_context_window`;
/// a rate-limit row's `info` is null), as Codex draws it
/// ([`context_left_of`]). A line that is not JSON — the tail's cut first
/// line — is skipped; `None` where no row counts one.
#[must_use]
pub fn rollout_context_left(tail: &str) -> Option<u8> {
    tail.lines().rev().find_map(|line| {
        if !line.contains("\"token_count\"") || !line.contains("\"model_context_window\"") {
            return None;
        }
        let v: Value = aterm_json::from_str(line).ok()?;
        let p = v.get("payload")?;
        if p.get("type").and_then(Value::as_str) != Some("token_count") {
            return None;
        }
        let info = p.get("info")?;
        let used = info
            .get("last_token_usage")?
            .get("total_tokens")?
            .as_u64()?;
        let window = info.get("model_context_window")?.as_u64()?;
        Some(context_left_of(used, window))
    })
}

/// The last `turn_context` in `tail`.
#[must_use]
pub fn rollout_turn_context(tail: &str) -> Option<TurnContext> {
    tail.lines().rev().find_map(turn_context_line)
}

impl TurnContext {
    /// The permission mode the turn ran in, as a relaunch carries it
    /// ([`crate::harness::upgrade_codex::carried_mode`]).
    #[must_use]
    pub fn mode(&self) -> crate::harness::upgrade_codex::ThreadMode {
        crate::harness::upgrade_codex::ThreadMode::of(
            self.approval.as_deref(),
            self.reviewer.as_deref(),
            Some(&self.sandbox),
        )
    }
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

/// The context left after the last `token_count` of the rollout at `path`
/// that counts one ([`rollout_context_left`]), read backward from its end in
/// [`CONTEXT_CHUNK`] steps, at most `reach` bytes, as [`last_turn_context`]
/// reads (a tool's output after the last response can be megabytes). `None`
/// for a file that cannot be read, or no such row within reach.
#[must_use]
pub fn last_context_left(path: &Path, reach: u64) -> Option<u8> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    crate::harness::upgrade_codex::read_back(
        &mut file,
        len,
        CONTEXT_CHUNK,
        reach,
        rollout_context_left,
    )
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

/// The `$CODEX_HOME` a Codex started with `env` in the working directory
/// `cwd` uses: `CODEX_HOME`, a relative one joined to `cwd` (Codex
/// canonicalizes it against its own working directory as it starts), else
/// `$HOME/.codex` (the Codex upgrade's own rule); an empty `CODEX_HOME` is
/// unset. A relative `CODEX_HOME` with no absolute `cwd` names no home aterm
/// can place — never `$HOME/.codex`, which may be another Codex's records —
/// so nothing is read (`None`).
#[must_use]
pub fn codex_home(
    codex_home: Option<&str>,
    home: Option<&str>,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    match codex_home.filter(|v| !v.is_empty()) {
        Some(set) if Path::new(set).is_absolute() => Some(PathBuf::from(set)),
        Some(relative) => cwd.filter(|c| c.is_absolute()).map(|c| c.join(relative)),
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
    /// The context the session's thread has left, as Codex would draw it
    /// ([`rollout_context_left`]): its own thread's where known, else the
    /// one candidate rollout's (module header). `None`: unread, or no one
    /// thread to read it from.
    pub context_left: Option<u8>,
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
/// where known ([`own_thread`]), when it last worked (the loop's last busy
/// read; `now` when none was seen), and when its TUI started (unix
/// milliseconds, to the second; `None` unread), which a thread not named
/// must have begun after to be its own.
#[derive(Debug, Clone, Copy)]
pub struct Whose<'a> {
    pub cwd: Option<&'a str>,
    pub argv: &'a [String],
    pub thread: Option<&'a str>,
    pub active_at: SystemTime,
    pub started_ms: Option<u64>,
}

/// One look at `home`'s rollouts for `who` at `now` (module header): the usage
/// window from the newest rollouts carrying a reading; the sandbox verdict,
/// the thread's model and its context left from the session's own thread's
/// rollout where known, else over those written within [`RECENT_S`] of its
/// last work, each by its last `turn_context` ([`last_turn_context`], up to
/// [`CONTEXT_REACH`] back) — the context only where one of them is the
/// session's ([`one_candidate`]).
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
            recent.push((path, last_turn_context(path, reach)));
        }
    }
    let (sandbox_fell, thread_model, thread_model_at, context_left) = match who.thread {
        // The session's own thread decides alone.
        Some(thread) => {
            let own = files
                .iter()
                .find(|(p, _)| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| crate::harness::upgrade_codex::is_rollout_of(n, thread))
                })
                .map(|(p, _)| p);
            let context = own.and_then(|p| last_context_left(p, reach));
            let own = own.and_then(|p| last_turn_context(p, reach));
            let fell = own
                .as_ref()
                .filter(|t| bypass && t.sandbox != "danger-full-access")
                .map(|t| t.sandbox.clone());
            let at = own.as_ref().and_then(|t| t.at_unix);
            (fell, own.and_then(|t| t.model), at, context)
        }
        None => {
            let contexts: Vec<Option<TurnContext>> =
                recent.iter().map(|(_, t)| t.clone()).collect();
            let context = who
                .cwd
                .and_then(|c| one_candidate(&recent, c, who.started_ms))
                .and_then(|p| last_context_left(p, reach));
            (
                who.cwd.and_then(|c| sandbox_fell(&contexts, c, bypass)),
                None,
                None,
                context,
            )
        }
    };
    CodexSeen {
        limits: read_limits(reading.as_ref(), now_unix),
        sandbox_fell,
        thread_model,
        thread_model_at,
        context_left,
    }
}

/// The ONE rollout among `recent` (each with its last `turn_context`) that
/// can be a session's own when its thread is not named: its last turn ran in
/// `cwd`, and its thread began since the session's TUI started
/// (`started_ms`, with a second's slack; [`crate::harness::upgrade_codex::thread_created_ms`]
/// of the thread its file names) where that is known. `None` where any
/// candidate's `turn_context` could not be read (it may be this session's),
/// or where none, or more than one, fits: another session's figure is never
/// this one's.
fn one_candidate<'a>(
    recent: &[(&'a PathBuf, Option<TurnContext>)],
    cwd: &str,
    started_ms: Option<u64>,
) -> Option<&'a PathBuf> {
    use crate::harness::upgrade_codex::thread_created_ms;
    if recent.iter().any(|(_, t)| t.is_none()) {
        return None;
    }
    let begun_since = |path: &Path| {
        started_ms.is_none_or(|started| {
            rollout_thread(path)
                .and_then(thread_created_ms)
                .is_some_and(|ms| ms.saturating_add(1000) >= started)
        })
    };
    let mut fits = recent
        .iter()
        .filter(|(path, t)| t.as_ref().is_some_and(|t| same_dir(&t.cwd, cwd)) && begun_since(path));
    let one = fits.next()?;
    fits.next().is_none().then_some(one.0)
}

/// The thread a rollout's file name names (`rollout-<stamp>-<thread>.jsonl`).
fn rollout_thread(path: &Path) -> Option<&str> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".jsonl")?;
    let thread = stem.get(stem.len().checked_sub(36)?..)?;
    crate::harness::upgrade_codex::is_rollout_of(path.file_name()?.to_str()?, thread)
        .then_some(thread)
        .filter(|t| crate::harness::upgrade_codex::is_thread_id(t))
}

// ---------------------------------------------------------------------------
// Spend: each served turn of a rollout, once (module header)
// ---------------------------------------------------------------------------

/// How far apart, in milliseconds, two lines of a fork's copied prefix may be
/// stamped: the prefix runs from the fork's `session_meta` while each line is
/// stamped within this of the newest stamp before it (module header: the
/// copies measured at most 26 ms apart, the fork's own first line at least
/// 1.47 s after them).
pub const FORK_BURST_MS: i64 = 500;

/// One turn's token counts as Codex records them (`last_token_usage`), in
/// OpenAI's classes: `input` holds `cached`, and `output` holds `reasoning`.
/// Whether `input` holds `cache_write` too is UNKNOWN (none was seen, module
/// header); the spend fold reads it as a part, as OpenAI's usage details are
/// parts of the input, and calls such a turn approximate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TurnTokens {
    /// `input_tokens`.
    pub input: u64,
    /// `cached_input_tokens`.
    pub cached: u64,
    /// `cache_write_input_tokens`.
    pub cache_write: u64,
    /// `output_tokens`.
    pub output: u64,
    /// `reasoning_output_tokens`.
    pub reasoning: u64,
}

impl TurnTokens {
    /// The counts of a usage object; a missing class reads 0. `None` for
    /// anything but an object holding `input_tokens` or `output_tokens` as a
    /// whole number.
    fn of(v: Option<&Value>) -> Option<Self> {
        let o = v?.as_object()?;
        let n = |key: &str| o.get(key).and_then(Value::as_u64);
        if n("input_tokens").is_none() && n("output_tokens").is_none() {
            return None;
        }
        Some(Self {
            input: n("input_tokens").unwrap_or(0),
            cached: n("cached_input_tokens").unwrap_or(0),
            cache_write: n("cache_write_input_tokens").unwrap_or(0),
            output: n("output_tokens").unwrap_or(0),
            reasoning: n("reasoning_output_tokens").unwrap_or(0),
        })
    }
}

/// One served turn, counted once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// The `token_count`'s stamp, unix seconds.
    pub at: i64,
    /// The model of the latest `turn_context` before it, outside a fork's
    /// copied prefix; `None` where none named one.
    pub model: Option<String>,
    /// Its counts (`last_token_usage`).
    pub tokens: TurnTokens,
    /// That `turn_context` named a service tier (`fast`, `flex`): the list
    /// price is another than the standard one.
    pub tier: bool,
    /// FNV-1a/64 of its running total and its counts: one per turn within
    /// its rollout, never an identity across rollouts (module header).
    pub key: u64,
}

/// Where a [`TurnReader`] stands in its rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prefix {
    /// No line read yet.
    #[default]
    Start,
    /// Inside a fork's copied prefix, or the first burst of a rollout whose
    /// first line names no session: the newest stamp read, unix ms.
    Copy(i64),
    /// Past it, or a rollout that is no fork: every line counts.
    Own,
}

/// What one rollout line was to the fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A served turn.
    Turn(Turn),
    /// A `token_count` whose running total did not grow: written again (after
    /// a rate-limit update), no turn.
    Repeat,
    /// A `token_count` in a fork's copied prefix: its parent's turn.
    Copied,
    /// A turn with no readable stamp: placed in no window, so not counted.
    Untimed,
    /// A line that is not JSON, or a `token_count` whose counts cannot be
    /// read.
    Unparsed,
    /// Any other line.
    Other,
}

/// The spend fold of one rollout, a line at a time from its start (module
/// header), its state kept between reads: where it stands against a fork's
/// copied prefix, the largest running total read, and the model and service
/// tier of the latest `turn_context`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnReader {
    /// Where the reader stands.
    pub prefix: Prefix,
    /// The largest `total_token_usage.total_tokens` read, a fork's copied
    /// prefix included: the fork's own totals go on from its copy's (module
    /// header), so a count its own lines write again with the copy's total
    /// is a repeat, not its parent's last turn again.
    pub top: u64,
    /// The latest `turn_context`'s `model` past the prefix.
    pub model: Option<String>,
    /// That `turn_context` named a service tier other than the standard one.
    pub tier: bool,
}

/// Unix milliseconds of a rollout stamp (`2026-08-01T17:36:47.998Z`): the
/// whole second, plus the fraction's first three digits.
fn stamp_ms(stamp: &str) -> Option<i64> {
    let secs = aterm_types::rfc3339::parse_utc_fractional(stamp)?;
    // The parser accepted the fraction: ASCII digits only.
    let millis = stamp
        .strip_suffix('Z')
        .and_then(|head| head.get(19..))
        .and_then(|fraction| fraction.strip_prefix('.'))
        .map_or(0, |digits| {
            digits
                .bytes()
                .chain(std::iter::repeat(b'0'))
                .take(3)
                .fold(0_i64, |ms, d| ms * 10 + i64::from(d.wrapping_sub(b'0')))
        });
    secs.checked_mul(1000)?.checked_add(millis)
}

/// The fingerprint of a `token_count` ([`Turn::key`]).
fn turn_key(total: &TurnTokens, total_tokens: u64, last: &TurnTokens) -> u64 {
    let text = format!(
        "{},{},{},{},{},{total_tokens};{},{},{},{},{}",
        total.input,
        total.cached,
        total.cache_write,
        total.output,
        total.reasoning,
        last.input,
        last.cached,
        last.cache_write,
        last.output,
        last.reasoning,
    );
    aterm_hash::fnv1a_64(text.as_bytes())
}

/// A `token_count` payload's running total, its `total_tokens` (else input
/// plus output), and its last turn; `None` where `info` holds no readable
/// pair of counts (the rate limits alone, or counts that cannot be read).
fn counts_of(payload: Option<&Value>) -> Option<(TurnTokens, u64, TurnTokens)> {
    let info = payload?.get("info").filter(|i| i.is_object())?;
    let total_usage = info.get("total_token_usage");
    let total = TurnTokens::of(total_usage)?;
    let last = TurnTokens::of(info.get("last_token_usage"))?;
    let total_tokens = total_usage
        .and_then(|t| t.get("total_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| total.input.saturating_add(total.output));
    Some((total, total_tokens, last))
}

impl TurnReader {
    /// Read the rollout's next line (module header). Past the prefix only a
    /// `token_count` or `turn_context` line is opened; inside it every line
    /// is, for its stamp.
    pub fn line(&mut self, line: &str) -> Row {
        let line = line.trim();
        if line.is_empty() {
            return Row::Other;
        }
        let counts = line.contains("\"token_count\"");
        if self.prefix == Prefix::Own && !counts && !line.contains("\"turn_context\"") {
            return Row::Other;
        }
        let Ok(v) = crate::harness::transcript::codex_spend(line) else {
            return Row::Unparsed;
        };
        let word = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
        let kind = word(v.get("type"));
        let payload = v.get("payload");
        let is_count = kind.as_deref() == Some("event_msg")
            && word(payload.and_then(|p| p.get("type"))).as_deref() == Some("token_count");
        let stamp = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(stamp_ms);
        // A copied count counts nothing, but its total stands: the fork's
        // own totals go on from it (module header).
        let copied = |reader: &mut Self| {
            if !is_count {
                return Row::Other;
            }
            if let Some((_, total_tokens, _)) = counts_of(payload) {
                reader.top = reader.top.max(total_tokens);
            }
            Row::Copied
        };
        match self.prefix {
            Prefix::Start => {
                let fork = word(payload.and_then(|p| p.get("forked_from_id")))
                    .is_some_and(|parent| !parent.is_empty());
                if kind.as_deref() == Some("session_meta") && !fork {
                    self.prefix = Prefix::Own;
                    return Row::Other;
                }
                // A fork's `session_meta`, or a first line naming no
                // session: the burst starts at its stamp. One with none is
                // no start; the next line's stamp is.
                if let Some(at) = stamp {
                    self.prefix = Prefix::Copy(at);
                }
                return copied(self);
            }
            Prefix::Copy(newest) => match stamp {
                Some(at) if at.saturating_sub(newest) > FORK_BURST_MS => self.prefix = Prefix::Own,
                Some(at) => {
                    self.prefix = Prefix::Copy(newest.max(at));
                    return copied(self);
                }
                None => return copied(self),
            },
            Prefix::Own => {}
        }
        if kind.as_deref() == Some("turn_context") {
            self.model = word(payload.and_then(|p| p.get("model")))
                .filter(|m| crate::harness::upgrade::is_model_id(m));
            self.tier = word(payload.and_then(|p| p.get("service_tier")))
                .is_some_and(|t| !matches!(t.as_str(), "" | "standard" | "default" | "auto"));
            return Row::Other;
        }
        if !is_count {
            return Row::Other;
        }
        if !payload
            .and_then(|p| p.get("info"))
            .is_some_and(Value::is_object)
        {
            // A count of the rate limits alone.
            return Row::Other;
        }
        let Some((total, total_tokens, last)) = counts_of(payload) else {
            return Row::Unparsed;
        };
        if total_tokens <= self.top {
            return Row::Repeat;
        }
        self.top = total_tokens;
        let Some(at) = stamp else {
            return Row::Untimed;
        };
        Row::Turn(Turn {
            at: at.div_euclid(1000),
            model: self.model.clone(),
            tokens: last,
            tier: self.tier,
            key: turn_key(&total, total_tokens, &last),
        })
    }
}

/// What one whole read of a rollout ([`read_rollout`]) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RolloutRead {
    /// Bytes read.
    pub read: u64,
    /// It was read to the end it had (a torn last line aside), not stopped
    /// at the cap or by an error.
    pub complete: bool,
    /// Lines longer than [`crate::harness::usage::MAX_LINE_BYTES`], skipped
    /// unread.
    pub long: u64,
}

/// Read the rollout at `path` from its start, at most `cap` bytes, each
/// complete line through `reader` into `each`. A line longer than
/// [`crate::harness::usage::MAX_LINE_BYTES`] is skipped as it streams, never
/// held, and a torn last line (one still being written) is not read. `None`
/// when the file cannot be opened.
pub fn read_rollout(
    path: &Path,
    reader: &mut TurnReader,
    cap: u64,
    mut each: impl FnMut(Row),
) -> Option<RolloutRead> {
    use std::io::BufRead as _;
    let max_line = crate::harness::usage::MAX_LINE_BYTES;
    let file = crate::harness::footer::open_regular(path)?;
    let mut file = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut out = RolloutRead::default();
    let mut line: Vec<u8> = Vec::new();
    let mut skipping = false;
    loop {
        let buf = match file.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Some(out),
        };
        if buf.is_empty() {
            break;
        }
        // Bytes are left past the cap: the read stops short.
        if out.read >= cap {
            return Some(out);
        }
        let room = usize::try_from(cap - out.read).unwrap_or(usize::MAX);
        let chunk = &buf[..buf.len().min(room)];
        let newline = chunk.iter().position(|&b| b == b'\n');
        let used = newline.map_or(chunk.len(), |i| i + 1);
        if !skipping {
            line.extend_from_slice(&chunk[..newline.unwrap_or(used)]);
            if line.len() > max_line {
                out.long += 1;
                line = Vec::new();
                skipping = true;
            }
        }
        file.consume(used);
        out.read += used as u64;
        if newline.is_some() {
            if !skipping {
                each(match std::str::from_utf8(&line) {
                    Ok(text) => reader.line(text),
                    Err(_) => Row::Unparsed,
                });
            }
            line.clear();
            skipping = false;
        }
    }
    out.complete = true;
    Some(out)
}

/// How much of a rollout's first line is read for its `session_meta`
/// ([`conversation_rollouts`]); the line carries base instructions (about
/// 20 KB).
pub const META_BYTES: u64 = 1 << 20;

/// The most rollouts [`conversation_rollouts`] opens the first line of.
pub const MAX_CONVERSATION_SCAN: usize = 16_384;

/// The rollouts of one Codex conversation under a home, for `harness usage`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Conversation {
    /// The thread whose conversation it is: the `session_id` the named
    /// thread's `session_meta` gives (its conversation's first thread, the
    /// TUI's own), else the named thread.
    pub root: String,
    /// The named thread's own rollout, where it has one.
    pub own: Option<PathBuf>,
    /// The root's rollout first, where found, then every rollout whose
    /// `session_meta` names the root as its `session_id` (the subagents),
    /// oldest write first.
    pub files: Vec<PathBuf>,
    /// How many of `files` are subagents'.
    pub subagents: usize,
    /// Rollouts written since the root began whose first line could not be
    /// read: any of them may be one of its subagents. Past
    /// [`MAX_CONVERSATION_SCAN`], each one not opened.
    pub unread: usize,
}

/// The `session_meta` of a rollout's first line: its `session_id` and `id`.
fn session_ids(first_line: &str) -> Option<(Option<String>, Option<String>)> {
    let v: Value = aterm_json::from_str(first_line.trim()).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let p = v.get("payload")?;
    let id = |key: &str| p.get(key).and_then(Value::as_str).map(str::to_owned);
    Some((id("session_id"), id("id")))
}

/// The first line of `path`, at most [`META_BYTES`] of it; `None` when it
/// cannot be read or runs past the bound.
fn first_line(path: &Path) -> Option<String> {
    use std::io::BufRead as _;
    let file = crate::harness::footer::open_regular(path)?;
    let mut line = Vec::new();
    std::io::BufReader::new(std::io::Read::take(file, META_BYTES))
        .read_until(b'\n', &mut line)
        .ok()?;
    line.ends_with(b"\n")
        .then(|| String::from_utf8_lossy(&line).into_owned())
}

/// The rollouts of the conversation `thread` belongs to under `home`
/// ([`Conversation`]): the conversation's own thread, by the `session_id`
/// the named thread's rollout gives, and every subagent whose
/// `session_meta` names it. Only rollouts written since the root began are
/// opened (a UUIDv7 id says when; another id opens every one).
#[must_use]
pub fn conversation_rollouts(home: &Path, thread: &str) -> Conversation {
    use crate::harness::upgrade_codex::{is_rollout_of, is_thread_id, thread_created_ms};
    let all = rollouts_by_write(home);
    let name_of = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_default()
    };
    let own = all
        .iter()
        .find(|(p, _)| is_rollout_of(&name_of(p), thread))
        .map(|(p, _)| p.clone());
    let root = own
        .as_deref()
        .and_then(first_line)
        .and_then(|l| session_ids(&l))
        .and_then(|(session, _)| session)
        .filter(|s| is_thread_id(s))
        .unwrap_or_else(|| thread.to_owned());
    let since = thread_created_ms(&root)
        .map(|ms| SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(ms));
    let mut roots = Vec::new();
    let mut subagents = Vec::new();
    let mut unread = 0;
    let mut opened = 0;
    // Oldest write first, so the root's first turns are read first.
    for (path, at) in all.iter().rev() {
        if since.is_some_and(|s| *at < s) {
            continue;
        }
        if is_rollout_of(&name_of(path), &root) {
            roots.push(path.clone());
            continue;
        }
        opened += 1;
        if opened > MAX_CONVERSATION_SCAN {
            unread += 1;
            continue;
        }
        match first_line(path).and_then(|l| session_ids(&l)) {
            Some((Some(session), id))
                if session == root && id.as_deref() != Some(root.as_str()) =>
            {
                subagents.push(path.clone());
            }
            Some(_) => {}
            None => unread += 1,
        }
    }
    let spawned = subagents.len();
    roots.extend(subagents);
    Conversation {
        root,
        own,
        files: roots,
        subagents: spawned,
        unread,
    }
}

/// Composed rollout lines in the shapes Codex writes them (key names as on
/// 0.145–0.163; the `token_count` as the survey in the module header read
/// it), each with its newline, stamped in unix milliseconds. Stand-in values
/// only: no real rollout is copied.
#[cfg(test)]
pub(crate) mod fixtures {
    /// A rollout stamp: `2026-08-01T17:36:47.998Z`.
    pub(crate) fn stamp(ms: i64) -> String {
        let whole = crate::harness::usage::rfc3339_utc(ms.div_euclid(1000));
        format!("{}.{:03}Z", &whole[..19], ms.rem_euclid(1000))
    }

    /// A thread's `session_meta`: its id, its conversation (`session_id`),
    /// and the thread it was forked from, if any.
    pub(crate) fn meta(ms: i64, thread: &str, session: &str, fork: Option<&str>) -> String {
        let fork = fork.map_or_else(String::new, |f| format!(",\"forked_from_id\":\"{f}\""));
        format!(
            "{{\"timestamp\":\"{at}\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\
             \"{session}\",\"id\":\"{thread}\"{fork},\"timestamp\":\"{at}\",\"cwd\":\
             \"/stand-in/project\",\"originator\":\"codex-tui\",\"cli_version\":\"0.163.0\",\
             \"source\":\"cli\",\"model_provider\":\"openai\",\"base_instructions\":{{\"text\":\
             \"You are Codex (stand-in)\"}}}}}}\n",
            at = stamp(ms),
        )
    }

    /// A `turn_context` naming `model`, and `tier` as its service tier.
    pub(crate) fn context(ms: i64, model: &str, tier: Option<&str>) -> String {
        let tier = tier.map_or_else(String::new, |t| format!(",\"service_tier\":\"{t}\""));
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t\",\
             \"cwd\":\"/stand-in/project\",\"approval_policy\":\"never\",\"sandbox_policy\":\
             {{\"type\":\"danger-full-access\"}},\"model\":\"{model}\",\"effort\":\"high\"{tier}}}}}\n",
            stamp(ms)
        )
    }

    /// One count's classes: input (cached ones in it), cached input, output.
    pub(crate) type Counts = (u64, u64, u64);

    fn usage((input, cached, output): Counts) -> String {
        format!(
            "{{\"input_tokens\":{input},\"cached_input_tokens\":{cached},\
             \"cache_write_input_tokens\":0,\"output_tokens\":{output},\
             \"reasoning_output_tokens\":{},\"total_tokens\":{}}}",
            output / 2,
            input + output
        )
    }

    /// A `token_count`: the running total and the last turn's counts, with
    /// the rate limits Codex writes beside them.
    pub(crate) fn count(ms: i64, total: Counts, last: Counts) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
             \"info\":{{\"total_token_usage\":{},\"last_token_usage\":{},\
             \"model_context_window\":272000}},\"rate_limits\":{{\"limit_id\":\"codex\",\
             \"primary\":{{\"used_percent\":12.0,\"window_minutes\":300,\"resets_at\":1790000000}},\
             \"secondary\":null,\"plan_type\":\"pro\"}}}}}}\n",
            stamp(ms),
            usage(total),
            usage(last)
        )
    }

    /// A `token_count` of the rate limits alone (`info` null).
    pub(crate) fn limits_only(ms: i64) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
             \"info\":null,\"rate_limits\":{{\"limit_id\":\"codex\",\"primary\":null}}}}}}\n",
            stamp(ms)
        )
    }

    /// Any other line: the model's message.
    pub(crate) fn message(ms: i64) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\
             \"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"done\"}}]}}}}\n",
            stamp(ms)
        )
    }

    /// `lines` re-stamped the way a fork copies them: in one burst from
    /// `ms`, a millisecond apart.
    pub(crate) fn copied(lines: &str, ms: i64) -> String {
        lines
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let at = line
                    .find("\"timestamp\":\"")
                    .map(|s| s + "\"timestamp\":\"".len())
                    .expect("a stamped line");
                let i = i64::try_from(i).expect("few lines");
                format!("{}{}{}\n", &line[..at], stamp(ms + i), &line[at + 24..])
            })
            .collect()
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
            started_ms: None,
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
            codex_home(None, Some("/Users//x"), None),
            Some(PathBuf::from("/Users//x/.codex"))
        );
        assert_eq!(
            codex_home(Some("/alt/home"), Some("/Users//x"), None),
            Some(PathBuf::from("/alt/home"))
        );
        assert_eq!(codex_home(Some("rel"), None, None), None);
        assert_eq!(
            codex_home(Some("rel"), Some("/Users//x"), None),
            None,
            "never another home's records"
        );
        // A relative home is the TUI's directory's, as Codex resolves it.
        assert_eq!(
            codex_home(Some("rel"), Some("/Users//x"), Some(Path::new("/w/pj"))),
            Some(PathBuf::from("/w/pj/rel"))
        );
        assert_eq!(
            codex_home(Some("rel"), Some("/Users//x"), Some(Path::new("pj"))),
            None
        );
        assert_eq!(
            codex_home(Some(""), Some("/Users//x"), Some(Path::new("/w/pj"))),
            Some(PathBuf::from("/Users//x/.codex")),
            "an empty one is unset"
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

    /// A `token_count` line as Codex 0.163.0 writes it (the lab's rollouts,
    /// 2026-10-16): `used` the last response's `total_tokens`, the window the
    /// lab's model's.
    fn context_count(at: &str, used: u64) -> String {
        format!(
            "{{\"timestamp\":\"{at}\",\"ordinal\":28,\"type\":\"event_msg\",\"payload\":{{\
             \"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":30038,\
             \"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\"output_tokens\":24,\
             \"reasoning_output_tokens\":0,\"total_tokens\":159657}},\"last_token_usage\":{{\
             \"input_tokens\":{used},\"cached_input_tokens\":0,\"cache_write_input_tokens\":0,\
             \"output_tokens\":0,\"reasoning_output_tokens\":0,\"total_tokens\":{used}}},\
             \"model_context_window\":258400}},\"rate_limits\":{{\"limit_id\":\"codex\",\
             \"limit_name\":null,\"primary\":null,\"secondary\":null,\"credits\":null,\
             \"individual_limit\":null,\"spend_control_reached\":null,\"plan_type\":null,\
             \"rate_limit_reached_type\":null}}}}}}"
        )
    }

    /// CODEX'S CONTEXT FIGURE ([`context_left_of`], [`rollout_context_left`])
    /// is drawn from the last response's tokens against the model's window,
    /// a 12000-token floor off both: every figure measured on 0.163.0 (the
    /// lab, 2026-10-16, window 258400) — 99% left at 15036 used, 52% at
    /// 129200, 11% at 230400, 5% at 245500, 0% at 257300, 100% after a
    /// compaction (8276) — and the rollout's last counting row decides.
    /// NEGATIVE CONTROLS: the plain share, without the floor, misses the
    /// first one (94%); a rate-limit row (`info` null) and a cut line count
    /// nothing; no counting row reads none.
    #[test]
    fn codexs_context_figure_is_its_rollouts_last_count_as_codex_draws_it() {
        for (used, drawn) in [
            (15_036, 99),
            (129_200, 52),
            (230_400, 11),
            (245_500, 5),
            (257_300, 0),
            (8_276, 100),
        ] {
            assert_eq!(context_left_of(used, 258_400), drawn, "{used}");
        }
        assert_eq!((258_400 - 15_036) * 100 / 258_400, 94, "the plain share");
        assert_eq!(context_left_of(300_000, 258_400), 0);
        assert_eq!(context_left_of(5, 12_000), 0, "no window past the floor");
        let tail = [
            context_count("2026-10-16T18:20:07.018Z", 129_200),
            context_count("2026-10-16T18:20:59.763Z", 245_500),
            PREMIUM.to_string(),
            "{\"cut".to_string(),
            "".to_string(),
        ]
        .join("\n");
        assert_eq!(rollout_context_left(&tail), Some(5), "the last count");
        assert_eq!(rollout_context_left(PREMIUM), None);
        assert_eq!(
            rollout_context_left(&token_count("2026-09-27T20:30:00.000Z", "codex", 97.0, 1)),
            None,
            "a rate-limit row with info null"
        );
    }

    /// THE SESSION'S CONTEXT FIGURE ([`CodexSeen::context_left`]) is its own
    /// thread's rollout's, read backward past a long tool row; a thread not
    /// named reads it only from the ONE rollout written around its work, in
    /// its folder, begun since its TUI started ([`one_candidate`]).
    /// NEGATIVE CONTROLS: a second candidate in the folder, or the only one
    /// begun before the TUI started (another session's), reads none; so does
    /// a candidate whose `turn_context` cannot be read.
    #[test]
    fn the_sessions_context_figure_is_its_own_threads_or_none() {
        const OWN: &str = "01a145f0-3bb2-7533-a346-44755e9f8e86";
        const OTHER: &str = "01a145ed-117a-77c3-8914-e0f8651e30cb";
        let home = std::env::temp_dir().join(format!("codex-usage-ctx-{}", std::process::id()));
        let day = home.join("sessions/2026/10/16");
        std::fs::create_dir_all(&day).expect("mkdir");
        let rollout =
            |thread: &str| day.join(format!("rollout-2026-10-16T11-18-54-{thread}.jsonl"));
        let ctx = turn_context("2026-10-16T18:20:07.010Z", "/w/pj", "read-only");
        // A tool row longer than a read-back step lies after the last count.
        let long = format!(
            "{{\"type\":\"response_item\",\"pad\":\"{}\"}}",
            "x".repeat(1_200_000)
        );
        std::fs::write(
            rollout(OWN),
            format!(
                "{ctx}\n{}\n{}\n{long}\n",
                context_count("2026-10-16T18:20:07.018Z", 15_036),
                context_count("2026-10-16T18:20:59.763Z", 129_200)
            ),
        )
        .expect("write");
        let now = SystemTime::now();
        let argv = Vec::new();
        let own = look(&home, whose(Some("/w/pj"), &argv, Some(OWN), now), now);
        assert_eq!(own.context_left, Some(52));
        assert_eq!(
            last_context_left(&rollout(OWN), 1024),
            None,
            "past the reach"
        );
        let started = crate::harness::upgrade_codex::thread_created_ms(OWN).expect("v7");
        let unnamed = |started_ms: Option<u64>| {
            let who = Whose {
                started_ms,
                ..whose(Some("/w/pj"), &argv, None, now)
            };
            look(&home, who, now).context_left
        };
        assert_eq!(unnamed(Some(started - 500)), Some(52), "the one candidate");
        assert_eq!(unnamed(None), Some(52), "no start read: the folder decides");
        assert_eq!(unnamed(Some(started + 5_000)), None, "begun before the TUI");
        // Another folder's thread is no candidate; one in this folder is.
        let other_ctx = turn_context("2026-10-16T18:20:07.010Z", "/w/elsewhere", "read-only");
        std::fs::write(
            rollout(OTHER),
            format!(
                "{other_ctx}\n{}\n",
                context_count("2026-10-16T18:20:07.018Z", 245_500)
            ),
        )
        .expect("write");
        assert_eq!(unnamed(None), Some(52), "another folder's");
        std::fs::write(
            rollout(OTHER),
            format!(
                "{ctx}\n{}\n",
                context_count("2026-10-16T18:20:07.018Z", 245_500)
            ),
        )
        .expect("write");
        assert_eq!(unnamed(None), None, "two candidates");
        assert_eq!(
            unnamed(Some(started - 500)),
            Some(52),
            "the other begun before the TUI"
        );
        std::fs::write(rollout(OTHER), "not json\n").expect("write");
        assert_eq!(unnamed(None), None, "a candidate unread");
        let _ = std::fs::remove_dir_all(&home);
    }

    // -- spend (C1) ---------------------------------------------------------

    use super::fixtures::{context, copied, count, limits_only, message, meta};

    /// Thread ids in UUIDv7 form, made in 2024: every file a test writes
    /// now is written since they began.
    const ROOT: &str = "01900000-0000-7000-8000-000000000001";
    const SUB: &str = "01900000-0000-7000-8000-000000000002";
    const OTHER: &str = "01900000-0000-7000-8000-000000000003";

    /// A stand-in instant, unix ms.
    const T0: i64 = 1_790_000_000_000;

    fn rows(reader: &mut TurnReader, text: &str) -> Vec<Row> {
        text.lines().map(|l| reader.line(l)).collect()
    }

    fn turns(rows: &[Row]) -> Vec<&Turn> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Turn(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    /// EACH SERVED TURN ONCE: a `token_count` whose running total grew is
    /// one turn, its `last_token_usage` on the latest `turn_context`'s
    /// model, stamped with its own second. A count written again with the
    /// same total, one of the rate limits alone, a line that is not JSON and
    /// a turn with no stamp count nothing. Negative control: counting every
    /// `token_count` that carries counts, as a per-line sum would, finds
    /// four where two turns were served.
    #[test]
    fn each_served_turn_counts_once_on_its_turns_model() {
        let untimed = count(T0 + 20_000, (5000, 4000, 300), (2000, 1400, 150))
            .replace(&fixtures::stamp(T0 + 20_000), "not a stamp");
        let cut = count(T0 + 15_000, (4000, 3000, 200), (1000, 400, 50));
        let text = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1000, "gpt-5.6-sol", None),
            count(T0 + 5000, (1000, 800, 50), (1000, 800, 50)),
            count(T0 + 5001, (1000, 800, 50), (1000, 800, 50)),
            limits_only(T0 + 5002),
            message(T0 + 5003),
            context(T0 + 9000, "gpt-6-luna", None),
            count(T0 + 12_000, (3000, 2600, 150), (2000, 1800, 100)),
            format!("{}\n", &cut[..cut.len() / 2]),
            untimed,
        ]
        .concat();
        let mut reader = TurnReader::default();
        let got = rows(&mut reader, &text);
        assert_eq!(got[0], Row::Other, "a session that is no fork");
        assert_eq!(reader.prefix, Prefix::Own);
        assert_eq!(got[3], Row::Repeat);
        assert_eq!(got[4], Row::Other, "the rate limits alone");
        assert_eq!(got[8], Row::Unparsed);
        assert_eq!(got[9], Row::Untimed);
        let served = turns(&got);
        assert_eq!(served.len(), 2);
        assert_eq!(served[0].at, (T0 + 5000) / 1000);
        assert_eq!(served[0].model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(
            served[0].tokens,
            TurnTokens {
                input: 1000,
                cached: 800,
                cache_write: 0,
                output: 50,
                reasoning: 25,
            }
        );
        assert_eq!(served[1].model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(served[1].tokens.input, 2000);
        assert_ne!(served[0].key, served[1].key);
        assert_eq!(reader.top, 5300, "the untimed turn still raised the total");
        // NEGATIVE CONTROL: every count that carries counts.
        let per_line = text
            .lines()
            .filter(|l| l.ends_with('}') && l.contains("\"last_token_usage\""))
            .count();
        assert_eq!(per_line, 4);
    }

    /// THE FORK RULE (the design's §3.5, §8.4): a fork opens with its
    /// parent's lines rewritten in one burst; that copied prefix counts
    /// nothing, and the fork's own turn — its total continuing from the
    /// copy's — counts once. Negative control: the same fork read without
    /// its `forked_from_id`, as the per-file rule reads it, counts the
    /// parent's two turns again.
    #[test]
    fn a_forks_copied_prefix_counts_nothing() {
        let parent = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1000, "gpt-5.6-sol", None),
            count(T0 + 5000, (1000, 800, 50), (1000, 800, 50)),
            count(T0 + 9000, (3000, 2600, 150), (2000, 1800, 100)),
        ]
        .concat();
        let fork_at = T0 + 60_000;
        let fork = [
            meta(fork_at, SUB, ROOT, Some(ROOT)),
            copied(&parent, fork_at + 1),
            message(fork_at + 2500),
            context(fork_at + 2501, "gpt-6-luna", None),
            count(fork_at + 7000, (3500, 2600, 250), (500, 0, 100)),
        ]
        .concat();
        let mut reader = TurnReader::default();
        let got = rows(&mut reader, &fork);
        assert_eq!(got.iter().filter(|r| **r == Row::Copied).count(), 2);
        let served = turns(&got);
        assert_eq!(served.len(), 1, "{got:?}");
        assert_eq!(served[0].model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(served[0].tokens.input, 500);
        // The parent read alone: its own two turns.
        let parents = rows(&mut TurnReader::default(), &parent);
        assert_eq!(turns(&parents).len(), 2);
        // The copies carry the parent's fingerprints exactly.
        let fork_keys: Vec<u64> = rows(
            &mut TurnReader::default(),
            &fork.replace(&format!(",\"forked_from_id\":\"{ROOT}\""), ""),
        )
        .iter()
        .filter_map(|r| match r {
            Row::Turn(t) => Some(t.key),
            _ => None,
        })
        .collect();
        // NEGATIVE CONTROL: no fork named, the copies count again.
        assert_eq!(fork_keys.len(), 3, "the parent's two again, and its own");
        let parent_keys: Vec<u64> = turns(&parents).iter().map(|t| t.key).collect();
        assert_eq!(fork_keys[..2], parent_keys[..]);
    }

    /// The burst's edge: a line stamped [`FORK_BURST_MS`] after the newest
    /// before it is still the copy; one a millisecond later is the fork's
    /// own. A slow copy whose every step is within the burst stays a copy
    /// however long it runs. Negative control: the same counts in a rollout
    /// that is no fork are turns.
    #[test]
    fn the_copy_ends_at_the_first_gap_past_the_burst() {
        let fork = |gap: i64| {
            [
                meta(T0, SUB, ROOT, Some(ROOT)),
                count(T0 + 400, (100, 0, 10), (100, 0, 10)),
                count(T0 + 800, (200, 0, 20), (100, 0, 10)),
                count(T0 + 800 + gap, (300, 0, 30), (100, 0, 10)),
            ]
            .concat()
        };
        let edge = rows(&mut TurnReader::default(), &fork(FORK_BURST_MS));
        assert!(turns(&edge).is_empty(), "{edge:?}");
        let past = rows(&mut TurnReader::default(), &fork(FORK_BURST_MS + 1));
        assert_eq!(turns(&past).len(), 1, "{past:?}");
        assert_eq!(turns(&past)[0].tokens.input, 100);
        // NEGATIVE CONTROL.
        let own = fork(FORK_BURST_MS).replace(&format!(",\"forked_from_id\":\"{ROOT}\""), "");
        assert_eq!(turns(&rows(&mut TurnReader::default(), &own)).len(), 3);
    }

    /// A FORK'S OWN TOTALS GO ON FROM ITS COPY'S (module header): a
    /// `token_count` its own lines write again with the copy's last total —
    /// as Codex writes one at a turn's start once the thread has a total —
    /// is a repeat, never its parent's last turn counted again; its own turn
    /// after it counts once. Negative control: the same own lines with no
    /// copy before them count that total as a turn.
    #[test]
    fn a_count_repeating_the_copys_total_is_no_turn() {
        let parent = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1000, "gpt-5.6-sol", None),
            count(T0 + 5000, (1000, 800, 50), (1000, 800, 50)),
            count(T0 + 9000, (3000, 2600, 150), (2000, 1800, 100)),
        ]
        .concat();
        let fork_at = T0 + 60_000;
        let own = [
            context(fork_at + 2000, "gpt-6-luna", None),
            count(fork_at + 2500, (3000, 2600, 150), (2000, 1800, 100)),
            count(fork_at + 7000, (3500, 2600, 250), (500, 0, 100)),
        ]
        .concat();
        let fork = [
            meta(fork_at, SUB, ROOT, Some(ROOT)),
            copied(&parent, fork_at + 1),
            own.clone(),
        ]
        .concat();
        let mut reader = TurnReader::default();
        let got = rows(&mut reader, &fork);
        assert_eq!(got.iter().filter(|r| **r == Row::Copied).count(), 2);
        assert_eq!(got.iter().filter(|r| **r == Row::Repeat).count(), 1);
        let served = turns(&got);
        assert_eq!(served.len(), 1, "{got:?}");
        assert_eq!(served[0].tokens.input, 500);
        assert_eq!(reader.top, 3750);
        // A reader stopped inside the copy holds the copy's total so far.
        let mut stopped = TurnReader::default();
        let lines: Vec<&str> = fork.lines().collect();
        for line in &lines[..lines.len() - 3] {
            stopped.line(line);
        }
        assert!(matches!(stopped.prefix, Prefix::Copy(_)));
        assert_eq!(stopped.top, 3150);
        // NEGATIVE CONTROL.
        let alone = [meta(fork_at, SUB, SUB, None), own].concat();
        assert_eq!(turns(&rows(&mut TurnReader::default(), &alone)).len(), 2);
    }

    /// A rollout whose first line names no session (cut, or of a shape this
    /// reader does not know) may be a fork: its first burst counts nothing,
    /// and turns after it count. Negative control: the same lines under a
    /// plain `session_meta` count from the first.
    #[test]
    fn a_rollout_whose_first_line_names_no_session_is_read_as_a_fork() {
        let body = [
            count(T0, (100, 0, 10), (100, 0, 10)),
            count(T0 + 30, (200, 0, 20), (100, 0, 10)),
            context(T0 + 3000, "gpt-6-sol", None),
            count(T0 + 9000, (300, 0, 30), (100, 0, 10)),
        ]
        .concat();
        let got = rows(&mut TurnReader::default(), &format!("{{\"cut\n{body}"));
        assert_eq!(got[0], Row::Unparsed);
        assert_eq!(got[1], Row::Copied);
        assert_eq!(got[2], Row::Copied);
        assert_eq!(turns(&got).len(), 1);
        assert_eq!(turns(&got)[0].model.as_deref(), Some("gpt-6-sol"));
        // NEGATIVE CONTROL.
        let plain = format!("{}{body}", meta(T0 - 10, ROOT, ROOT, None));
        assert_eq!(turns(&rows(&mut TurnReader::default(), &plain)).len(), 3);
    }

    /// A turn whose `turn_context` named a service tier other than the
    /// standard one says so; one naming none or `standard` does not, and a
    /// model that is no model id is none.
    #[test]
    fn a_service_tier_and_a_bad_model_are_read_as_they_are() {
        let text = |tier: Option<&str>, model: &str| {
            [
                meta(T0, ROOT, ROOT, None),
                context(T0 + 1, model, tier),
                count(T0 + 2, (10, 0, 1), (10, 0, 1)),
            ]
            .concat()
        };
        let turn = |tier, model| {
            let got = rows(&mut TurnReader::default(), &text(tier, model));
            turns(&got)[0].clone()
        };
        assert!(turn(Some("fast"), "gpt-6-sol").tier);
        assert!(turn(Some("flex"), "gpt-6-sol").tier);
        // NEGATIVE CONTROLS.
        assert!(!turn(Some("standard"), "gpt-6-sol").tier);
        assert!(!turn(None, "gpt-6-sol").tier);
        assert_eq!(turn(None, "gpt 6 sol!").model, None);
        assert_eq!(turn(None, "gpt-6-sol").model.as_deref(), Some("gpt-6-sol"));
    }

    /// A whole read streams the rollout's complete lines through the reader:
    /// a torn last line (one still being written) is not read, and a cap
    /// stops the read short and says so. Negative control: the whole file
    /// within its cap is complete.
    #[test]
    fn a_whole_read_takes_complete_lines_within_its_cap() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("codex-usage-read-")
            .tempdir()
            .expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        let whole = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1, "gpt-6-sol", None),
            count(T0 + 2, (10, 0, 1), (10, 0, 1)),
        ]
        .concat();
        let torn = count(T0 + 3, (20, 0, 2), (10, 0, 1));
        std::fs::write(&path, format!("{whole}{}", &torn[..torn.len() / 2])).expect("write");
        let mut n = 0;
        let read = read_rollout(&path, &mut TurnReader::default(), u64::MAX, |r| {
            n += usize::from(matches!(r, Row::Turn(_)));
        })
        .expect("read");
        assert_eq!(n, 1, "the torn count is not read");
        assert!(read.complete);
        assert_eq!(read.read, std::fs::metadata(&path).expect("meta").len());
        let short = read_rollout(&path, &mut TurnReader::default(), 10, |_| {}).expect("read");
        assert!(!short.complete);
        assert_eq!(short.read, 10);
        // A cap the file ends at is a whole read.
        let len = std::fs::metadata(&path).expect("meta").len();
        assert!(
            read_rollout(&path, &mut TurnReader::default(), len, |_| {})
                .expect("read")
                .complete
        );
        // NEGATIVE CONTROL.
        std::fs::write(&path, &whole).expect("write");
        let full = read_rollout(&path, &mut TurnReader::default(), u64::MAX, |_| {}).expect("read");
        assert!(full.complete);
        assert!(
            read_rollout(
                &dir.path().join("none"),
                &mut TurnReader::default(),
                1,
                |_| {}
            )
            .is_none()
        );
    }

    /// A CONVERSATION's rollouts: the TUI's own thread — the `session_id`
    /// the named thread's rollout gives — first, then every subagent whose
    /// `session_meta` names it, oldest write first, named by a subagent or
    /// by the root alike; a rollout whose first line cannot be read is
    /// counted unread. Negative control: another conversation's rollout is
    /// never among them, and it alone is its own.
    #[test]
    fn a_conversation_is_its_tuis_thread_and_the_subagents_it_spawned() {
        let home = aterm_tempfile::Builder::new()
            .prefix("codex-usage-conv-")
            .tempdir()
            .expect("temp dir");
        let day = home.path().join("sessions/2026/10/15");
        std::fs::create_dir_all(&day).expect("mkdir");
        let write = |name: &str, body: String, age_s: u64| {
            let path = day.join(name);
            std::fs::write(&path, body).expect("write");
            std::fs::File::options()
                .write(true)
                .open(&path)
                .and_then(|f| {
                    f.set_modified(SystemTime::now() - std::time::Duration::from_secs(age_s))
                })
                .expect("mtime");
            path
        };
        let root = write(
            &format!("rollout-2026-10-15T10-00-00-{ROOT}.jsonl"),
            meta(T0, ROOT, ROOT, None),
            30,
        );
        let sub = write(
            &format!("rollout-2026-10-15T10-05-00-{SUB}.jsonl"),
            meta(T0, SUB, ROOT, Some(ROOT)),
            20,
        );
        let other = write(
            &format!("rollout-2026-10-15T11-00-00-{OTHER}.jsonl"),
            meta(T0, OTHER, OTHER, None),
            10,
        );
        write(
            "rollout-2026-10-15T12-00-00-cut.jsonl",
            "{\"cut".to_owned(),
            5,
        );
        for named in [SUB, ROOT] {
            let c = conversation_rollouts(home.path(), named);
            assert_eq!(c.root, ROOT, "named {named}");
            assert_eq!(c.files, [root.clone(), sub.clone()], "named {named}");
            assert_eq!(c.subagents, 1);
            assert_eq!(c.unread, 1, "the cut first line");
        }
        assert_eq!(
            conversation_rollouts(home.path(), SUB).own.as_deref(),
            Some(sub.as_path())
        );
        // NEGATIVE CONTROL.
        let c = conversation_rollouts(home.path(), OTHER);
        assert_eq!(c.root, OTHER);
        assert_eq!(c.files, [other]);
        assert_eq!(c.subagents, 0);
        // A thread with no rollout yet: nothing of anyone's.
        let none = conversation_rollouts(home.path(), "01900000-0000-7000-8000-00000000000f");
        assert!(none.own.is_none() && none.files.is_empty());
    }

    /// A rollout stamp to the millisecond: the fraction's first three
    /// digits, padded.
    #[test]
    fn a_stamp_reads_to_the_millisecond() {
        let s = unix("2026-08-01T17:36:47Z");
        assert_eq!(stamp_ms("2026-08-01T17:36:47.998Z"), Some(s * 1000 + 998));
        assert_eq!(stamp_ms("2026-08-01T17:36:47.9Z"), Some(s * 1000 + 900));
        assert_eq!(
            stamp_ms("2026-08-01T17:36:47.123456Z"),
            Some(s * 1000 + 123)
        );
        assert_eq!(stamp_ms("2026-08-01T17:36:47Z"), Some(s * 1000));
        assert_eq!(fixtures::stamp(s * 1000 + 7), "2026-08-01T17:36:47.007Z");
        assert_eq!(stamp_ms("2026-08-01T17:36:47.1+00:00"), None);
    }
}
