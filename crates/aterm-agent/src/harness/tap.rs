// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The usage tap (`docs/DESIGN-claude-accounts-v2-2026-10-15.md` §3.1, §3.2). Claude Code's own
//! status-line setting, passed as `--settings` on the relaunch lines aterm types, runs
//! `<aterm> harness tap` after every main-agent reply and every [`TAP_REFRESH_S`] seconds. The
//! tap writes the usage figures from its stdin into one private file per process ([`Reading`],
//! under [`rl_dir`]) and prints the row Claude Code draws under its composer ([`Reading::line`]).
//! The tab's host reads its own file at decided turn ends, keeps a reading only where it is
//! [`Fresh`] and records it for the machine's own account ([`super::accounts::note_reading`]). A
//! reading moves no session.
//!
//! * The switch is `[harness] usage_tap`, off by default. A session tapped while it was on keeps
//!   its tap until its next relaunch sheds it, and the host reads and records its file till then.
//! * With autopilot Off (`docs/DESIGN-harness-autopilot-2026-10-15.md` §7) the host stops the
//!   session's worker, or its loop parks, so no decided turn end reads the file: a tapped Claude
//!   Code keeps drawing its row and writing its file until its next relaunch, but nothing of it is
//!   recorded, nor a silent tap noticed. Another tapped session on the same login records the same
//!   figures.
//! * [`gate`] adds it only for a Claude Code on the machine's own stored login whose settings claim
//!   neither the status line nor the login, from an aterm at a lasting, shell-inert path
//!   ([`stable_exe`]); aterm never chains a person's own status line and never runs a command read
//!   from settings. The machine's own login is the default one or, with a roster of one account,
//!   the login its row is bound to ([`own_login`]), so the tap follows that account through its
//!   renewals. The chain is read when the line is typed, and the line's `--settings` outranks the
//!   user, project and local documents, so a status line set later shows only from the next
//!   relaunch, whose gate refuses the tap.
//! * [`super::upgrade::rewrite_argv`] drops a `--settings` value exactly [`is_ours`] and carries
//!   every other verbatim, so the resume line a person is shown never carries it.
//! * The file is `<sid>-<CLAUDE_PID>.json`, `sid` the tab's own id (`ATERM_PARENT_SESSION_ID`; a
//!   nested aterm's `ATERM_SESSION_ID` is the id it adopts): mode 0600, temp file renamed, in a
//!   0700 directory. It never holds stdin as it came, nor a token.
//! * The row, between the composer's bottom rule and the mode row, is never empty
//!   (`5h 42% · 7d 61%`, or [`NO_FIGURES`]): on 2.1.282 a status line that prints nothing still
//!   takes a row, and the footer's readers stop at a blank one, short of the mode row.
//!
//! Measured on Claude Code 2.1.300 (`docs/measured/real-claude-lab-2026-10-15.md`, "Usage
//! figures"): stdin carries `rate_limits.{five_hour,seven_day}.{used_percentage,resets_at}`,
//! `cost.total_api_duration_ms`, `session_id`, `prompt_cache.{warm,expires_at}` and `version`; the
//! environment carries `CLAUDE_PID`, `ATERM_PARENT_SESSION_ID` and both login variables, not
//! `CLAUDE_CODE_OAUTH_TOKEN`; an idle process re-reports the figures it holds, so a reading is only
//! as fresh as its last request; `disableAllHooks` stops the command, and so does a standing
//! permission box until it is answered, which is why silence is judged only at idle points.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::accounts;
use super::netwatch::{self, Source};

/// How often Claude Code re-runs the tap while idle (`refreshInterval`,
/// seconds): a constant, not a key.
pub const TAP_REFRESH_S: u32 = 10;

/// The file's format version ([`Reading::to_json`]).
pub const FORMAT_V: u64 = 1;

/// The most of stdin the verb reads: a payload longer than this is unusable,
/// drained and dropped.
pub const STDIN_MAX: usize = 256 * 1024;

/// The most of a reading's file the host reads.
const FILE_MAX: u64 = 16 * 1024;

/// A tapped process that served a turn, whose file is missing or older than this at two decided
/// points this far apart, is silent ([`Silence`]).
pub const SILENT_S: i64 = 30;

/// A file older than this is swept ([`sweep`]), whoever wrote it.
pub const SWEEP_AFTER_S: u64 = 24 * 3600;

/// What follows the executable in the status-line command.
const COMMAND_TAIL: &str = " harness tap";

/// The value up to the executable ([`settings_value`]).
const VALUE_HEAD: &str = r#"{"statusLine":{"type":"command","command":""#;

/// The value after the command ([`settings_value`]).
const VALUE_TAIL: &str = r#"","refreshInterval":10}}"#;

/// The row printed when there are no figures ([`Reading::line`]).
pub const NO_FIGURES: &str = "5h \u{2013} \u{b7} 7d \u{2013}";

/// Whether `exe` can be named in the status-line command: an absolute UTF-8 path every byte of
/// which is a letter, a digit or one of `/ . _ - +`. Claude Code runs the command through a shell,
/// so the path must be inert to one. A space is refused too: its round trip through each shell's
/// quoting of the relaunch line is unmeasured, and the installed `aterm.app` path has none.
#[must_use]
pub fn exe_admissible(exe: &Path) -> bool {
    let Some(text) = exe.to_str() else {
        return false;
    };
    exe.is_absolute()
        && text.len() > 1
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+'))
}

/// The `--settings` value for `exe`, byte for byte
/// `{"statusLine":{"type":"command","command":"<exe> harness tap","refreshInterval":10}}`, or
/// `None` where `exe` is not [`exe_admissible`].
#[must_use]
pub fn settings_value(exe: &Path) -> Option<String> {
    exe_admissible(exe).then(|| {
        format!(
            "{VALUE_HEAD}{}{COMMAND_TAIL}{VALUE_TAIL}",
            exe.to_str().unwrap_or_default()
        )
    })
}

/// Whether a `--settings` value is exactly aterm's tap ([`settings_value`] of some admissible
/// executable): the only `--settings` a relaunch drops. A key more, another command, an argument
/// after `tap`, a relative path or a space anywhere makes it another's, carried verbatim.
#[must_use]
pub fn is_ours(value: &str) -> bool {
    value
        .strip_prefix(VALUE_HEAD)
        .and_then(|rest| rest.strip_suffix(VALUE_TAIL))
        .and_then(|command| command.strip_suffix(COMMAND_TAIL))
        .is_some_and(|exe| settings_value(Path::new(exe)).as_deref() == Some(value))
}

/// Whether `argv` (`argv[0]` first) carries aterm's tap: a `--settings` (as
/// `--settings v` or `--settings=v`) whose value [`is_ours`]. The host reads
/// a process's file only where its argv says it was tapped.
#[must_use]
pub fn carried_by(argv: &[String]) -> bool {
    let mut it = argv.iter().skip(1);
    while let Some(tok) = it.next() {
        if tok == "--" {
            return false;
        }
        let value = match tok.strip_prefix("--settings") {
            Some("") => it.next().map(String::as_str),
            Some(rest) => rest.strip_prefix('='),
            None => None,
        };
        if value.is_some_and(is_ours) {
            return true;
        }
    }
    false
}

/// This aterm's canonical `current_exe`, where it is at a lasting place (`atpkg::stub::lasting`:
/// not a temp or cargo target directory, nor a translocated or mounted copy) and
/// [`exe_admissible`]; `None` otherwise, and then no line carries the tap. Computed once per
/// process.
#[must_use]
pub fn stable_exe() -> Option<PathBuf> {
    static EXE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
        (atpkg::stub::lasting(&exe) && exe_admissible(&exe)).then_some(exe)
    })
    .clone()
}

/// Where the readings are: `<aterm state>/harness/claude-harness/rl`. `None` where no state root
/// resolves.
#[must_use]
pub fn rl_dir() -> Option<PathBuf> {
    crate::operator::default_state_root()
        .ok()
        .map(|root| rl_dir_in(&root.join("harness").join(super::cli::HARNESS)))
}

/// [`rl_dir`] under the harness state directory `state`.
#[must_use]
pub fn rl_dir_in(state: &Path) -> PathBuf {
    state.join("rl")
}

/// Whether `sid` is a tab id (`s-<hex>`): the only kind joined onto a path.
fn is_tab_id(sid: &str) -> bool {
    sid.strip_prefix("s-").is_some_and(|hex| {
        (1..=64).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// The file of process `pid` in tab `sid`: `<sid>-<pid>.json`. `None` for a
/// `sid` that is not a tab id or a zero `pid`.
#[must_use]
pub fn file_name(sid: &str, pid: u32) -> Option<String> {
    (is_tab_id(sid) && pid > 0).then(|| format!("{sid}-{pid}.json"))
}

// ---------------------------------------------------------------------------
// The reading
// ---------------------------------------------------------------------------

/// One window's figure as Claude Code hands it: whole percent used, and the
/// instant (Unix seconds) the window resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// `used_percentage`.
    pub pct: u32,
    /// `resets_at`.
    pub resets_at: i64,
}

/// One run of the tap, as its file holds it (format [`FORMAT_V`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// When it was written (Unix seconds).
    pub at: i64,
    /// The tab (`ATERM_PARENT_SESSION_ID`).
    pub sid: String,
    /// The Claude Code process (`CLAUDE_PID`).
    pub pid: u32,
    /// Its conversation (`session_id`), when one is named.
    pub session: Option<String>,
    /// The raw value of [`accounts::LOGIN_ENV`] in the tap's environment.
    pub login: Option<String>,
    /// The raw value of [`accounts::HOME_ENV`] there.
    pub home: Option<String>,
    /// `cost.total_api_duration_ms`: the process's API time so far.
    pub api_ms: u64,
    /// `rate_limits.five_hour`.
    pub five_hour: Option<Window>,
    /// `rate_limits.seven_day`.
    pub seven_day: Option<Window>,
    /// `prompt_cache.warm`.
    pub warm: Option<bool>,
    /// `prompt_cache.expires_at`, a number where the vendor gave one.
    pub cache_expires_at: Option<i64>,
    /// The vendor's `version`.
    pub version: Option<String>,
    /// How long the verb took up to its write, microseconds (the tap's own
    /// cost, beside its process's start).
    pub dur_us: u64,
}

/// What the tap reads of its environment ([`run`]): the tab, the process and
/// the two login variables, raw. Nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TapVars {
    /// `ATERM_PARENT_SESSION_ID`.
    pub sid: Option<String>,
    /// `CLAUDE_PID`.
    pub pid: Option<String>,
    /// [`accounts::LOGIN_ENV`].
    pub login: Option<String>,
    /// [`accounts::HOME_ENV`].
    pub home: Option<String>,
}

impl TapVars {
    /// The four variables of this process's environment.
    #[must_use]
    pub fn from_process() -> TapVars {
        let get = |key: &str| std::env::var(key).ok();
        TapVars {
            sid: get("ATERM_PARENT_SESSION_ID"),
            pid: get("CLAUDE_PID"),
            login: get(accounts::LOGIN_ENV),
            home: get(accounts::HOME_ENV),
        }
    }
}

/// A whole percent from the vendor: an integer in `0..=100`, or a finite
/// number in that range, rounded.
fn pct_of(value: &Value) -> Option<u32> {
    if let Some(n) = value.as_u64() {
        return u32::try_from(n).ok().filter(|p| *p <= 100);
    }
    let f = value
        .as_f64()
        .filter(|f| f.is_finite() && (0.0..=100.0).contains(f))?;
    // In range by the filter: the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(f.round() as u32)
}

/// `rate_limits.<name>` as a [`Window`]: both fields, or none.
fn window_of(limits: Option<&Value>, name: &str) -> Option<Window> {
    let w = limits?.get(name)?;
    Some(Window {
        pct: pct_of(w.get("used_percentage")?)?,
        resets_at: w.get("resets_at")?.as_i64().filter(|t| *t > 0)?,
    })
}

/// A short vendor word, kept only where it is printable ASCII and short.
fn word_of(value: Option<&Value>, max: usize) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty() && s.len() <= max && s.bytes().all(|b| (b' '..=b'~').contains(&b))
        })
        .map(str::to_owned)
}

impl Reading {
    /// The vendor's status-line payload `text` as a reading for the tab and
    /// process `vars` name, at `now`. `None` where `vars` names no tab or no
    /// process (nothing is written then). A payload that is not a JSON
    /// object is a reading with no figures.
    #[must_use]
    pub fn of_stdin(text: &str, vars: &TapVars, now: i64) -> Option<Reading> {
        let sid = vars.sid.clone().filter(|s| is_tab_id(s))?;
        let pid = vars
            .pid
            .as_deref()
            .and_then(|p| p.parse::<u32>().ok())
            .filter(|p| *p > 0)?;
        let doc = aterm_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_object)
            .unwrap_or(Value::Null);
        let limits = doc.get("rate_limits");
        let cache = doc.get("prompt_cache");
        Some(Reading {
            at: now,
            sid,
            pid,
            session: word_of(doc.get("session_id"), 64)
                .filter(|s| super::upgrade::is_session_id(s)),
            login: vars.login.clone(),
            home: vars.home.clone(),
            api_ms: doc
                .get("cost")
                .and_then(|c| c.get("total_api_duration_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            five_hour: window_of(limits, "five_hour"),
            seven_day: window_of(limits, "seven_day"),
            warm: cache.and_then(|c| c.get("warm")).and_then(Value::as_bool),
            cache_expires_at: cache
                .and_then(|c| c.get("expires_at"))
                .and_then(Value::as_i64),
            version: word_of(doc.get("version"), 32),
            dur_us: 0,
        })
    }

    /// Whether it carries a figure.
    #[must_use]
    pub fn has_figures(&self) -> bool {
        self.five_hour.is_some() || self.seven_day.is_some()
    }

    /// The row the tap prints: `5h 42% · 7d 61%`, a window with no figure as
    /// `–` ([`NO_FIGURES`] with none). At most 17 columns, so a vendor notice
    /// drawn on the right of the same row keeps its place.
    #[must_use]
    pub fn line(&self) -> String {
        line_of(self.five_hour, self.seven_day)
    }

    /// The file's text, one JSON object, keys sorted.
    #[must_use]
    pub fn to_json(&self) -> String {
        let window = |w: Option<Window>| {
            w.map_or(Value::Null, |w| {
                let mut o = Map::new();
                o.insert("pct".to_owned(), Value::from(u64::from(w.pct)));
                o.insert("resets_at".to_owned(), Value::from(w.resets_at));
                Value::Object(o)
            })
        };
        let mut cache = Map::new();
        cache.insert("warm".to_owned(), Value::from(self.warm));
        cache.insert("expires_at".to_owned(), Value::from(self.cache_expires_at));
        let mut o = Map::new();
        o.insert("v".to_owned(), Value::from(FORMAT_V));
        o.insert("at".to_owned(), Value::from(self.at));
        o.insert("sid".to_owned(), Value::from(self.sid.clone()));
        o.insert("pid".to_owned(), Value::from(u64::from(self.pid)));
        o.insert("session".to_owned(), Value::from(self.session.clone()));
        o.insert("login".to_owned(), Value::from(self.login.clone()));
        o.insert("home".to_owned(), Value::from(self.home.clone()));
        o.insert("api_ms".to_owned(), Value::from(self.api_ms));
        o.insert("five_hour".to_owned(), window(self.five_hour));
        o.insert("seven_day".to_owned(), window(self.seven_day));
        o.insert("cache".to_owned(), Value::Object(cache));
        o.insert("version".to_owned(), Value::from(self.version.clone()));
        o.insert("dur_us".to_owned(), Value::from(self.dur_us));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    }

    /// A file's text as a reading: format [`FORMAT_V`] and every field of its
    /// type, or `None`.
    #[must_use]
    pub fn of_json(text: &str) -> Option<Reading> {
        let doc = aterm_json::from_str::<Value>(text).ok()?;
        if doc.get("v")?.as_u64()? != FORMAT_V {
            return None;
        }
        let opt_str = |key: &str| -> Option<Option<String>> {
            match doc.get(key)? {
                Value::Null => Some(None),
                Value::String(s) => Some(Some(s.clone())),
                _ => None,
            }
        };
        let window = |key: &str| -> Option<Option<Window>> {
            match doc.get(key)? {
                Value::Null => Some(None),
                w => Some(Some(Window {
                    pct: u32::try_from(w.get("pct")?.as_u64()?)
                        .ok()
                        .filter(|p| *p <= 100)?,
                    resets_at: w.get("resets_at")?.as_i64()?,
                })),
            }
        };
        let cache = doc.get("cache")?;
        Some(Reading {
            at: doc.get("at")?.as_i64()?,
            sid: doc.get("sid")?.as_str()?.to_owned(),
            pid: u32::try_from(doc.get("pid")?.as_u64()?).ok()?,
            session: opt_str("session")?,
            login: opt_str("login")?,
            home: opt_str("home")?,
            api_ms: doc.get("api_ms")?.as_u64()?,
            five_hour: window("five_hour")?,
            seven_day: window("seven_day")?,
            warm: cache.get("warm").and_then(Value::as_bool),
            cache_expires_at: cache.get("expires_at").and_then(Value::as_i64),
            version: opt_str("version")?,
            dur_us: doc.get("dur_us").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    /// The reading of process `pid` in tab `sid` from its own file in `dir` ([`file_name`]), with
    /// the file's mtime (Unix seconds). Only a regular file (a symlink is refused) of bounded size,
    /// modified at or after `started` (the process's kernel start, so a file a reused pid left is
    /// not its), naming the same tab and process.
    #[must_use]
    pub fn read(dir: &Path, sid: &str, pid: u32, started: i64) -> Option<(Reading, i64)> {
        let path = dir.join(file_name(sid, pid)?);
        let mtime = file_mtime(&path)?;
        if mtime < started {
            return None;
        }
        let text = read_regular(&path)?;
        Reading::of_json(&text)
            .filter(|r| r.sid == sid && r.pid == pid)
            .map(|r| (r, mtime))
    }

    /// The reading as the accounts state keeps a sample
    /// ([`accounts::Sample`]): the two windows, at `at`.
    #[must_use]
    pub fn sample(&self, at: i64) -> accounts::Sample {
        accounts::Sample {
            session_pct: self.five_hour.map(|w| w.pct),
            session_reset: self.five_hour.map(|w| w.resets_at),
            week_pct: self.seven_day.map(|w| w.pct),
            week_reset: self.seven_day.map(|w| w.resets_at),
            at,
            by: String::new(),
        }
    }
}

/// The tap's row for two windows ([`Reading::line`]).
#[must_use]
pub fn line_of(five_hour: Option<Window>, seven_day: Option<Window>) -> String {
    let pct =
        |w: Option<Window>| w.map_or_else(|| "\u{2013}".to_owned(), |w| format!("{}%", w.pct));
    format!("5h {} \u{b7} 7d {}", pct(five_hour), pct(seven_day))
}

/// The modification time of the regular file at `path` (Unix seconds), not
/// following a symlink; `None` for anything else.
fn file_mtime(path: &Path) -> Option<i64> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let at = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    i64::try_from(at.as_secs()).ok()
}

/// The text of the regular file at `path`, opened without following a
/// symlink, at most [`FILE_MAX`] bytes.
fn read_regular(path: &Path) -> Option<String> {
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = open.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(FILE_MAX + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= FILE_MAX).then_some(text)
}

// ---------------------------------------------------------------------------
// The verb
// ---------------------------------------------------------------------------

/// Write `text` to `dir/name`: the directory made 0700, a temp file of mode
/// 0600 (never through a link) renamed over it. No sync: a reading lost to a
/// crash is replaced within [`TAP_REFRESH_S`] seconds.
fn write_reading(dir: &Path, name: &str, text: &str) -> io::Result<()> {
    aterm_types::fs_restricted::ensure_private_dir(dir)?;
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let written = open
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    match written.and_then(|()| std::fs::rename(&tmp, &path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Read stdin as the verb takes it: at most [`STDIN_MAX`] bytes of UTF-8,
/// the rest drained. `None` for a payload over the bound, not UTF-8, or one
/// that could not be read.
fn read_payload(stdin: &mut dyn Read) -> Option<String> {
    let mut bytes = Vec::new();
    let read = stdin
        .take(STDIN_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .ok();
    if bytes.len() > STDIN_MAX {
        // Drained, so the writer never meets a closed pipe.
        let _ = io::copy(stdin, &mut io::sink());
        return None;
    }
    read?;
    String::from_utf8(bytes).ok()
}

/// `aterm harness tap`, Claude Code's status-line command: reads the payload from `stdin` (none
/// where `stdin_is_tty`), writes the reading of the process `vars` name into `dir` (`None`:
/// nothing written) and prints its row on `out`. Always exits 0 with one non-empty line: a failing
/// or silent command leaves the row blank or wrong. Reads no variable but the four of [`TapVars`].
pub fn run(
    stdin: &mut dyn Read,
    stdin_is_tty: bool,
    vars: &TapVars,
    dir: Option<&Path>,
    now: i64,
    out: &mut dyn Write,
) -> std::process::ExitCode {
    let started = std::time::Instant::now();
    let payload = if stdin_is_tty {
        None
    } else {
        read_payload(stdin)
    };
    let reading = Reading::of_stdin(payload.as_deref().unwrap_or(""), vars, now);
    if let (Some(reading), Some(dir)) = (&reading, dir) {
        let reading = Reading {
            dur_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            ..reading.clone()
        };
        if let Some(name) = file_name(&reading.sid, reading.pid) {
            let _ = write_reading(dir, &name, &reading.to_json());
        }
    }
    let line = reading
        .as_ref()
        .map_or_else(|| NO_FIGURES.to_owned(), Reading::line);
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    std::process::ExitCode::SUCCESS
}

/// Remove from `dir` every reading whose process is gone (`alive` answers
/// for a pid) or that is older than [`SWEEP_AFTER_S`] at `now`, and every
/// temp file a tap left: what a host does as it starts. Only names this
/// module writes are touched, and never through a link.
pub fn sweep(dir: &Path, now: i64, alive: &dyn Fn(u32) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        if name.starts_with(".s-") && name.contains(".json.tmp") {
            if file_mtime(&path).is_some_and(|at| now.saturating_sub(at) > SILENT_S) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        let Some((sid, pid)) = name
            .strip_suffix(".json")
            .and_then(|stem| stem.rsplit_once('-'))
        else {
            continue;
        };
        let Some(pid) = pid.parse::<u32>().ok().filter(|_| is_tab_id(sid)) else {
            continue;
        };
        let Some(at) = file_mtime(&path) else {
            continue;
        };
        let old = u64::try_from(now.saturating_sub(at)).unwrap_or(0) > SWEEP_AFTER_S;
        if old || !alive(pid) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ---------------------------------------------------------------------------
// Where the tap is added
// ---------------------------------------------------------------------------

/// `[harness] usage_tap` on, for the relaunch lines one pass types
/// (`super::upgrade_drive::Opts::usage_tap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Armed {
    /// The aterm the tap's command names ([`stable_exe`]).
    pub exe: PathBuf,
    /// The login of the machine's one account where it has a directory of its own, as this
    /// machine's logins bind the row (a renewal rebinds it), read as the pass began
    /// (`accounts::lone_login_at`). A line on it carries the tap as one on the default login does
    /// ([`own_login`]). `None`: the default login alone.
    pub lone_login: Option<String>,
}

/// Whether a line's login is the machine's own, for [`gate`]: the default home (`home` unset), and
/// the default login (`login` unset or empty) or the one account's (`lone`,
/// [`Armed::lone_login`]), byte for byte.
#[must_use]
pub fn own_login(login: Option<&str>, home: Option<&str>, lone: Option<&str>) -> bool {
    home.is_none() && (login.is_none_or(str::is_empty) || login == lone)
}

/// What [`gate`] decides on, gathered by the relaunch's plan.
#[derive(Debug, Clone, Copy)]
pub struct GateFacts<'a> {
    /// `[harness] usage_tap` on: the executable to name ([`stable_exe`]);
    /// `None`, off.
    pub exe: Option<&'a Path>,
    /// The process the line replaces authenticates by its stored login, as its exec environment
    /// reads (`super::relaunch::Snapshot::stored_login`).
    pub stored_login: bool,
    /// The login variable's value the line carries ([`accounts::LOGIN_ENV`]):
    /// unset or empty is the machine's own login.
    pub login: Option<&'a str>,
    /// The login of the machine's one account ([`Armed::lone_login`]), which [`own_login`] counts
    /// as the machine's own.
    pub lone_login: Option<&'a str>,
    /// The Claude home the line carries ([`accounts::HOME_ENV`]): set at all
    /// is not the default home.
    pub home: Option<&'a str>,
    /// The argv the line is made from, `argv[0]` first.
    pub argv: &'a [String],
    /// Every document of the settings chain Claude Code reads for it, and
    /// its managed preference domains, as the chain's reader returned them.
    pub settings: &'a [(String, Source)],
    /// The vendor's profile store holds a config
    /// ([`accounts::profile_configured`]).
    pub profile_store: bool,
}

/// The `--settings` value a relaunch line carries, or `None`. It needs the switch on and the
/// executable admissible; the machine's own stored login in the default home ([`own_login`]); no
/// `--settings` left once aterm's own is shed, and no `--bare` (which drops the status line); every
/// settings document readable, none setting `statusLine`, `disableAllHooks: true`, `apiKeyHelper`
/// or a credential, profile or route in an `env` block; no managed preference domain (this reader
/// cannot open one); and no profile store config. Pure.
#[must_use]
pub fn gate(f: &GateFacts<'_>) -> Option<String> {
    let value = settings_value(f.exe?)?;
    if !(f.stored_login && own_login(f.login, f.home, f.lone_login)) || f.profile_store {
        return None;
    }
    let flags = super::upgrade::carried_names(f.argv).ok()?;
    if flags
        .iter()
        .any(|name| matches!(*name, "--settings" | "--bare"))
    {
        return None;
    }
    // The credential side, as the accounts' judgment reads it: an `env`
    // block naming a credential, a profile or a route, the login or the
    // home; an `apiKeyHelper`; a document that cannot be read or parsed.
    let stored = accounts::ProcessLogin::of_env([])
        .with_settings(f.settings)
        .stored_login;
    let quiet = f.settings.iter().all(|(_, source)| match source {
        Source::Missing => true,
        Source::Unreadable => false,
        Source::Text(text) => {
            netwatch::settings_of(text).is_some_and(|read| !read.status_line && !read.hooks_off)
        }
    });
    (stored && quiet).then_some(value)
}

// ---------------------------------------------------------------------------
// The host's half: freshness and silence
// ---------------------------------------------------------------------------

/// The freshness rule (design §3.2), per tab: a reading counts only where it is the first with a
/// figure for its process, its process was served a request since the last one (`api_ms` rose),
/// or its figures changed. An idle process re-reports what it holds every [`TAP_REFRESH_S`]
/// seconds; that is no news, and a figure it repeats may be another session's past.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fresh {
    /// The last reading admitted: its process, API time and figures.
    last: Option<(u32, u64, Option<Window>, Option<Window>)>,
}

impl Fresh {
    /// Whether `reading` is fresh; an admitted one becomes the last.
    pub fn admit(&mut self, reading: &Reading) -> bool {
        if !reading.has_figures() {
            return false;
        }
        let fresh = self.last.is_none_or(|(pid, api_ms, five, seven)| {
            pid != reading.pid
                || reading.api_ms > api_ms
                || (five, seven) != (reading.five_hour, reading.seven_day)
        });
        if fresh {
            self.last = Some((
                reading.pid,
                reading.api_ms,
                reading.five_hour,
                reading.seven_day,
            ));
        }
        fresh
    }
}

/// What one decided point says of a tapped process's file ([`Silence::step`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// Nothing changed.
    Same,
    /// The process went silent at this point: marked once.
    Silent,
    /// The mark ends: its process is heard again, or another process holds
    /// the tab now ([`Silence::untapped`]).
    Again,
}

/// The silent-tap rule (design §3.1): a tapped process up at least [`SILENT_S`] seconds that has
/// served a turn, whose file is missing or older than [`SILENT_S`] at two decided points at least
/// that far apart, is marked silent, once per process. A fresh file unmarks it, and so does
/// another process taking the tab: the mark ends with its process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Silence {
    /// The process the strikes are about.
    pid: u32,
    /// When the first strike was taken.
    strike: Option<i64>,
    /// Marked silent.
    silent: bool,
}

impl Silence {
    /// One decided point at `now` for process `pid`, up `up_s` seconds,
    /// `served` a turn since it started, its file `file_age` seconds old
    /// (`None`: no file). A new process's first point takes a strike at
    /// most, and a mark on the one before ends there ([`Heard::Again`]).
    pub fn step(
        &mut self,
        now: i64,
        pid: u32,
        up_s: i64,
        served: bool,
        file_age: Option<i64>,
    ) -> Heard {
        let ended = pid != self.pid && self.untapped(pid) == Heard::Again;
        let heard = self.judge(now, up_s, served, file_age);
        if ended { Heard::Again } else { heard }
    }

    /// Process `pid` now holds the tab and carries no tap, so it is never judged: the rule starts
    /// over on it, and a standing mark ends ([`Heard::Again`]).
    pub fn untapped(&mut self, pid: u32) -> Heard {
        let before = std::mem::replace(
            self,
            Silence {
                pid,
                ..Silence::default()
            },
        );
        if before.silent {
            Heard::Again
        } else {
            Heard::Same
        }
    }

    /// [`Self::step`] on the process the strikes are about.
    fn judge(&mut self, now: i64, up_s: i64, served: bool, file_age: Option<i64>) -> Heard {
        let heard = file_age.is_some_and(|age| age <= SILENT_S);
        if heard {
            self.strike = None;
            return if std::mem::take(&mut self.silent) {
                Heard::Again
            } else {
                Heard::Same
            };
        }
        if self.silent || up_s < SILENT_S || !served {
            if !self.silent {
                self.strike = None;
            }
            return Heard::Same;
        }
        match self.strike {
            None => {
                self.strike = Some(now);
                Heard::Same
            }
            Some(first) if now.saturating_sub(first) >= SILENT_S => {
                self.silent = true;
                Heard::Silent
            }
            Some(_) => Heard::Same,
        }
    }

    /// Whether the process is marked silent.
    #[must_use]
    pub fn silent(&self) -> bool {
        self.silent
    }
}

#[path = "tap_tests.rs"]
#[cfg(test)]
mod tests;
