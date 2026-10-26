// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code status-line usage tap ([`TAP_REFRESH_S`], [`Reading`], [`rl_dir`],
//! [`Reading::line`], [`NO_FIGURES`]): launch gating ([`gate`], [`stable_exe`], [`own_login`],
//! [`is_ours`], [`super::upgrade::rewrite_argv`]) and fresh reading recording ([`Fresh`],
//! [`Fresh::beside`], [`super::accounts::note_reading`]).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::accounts;
use super::netwatch::{self, Source};

/// How often Claude Code re-runs the tap while idle (`refreshInterval`, seconds).
pub const TAP_REFRESH_S: u32 = 10;

/// The file's format version ([`Reading::to_json`]).
pub const FORMAT_V: u64 = 1;

/// The most of stdin the verb reads.
pub const STDIN_MAX: usize = 256 * 1024;

/// The most of a reading's file the host reads.
const FILE_MAX: u64 = 16 * 1024;

/// A tapped process that served a turn.
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

/// Whether `exe` can be named in the status-line command.
#[must_use]
pub fn exe_admissible(exe: &Path) -> bool {
    command_spelling(exe).is_some()
}

/// How `exe` is spelled in the status-line command, or `None` where it is not
/// [`exe_admissible`].
fn command_spelling(exe: &Path) -> Option<String> {
    let text = exe.to_str()?;
    let inert = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+');
    if cfg!(windows) {
        let text = text.strip_prefix(r"\\?\").unwrap_or(text);
        let bytes = text.as_bytes();
        let drive = bytes.len() > 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
        let spelled = text.replace('\\', "/");
        return (drive && spelled.bytes().skip(2).all(inert)).then_some(spelled);
    }
    (exe.is_absolute() && text.len() > 1 && text.bytes().all(inert)).then(|| text.to_owned())
}

/// The `--settings` value for `exe`.
#[must_use]
pub fn settings_value(exe: &Path) -> Option<String> {
    settings_value_carrying(exe, false)
}

/// The flag of the tap that carries the owner's own statusLine ([`settings_value_carrying`]):
/// the tap writes its reading, then runs the command the user's own `settings.json` names
/// ([`owner_command`]), read when it runs, so the line names no command and is as short as the
/// plain tap's.
pub const CARRY_FLAG: &str = "--carry";

/// The flag of the carrying tap as main's builds from 864de5a37b spelled it, followed by the
/// lowercase hex of the owner's command: still exactly ours ([`carrying_of`]), so a relaunch
/// sheds it, and still served by the verb, which runs the command the user's `settings.json`
/// names now. Never written: that value put the whole command on the typed line, which a
/// Darwin tty's line cap then refused, so every relaunch fell back to no tap at all.
pub const LEGACY_CHAIN_FLAG: &str = "--then";

/// The longest owner's statusLine command the tap carries, in bytes ([`settings_line`],
/// [`owner_command`]).
pub const CHAIN_MAX: usize = 4096;

/// The environment variable the carrying tap sets on the owner's command ([`chain_command`]): a
/// carrying tap that finds it set runs no owner's command again (one whose own command is the
/// carrying tap would otherwise exec itself without end), and prints its own row.
pub const CARRIED_ENV: &str = "ATERM_TAP_CARRIED";

/// THE `--settings` VALUE FOR `exe`, carrying the owner's own statusLine where `carry` (owner
/// ruling R-12): `<exe> harness tap --carry`, so the tap writes its reading and then runs the
/// owner's command ([`owner_command`]) on the same payload, whose row is the one shown;
/// `<exe> harness tap` where not. Its length does not depend on the owner's command: a relaunch
/// line with room for the plain tap has room for this one but [`CARRY_FLAG`]'s eight bytes.
/// `None` where `exe` cannot be named.
#[must_use]
pub fn settings_value_carrying(exe: &Path, carry: bool) -> Option<String> {
    let exe = command_spelling(exe)?;
    let tail = if carry {
        format!(" {CARRY_FLAG}")
    } else {
        String::new()
    };
    Some(format!("{VALUE_HEAD}{exe}{COMMAND_TAIL}{tail}{VALUE_TAIL}"))
}

/// Whether `hex` is lowercase hex of a non-empty command no longer than [`CHAIN_MAX`] that holds
/// no NUL, as [`LEGACY_CHAIN_FLAG`]'s operand was written.
fn legacy_operand(hex: &str) -> bool {
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    !pairs.is_empty()
        && rest.is_empty()
        && pairs.len() <= CHAIN_MAX
        && pairs
            .iter()
            .map(|&[hi, lo]| Some((digit(hi)? << 4) | digit(lo)?))
            .collect::<Option<Vec<u8>>>()
            .is_some_and(|bytes| !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok())
}

/// Whether an exact tap value carries the owner's own statusLine (`Some(false)`: the plain tap),
/// or `None` where `value` is not exactly aterm's tap ([`is_ours`]): [`settings_value_carrying`]'s,
/// or [`LEGACY_CHAIN_FLAG`]'s.
#[must_use]
pub fn carrying_of(value: &str) -> Option<bool> {
    let command = value
        .strip_prefix(VALUE_HEAD)
        .and_then(|rest| rest.strip_suffix(VALUE_TAIL))?;
    let (exe, carry) = match command.strip_suffix(COMMAND_TAIL) {
        Some(exe) => (exe, false),
        None => match command.strip_suffix(&format!("{COMMAND_TAIL} {CARRY_FLAG}")) {
            Some(exe) => (exe, true),
            None => {
                let (head, hex) = command.rsplit_once(&format!(" {LEGACY_CHAIN_FLAG} "))?;
                let exe = head.strip_suffix(COMMAND_TAIL)?;
                let spelled = command_spelling(Path::new(exe))?;
                return (spelled == exe && legacy_operand(hex)).then_some(true);
            }
        },
    };
    (settings_value_carrying(Path::new(exe), carry).as_deref() == Some(value)).then_some(carry)
}

/// Whether a `--settings` value is exactly aterm's tap, plain or carrying the owner's own
/// statusLine ([`carrying_of`]).
#[must_use]
pub fn is_ours(value: &str) -> bool {
    carrying_of(value).is_some()
}

/// THE OWNER'S OWN STATUSLINE COMMAND the carrying tap runs, from the text of the user's own
/// `settings.json` as it reads when the tap runs: the plain command line
/// ([`netwatch::Settings::status_command`]), at most [`CHAIN_MAX`] bytes with no NUL, where the
/// document sets no `disableAllHooks`; `None` for anything else (no statusLine, another shape, an
/// unparsed document), and the tap then prints its own row, as the plain tap does. Read at run
/// time, not when the line was typed, so the line names no command, and a statusLine the owner
/// edits shows from the next run, as Claude Code's own reload of the file would show it.
#[must_use]
pub fn owner_command(user_settings: &str) -> Option<String> {
    let read = netwatch::settings_of(user_settings)?;
    if read.hooks_off {
        return None;
    }
    read.status_command
        .filter(|cmd| cmd.len() <= CHAIN_MAX && !cmd.contains('\0'))
}

/// The most of the user's `settings.json` the carrying tap reads ([`read_owner_command`]).
const OWNER_SETTINGS_MAX: u64 = 1024 * 1024;

/// [`owner_command`] of the user's own `settings.json` under the user home `home`
/// (`<home>/.claude/settings.json`), opened without following a symlink at its last component
/// and read to at most a mebibyte; `None` where it cannot be.
#[must_use]
pub fn read_owner_command(home: &Path) -> Option<String> {
    let path = home.join(".claude").join("settings.json");
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
    file.take(OWNER_SETTINGS_MAX + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > OWNER_SETTINGS_MAX {
        return None;
    }
    owner_command(&text)
}

/// Whether `argv` (`argv[0]` first) carries aterm's tap.
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

/// This aterm's canonical `current_exe`.
#[must_use]
pub fn stable_exe() -> Option<PathBuf> {
    static EXE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    EXE.get_or_init(|| {
        let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
        let exe = tap_image(&exe, |path| path.is_file())?;
        (atpkg::stub::lasting(&exe) && exe_admissible(&exe)).then_some(exe)
    })
    .clone()
}

/// The image the tap's command runs for a running `exe`.
pub(crate) fn tap_image(exe: &Path, is_file: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let windowed = cfg!(windows)
        && exe
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| {
                stem.eq_ignore_ascii_case("aterm-gui")
                    || stem.eq_ignore_ascii_case("aterm-windowed")
            });
    if !windowed {
        return Some(exe.to_path_buf());
    }
    let console = exe.with_file_name("aterm.exe");
    is_file(&console).then_some(console)
}

/// Where the readings live.
#[must_use]
pub fn rl_dir() -> Option<PathBuf> {
    aterm_types::dirs::state_dir()
        .map(|root| rl_dir_in(&root.join("harness").join(super::cli::HARNESS)))
}

/// [`rl_dir`] under the harness state directory `state`.
#[must_use]
pub fn rl_dir_in(state: &Path) -> PathBuf {
    state.join("rl")
}

/// Whether `sid` is a tab id (`s-<hex>`).
pub(crate) fn is_tab_id(sid: &str) -> bool {
    sid.strip_prefix("s-").is_some_and(|hex| {
        (1..=64).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// The file of process `pid` in tab `sid`.
#[must_use]
pub fn file_name(sid: &str, pid: u32) -> Option<String> {
    (is_tab_id(sid) && pid > 0).then(|| format!("{sid}-{pid}.json"))
}

/// One window's figure as Claude Code hands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// `used_percentage`, rounded to a whole percent.
    pub pct: u32,
    /// `resets_at`.
    pub resets_at: i64,
    /// `used_percentage` in tenths of a percent.
    pub pct_x10: Option<u32>,
}

impl Window {
    /// The figure in tenths: [`Self::pct_x10`], else the whole percent.
    #[must_use]
    pub fn x10(&self) -> u32 {
        self.pct_x10.unwrap_or(self.pct.saturating_mul(10))
    }
}

/// One run of the tap, as its file holds it (format [`FORMAT_V`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// When its figures were first reported since its process was last served (Unix seconds).
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
    /// `cost.total_api_duration_ms`.
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
    /// The process's model (`model.id`), where it is a model id.
    pub model: Option<String>,
    /// How long the verb took up to its write, microseconds.
    pub dur_us: u64,
}

/// What the tap reads of its environment ([`run`]).
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

/// A percent from the vendor, an integer or a finite number in `0..=100`,
/// as the whole percent it rounds to and in tenths, rounded.
fn pct_of(value: &Value) -> Option<(u32, u32)> {
    if let Some(n) = value.as_u64() {
        let pct = u32::try_from(n).ok().filter(|p| *p <= 100)?;
        return Some((pct, pct * 10));
    }
    let f = value
        .as_f64()
        .filter(|f| f.is_finite() && (0.0..=100.0).contains(f))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((f.round() as u32, (f * 10.0).round() as u32))
}

/// `rate_limits.<name>` as a [`Window`]: both fields, or none.
fn window_of(limits: Option<&Value>, name: &str) -> Option<Window> {
    let w = limits?.get(name)?;
    let (pct, x10) = pct_of(w.get("used_percentage")?)?;
    Some(Window {
        pct,
        resets_at: w.get("resets_at")?.as_i64().filter(|t| *t > 0)?,
        pct_x10: Some(x10),
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
    /// The vendor's status-line payload `text` as a reading for the tab and process `vars` name.
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
            model: word_of(doc.get("model").and_then(|m| m.get("id")), 64)
                .filter(|m| super::upgrade::is_model_id(m)),
            dur_us: 0,
        })
    }

    /// Whether it carries a figure.
    #[must_use]
    pub const fn has_figures(&self) -> bool {
        self.five_hour.is_some() || self.seven_day.is_some()
    }

    /// The row the tap prints.
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
                o.insert("pct_x10".to_owned(), Value::from(w.pct_x10));
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
        o.insert("model".to_owned(), Value::from(self.model.clone()));
        o.insert("dur_us".to_owned(), Value::from(self.dur_us));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    }

    /// A file's text as a reading.
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
                    pct_x10: w
                        .get("pct_x10")
                        .and_then(Value::as_u64)
                        .and_then(|p| u32::try_from(p).ok())
                        .filter(|p| *p <= 1000),
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
            model: doc
                .get("model")
                .and_then(Value::as_str)
                .filter(|m| super::upgrade::is_model_id(m))
                .map(str::to_owned),
            dur_us: doc.get("dur_us").and_then(Value::as_u64).unwrap_or(0),
        })
    }

    /// The reading of process `pid` in tab `sid` from its own file in `dir` ([`file_name`]).
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

    /// The reading as the accounts state records it ([`accounts::note_reading`]).
    #[must_use]
    pub fn tap_read(
        &self,
        beside: Beside,
        key: Option<super::profile::AccountKey>,
    ) -> accounts::TapRead {
        let window = |w: Option<Window>, refilled: Option<u32>| {
            w.map(|w| accounts::WindowRead {
                pct_x10: w.x10(),
                reset: w.resets_at,
                refilled,
            })
        };
        accounts::TapRead {
            five_hour: window(self.five_hour, beside.refilled[0]),
            seven_day: window(self.seven_day, beside.refilled[1]),
            at: self.at,
            seen_before: beside.seen,
            born: None,
            key,
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

/// Write `text` to `dir/name`: the directory made 0700, a temp file of mode
/// 0600 (never through a link) renamed over it.
fn write_reading(dir: &Path, name: &str, text: &str) -> io::Result<()> {
    aterm_types::fs_restricted::ensure_private_dir(dir)?;
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = open.open(&tmp)?;
    let written = file.write_all(text.as_bytes());
    drop(file);
    match written.and_then(|()| std::fs::rename(&tmp, &path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// WHEN `reading`'S FIGURES WERE FIRST REPORTED since its process was last
/// served: the `at` of the reading already at `path` where that names the
/// same tab and process with the same `api_ms`, and every window `reading`
/// carries is the held one's — a re-report, which Claude Code makes every
/// [`TAP_REFRESH_S`] seconds while idle, or one a window only dropped out of
/// at its reset (Claude Code hands a window only while it runs), which tells
/// nothing new of the rest — else the reading's own. A window that newly
/// appears or moves is a new report.
fn first_reported(path: &Path, reading: &Reading) -> i64 {
    let figure = |w: Option<Window>| w.map(|w| (w.x10(), w.resets_at));
    let kept = |held: Option<Window>, now: Option<Window>| {
        figure(now).is_none_or(|now| figure(held) == Some(now))
    };
    read_regular(path)
        .and_then(|text| Reading::of_json(&text))
        .filter(|held| {
            (held.sid.as_str(), held.pid, held.api_ms)
                == (reading.sid.as_str(), reading.pid, reading.api_ms)
                && kept(held.five_hour, reading.five_hour)
                && kept(held.seven_day, reading.seven_day)
        })
        .map_or(reading.at, |held| held.at.min(reading.at))
}

/// Read stdin as the verb takes it: at most [`STDIN_MAX`] bytes, the rest
/// drained (`None` past it, or where it could not be read).
fn read_bytes(stdin: &mut dyn Read) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let read = stdin
        .take(STDIN_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .ok();
    if bytes.len() > STDIN_MAX {
        let _ = io::copy(stdin, &mut io::sink());
        return None;
    }
    read?;
    Some(bytes)
}

/// Write the reading the payload `payload` gives for the tab and process `vars` name into
/// `dir` ([`first_reported`]'s stamp), and return it.
fn record(
    payload: Option<&str>,
    vars: &TapVars,
    dir: Option<&Path>,
    now: i64,
    started: std::time::Instant,
) -> Option<Reading> {
    let reading = Reading::of_stdin(payload.unwrap_or(""), vars, now);
    if let (Some(reading), Some(dir)) = (&reading, dir)
        && let Some(name) = file_name(&reading.sid, reading.pid)
    {
        let reading = Reading {
            at: first_reported(&dir.join(&name), reading),
            dur_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            ..reading.clone()
        };
        let _ = write_reading(dir, &name, &reading.to_json());
    }
    reading
}

/// `aterm harness tap`, Claude Code's status-line command.
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
        read_bytes(stdin).and_then(|bytes| String::from_utf8(bytes).ok())
    };
    let reading = record(payload.as_deref(), vars, dir, now, started);
    let line = reading
        .as_ref()
        .map_or_else(|| NO_FIGURES.to_owned(), Reading::line);
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    std::process::ExitCode::SUCCESS
}

/// What one run of the carrying tap leaves to do ([`run_carrying`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carried {
    /// Become the owner's command `command` with the payload `payload` on its stdin
    /// ([`chain_command`]): its row and exit status are the ones Claude Code shows.
    Owner {
        /// The owner's statusLine command ([`owner_command`]).
        command: String,
        /// The payload Claude Code wrote, byte for byte.
        payload: Vec<u8>,
    },
    /// The tap printed its own row ([`run`]'s): the user's `settings.json` names no command it
    /// carries now, the run is itself one an owner's command started ([`CARRIED_ENV`]), or the
    /// payload could not be handed on (over [`STDIN_MAX`], or unread).
    Own,
}

/// `aterm harness tap --carry`, the tap that carries the owner's own statusLine
/// ([`settings_value_carrying`]): it writes the reading as [`run`] does, then, where `owner`
/// names the owner's command now ([`read_owner_command`]), prints nothing and hands the payload
/// on, byte for byte ([`Carried::Owner`]); else it prints its own row as [`run`] does
/// ([`Carried::Own`]), so a run never shows less than the plain tap.
pub fn run_carrying(
    stdin: &mut dyn Read,
    stdin_is_tty: bool,
    vars: &TapVars,
    dir: Option<&Path>,
    now: i64,
    owner: impl FnOnce() -> Option<String>,
    out: &mut dyn Write,
) -> Carried {
    let started = std::time::Instant::now();
    let bytes = if stdin_is_tty {
        Some(Vec::new())
    } else {
        read_bytes(stdin)
    };
    let payload = bytes.as_deref().and_then(|b| std::str::from_utf8(b).ok());
    let reading = record(payload, vars, dir, now, started);
    if let Some(payload) = bytes
        && let Some(command) = owner()
    {
        return Carried::Owner { command, payload };
    }
    let line = reading
        .as_ref()
        .map_or_else(|| NO_FIGURES.to_owned(), Reading::line);
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    Carried::Own
}

/// THE OWNER'S OWN STATUSLINE COMMAND `cmd`, as Claude Code runs one (`/bin/sh -c`, in this
/// process's working directory and environment, which are the ones Claude Code gave the tap,
/// with [`CARRIED_ENV`] set), with `payload` on its stdin: an unlinked 0600 file in the temporary
/// directory, so a payload of any size is there before the command starts. Its stdout and stderr
/// are this process's, so an exec of it ([`aterm_uds::spawnfd::Command::exec`]) leaves Claude
/// Code its row and its exit status, as if aterm were not there.
#[cfg(unix)]
pub fn chain_command(cmd: &str, payload: &[u8]) -> io::Result<aterm_uds::spawnfd::Command> {
    use std::io::{Seek as _, SeekFrom};
    use std::os::unix::fs::OpenOptionsExt as _;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path =
        std::env::temp_dir().join(format!(".aterm-tap-stdin-{}-{stamp}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let _ = std::fs::remove_file(&path);
    file.write_all(payload)?;
    file.seek(SeekFrom::Start(0))?;
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command.arg("-c").arg(cmd).env(CARRIED_ENV, "1").stdin(file);
    Ok(command)
}

/// Remove from `dir` every reading whose process is gone (`alive` answers for a pid).
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

/// `[harness] usage_tap` on, for the relaunch lines one pass types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Armed {
    /// The aterm the tap's command names ([`stable_exe`]).
    pub exe: PathBuf,
    /// The login of the machine's one account where it has a directory of its own.
    pub lone_login: Option<String>,
    /// Where the machine has no lone login, every login a roster of two or more accounts
    /// names as the store binds it, a renewal's old logins too
    /// ([`accounts::listed_logins_at`]): a line on one of them is tapped as that listed
    /// account's ([`Self::lone_for`]).
    pub listed: Vec<String>,
}

/// Model evidence and tap identities derived from one recovered account roster.
/// An unavailable roster or clock excludes account file evidence and all taps.
/// This is a read snapshot, not a lease on later changes to the account pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPolicy {
    /// Exclude model and billing file evidence, also when the pool is unknown.
    pub several_logins: bool,
    /// The tap and account directories admitted by that same roster.
    pub usage_tap: Option<Armed>,
}

impl LaunchPolicy {
    /// Recover and read the account roster once for this launch decision. Failed
    /// recovery or binding reads are restrictive even when the tap is switched off.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLaunchRead",
            action = "Read",
            project = "aterm_agent::harness::cli::tests::upgrade_policy::launch_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountLaunchRead",
            action = "AnotherShape",
            reason = "Fixture selection chooses the committed roster size"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountLaunchRead",
            action = "AnotherFault",
            reason = "Fixture selection supplies a publication or read failure"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountLaunchRead",
            action = "EnableTap",
            reason = "Fixture input sets the existing usage tap switch"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountLaunchRead",
            action = "HaveExecutable",
            reason = "Fixture input supplies an executable without discovering a live installation"
        )
    )]
    pub fn read(
        on: bool,
        exe: impl FnOnce() -> Option<PathBuf>,
        path: Option<&Path>,
        store: &accounts::Store,
        now: Option<i64>,
    ) -> Self {
        let roster = now.and_then(|now| store.read_roster(path, now).ok());
        Self {
            several_logins: roster.as_ref().is_none_or(accounts::Roster::several),
            usage_tap: roster.as_ref().and_then(|roster| {
                on.then(exe).flatten().map(|exe| Armed {
                    exe,
                    lone_login: roster.lone_login().map(str::to_owned),
                    listed: if roster.several() {
                        roster
                            .rows
                            .iter()
                            .flat_map(|row| row.dir.iter().chain(row.was.iter().flatten()))
                            .filter(|dir| !dir.is_empty())
                            .cloned()
                            .collect()
                    } else {
                        Vec::new()
                    },
                })
            }),
        }
    }
}

impl Armed {
    /// THE TAP ARMED for the relaunch lines one pass types, the one block every pass that
    /// relaunches shares (the window's background lane, `aterm harness upgrade`): when `on`,
    /// for the aterm `exe` names ([`stable_exe`]), with the lone login of the roster at
    /// `roster` as `store` binds it at `now`, else, on a roster of two or more rows,
    /// every current and previous login it names. No tap is armed where the clock,
    /// account publication or required bindings cannot be read ([`LaunchPolicy::read`]).
    #[must_use]
    pub fn arm(
        on: bool,
        exe: impl FnOnce() -> Option<PathBuf>,
        roster: Option<&Path>,
        store: &accounts::Store,
        now: Option<i64>,
    ) -> Option<Self> {
        if !on {
            return None;
        }
        let exe = exe()?;
        LaunchPolicy::read(true, || Some(exe), roster, store, now).usage_tap
    }

    /// THE LOGIN [`gate`] JUDGES A LINE ON `login` BY ([`GateFacts::lone_login`]): the
    /// machine's lone login, else `login` itself where the roster names it ([`Self::listed`]).
    /// The one rule every relaunch line keeps (the restart in place, an account move, the
    /// relaunch on exit, the live upgrade) and the Claude Code footer's reason judges by.
    #[must_use]
    pub fn lone_for<'a>(&'a self, login: Option<&'a str>) -> Option<&'a str> {
        self.lone_login.as_deref().or_else(|| {
            login.filter(|login| !login.is_empty() && self.listed.iter().any(|l| l == login))
        })
    }
}

/// Whether a line's login is the machine's own, for [`gate`].
#[must_use]
pub fn own_login(login: Option<&str>, home: Option<&str>, lone: Option<&str>) -> bool {
    home.is_none() && (login.is_none_or(str::is_empty) || login == lone)
}

/// What [`gate`] decides on, gathered by the relaunch's plan.
#[derive(Debug, Clone, Copy)]
pub struct GateFacts<'a> {
    /// `[harness] usage_tap`.
    pub exe: Option<&'a Path>,
    /// The process the line replaces authenticates by its stored login.
    pub stored_login: bool,
    /// The login variable's value the line carries ([`accounts::LOGIN_ENV`]).
    pub login: Option<&'a str>,
    /// The login of the machine's one account, or the line's own where the roster names it
    /// ([`Armed::lone_for`]).
    pub lone_login: Option<&'a str>,
    /// The Claude home the line carries ([`accounts::HOME_ENV`]).
    pub home: Option<&'a str>,
    /// The argv the line is made from, `argv[0]` first.
    pub argv: &'a [String],
    /// Every document of the settings chain Claude Code reads for it.
    pub settings: &'a [(String, Source)],
    /// Which of [`Self::settings`] is the user's own `settings.json`, the one document whose
    /// statusLine the tap carries ([`settings_line`]).
    pub user_settings: Option<&'a str>,
    /// The vendor's profile store holds a config ([`accounts::profile_configured`]).
    pub profile_store: bool,
}

/// The `--settings` value a relaunch line carries, or `None`: the plain tap, or the tap that
/// carries the owner's own statusLine ([`settings_line`], [`settings_value_carrying`]).
#[must_use]
pub fn gate(f: &GateFacts<'_>) -> Option<String> {
    let exe = f.exe?;
    settings_value(exe)?;
    if !(f.stored_login && own_login(f.login, f.home, f.lone_login)) || f.profile_store {
        return None;
    }
    if !line_admits(f.argv) {
        return None;
    }
    let chain = settings_line(f.settings, f.user_settings)?;
    settings_value_carrying(exe, chain.is_some())
}

/// What a Claude Code launch's own environment gives the tap at launch ([`launch_value`]):
/// `aterm harness tap --launch`, which atpkg's managed twin asks inside an aterm tab, and a
/// worker's line (`spawn agent=claude`).
#[derive(Debug, Clone, Copy)]
pub struct LaunchFacts<'a> {
    /// `[harness] usage_tap` armed for this launch ([`Armed::arm`]).
    pub armed: Option<&'a Armed>,
    /// The tab it runs in (`ATERM_PARENT_SESSION_ID`), which keys the reading ([`Reading::sid`]).
    pub sid: Option<&'a str>,
    /// `ATERM_NO_HARNESS` is set (`claude --no-harness`): the harness keeps off this one.
    pub no_harness: bool,
    /// Every pair of the environment the launch runs with.
    pub env: &'a [(String, String)],
    /// Its `HOME`.
    pub home: Option<&'a Path>,
    /// The account's own home (`getpwuid_r`): a launch under another `HOME` keeps its records
    /// where no footer reads them, and is not tapped.
    pub account_home: Option<&'a Path>,
    /// Its argv, `argv[0]` first.
    pub argv: &'a [String],
    /// Every document of the settings chain Claude Code reads for it, in its directory.
    pub settings: &'a [(String, Source)],
    /// Which of [`Self::settings`] is the user's own `settings.json`.
    pub user_settings: Option<&'a str>,
    /// The vendor's profile store holds a config ([`accounts::profile_configured`]).
    pub profile_store: bool,
}

/// THE `--settings` VALUE A CLAUDE CODE LAUNCH CARRIES FROM ITS START (owner ruling R-12: the
/// harness reads the week itself, so a session needs no relaunch for its figures): [`gate`]'s,
/// judged on the launch's own environment ([`LaunchFacts`]), for an interactive launch only
/// ([`launch_admits`]), in an aterm tab, with the harness on for it, under the account's own
/// `HOME`, and not carrying the tap already; `None` (the launch runs as typed) anywhere else.
#[must_use]
pub fn launch_value(f: &LaunchFacts<'_>) -> Option<String> {
    let armed = f.armed?;
    if f.no_harness || !f.sid.is_some_and(is_tab_id) || carried_by(f.argv) {
        return None;
    }
    if f.home.is_none_or(|home| !home.is_absolute()) || f.home != f.account_home {
        return None;
    }
    if !launch_admits(f.argv) {
        return None;
    }
    let pairs = || f.env.iter().map(|(k, v)| (k.as_str(), v.as_str()));
    let var = |name: &str| pairs().find(|(k, _)| *k == name).map(|(_, v)| v);
    let login = var(accounts::LOGIN_ENV);
    gate(&GateFacts {
        exe: Some(&armed.exe),
        stored_login: accounts::ProcessLogin::of_env(pairs()).stored_login,
        login,
        lone_login: armed.lone_for(login),
        home: var(accounts::HOME_ENV).filter(|home| !home.is_empty()),
        argv: f.argv,
        settings: f.settings,
        user_settings: f.user_settings,
        profile_store: f.profile_store,
    })
}

/// Whether a launch's argv (`argv[0]` first) is an interactive Claude Code the tap rides on from
/// its start: [`line_admits`] it, and its first operand, where it has one, is no word a
/// subcommand's name could be (`mcp`, `doctor`, `plugin`, `setup-token`, …: a lowercase word of
/// letters, digits and hyphens), so the flag is never put ahead of a subcommand. A prompt of one
/// such word (`claude hello`) is not tapped at launch; one of more words is.
#[must_use]
pub fn launch_admits(argv: &[String]) -> bool {
    let subcommand_like = |word: &str| {
        !word.is_empty()
            && word
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    line_admits(argv) && super::upgrade::first_operand(argv).is_none_or(|w| !subcommand_like(w))
}

/// [`gate`]'s argv half: the line (`argv[0]` first) parses, and carries no `--settings` or
/// `--bare` of its own ([`line_refusal`]).
#[must_use]
pub fn line_admits(argv: &[String]) -> bool {
    line_refusal(argv).is_none()
}

/// Why [`line_admits`] refuses `argv` (`argv[0]` first): an option no relaunch line can repeat
/// ([`Refusal::Argv`]: one this build does not know, or one that cannot resume), or a
/// `--settings` or `--bare` of its own ([`Refusal::Line`]); `None` where it admits it.
#[must_use]
pub fn line_refusal(argv: &[String]) -> Option<Refusal> {
    match super::upgrade::carried_names(argv) {
        Err(_) => Some(Refusal::Argv),
        Ok(flags) => flags
            .iter()
            .any(|name| matches!(*name, "--settings" | "--bare"))
            .then_some(Refusal::Line),
    }
}

/// [`gate`]'s SETTINGS HALF, the one judgment the relaunch's gate and the Claude Code footer's
/// reason for a missing week share (`upgrade_drive::tap_settings_admit`): [`settings_line`]
/// admits the chain, and no document of it moves the line off its stored login (an
/// `apiKeyHelper`, or a credential, login or home variable in an `env` block:
/// [`accounts::ProcessLogin::with_settings`]). `user` names the user's own `settings.json`
/// among the documents.
#[must_use]
pub fn settings_admit(settings: &[(String, Source)], user: Option<&str>) -> bool {
    settings_line(settings, user).is_some()
}

/// WHAT THE SETTINGS CHAIN LEAVES THE TAP (owner ruling R-12): `Some(None)` where every
/// document was read and none sets a statusLine (the plain tap), `Some(Some(cmd))` where the
/// one statusLine the chain sets is the owner's own in the user's `settings.json` (`user`), a
/// plain command line ([`netwatch::Settings::status_command`]), so the tap carries it
/// ([`settings_value_carrying`], which reads it again when it runs: [`owner_command`]) and the
/// owner's row still shows; `None` (no tap) where a
/// document cannot be read or parsed, one turns hooks off, a statusLine is set anywhere else (a
/// project's or a local one, a managed one, a `--settings` value's, `cowork_settings.json`'s:
/// carrying those would mean re-deciding Claude Code's merge and trust), two documents set one,
/// the owner's is of another shape or longer than [`CHAIN_MAX`], or the platform is not Unix
/// (the carried command runs under `/bin/sh`); and wherever a document moves the line off its
/// stored login ([`accounts::ProcessLogin::with_settings`]).
#[must_use]
pub fn settings_line(settings: &[(String, Source)], user: Option<&str>) -> Option<Option<String>> {
    if !accounts::ProcessLogin::of_env([])
        .with_settings(settings)
        .stored_login
    {
        return None;
    }
    let mut lines = Vec::new();
    for (name, source) in settings {
        match source {
            Source::Missing => {}
            Source::Unreadable => return None,
            Source::Text(text) => {
                let read = netwatch::settings_of(text)?;
                if read.hooks_off {
                    return None;
                }
                if read.status_line {
                    lines.push((name.as_str(), read.status_command));
                }
            }
        }
    }
    match lines.as_slice() {
        [] => Some(None),
        [(name, Some(cmd))]
            if cfg!(unix)
                && user == Some(*name)
                && cmd.len() <= CHAIN_MAX
                && !cmd.contains('\0') =>
        {
            Some(Some(cmd.clone()))
        }
        _ => None,
    }
}

/// Whether a process whose records are in `dir` runs under the `HOME` of the user whose home
/// is `user_home`: a relaunch line of any other carries a `HOME` of its own (or is refused),
/// and no such line is tapped (`relaunch::tap_of`).
#[must_use]
pub fn own_records(user_home: &Path, dir: &Path) -> bool {
    super::upgrade_drive::same_dir(dir, &user_home.join(".claude"))
}

/// WHY NO RELAUNCH LINE WOULD CARRY THE TAP for a process aterm would otherwise tap, as its
/// footer tells it (`profile::WeeklyGap::Unoffered`): [`stable_exe`], then `relaunch::tap_of`'s
/// and [`gate`]'s refusals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// No lasting aterm whose path the tap's command can name ([`stable_exe`]).
    Exe,
    /// A Claude home ([`accounts::HOME_ENV`]) or `HOME` of its own ([`own_records`]).
    Home,
    /// A login no relaunch line taps as the machine's own: neither the default one, the
    /// roster's lone one, nor, where it has none, one the roster names ([`own_login`],
    /// [`Armed::lone_for`]). Past the footer's account gate, only a login a renewal moved the
    /// roster's one row away from: the same account, whose tap follows its current login.
    Login,
    /// Its own `--settings` or `--bare` ([`line_refusal`]).
    Line,
    /// An option no relaunch line can repeat: one this build does not know, or one that
    /// cannot resume (`--print`, `--no-session-persistence`, …; [`line_refusal`]).
    Argv,
    /// A statusLine of its own, hooks turned off, a settings document unread, or an
    /// `apiKeyHelper` or credential in one ([`settings_admit`]).
    Settings,
    /// The relaunch line aterm would type for it has no room for the tap on the tty's line
    /// (`relaunch::tap_room`): the line would fall back to no tap rather than to no relaunch.
    Room,
}

/// The freshness rule, per tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fresh {
    /// The last reading admitted.
    last: Option<(u32, u64, Option<Window>, Option<Window>)>,
}

/// What a fresh reading says beside the one admitted before it ([`Fresh::beside`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Beside {
    /// The last reading admitted was its own process's: what it reads now
    /// was served since then ([`accounts::TapRead::seen_before`]).
    pub seen: bool,
    /// Per window (five-hour, week), the figure the vendor refilled it to.
    pub refilled: [Option<u32>; 2],
}

impl Fresh {
    /// What `reading` says beside the last reading admitted ([`Beside`]).
    #[must_use]
    pub fn beside(&self, reading: &Reading) -> Beside {
        let Some((_, api_ms, five, seven)) = self.last.filter(|(pid, ..)| *pid == reading.pid)
        else {
            return Beside::default();
        };
        let served = reading.api_ms > api_ms;
        let refilled = |was: Option<Window>, now: Option<Window>| match (was, now) {
            (Some(was), Some(now))
                if served
                    && was.resets_at == now.resets_at
                    && now.x10().saturating_add(accounts::REFILL_MIN_X10) <= was.x10() =>
            {
                Some(was.x10())
            }
            _ => None,
        };
        Beside {
            seen: true,
            refilled: [
                refilled(five, reading.five_hour),
                refilled(seven, reading.seven_day),
            ],
        }
    }

    /// Whether `reading` is fresh.
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
    /// The process went silent at this point.
    Silent,
    /// The mark ends: its process is heard again, or another process holds the tab now.
    Again,
}

/// The silent-tap rule.
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
    /// One decided point at `now` for process `pid`, up `up_s` seconds.
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

    /// Process `pid` now holds the tab and carries no tap, so it is never judged.
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
    pub const fn silent(&self) -> bool {
        self.silent
    }
}

#[path = "tap_tests.rs"]
#[cfg(test)]
mod tests;
