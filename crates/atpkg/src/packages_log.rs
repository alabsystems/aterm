// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The package log: `packages.log`, beside `aterm.log` in the per-user log directory ([`aterm_types::dirs::logs_dir`]).

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::Status;
use crate::store::Layout;

/// The file's name inside the log directory.
pub const LOG_NAME: &str = "packages.log";

/// Rotate once the file reaches this size (1 MiB — a few thousand passes).
pub const ROTATE_AT_BYTES: u64 = 1024 * 1024;

/// Rotated files kept: `packages.log.1` (newest) … `packages.log.5` (oldest).
pub const KEEP_ROTATED: u32 = 5;

/// How many events Settings ▸ Packages' Activity reads, of the kinds it shows.
pub const ACTIVITY_EVENTS: usize = 50;

/// The longest value a line carries, in characters; the rest is `…`.
const MAX_VALUE_CHARS: usize = 400;

/// The most a tail reads from the end of one file: far more than [`ACTIVITY_EVENTS`] lines,
/// and never the whole of a file someone grew by hand.
const TAIL_READ_BYTES: u64 = 128 * 1024;

/// The kinds of line.
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

/// Where the log lives: `<logs dir>/packages.log`, or `None` when no log directory resolves (no home).
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
pub(crate) mod test_bind {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static BOUND: RefCell<Option<(PathBuf, PathBuf)>> = const { RefCell::new(None) };
    }

    /// Log `prefix`'s events into `dir` on this thread until the guard drops.
    pub(crate) fn bind(dir: PathBuf, prefix: PathBuf) -> Guard {
        BOUND.with(|b| *b.borrow_mut() = Some((dir, prefix)));
        Guard
    }

    pub(super) fn get() -> Option<(PathBuf, PathBuf)> {
        BOUND.with(|b| b.borrow().clone())
    }

    pub(crate) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            BOUND.with(|b| *b.borrow_mut() = None);
        }
    }
}

/// One program's row moving, as the log records it.
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

/// One event.
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
        /// How it ended: `exit=<code>`, `signal=<n>`, `killed` (at the deadline) or `spawn-failed`; or why it did not run: `opted-out`, `never-run`, `setup-unread`.
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

/// Append `event` about `layout`'s store when this log records it ([`records`]), rotating first when due and this process holds the store lock.
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

/// Milliseconds since this process's pass began, when it is inside one.
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
mod pass_clock {
    use std::cell::Cell;

    thread_local! {
        static START: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    }

    pub(super) fn set(at: Option<std::time::Instant>) {
        START.with(|start| start.set(at));
    }

    pub(super) fn get() -> Option<std::time::Instant> {
        START.with(Cell::get)
    }
}

/// Log every program transition from `before` to `after`: [`crate::status::write`]'s hook, which every row writer goes through (it asks [`records`] before reading `before`).
pub(crate) fn note_rows(layout: &Layout, before: Option<&Status>, after: &Status) {
    for transition in transitions(before, after) {
        append(layout, &Event::Program(&transition));
    }
}

/// The transitions from `before` to `after`, in program order: a moved build (installed, updated, rolled back, removed), a row that became not current or changed its reason ([`crate::state::not_current`]), or one current again.
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

/// `event` as its one line, `\n` included.
#[must_use]
pub fn render(unix: i64, pid: u32, event: &Event<'_>) -> String {
    render_at(unix, pid, None, event)
}

/// [`render`], with the line's offset into its pass (`t_ms`) when it was written inside one.
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

/// One line of the grammar, `\n` included: the stamp (`-` for a clock before the epoch or `now_unix`'s fail-closed `i64::MAX`), `kind`, the pid, then `\t<key>=<value>` per field, each value [`sanitize_capped`] to `max_chars` and an empty one left out.
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

/// `\t<key>=<sanitized value>`, or nothing for an empty value.
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

/// `sanitize`, capped at `max_chars` characters instead: a caller that keeps a whole diagnostic in one value.
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

/// The Unicode bidirectional formatting characters (LRM/RLM/ALM, the embeddings and
/// overrides, the isolates): invisible, and able to make a line read as something else.
const fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// The zero-width characters (ZWSP, ZWNJ, ZWJ, the word joiner, the BOM): invisible, and
/// able to hide a word from a reader and split one from the redactor.
const fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
}

/// Every `scheme://…` in `text` cut to `scheme://host/path`: the userinfo (`user:pass@`) is
/// dropped, and a query or fragment (where signed download URLs carry credentials) becomes
/// `?…`.
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

/// What a redacted credential reads as.
const REDACTED: &str = "[redacted]";

/// Every credential in `text` replaced by `[redacted]`, word by word (split on spaces; the
/// controls are spaces by now):
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
    let mut out = String::with_capacity(text.len());
    let mut hide = Hide::Nothing;
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if word.is_empty() {
            continue;
        }
        let wrapping = |c: char| {
            matches!(
                c,
                '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | ',' | ';' | '.' | '<' | '>' | '`'
            )
        };
        let bare = word.trim_matches(wrapping);
        match hide {
            Hide::Next => {
                out.push_str(REDACTED);
                hide = Hide::Nothing;
                continue;
            }
            Hide::Scheme => {
                if matches!(
                    bare.to_ascii_lowercase().as_str(),
                    "bearer" | "basic" | "token" | "digest"
                ) {
                    out.push_str(word);
                    hide = Hide::Next;
                } else {
                    out.push_str(REDACTED);
                    hide = Hide::Nothing;
                }
                continue;
            }
            Hide::Nothing => {}
        }
        if is_secret_shaped(bare) {
            out.push_str(REDACTED);
            continue;
        }
        if let Some(at) = bare.find(['=', ':']) {
            let key = bare[..at].trim_matches(['"', '\'']).to_ascii_lowercase();
            let glued = bare[at + 1..].trim_matches(['"', '\'']);
            let sep = &bare[at..=at];
            if key == "authorization" && glued.is_empty() {
                out.push_str(word);
                hide = Hide::Scheme;
                continue;
            }
            if is_credential_key(&key, sep == "=") || key == "authorization" {
                if glued.is_empty() {
                    out.push_str(word);
                    hide = Hide::Next;
                } else {
                    let lead = word.len() - word.trim_start_matches(wrapping).len();
                    out.push_str(&word[..lead]);
                    out.push_str(&bare[..=at]);
                    out.push_str(REDACTED);
                }
                continue;
            }
        }
        if matches!(bare.to_ascii_lowercase().as_str(), "bearer" | "token") {
            hide = Hide::Next;
        }
        out.push_str(word);
    }
    out
}

/// Whether `key` (lowercase, unquoted) names a credential.
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

/// Whether `word` looks like a credential by shape alone: a known token prefix (GitHub, `sk-`,
/// GitLab, Slack, npm), an AWS access key id, or a run of 32 or more `[A-Za-z0-9_-]` with
/// upper case, lower case and a digit.
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

/// Append `line` to `<dir>/packages.log`: the directory made private when missing, the file rotated first when `may_rotate` and due ([`rotate_if_due`]), then one write on an `O_APPEND` descriptor opened for this line (`0600`, never through a link).
/// # Errors
/// The directory, the rotation's renames, the open or the write failed.
pub fn append_line(dir: &Path, line: &str, may_rotate: bool) -> io::Result<()> {
    aterm_types::fs_restricted::ensure_private_dir(dir)?;
    let path = dir.join(LOG_NAME);
    if may_rotate {
        // A rotation that fails leaves the file where it is: the line still lands.
        let _ = rotate_if_due(&path);
    }
    open_append(&path)?.write_all(line.as_bytes())
}

#[cfg(unix)]
fn open_append(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_append(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
}

/// `packages.log.<n>` beside `path`.
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

/// Rotate `path` when it is a regular file of at least [`ROTATE_AT_BYTES`]: `.4` → `.5` … `.1` → `.2`, then the file → `.1` (the oldest, `.5`, is replaced).
/// # Errors
/// A rename failed (a missing rotated file is not a failure).
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

/// One line read back.
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
    /// The value of `key`, if the line carried it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// `line` (with or without its `\n`) read back; `None` for anything this module did not write — a torn line, a foreign one, an unknown kind.
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

/// Any line of the grammar ([`render_record`]) read back, whatever its `[a-z-]` kind; `None` for a torn or foreign line.
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
    let mut pid = None;
    let mut fields = Vec::new();
    for part in parts {
        let (key, value) = part.split_once('=')?;
        if key == "pid" {
            pid = value.parse().ok();
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

/// The newest `max` events of the log in `dir`, newest first: the tail of `packages.log`, then of `packages.log.1` when that is not enough.
#[must_use]
pub fn read_tail(dir: &Path, max: usize) -> Vec<Entry> {
    read_tail_of(dir, max, |_| true)
}

/// [`read_tail`] of the events `keep` admits: `max` of those, read past every other kind — so a reader that shows some kinds is not short of them where another kind is frequent (each discovery's `index-resolved` line beside every pass's two).
#[must_use]
pub fn read_tail_of(dir: &Path, max: usize, keep: impl Fn(&Entry) -> bool) -> Vec<Entry> {
    let path = dir.join(LOG_NAME);
    let mut out = Vec::new();
    for file in [path.clone(), rotated_path(&path, 1)] {
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

/// The package log's newest `max` events ([`read_tail`] of [`log_path`]'s directory).
#[must_use]
pub fn tail(max: usize) -> Vec<Entry> {
    tail_of(max, |_| true)
}

/// The package log's newest `max` events that `keep` admits ([`read_tail_of`] of [`log_path`]'s directory).
#[must_use]
pub fn tail_of(max: usize, keep: impl Fn(&Entry) -> bool) -> Vec<Entry> {
    log_dir().map_or_else(Vec::new, |dir| read_tail_of(&dir, max, keep))
}

/// Every record ([`parse_record`]) in the last `read_bytes` of `path`, oldest first; a line cut by that window, torn or foreign is skipped.
/// # Errors
/// `path` exists but is not a regular file, or opening, seeking or reading it failed.
#[cfg(test)]
pub(crate) fn read_records(path: &Path, read_bytes: u64) -> io::Result<Vec<Entry>> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a regular file",
            ));
        }
        Ok(_) => {}
    }
    Ok(read_tail_lines(path, read_bytes)?
        .iter()
        .filter_map(|line| parse_record(line))
        .collect())
}

/// The whole lines of the last `read_bytes` of `path`, oldest first; empty when it is
/// absent, unreadable or not a regular file.
fn tail_lines(path: &Path, read_bytes: u64) -> Vec<String> {
    let regular = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file());
    if !regular {
        return Vec::new();
    }
    read_tail_lines(path, read_bytes).unwrap_or_default()
}

/// [`tail_lines`]' read, its failures kept.
fn read_tail_lines(path: &Path, read_bytes: u64) -> io::Result<Vec<String>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(read_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
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
mod tests {
    use super::*;

    fn scratch(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-packages-log-{label}-"))
    }

    fn transition() -> Transition {
        Transition {
            program: "claude".into(),
            from: Some("2.1.278".into()),
            to: Some("2.1.280".into()),
            source: "Anthropic latest".into(),
            result: "updated",
            reason: String::new(),
        }
    }

    /// A stamp, a kind, the pid, then tab-separated `key=value` fields on one line, empty
    /// fields left out; it reads back field for field.
    #[test]
    fn a_line_is_one_tab_separated_record_that_reads_back() {
        let t = transition();
        let line = render(1_790_000_000, 4242, &Event::Program(&t));
        assert_eq!(
            line,
            "2026-09-21T14:13:20Z\tprogram\tpid=4242\tprogram=claude\tfrom=2.1.278\t\
             to=2.1.280\tsource=Anthropic latest\tresult=updated\n"
        );
        assert_eq!(line.matches('\n').count(), 1);
        let entry = parse_line(&line).unwrap();
        assert_eq!(entry.at, Some(1_790_000_000));
        assert_eq!(entry.kind, kind::PROGRAM);
        assert_eq!(entry.pid, 4242);
        assert_eq!(entry.get("to"), Some("2.1.280"));
        assert_eq!(entry.get("reason"), None, "an empty field is not written");

        let end = render(
            1_790_000_000,
            7,
            &Event::PassEnd {
                lane: "window",
                verb: "update",
                exit: 69,
                secs: 12,
                outcome: "offline",
            },
        );
        assert_eq!(
            end,
            "2026-09-21T14:13:20Z\tpass-end\tpid=7\tlane=window\tverb=update\texit=69\t\
             secs=12\toutcome=offline\n"
        );
        let seam = render(
            1_790_000_000,
            7,
            &Event::Seam {
                seam: "rustup:trust",
                from: "/h/trust/build/host/stage2",
                to: "/p/rustup/trust",
                why: "a toolchain from 2026-08-20, older than the store's 2026-09-17",
                undo: "to put it back: ln -sfn '/h/trust/build/host/stage2' '/h/.rustup/toolchains/trust'",
            },
        );
        let read = parse_line(&seam).expect("a seam line is one of the log's own");
        assert_eq!(read.kind, kind::SEAM);
        assert_eq!(read.get("from"), Some("/h/trust/build/host/stage2"));
        assert_eq!(
            read.get("undo"),
            Some(
                "to put it back: ln -sfn '/h/trust/build/host/stage2' '/h/.rustup/toolchains/trust'"
            ),
            "a path and its quotes pass the sanitiser whole"
        );
        let start = render(
            i64::MAX,
            7,
            &Event::PassStart {
                lane: "typed",
                verb: "seed",
            },
        );
        assert!(start.starts_with("-\tpass-start\t"), "{start}");
        assert_eq!(parse_line(&start).unwrap().at, None);
        let warm = render(
            1_790_000_000,
            9,
            &Event::Warm {
                program: "claude",
                build: crate::vendor_direct::Version::parse("2.1.280")
                    .unwrap()
                    .build_id(),
                outcome: "exit=0",
                ms: 10_412,
            },
        );
        assert_eq!(
            warm,
            "2026-09-21T14:13:20Z\twarm\tpid=9\tprogram=claude\tbuild=2.1.280\t\
             outcome=exit=0\tms=10412\n"
        );
        let entry = parse_line(&warm).unwrap();
        assert_eq!(entry.kind, kind::WARM);
        assert_eq!(entry.pid, 9);
        assert_eq!(
            entry.fields,
            [
                ("program", "claude"),
                ("build", "2.1.280"),
                ("outcome", "exit=0"),
                ("ms", "10412"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        for foreign in [
            "",
            "hello",
            "2026-09-21T14:13:20Z\tsomething\tpid=1",
            "2026-09-21T14:13:20Z\tprogram",
            "2026-09-21T14:13:20Z\tprogram\tpid=1\tno-equals",
            "2026-09-21 14:13:20\tprogram\tpid=1",
        ] {
            assert_eq!(parse_line(foreign), None, "{foreign:?}");
        }
    }

    /// No value forges a field or a line, reorders the display or carries a credential, and
    /// a long one is capped.
    #[test]
    fn values_lose_controls_bidi_credentials_and_length() {
        let forged = sanitize("ok\tresult=installed\nfake line\u{1b}[31m");
        assert!(!forged.contains(['\t', '\n', '\u{1b}']), "{forged:?}");
        assert_eq!(sanitize("a\u{202E}b\u{2066}c\u{200F}d"), "abcd");
        assert_eq!(
            sanitize("fetch https://user:s3cret@github.com/o/r/releases?token=abc#x failed"),
            "fetch https://github.com/o/r/releases?… failed"
        );
        assert_eq!(
            sanitize("GET https://objects.githubusercontent.com/a?X-Amz-Signature=zz"),
            "GET https://objects.githubusercontent.com/a?…"
        );
        assert_eq!(
            sanitize("auth failed (ghp_abcdef123) with Bearer xyz and token ghs_q"),
            "auth failed [redacted] with Bearer [redacted] and token [redacted]"
        );
        for (raw, clean) in [
            (
                "fetch failed: Authorization: Bearer sk-ant-abc123secret",
                "fetch failed: Authorization: Bearer [redacted]",
            ),
            (
                "fetch failed: authorization: abc123secret now",
                "fetch failed: authorization: [redacted] now",
            ),
            ("error: token=abc123secret", "error: token=[redacted]"),
            ("error: token: abc123secret", "error: token: [redacted]"),
            ("password=hunter2 (retry)", "password=[redacted] (retry)"),
            (
                "{\"api_key\":\"abc\"} refused",
                "{\"api_key\":[redacted] refused",
            ),
            (
                "GITHUB_TOKEN=abc api_key: xyz",
                "GITHUB_TOKEN=[redacted] api_key: [redacted]",
            ),
            ("key=abc", "key=[redacted]"),
            ("api key sk-proj-XYZsecret", "api key [redacted]"),
            (
                concat!("AKIA", "IOSFODNN7EXAMPLE leaked"),
                "[redacted] leaked",
            ),
            (
                "session aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3z expired",
                "session [redacted] expired",
            ),
        ] {
            assert_eq!(sanitize(raw), clean, "{raw:?}");
        }
        let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        for kept in [
            format!("digest mismatch: expected {digest}"),
            "claude 2.1.281 refused: SHA256SUMS disagrees".to_string(),
            "names the refused key: channel".to_string(),
            "x86_64-apple-darwin-unknown-target-triple-long-name".to_string(),
        ] {
            assert_eq!(sanitize(&kept), kept);
        }
        assert_eq!(sanitize("a\u{2028}b\u{2029}c"), "a b c");
        assert_eq!(
            sanitize("gh\u{200B}p_x\u{200C}\u{200D}\u{2060}\u{FEFF}"),
            "[redacted]"
        );
        assert_eq!(sanitize("   spaced    out  "), "spaced out");
        let long = sanitize(&"x".repeat(10_000));
        assert_eq!(long.chars().count(), MAX_VALUE_CHARS + 1);
        assert!(long.ends_with('…'));
        let t = Transition {
            reason: "error: a\tb\nc https://u:p@h/x?y".into(),
            ..transition()
        };
        let line = render(1, 1, &Event::Program(&t));
        assert_eq!(line.matches('\n').count(), 1);
        let entry = parse_line(&line).unwrap();
        assert_eq!(entry.get("reason"), Some("error: a b c https://h/x?…"));
    }

    /// At 1 MiB the file becomes `.1`, the older ones shift, `.5` is the last kept; a writer
    /// without the store lock only appends.
    #[test]
    fn the_log_rotates_at_its_bound_keeping_five() {
        let dir = scratch("rotate");
        let path = dir.join(LOG_NAME);
        let big = "y".repeat(usize::try_from(ROTATE_AT_BYTES).unwrap());
        for n in 1..=KEEP_ROTATED {
            std::fs::write(rotated_path(&path, n), format!("old {n}\n")).unwrap();
        }
        std::fs::write(&path, &big).unwrap();
        append_line(&dir, "no-lock\n", false).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > ROTATE_AT_BYTES);
        assert_eq!(
            std::fs::read_to_string(rotated_path(&path, 1)).unwrap(),
            "old 1\n"
        );
        append_line(&dir, "fresh\n", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");
        assert!(
            std::fs::read_to_string(rotated_path(&path, 1))
                .unwrap()
                .ends_with("no-lock\n")
        );
        for n in 2..=KEEP_ROTATED {
            assert_eq!(
                std::fs::read_to_string(rotated_path(&path, n)).unwrap(),
                format!("old {}\n", n - 1)
            );
        }
        assert!(
            !rotated_path(&path, KEEP_ROTATED + 1).exists(),
            "only five kept"
        );
        assert!(!rotate_if_due(&path).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Any `[a-z-]` kind round-trips through `render_record`/`parse_record` under the
    /// caller's cap; `parse_line` admits only the package log's own kinds.
    #[test]
    fn the_grammar_is_shared_by_any_kind_with_its_own_cap() {
        let long = "e".repeat(1000);
        let line = render_record(
            1_790_000_000,
            "update",
            9,
            &[
                ("outcome", "warn"),
                ("title", "Update failed"),
                ("detail", &long),
            ],
            2000,
        );
        let entry = parse_record(&line).unwrap();
        assert_eq!(entry.kind, "update");
        assert_eq!(entry.pid, 9);
        assert_eq!(
            entry.get("detail"),
            Some(long.as_str()),
            "the whole value, no cut"
        );
        assert_eq!(parse_line(&line), None, "not a package-log kind");
        assert_eq!(sanitize_capped(&long, 10).chars().count(), 11);
        for foreign in [
            "2026-09-21T14:13:20Z	Upper	pid=1",
            "2026-09-21T14:13:20Z		pid=1",
            "2026-09-21T14:13:20Z	kind two	pid=1",
        ] {
            assert_eq!(parse_record(foreign), None, "{foreign:?}");
        }
        let dir = scratch("records");
        let path = dir.join("records.log");
        assert!(read_records(&path, 1024).unwrap().is_empty());
        assert!(read_records(&dir, 1024).is_err());
        let one = render_record(1_790_000_000, "app", 1, &[("title", "a")], 100);
        let two = render_record(1_790_000_001, "app", 1, &[("title", "b")], 100);
        std::fs::write(&path, format!("{one}{two}torn")).unwrap();
        let all = read_records(&path, 1024).unwrap();
        assert_eq!(
            all.iter()
                .filter_map(|e| e.get("title"))
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        let cut = read_records(&path, (two.len() + 8) as u64).unwrap();
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].get("title"), Some("b"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tail reads newest first across the rotation boundary, bounded by `max`.
    #[test]
    fn the_tail_is_newest_first_across_the_rotation() {
        let dir = scratch("tail");
        let path = dir.join(LOG_NAME);
        let line = |n: i64| {
            render(
                1_790_000_000 + n,
                1,
                &Event::PassStart {
                    lane: "window",
                    verb: "update",
                },
            )
        };
        std::fs::write(rotated_path(&path, 1), (0..3).map(line).collect::<String>()).unwrap();
        std::fs::write(&path, format!("{}{}torn-half", line(3), line(4))).unwrap();
        let tail = read_tail(&dir, 4);
        let ats: Vec<i64> = tail.iter().filter_map(|e| e.at).collect();
        assert_eq!(
            ats,
            vec![1_790_000_004, 1_790_000_003, 1_790_000_002, 1_790_000_001]
        );
        assert_eq!(read_tail(&dir, 50).len(), 5);
        assert!(read_tail(&dir.join("absent"), 50).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn row(build: Option<u64>, state: &str) -> crate::ProgramStatus {
        crate::ProgramStatus {
            installed_build: build,
            state: state.to_string(),
            tree_root: String::new(),
        }
    }

    fn status(rows: &[(&str, crate::ProgramStatus)]) -> Status {
        Status {
            schema: 1,
            last_index_build: 44,
            programs: rows
                .iter()
                .map(|(n, r)| ((*n).to_string(), r.clone()))
                .collect(),
            ..Default::default()
        }
    }

    /// A transition is a moved build or a change into, out of or within not-current; never a
    /// re-pin of the same build, a rebuilt record's `active`, or a bookkeeping row.
    #[test]
    fn transitions_are_moves_and_changes_of_reason() {
        use crate::state;
        let v = |s: &str| crate::vendor_direct::Version::parse(s).unwrap().build_id();
        let before = status(&[
            ("ay", row(Some(1971), &state::managed(1971, 43))),
            (
                "claude",
                row(
                    Some(v("2.1.278")),
                    &state::vendor_managed("2.1.278", "Anthropic"),
                ),
            ),
            (
                "codex",
                row(
                    Some(v("0.156.0")),
                    &state::vendor_managed("0.156.0", "OpenAI"),
                ),
            ),
            ("gh", row(Some(12), &state::managed(12, 43))),
            ("ty", row(Some(5), "error: stage: disk full")),
            ("*toolset*", row(None, "unavailable: index unreachable")),
        ]);
        let after = status(&[
            ("ay", row(Some(1971), &state::managed(1971, 44))),
            (
                "claude",
                row(
                    Some(v("2.1.280")),
                    &state::vendor_managed("2.1.280", "Anthropic"),
                ),
            ),
            (
                "codex",
                row(
                    Some(v("0.156.0")),
                    &state::vendor_kept("0.156.0", "held by local pin"),
                ),
            ),
            ("nn", row(Some(108), &state::managed(108, 44))),
            ("ty", row(Some(5), &state::managed(5, 44))),
            ("*toolset*", row(None, "")),
        ]);
        let got = transitions(Some(&before), &after);
        type Summary = (String, &'static str, Option<String>, Option<String>, String);
        let summary: Vec<Summary> = got
            .iter()
            .map(|t| {
                (
                    t.program.clone(),
                    t.result,
                    t.from.clone(),
                    t.to.clone(),
                    t.source.clone(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "claude".into(),
                    "updated",
                    Some("2.1.278".into()),
                    Some("2.1.280".into()),
                    "Anthropic latest".into()
                ),
                (
                    "codex".into(),
                    "held",
                    Some("0.156.0".into()),
                    None,
                    "OpenAI latest".into()
                ),
                (
                    "gh".into(),
                    "removed",
                    Some("build 12".into()),
                    None,
                    "ALab index 43".into()
                ),
                (
                    "nn".into(),
                    "installed",
                    None,
                    Some("build 108".into()),
                    "ALab index 44".into()
                ),
                (
                    "ty".into(),
                    "current",
                    Some("build 5".into()),
                    None,
                    "ALab index 44".into()
                ),
            ]
        );
        assert_eq!(got[1].reason, "held by local pin");
        let refused = "error: claude 2.1.281 refused: digest mismatch — keeping 2.1.280";
        let mut once = after.clone();
        once.programs
            .insert("claude".into(), row(Some(v("2.1.280")), refused));
        let t = transitions(Some(&after), &once);
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].result, t[0].reason.as_str()), ("refused", refused));
        assert!(transitions(Some(&once), &once).is_empty());
        let fresh = transitions(None, &once);
        let result_of = |p: &str| fresh.iter().find(|t| t.program == p).map(|t| t.result);
        assert_eq!(result_of("claude"), Some("refused"));
        assert_eq!(result_of("ay"), Some("installed"));
    }

    /// A row written through `status::write` for the bound store lands in the log; one for
    /// any other store does not.
    #[test]
    fn a_status_write_for_the_bound_store_logs_its_transitions() {
        let dir = scratch("hook");
        let store = scratch("hook-store");
        let other = scratch("hook-other");
        let _bound = test_bind::bind(dir.clone(), store.clone());
        let layout = Layout {
            prefix: store.clone(),
        };
        let st = status(&[("ay", row(Some(1971), &crate::state::managed(1971, 44)))]);
        crate::status::write(&layout, &st).unwrap();
        crate::status::write(
            &Layout {
                prefix: other.clone(),
            },
            &st,
        )
        .unwrap();
        let tail = read_tail(&dir, 10);
        assert_eq!(tail.len(), 1, "{tail:?}");
        assert_eq!(tail[0].get("program"), Some("ay"));
        assert_eq!(tail[0].get("result"), Some("installed"));
        crate::status::write(&layout, &st).unwrap();
        assert_eq!(read_tail(&dir, 10).len(), 1);
        for d in [&dir, &store, &other] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// THE PASS CLOCK: a pass over THE store stamps every line written inside it with `t_ms`,
    /// its offset from the pass's start — the fetcher's layout-free `index-resolved` line too,
    /// which reads back as the log's own kind — and nothing outside a pass, or inside a pass
    /// over another store, is stamped or written that way.
    #[test]
    fn lines_inside_a_pass_carry_their_offset_into_it() {
        let dir = scratch("clock");
        let store = scratch("clock-store");
        let other = scratch("clock-other");
        let _bound = test_bind::bind(dir.clone(), store.clone());
        let layout = Layout {
            prefix: store.clone(),
        };
        let start = Event::PassStart {
            lane: "typed",
            verb: "update",
        };
        let index = Event::IndexResolved {
            heads: 3,
            rounds: 2,
            gets: 8,
            found: "atpkg-index-45",
        };
        append(&layout, &start);
        append_in_pass(&index);
        start_pass_clock(&Layout {
            prefix: other.clone(),
        });
        append_in_pass(&index);
        start_pass_clock(&layout);
        append(&layout, &start);
        append_in_pass(&index);
        stop_pass_clock();
        append_in_pass(&index);
        let lines = read_tail(&dir, 10);
        let kinds: Vec<(&str, Option<&str>)> = lines
            .iter()
            .rev()
            .map(|e| (e.kind.as_str(), e.get("t_ms")))
            .collect();
        assert_eq!(kinds.len(), 3, "{lines:?}");
        assert_eq!(kinds[0], (kind::PASS_START, None), "before the pass");
        assert_eq!(kinds[1].0, kind::PASS_START);
        assert_eq!(kinds[2].0, kind::INDEX_RESOLVED);
        let t: Vec<u64> = kinds[1..]
            .iter()
            .map(|(_, t)| t.expect("stamped inside the pass").parse().unwrap())
            .collect();
        assert!(t[0] <= t[1], "{t:?}");
        let resolved = &lines[0];
        assert_eq!(resolved.get("heads"), Some("3"));
        assert_eq!(resolved.get("rounds"), Some("2"));
        assert_eq!(resolved.get("gets"), Some("8"));
        assert_eq!(resolved.get("found"), Some("atpkg-index-45"));
        for d in [&dir, &store, &other] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
