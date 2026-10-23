// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Plain-text file citation parsing and safe local path resolution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Sentinel char for a wide glyph's continuation cell in [`find_path_span`].
pub(crate) const WIDE_CONTINUATION: char = '\0';

/// The longest token read as a path, in cells. A citation longer than this
/// is not one a person reads, and the bound keeps a hover's work fixed.
const MAX_TOKEN_CELLS: usize = 1024;

/// Plain-text file citation span found in a row ([`find_path_span`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathSpan {
    /// Printed path without `:line[:col]` or `#L` suffix.
    pub(crate) path: String,
    /// Optional 1-based line number.
    pub(crate) line: Option<u32>,
    /// Optional 1-based column number.
    pub(crate) col: Option<u32>,
    /// Inclusive start column.
    pub(crate) start: usize,
    /// Inclusive end column.
    pub(crate) end: usize,
}

/// Characters that end a path token: whitespace, the quotes and brackets a
/// citation is wrapped in, and the pipes of a table.
fn is_delimiter(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
        )
}

/// Trailing sentence punctuation that is never part of a citation.
fn is_trailing_punct(ch: char) -> bool {
    matches!(ch, '.' | ',' | ';' | ':' | '!' | '?')
}

/// Parse a run of ASCII digits into a positive `u32`.
fn positive(digits: &str) -> Option<u32> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok().filter(|n| *n > 0)
}

/// Split a token into `(path, line, col)`: `path:L[:C]` or `path#L<l>[C<c>]
/// [-L<m>[C<n>]]`.
fn split_suffix(token: &str) -> (&str, Option<u32>, Option<u32>) {
    if let Some(hash) = token.rfind("#L") {
        let frag = &token[hash + 2..];
        let first = frag.split('-').next().unwrap_or("");
        let (line, col) = match first.split_once('C') {
            Some((l, c)) => (positive(l), positive(c)),
            None => (positive(first), None),
        };
        if line.is_some() {
            return (&token[..hash], line, col);
        }
    }
    if let Some((head, last)) = token.rsplit_once(':')
        && let Some(n) = positive(last)
    {
        if let Some((path, mid)) = head.rsplit_once(':')
            && let Some(line) = positive(mid)
        {
            return (path, Some(line), Some(n));
        }
        return (head, Some(n), None);
    }
    (token, None, None)
}

/// Whether `path` reads as a file path at all: it has a separator or an
/// extension, and a letter or digit somewhere.
fn looks_like_path(path: &str) -> bool {
    if path.is_empty() || !path.chars().any(char::is_alphanumeric) {
        return false;
    }
    if path.contains('/') || (cfg!(windows) && path.contains('\\')) {
        return true;
    }
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| !ext.is_empty() && ext.chars().all(char::is_alphanumeric))
}

/// Find a plain-text file citation covering column `col` of `chars`.
pub(crate) fn find_path_span(chars: &[char], col: usize) -> Option<PathSpan> {
    let at = *chars.get(col)?;
    let in_token = |ch: char| ch == WIDE_CONTINUATION || !(is_delimiter(ch) || ch.is_control());
    if !in_token(at) {
        return None;
    }
    let mut start = col;
    while start > 0 && in_token(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && in_token(chars[end]) {
        end += 1;
    }
    if end - start > MAX_TOKEN_CELLS {
        return None;
    }
    while start < end && chars[start] == WIDE_CONTINUATION {
        start += 1;
    }
    while end > start && is_trailing_punct(chars[end - 1]) {
        end -= 1;
    }
    if !(start..end).contains(&col) {
        return None;
    }
    let token: String = chars[start..end]
        .iter()
        .filter(|ch| **ch != WIDE_CONTINUATION)
        .collect();
    if token.contains("://") {
        return None;
    }
    let (path, line, col_no) = split_suffix(&token);
    if !looks_like_path(path) {
        return None;
    }
    Some(PathSpan {
        path: path.to_string(),
        line,
        col: col_no,
        start,
        end: end - 1,
    })
}

/// Reason a file citation refuses to open ([`resolve`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathRefusal {
    /// Path or canonical target contains a control, format, or non-printable character ([`hides_text`]).
    Control,
    /// Relative path with no session base directory.
    NoBase,
    /// Target does not exist (or starts with unexpanded `~user`).
    Missing,
    /// Target is a directory.
    Directory,
    /// Target is or lies inside an `.app` bundle.
    AppBundle,
    /// Target exists but is not a regular file.
    NotRegular,
    /// Target is a regular file that cannot be opened for reading.
    Unreadable,
}

/// Characters that may reorder, hide or impersonate text, by general rule
/// rather than by list of known tricks: every control (general category `Cc`,
/// [`char::is_control`]), every format character (`Cf`: the bidi marks,
/// embeddings, overrides and isolates, the zero-width characters, the word
/// joiner and invisible operators, the soft hyphen, the Arabic letter mark,
/// the BOM, the tag characters, …), and every other code point Unicode says
/// a renderer draws as nothing (`Default_Ignorable_Code_Point`: the
/// combining grapheme joiner, the Hangul fillers, the Khmer inherent vowels,
/// the Mongolian free variation selectors, the variation selectors and their
/// supplement, and the unassigned rest of the tag plane).
fn is_hostile_char(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061c}'
                | '\u{06dd}'
                | '\u{070f}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08e2}'
                | '\u{115f}'..='\u{1160}'
                | '\u{17b4}'..='\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}'
                | '\u{110bd}'
                | '\u{110cd}'
                | '\u{13430}'..='\u{1343f}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e0fff}'
        )
}

/// Whether `text` contains any control, bidi/format, or default-ignorable character that can hide or spoof text.
pub(crate) fn hides_text(text: &str) -> bool {
    let mut prev = None;
    for ch in text.chars() {
        let shown = ch == '\u{fe0f}' && prev.is_some_and(aterm_grapheme::is_vs16_emoji_capable);
        if is_hostile_char(ch) && !shown {
            return true;
        }
        prev = Some(ch);
    }
    false
}

/// Foreground process facts read by [`runs_here`] when resolving a citation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Front<'a> {
    /// Published foreground process name (`status program=`).
    pub(crate) program: Option<&'a str>,
    /// Identified agent in the foreground (`harness_host::identified_agent`).
    pub(crate) agent: Option<aterm_phase::Program>,
    /// Whether the foreground process group is the session's root shell.
    pub(crate) root: bool,
    /// Whether shell integration reports a command currently executing.
    pub(crate) running: bool,
}

/// Whether the foreground program is known to run locally on this machine.
pub(crate) fn runs_here(front: &Front<'_>, lone_leader: impl FnOnce() -> Option<String>) -> bool {
    let Some(program) = front.program else {
        return false;
    };
    if crate::quit_safety::is_shell_name(program) {
        return !(front.root && front.running)
            && lone_leader().is_some_and(|name| crate::quit_safety::is_shell_name(&name));
    }
    front.agent.is_some()
}

/// Kernel process name for `pgid`'s leader when it is the sole member of its process group.
pub(crate) fn lone_leader(pgid: i32) -> Option<String> {
    u32::try_from(pgid)
        .ok()
        .filter(|pgid| *pgid > 0)
        .and_then(aterm_sysprobe::lone_group_leader)
}

/// Whether cwd report `seq` still applies to foreground process group `front`.
pub(crate) fn report_stands(seq: Option<u64>, witness: Option<(u64, i32)>, front: i32) -> bool {
    seq.is_some_and(|seq| {
        witness.is_none_or(|(seen, group)| seen != seq || (group > 0 && group == front))
    })
}

/// Whether `program` is a remote-shell client (`ssh`, `mosh`, `docker`, etc.).
pub(crate) fn is_remote_shell(program: &str) -> bool {
    let word = program.split_whitespace().next().unwrap_or("");
    let word = word.rsplit('/').next().unwrap_or(word);
    let word = word.trim_start_matches('-');
    ["ssh", "slogin", "autossh", "mosh", "mosh-client", "et"]
        .iter()
        .any(|shell| word.eq_ignore_ascii_case(shell))
}

/// One candidate's admission: the canonical target, or why not.
fn admit(candidate: &Path) -> Result<PathBuf, PathRefusal> {
    let canonical = std::fs::canonicalize(candidate).map_err(|_| PathRefusal::Missing)?;
    let meta = std::fs::metadata(&canonical).map_err(|_| PathRefusal::Missing)?;
    let bundle = canonical.components().any(|c| {
        Path::new(c.as_os_str())
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
    });
    if bundle {
        return Err(PathRefusal::AppBundle);
    }
    if meta.is_dir() {
        return Err(PathRefusal::Directory);
    }
    if !meta.is_file() {
        return Err(PathRefusal::NotRegular);
    }
    let file = open_for_look(&canonical).map_err(|_| PathRefusal::Unreadable)?;
    if !file.metadata().is_ok_and(|opened| opened.is_file()) {
        return Err(PathRefusal::NotRegular);
    }
    Ok(canonical)
}

/// Open `path` read-only for [`admit`]'s readability look.
fn open_for_look(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_NOCTTY);
    }
    options.open(path)
}

/// Resolve `raw` against `bases` and `home` to a canonical readable regular file.
pub(crate) fn resolve(
    raw: &str,
    bases: &[PathBuf],
    home: Option<&Path>,
) -> Result<PathBuf, PathRefusal> {
    if raw.is_empty() {
        return Err(PathRefusal::Missing);
    }
    if hides_text(raw) {
        return Err(PathRefusal::Control);
    }
    let home_relative = raw.starts_with("~/") || (cfg!(windows) && raw.starts_with("~\\"));
    let candidates: Vec<PathBuf> = if raw == "~" || home_relative {
        let home = home.ok_or(PathRefusal::Missing)?;
        vec![home.join(raw[1..].trim_start_matches(|c| c == '/' || (cfg!(windows) && c == '\\')))]
    } else if raw.starts_with('~') {
        return Err(PathRefusal::Missing);
    } else if Path::new(raw).is_absolute() {
        vec![PathBuf::from(raw)]
    } else {
        let bases: Vec<PathBuf> = bases
            .iter()
            .filter(|base| base.is_absolute())
            .map(|base| base.join(raw))
            .collect();
        if bases.is_empty() {
            return Err(PathRefusal::NoBase);
        }
        bases
    };
    let mut refusal = None;
    for candidate in &candidates {
        match admit(candidate) {
            Ok(path) if path.to_str().is_none_or(hides_text) => {
                refusal.get_or_insert(PathRefusal::Control);
            }
            Ok(path) => return Ok(path),
            Err(PathRefusal::Missing) => {}
            Err(other) => {
                refusal.get_or_insert(other);
            }
        }
    }
    Err(refusal.unwrap_or(PathRefusal::Missing))
}

/// Resolved file citation ready to open ([`resolve`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathTarget {
    /// Canonical regular file path.
    pub(crate) path: PathBuf,
    /// Optional 1-based line number.
    pub(crate) line: Option<u32>,
    /// Optional 1-based column number.
    pub(crate) col: Option<u32>,
    /// Display caption ([`display`]).
    pub(crate) shown: String,
}

impl PathTarget {
    /// Resolve `span` synchronously against `bases` and `home`.
    #[cfg(test)]
    pub(crate) fn resolve(
        span: &PathSpan,
        bases: &[PathBuf],
        home: Option<&Path>,
    ) -> Result<Self, PathRefusal> {
        let found = look(&Question::new(&span.path, bases, home))?;
        Ok(Self::of(span, found))
    }

    /// The target for `span` from what a look found.
    fn of(span: &PathSpan, (path, name): (PathBuf, String)) -> Self {
        Self {
            path,
            line: span.line,
            col: span.col,
            shown: with_line(name, span.line, span.col),
        }
    }
}

/// What the filesystem is asked about one citation: the printed path and the
/// directories it is read against.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Question {
    raw: String,
    bases: Vec<PathBuf>,
    home: Option<PathBuf>,
}

impl Question {
    fn new(raw: &str, bases: &[PathBuf], home: Option<&Path>) -> Self {
        Self {
            raw: raw.to_string(),
            bases: bases.to_vec(),
            home: home.map(Path::to_path_buf),
        }
    }
}

/// What a look found: the canonical file and the caption's name for it (no
/// line), or why it opens nothing.
type Found = Result<(PathBuf, String), PathRefusal>;

/// The look itself: [`resolve`], then the caption's name: the file where it
/// really is (`~/…` under home, else in full), never relative to one of the
/// session's directories, which the band does not show.
fn look(question: &Question) -> Found {
    let path = resolve(&question.raw, &question.bases, question.home.as_deref())?;
    let name = display(&path, None, None, None, question.home.as_deref());
    Ok((path, name))
}

/// Maximum UI wait per filesystem lookup.
pub(crate) const UI_WAIT: Duration = Duration::from_millis(25);

/// How long a known answer is reused: a pointer resting on a citation while
/// output streams asks once, not on every frame.
const LOOK_FRESH: Duration = Duration::from_secs(2);

/// Looks in flight at once, at most: a hung mount strands no more threads than
/// this, and a citation asked past it is no link until one comes back.
const LOOKS_IN_FLIGHT: usize = 4;

/// Answers kept, at most; the stale ones go first.
const LOOKS_KEPT: usize = 256;

/// Bounded background filesystem resolver and cache for UI citation lookups.
pub(crate) struct Looks {
    book: Arc<(Mutex<Book>, Condvar)>,
    wait: Duration,
}

#[derive(Default)]
struct Book {
    answers: HashMap<Question, Answer>,
    in_flight: usize,
}

enum Answer {
    Asked,
    Known { found: Found, at: Instant },
}

impl Book {
    /// Make room: drop the stale answers, then (still full) every known one.
    fn prune(&mut self, now: Instant) {
        if self.answers.len() < LOOKS_KEPT {
            return;
        }
        self.answers.retain(|_, answer| match answer {
            Answer::Asked => true,
            Answer::Known { at, .. } => now.duration_since(*at) < LOOK_FRESH,
        });
        if self.answers.len() >= LOOKS_KEPT {
            self.answers
                .retain(|_, answer| matches!(answer, Answer::Asked));
        }
    }
}

impl Looks {
    /// Create a resolver that waits up to `wait` per lookup.
    pub(crate) fn new(wait: Duration) -> Self {
        Self {
            book: Arc::new((Mutex::new(Book::default()), Condvar::new())),
            wait,
        }
    }

    /// Resolve `span` against `bases` and `home`, returning `None` while the lookup is still in flight.
    pub(crate) fn target(
        &self,
        span: &PathSpan,
        bases: &[PathBuf],
        home: Option<&Path>,
    ) -> Option<Result<PathTarget, PathRefusal>> {
        let question = Question::new(&span.path, bases, home);
        if question.raw.is_empty() {
            return Some(Err(PathRefusal::Missing));
        }
        if hides_text(&question.raw) {
            return Some(Err(PathRefusal::Control));
        }
        self.ask(question)
            .map(|found| found.map(|found| PathTarget::of(span, found)))
    }

    fn ask(&self, question: Question) -> Option<Found> {
        let (lock, answered) = &*self.book;
        let mut book = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        match book.answers.get(&question) {
            Some(Answer::Known { found, at }) if now.duration_since(*at) < LOOK_FRESH => {
                return Some(found.clone());
            }
            Some(Answer::Asked) => return None,
            _ => {}
        }
        if book.in_flight >= LOOKS_IN_FLIGHT {
            return None;
        }
        book.prune(now);
        book.answers.insert(question.clone(), Answer::Asked);
        book.in_flight += 1;
        let shared = Arc::clone(&self.book);
        let asked = question.clone();
        let spawned = std::thread::Builder::new()
            .name("aterm-citation-look".into())
            .spawn(move || {
                let found = look(&asked);
                let (lock, answered) = &*shared;
                let mut book = lock.lock().unwrap_or_else(PoisonError::into_inner);
                book.in_flight = book.in_flight.saturating_sub(1);
                book.answers.insert(
                    asked,
                    Answer::Known {
                        found,
                        at: Instant::now(),
                    },
                );
                answered.notify_all();
            });
        if spawned.is_err() {
            book.answers.remove(&question);
            book.in_flight = book.in_flight.saturating_sub(1);
            return None;
        }
        let deadline = now + self.wait;
        loop {
            if let Some(Answer::Known { found, .. }) = book.answers.get(&question) {
                return Some(found.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            book = answered
                .wait_timeout(book, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Format a resolved `target` for display relative to `base` or `home`, appending `:line[:col]`.
pub(crate) fn display(
    path: &Path,
    line: Option<u32>,
    col: Option<u32>,
    base: Option<&Path>,
    home: Option<&Path>,
) -> String {
    let under = |base: &Path| -> Option<String> {
        let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
        path.strip_prefix(&base)
            .ok()
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(|rel| rel.to_string_lossy().into_owned())
    };
    let text = base
        .and_then(under)
        .or_else(|| {
            home.and_then(under)
                .map(|rel| format!("~{}{rel}", std::path::MAIN_SEPARATOR))
        })
        .unwrap_or_else(|| plain_text(path));
    with_line(text, line, col)
}

/// `path` as a person reads it.
fn plain_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{share}");
        }
        if let Some(disk) = text.strip_prefix(r"\\?\")
            && disk.as_bytes().get(1) == Some(&b':')
        {
            return disk.to_string();
        }
    }
    text.into_owned()
}

/// `text` with `:line[:col]` after it.
fn with_line(mut text: String, line: Option<u32>, col: Option<u32>) -> String {
    if let Some(line) = line {
        text.push_str(&format!(":{line}"));
        if let Some(col) = col {
            text.push_str(&format!(":{col}"));
        }
    }
    text
}

/// Action chosen for a link click ([`decide`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkAction {
    /// Open an allowlisted URL externally.
    External(String),
    /// Open a resolved file citation in aterm's editor tab.
    Document(PathTarget),
    /// Perform no link action.
    Nothing,
}

/// Choose the [`LinkAction`] for a link click from optional URL and resolved path targets.
#[must_use]
pub(crate) fn decide(url: Option<String>, path: Option<PathTarget>) -> LinkAction {
    match (url, path) {
        (Some(url), _) if crate::is_safe_url(&url) => LinkAction::External(url),
        (Some(_), _) => LinkAction::Nothing,
        (None, Some(target)) => LinkAction::Document(target),
        (None, None) => LinkAction::Nothing,
    }
}

/// Convert a 1-based `(line, col)` into a UTF-8 byte offset in `text`.
#[must_use]
pub(crate) fn line_col_offset(text: &str, line: u32, col: Option<u32>) -> usize {
    let mut start = 0;
    let mut seen = 1u32;
    while seen < line {
        let Some(at) = text[start..].find('\n') else {
            break;
        };
        start += at + 1;
        seen += 1;
    }
    let line_text = text[start..].split('\n').next().unwrap_or("");
    let skip = col.map_or(0, |c| c.saturating_sub(1) as usize);
    let within = line_text
        .char_indices()
        .nth(skip)
        .map_or(line_text.len(), |(at, _)| at);
    start + within
}

#[cfg(test)]
#[path = "path_link_tests.rs"]
mod tests;
