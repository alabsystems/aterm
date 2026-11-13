// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-user `packages.log` writer, sanitizer, rotator, and tail reader.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::Status;
use crate::store::Layout;

/// File name inside the log directory.
pub const LOG_NAME: &str = "packages.log";

/// File size threshold at which the log rotates (1 MiB).
pub const ROTATE_AT_BYTES: u64 = 1024 * 1024;

/// Number of rotated log files kept (`packages.log.1` .. `packages.log.5`).
pub const KEEP_ROTATED: u32 = 5;

/// Number of events read by the Settings ▸ Packages Activity view.
pub const ACTIVITY_EVENTS: usize = 50;

const MAX_VALUE_CHARS: usize = 400;

const TAIL_READ_BYTES: u64 = 128 * 1024;

/// Record kind identifiers.
pub mod kind {
    /// A pass began: it holds the store lock.
    pub const PASS_START: &str = "pass-start";
    /// A pass ended (or the store lock refused it).
    pub const PASS_END: &str = "pass-end";
    /// A program's row moved.
    pub const PROGRAM: &str = "program";
    /// A landed agent build was warmed ([`crate::warm`]).
    pub const WARM: &str = "warm";
    /// A rustup seam's entry replaced a stale foreign link.
    pub const SEAM: &str = "seam";
    /// A pass's index discovery ran: what it asked and what it found.
    pub const INDEX_RESOLVED: &str = "index-resolved";
}

/// Path to `<logs dir>/packages.log`, or `None` if no log directory resolves.
#[must_use]
pub fn log_path() -> Option<PathBuf> {
    log_dir().map(|dir| dir.join(LOG_NAME))
}

#[cfg(not(test))]
fn log_dir() -> Option<PathBuf> {
    aterm_types::dirs::logs_dir()
}

/// Under test the log is wherever the test bound it ([`test_bind`]), never the developer's
/// own log directory.
#[cfg(test)]
fn log_dir() -> Option<PathBuf> {
    test_bind::get().map(|(dir, _)| dir)
}

/// Whether events about `layout`'s store belong in this log: it is the store the configured `[packages]` table resolves (read once per process; atpkg is a short-lived verb).
#[cfg(not(test))]
pub(crate) fn records(layout: &Layout) -> bool {
    static BOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    BOUND
        .get_or_init(|| crate::store::resolve_configured().map(|l| l.prefix))
        .as_deref()
        == Some(layout.prefix.as_path())
}

#[cfg(test)]
pub(crate) fn records(layout: &Layout) -> bool {
    test_bind::get().is_some_and(|(_, prefix)| prefix == layout.prefix)
}

/// Tests bind a scratch log directory to a scratch store, per thread (tests run in parallel, each on its own thread; a pass runs on the thread that called it).
#[cfg(test)]
#[path = "packages_log_test_bind_tests.rs"]
pub(crate) mod test_bind;

/// One program row transition recorded in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    /// The program.
    pub program: String,
    /// The build it ran before, in words (`2.1.278`, `build 4790`); `None` when none.
    pub from: Option<String>,
    /// The build it runs now, when that moved; `None` when it did not move or is gone.
    pub to: Option<String>,
    /// Where its builds come from ([`crate::state::source_words`]).
    pub source: String,
    /// One word: `installed`, `updated`, `rolled back`, `removed`, `current`, or why it is not current ([`crate::state::not_current`]: `held`, `refused`, `failed`, …).
    pub result: &'static str,
    /// The row's own words when it is not current; empty otherwise.
    pub reason: String,
}

/// One package-log event.
#[derive(Clone, Copy, Debug)]
pub enum Event<'a> {
    /// A pass took the store lock.
    PassStart {
        /// Which lane ran it ([`crate::cli`]'s `pass_lane`).
        lane: &'a str,
        /// The verb and its first operand (`update`, `update claude`, `seed`).
        verb: &'a str,
    },
    /// A pass returned, or the store lock refused it.
    PassEnd {
        /// Which lane ran it.
        lane: &'a str,
        /// The verb and its first operand.
        verb: &'a str,
        /// Its exit code.
        exit: u8,
        /// How long it held the store, in seconds.
        secs: u64,
        /// The outcome it recorded in `status.toml` (empty when it recorded none).
        outcome: &'a str,
    },
    /// A program's row moved.
    Program(&'a Transition),
    /// A landed agent build was warmed ([`crate::warm`]).
    Warm {
        /// The program.
        program: &'a str,
        /// The build warmed.
        build: u64,
        /// How it ended: `exit=<code>`, `signal=<n>`, `killed` (at the deadline) or `spawn-failed`; or why it did not run: `opted-out`, `never-run`, `signed-out`, `setup-unread`.
        outcome: &'a str,
        /// How long it ran, in milliseconds.
        ms: u64,
    },
    /// A pass's index discovery ran ([`crate::net::GithubFetcher`]): what it cost — the HEADs, the rounds they went out in, the asset GETs — and the newest index it found.
    IndexResolved {
        /// HEADs asked.
        heads: u32,
        /// The rounds they went out in, each at once.
        rounds: u32,
        /// Asset GETs asked.
        gets: u32,
        /// The newest index found (`atpkg-index-<n>`), or why none was.
        found: &'a str,
    },
    /// A rustup seam's entry replaced a stale foreign link (`crate::seam::Attached::ReplacedStale`).
    Seam {
        /// The seam, `rustup:<name>`.
        seam: &'a str,
        /// What the entry named before, as it was written.
        from: &'a str,
        /// What it names now: atpkg's view of the store's build.
        to: &'a str,
        /// Why it was stale (`crate::seam::Stale`, in words).
        why: &'a str,
        /// The command that puts it back; empty when there is nothing to go back to.
        undo: &'a str,
    },
}

/// Append `event` for `layout` when recorded by this log.
pub fn append(layout: &Layout, event: &Event<'_>) {
    if !records(layout) {
        return;
    }
    let Some(dir) = log_dir() else {
        return;
    };
    let line = render_at(
        crate::flow::now_unix(),
        std::process::id(),
        pass_ms(),
        event,
    );
    let _ = append_line(&dir, &line, crate::lock::held_by_this_process(layout));
}

/// Append `event` from code that holds no layout — the fetcher — when, and only when, this process is inside a pass over the recorded store ([`start_pass_clock`]).
pub(crate) fn append_in_pass(event: &Event<'_>) {
    let Some(ms) = pass_ms() else {
        return;
    };
    let Some(dir) = log_dir() else {
        return;
    };
    let line = render_at(crate::flow::now_unix(), std::process::id(), Some(ms), event);
    let _ = append_line(&dir, &line, false);
}

/// Start this process's PASS CLOCK — a pass over `layout` took the store lock — when that store is the one this log records: every line written until [`stop_pass_clock`] carries `t_ms`, its offset from here in milliseconds, so one pass's lines read as a timeline finer than the seconds of their stamps.
pub(crate) fn start_pass_clock(layout: &Layout) {
    if records(layout) {
        pass_clock::set(Some(std::time::Instant::now()));
    }
}

/// The pass ended: lines after it carry no `t_ms`.
pub(crate) fn stop_pass_clock() {
    pass_clock::set(None);
}

fn pass_ms() -> Option<u64> {
    pass_clock::get().map(|start| u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX))
}

/// One pass per process: the clock is the process's.
#[cfg(not(test))]
mod pass_clock {
    static START: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

    pub(super) fn set(at: Option<std::time::Instant>) {
        if let Ok(mut start) = START.lock() {
            *start = at;
        }
    }

    pub(super) fn get() -> Option<std::time::Instant> {
        START.lock().ok().and_then(|start| *start)
    }
}

/// Under test a pass runs on the thread that called it, beside other tests' passes: the
/// clock is the thread's, as [`test_bind`]'s log directory is.
#[cfg(test)]
#[path = "packages_log_pass_clock_tests.rs"]
mod pass_clock;

/// Log every program transition from `before` to `after`: [`crate::status::write`]'s hook, which every row writer goes through (it asks [`records`] before reading `before`).
pub(crate) fn note_rows(layout: &Layout, before: Option<&Status>, after: &Status) {
    for transition in transitions(before, after) {
        append(layout, &Event::Program(&transition));
    }
}

/// Compute program transitions from `before` to `after` in program order.
#[must_use]
pub fn transitions(before: Option<&Status>, after: &Status) -> Vec<Transition> {
    let empty = std::collections::BTreeMap::new();
    let old = before.map_or(&empty, |s| &s.programs);
    let mut names: Vec<&String> = old.keys().chain(after.programs.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|name| !name.starts_with('*') && !name.starts_with('-'))
        .filter_map(|name| {
            transition(
                name,
                old.get(name),
                after.programs.get(name),
                after.last_index_build,
            )
        })
        .collect()
}

fn transition(
    program: &str,
    before: Option<&crate::ProgramStatus>,
    after: Option<&crate::ProgramStatus>,
    last_index_build: u64,
) -> Option<Transition> {
    let (b0, s0) = before.map_or((None, ""), |r| (r.installed_build, r.state.as_str()));
    let (b1, s1) = after.map_or((None, ""), |r| (r.installed_build, r.state.as_str()));
    if b0 == b1 && s0 == s1 {
        return None;
    }
    let words = |b: Option<u64>| b.map(crate::vendor_direct::build_words);
    let why0 = crate::state::not_current(s0);
    let why1 = crate::state::not_current(s1);
    let reason = why1.map_or_else(String::new, |(_, why)| why.to_string());
    let (result, to) = match (b0, b1) {
        (None, Some(_)) => why1.map_or(("installed", words(b1)), |(r, _)| (r, None)),
        (Some(_), None) => (why1.map_or("removed", |(r, _)| r), None),
        (Some(a), Some(b)) if b > a => ("updated", words(b1)),
        (Some(a), Some(b)) if b < a => ("rolled back", words(b1)),
        _ => match (why0, why1) {
            (_, Some((result, why))) if why0.map(|(_, w)| w) != Some(why) => (result, None),
            (Some(_), None) => ("current", None),
            _ => return None,
        },
    };
    let state = if s1.is_empty() { s0 } else { s1 };
    Some(Transition {
        program: program.to_string(),
        from: words(b0),
        to,
        source: crate::state::source_words(program, state, last_index_build),
        result,
        reason,
    })
}

/// Format `event` as a single log line including trailing newline.
#[must_use]
pub fn render(unix: i64, pid: u32, event: &Event<'_>) -> String {
    render_at(unix, pid, None, event)
}

fn render_at(unix: i64, pid: u32, t_ms: Option<u64>, event: &Event<'_>) -> String {
    let (exit, secs, build, ms, counts);
    let (kind, fields): (&str, Vec<(&str, &str)>) = match *event {
        Event::PassStart { lane, verb } => (kind::PASS_START, vec![("lane", lane), ("verb", verb)]),
        Event::PassEnd {
            lane,
            verb,
            exit: code,
            secs: held,
            outcome,
        } => {
            exit = code.to_string();
            secs = held.to_string();
            (
                kind::PASS_END,
                vec![
                    ("lane", lane),
                    ("verb", verb),
                    ("exit", &exit),
                    ("secs", &secs),
                    ("outcome", outcome),
                ],
            )
        }
        Event::Program(t) => (
            kind::PROGRAM,
            vec![
                ("program", &t.program),
                ("from", t.from.as_deref().unwrap_or("")),
                ("to", t.to.as_deref().unwrap_or("")),
                ("source", &t.source),
                ("result", t.result),
                ("reason", &t.reason),
            ],
        ),
        Event::Warm {
            program,
            build: warmed,
            outcome,
            ms: took,
        } => {
            build = crate::vendor_direct::build_label(warmed);
            ms = took.to_string();
            (
                kind::WARM,
                vec![
                    ("program", program),
                    ("build", &build),
                    ("outcome", outcome),
                    ("ms", &ms),
                ],
            )
        }
        Event::IndexResolved {
            heads,
            rounds,
            gets,
            found,
        } => {
            counts = [heads.to_string(), rounds.to_string(), gets.to_string()];
            (
                kind::INDEX_RESOLVED,
                vec![
                    ("heads", counts[0].as_str()),
                    ("rounds", counts[1].as_str()),
                    ("gets", counts[2].as_str()),
                    ("found", found),
                ],
            )
        }
        Event::Seam {
            seam,
            from,
            to,
            why,
            undo,
        } => (
            kind::SEAM,
            vec![
                ("seam", seam),
                ("from", from),
                ("to", to),
                ("why", why),
                ("undo", undo),
            ],
        ),
    };
    let stamp = t_ms.map(|t| t.to_string());
    let fields: Vec<(&str, &str)> = stamp
        .as_deref()
        .map(|t| ("t_ms", t))
        .into_iter()
        .chain(fields)
        .collect();
    render_record(unix, kind, pid, &fields, MAX_VALUE_CHARS)
}

/// Format a single tab-separated record line with sanitized fields.
#[must_use]
pub fn render_record(
    unix: i64,
    kind: &str,
    pid: u32,
    fields: &[(&str, &str)],
    max_chars: usize,
) -> String {
    let mut line = String::with_capacity(256);
    match u64::try_from(unix) {
        Ok(secs) if unix > 0 && unix != i64::MAX => {
            line.push_str(&aterm_types::rfc3339::format_rfc3339(secs));
        }
        _ => line.push('-'),
    }
    line.push('\t');
    line.push_str(kind);
    push_field(&mut line, "pid", &pid.to_string(), max_chars);
    for (key, value) in fields {
        push_field(&mut line, key, value, max_chars);
    }
    line.push('\n');
    line
}

fn push_field(line: &mut String, key: &str, value: &str, max_chars: usize) {
    let value = sanitize_capped(value, max_chars);
    if value.is_empty() {
        return;
    }
    line.push('\t');
    line.push_str(key);
    line.push('=');
    line.push_str(&value);
}

/// A value as a line may carry it: control characters (C0, DEL, C1) and the Unicode line and paragraph separators become spaces, bidi controls and zero-width characters are dropped, URLs lose userinfo, query and fragment ([`redact_urls`]), credentials become `[redacted]` ([`redact_tokens`]), runs of spaces fold, and the whole is capped at [`MAX_VALUE_CHARS`].
#[cfg(test)]
#[must_use]
pub(crate) fn sanitize(value: &str) -> String {
    sanitize_capped(value, MAX_VALUE_CHARS)
}

/// Sanitize and redact a field value, capping at `max_chars` characters.
#[must_use]
pub fn sanitize_capped(value: &str, max_chars: usize) -> String {
    let plain: String = value
        .chars()
        .filter(|c| !is_bidi_control(*c) && !is_zero_width(*c))
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let redacted = redact_tokens(&redact_urls(&plain));
    let mut out = String::with_capacity(redacted.len().min(max_chars.saturating_mul(4)));
    let mut count = 0usize;
    let mut last_space = true;
    for c in redacted.chars() {
        if c == ' ' && last_space {
            continue;
        }
        if count == max_chars {
            out.push('…');
            break;
        }
        last_space = c == ' ';
        out.push(c);
        count += 1;
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out
}

const fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

const fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
}

fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
            .unwrap_or(tail.len());
        let (url, after) = tail.split_at(end);
        let authority_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let (authority, path) = url.split_at(authority_end);
        out.push_str(
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host),
        );
        match path.find(['?', '#']) {
            Some(q) => {
                out.push_str(&path[..q]);
                out.push_str("?…");
            }
            None => out.push_str(path),
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

const REDACTED: &str = "[redacted]";

fn redact_tokens(text: &str) -> String {
    /// What the words that follow owe the redactor.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Hide {
        Nothing,
        /// The next word is a credential.
        Next,
        /// After `Authorization:` — a scheme word is kept and the word after it hidden;
        /// any other word is the credential.
        Scheme,
    }
    fn wrapping(c: char) -> bool {
        matches!(
            c,
            '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | ',' | ';' | '.' | '<' | '>' | '`'
        )
    }
    fn redact_word(word: &str, hide: &mut Hide) -> String {
        let bare = word.trim_matches(wrapping);
        match *hide {
            Hide::Next => {
                *hide = Hide::Nothing;
                return REDACTED.to_string();
            }
            Hide::Scheme => {
                if matches!(
                    bare.to_ascii_lowercase().as_str(),
                    "bearer" | "basic" | "token" | "digest"
                ) {
                    *hide = Hide::Next;
                    return word.to_string();
                }
                *hide = Hide::Nothing;
                return REDACTED.to_string();
            }
            Hide::Nothing => {}
        }
        if is_secret_shaped(bare) {
            return REDACTED.to_string();
        }
        let segments: Vec<&str> = word.split_inclusive([',', ';', '&']).collect();
        if segments.len() > 1 {
            let mut joined = String::with_capacity(word.len());
            let mut sub_hide = Hide::Nothing;
            for seg in segments {
                let (core, delim) = match seg.chars().next_back() {
                    Some(c @ (',' | ';' | '&')) => {
                        let cut = seg.len() - c.len_utf8();
                        (&seg[..cut], &seg[cut..])
                    }
                    _ => (seg, ""),
                };
                if !core.is_empty() {
                    joined.push_str(&redact_word(core, &mut sub_hide));
                }
                joined.push_str(delim);
            }
            if sub_hide != Hide::Nothing {
                *hide = sub_hide;
            }
            return joined;
        }
        if let Some(at) = bare.find(['=', ':']) {
            let key = bare[..at].trim_matches(['"', '\'']).to_ascii_lowercase();
            let glued = bare[at + 1..].trim_matches(['"', '\'']);
            let sep = &bare[at..=at];
            if key == "authorization" && glued.is_empty() {
                *hide = Hide::Scheme;
                return word.to_string();
            }
            if is_credential_key(&key, sep == "=") || key == "authorization" {
                if glued.is_empty() {
                    *hide = Hide::Next;
                    return word.to_string();
                }
                let lead = word.len() - word.trim_start_matches(wrapping).len();
                let mut out = String::with_capacity(lead + at + 1 + REDACTED.len());
                out.push_str(&word[..lead]);
                out.push_str(&bare[..=at]);
                out.push_str(REDACTED);
                return out;
            }
        }
        if matches!(bare.to_ascii_lowercase().as_str(), "bearer" | "token") {
            *hide = Hide::Next;
        }
        word.to_string()
    }
    let mut out = String::with_capacity(text.len());
    let mut hide = Hide::Nothing;
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if word.is_empty() {
            continue;
        }
        out.push_str(&redact_word(word, &mut hide));
    }
    out
}

fn is_credential_key(key: &str, equals: bool) -> bool {
    const NAMES: [&str; 12] = [
        "token",
        "secret",
        "password",
        "passwd",
        "pwd",
        "apikey",
        "api_key",
        "api-key",
        "x-api-key",
        "private_key",
        "client_secret",
        "credential",
    ];
    NAMES.contains(&key)
        || (equals && key == "key")
        || [
            "_token",
            "-token",
            "_secret",
            "-secret",
            "_password",
            "_key",
        ]
        .iter()
        .any(|suffix| key.ends_with(suffix))
}

fn is_secret_shaped(word: &str) -> bool {
    const INSIDE: [&str; 6] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"];
    const LEADING: [(&str, usize); 8] = [
        ("sk-", 12),
        ("glpat-", 20),
        ("xoxb-", 15),
        ("xoxp-", 15),
        ("xoxa-", 15),
        ("xoxr-", 15),
        ("xoxs-", 15),
        ("npm_", 20),
    ];
    if INSIDE.iter().any(|p| word.contains(p))
        || LEADING
            .iter()
            .any(|(p, min)| word.starts_with(p) && word.len() >= *min)
    {
        return true;
    }
    let aws = (word.starts_with("AKIA") || word.starts_with("ASIA"))
        && word.len() == 20
        && word
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let random = word.len() >= 32
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && word.bytes().any(|b| b.is_ascii_uppercase())
        && word.bytes().any(|b| b.is_ascii_lowercase())
        && word.bytes().any(|b| b.is_ascii_digit())
        && !word.bytes().all(|b| b.is_ascii_hexdigit());
    aws || random
}

/// Append `line` to `<dir>/packages.log`, rotating first when `may_rotate` and due.
/// # Errors
/// Returns an error if ensuring the private directory, opening, or writing fails.
pub fn append_line(dir: &Path, line: &str, may_rotate: bool) -> io::Result<()> {
    aterm_types::fs_restricted::ensure_private_dir(dir)?;
    let path = dir.join(LOG_NAME);
    if may_rotate {
        let _ = rotate_if_due(&path);
    }
    open_append(&path)?.write_all(line.as_bytes())
}

#[cfg(unix)]
fn open_append(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "packages.log is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_append(path: &Path) -> io::Result<std::fs::File> {
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && !meta.file_type().is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "packages.log is not a regular file",
        ));
    }
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "packages.log is not a regular file",
        ));
    }
    Ok(file)
}

/// Path to `packages.log.<n>` beside `path`.
#[must_use]
pub fn rotated_path(path: &Path, n: u32) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from(LOG_NAME),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".");
    name.push(n.to_string());
    path.with_file_name(name)
}

/// Rotate `path` when it is a regular file of at least [`ROTATE_AT_BYTES`].
/// # Errors
/// Returns an error if renaming an existing log file fails.
pub fn rotate_if_due(path: &Path) -> io::Result<bool> {
    let due = std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.file_type().is_file() && m.len() >= ROTATE_AT_BYTES);
    if !due {
        return Ok(false);
    }
    for n in (1..KEEP_ROTATED).rev() {
        match std::fs::rename(rotated_path(path, n), rotated_path(path, n + 1)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    std::fs::rename(path, rotated_path(path, 1))?;
    Ok(true)
}

/// Parsed package-log record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// When (Unix seconds); `None` for a line whose clock was unusable.
    pub at: Option<i64>,
    /// [`kind`].
    pub kind: String,
    /// The writing process.
    pub pid: u32,
    /// Every other field, in line order.
    pub fields: Vec<(String, String)>,
}

impl Entry {
    /// Value of `key`, if present on the line.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Parse a package-log line of a known [`kind`].
#[must_use]
pub fn parse_line(line: &str) -> Option<Entry> {
    parse_record(line).filter(|entry| {
        [
            kind::PASS_START,
            kind::PASS_END,
            kind::PROGRAM,
            kind::WARM,
            kind::SEAM,
            kind::INDEX_RESOLVED,
        ]
        .contains(&entry.kind.as_str())
    })
}

/// Parse any tab-separated record line produced by [`render_record`].
#[must_use]
pub fn parse_record(line: &str) -> Option<Entry> {
    let mut parts = line.trim_end_matches(['\n', '\r']).split('\t');
    let stamp = parts.next()?;
    let at = if stamp == "-" {
        None
    } else {
        Some(aterm_update_core::pkg_check::rfc3339_to_unix(stamp)?)
    };
    let kind = parts.next()?;
    if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
        return None;
    }
    let clean_field = |s: &str| {
        s.chars().all(|c| {
            !c.is_control()
                && !is_bidi_control(c)
                && !is_zero_width(c)
                && !matches!(c, '\u{2028}' | '\u{2029}')
        })
    };
    let mut pid = None;
    let mut fields = Vec::new();
    for part in parts {
        let (key, value) = part.split_once('=')?;
        if key.is_empty() || !clean_field(key) || !clean_field(value) {
            return None;
        }
        if key == "pid" {
            if pid.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            pid = Some(value.parse().ok()?);
        } else {
            fields.push((key.to_string(), value.to_string()));
        }
    }
    Some(Entry {
        at,
        kind: kind.to_string(),
        pid: pid?,
        fields,
    })
}

/// Read the newest `max` events from `dir`, newest first.
#[must_use]
pub fn read_tail(dir: &Path, max: usize) -> Vec<Entry> {
    read_tail_of(dir, max, |_| true)
}

/// Read the newest `max` events in `dir` matching `keep`, newest first.
#[must_use]
pub fn read_tail_of(dir: &Path, max: usize, keep: impl Fn(&Entry) -> bool) -> Vec<Entry> {
    let path = dir.join(LOG_NAME);
    let mut out = Vec::new();
    for file in
        std::iter::once(path.clone()).chain((1..=KEEP_ROTATED).map(|n| rotated_path(&path, n)))
    {
        if out.len() >= max {
            break;
        }
        let mut lines = tail_lines(&file, TAIL_READ_BYTES);
        lines.reverse();
        out.extend(
            lines
                .iter()
                .filter_map(|l| parse_line(l))
                .filter(|entry| keep(entry))
                .take(max - out.len()),
        );
    }
    out
}

/// Read the newest `max` events from [`log_path`].
#[must_use]
pub fn tail(max: usize) -> Vec<Entry> {
    tail_of(max, |_| true)
}

/// Read the newest `max` events from [`log_path`] matching `keep`.
#[must_use]
pub fn tail_of(max: usize, keep: impl Fn(&Entry) -> bool) -> Vec<Entry> {
    log_dir().map_or_else(Vec::new, |dir| read_tail_of(&dir, max, keep))
}

/// Every record ([`parse_record`]) in the last `read_bytes` of `path`, oldest first; a line cut by that window, torn or foreign is skipped.
///
/// # Errors
/// `path` exists but is not a regular file, or opening, seeking or reading it failed.
#[cfg(test)]
pub(crate) fn read_records(path: &Path, read_bytes: u64) -> io::Result<Vec<Entry>> {
    match read_tail_lines(path, read_bytes) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
        Ok(lines) => Ok(lines.iter().filter_map(|line| parse_record(line)).collect()),
    }
}

fn tail_lines(path: &Path, read_bytes: u64) -> Vec<String> {
    read_tail_lines(path, read_bytes).unwrap_or_default()
}

fn read_tail_lines(path: &Path, read_bytes: u64) -> io::Result<Vec<String>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let (mut file, meta) = crate::metadata_io::open_regular_with_meta(path)?;
    let len = meta.len();
    let start = len.saturating_sub(read_bytes);
    file.seek(SeekFrom::Start(start))?;
    let want = (len - start).min(read_bytes);
    let mut bytes = Vec::with_capacity(usize::try_from(want.saturating_add(1)).unwrap_or(0));
    file.take(read_bytes).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    lines.pop();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    Ok(lines)
}

#[cfg(test)]
#[path = "packages_log_tests.rs"]
mod tests;
