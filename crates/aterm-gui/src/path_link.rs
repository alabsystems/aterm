// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PLAIN-TEXT FILE CITATION: Cmd-click (Ctrl elsewhere) on
//! `path[:line[:col]]` or `path#L12` that a program printed opens aterm's own
//! editor tab at that line.
//!
//! Compilers (`src/main.rs:12:5`), grep (`src/lib.rs:40:`), Codex and Claude
//! Code (`crates/foo/src/bar.rs:42`, `README.md#L7`) all print citations as
//! plain text. One general reading serves every one of them; nothing here
//! knows which program wrote the row (law 3).
//!
//! # What a citation may open, and what it may never do
//!
//! * It opens ONE thing: aterm's own editor, an inert document tab
//!   (`App::open_document_tab_at`). It never reaches `open_url_external`:
//!   [`decide`] is the one place a click becomes an action, and a
//!   [`PathTarget`] has no arm that hands it to the OS. So the exec half of
//!   aterm's `file:` refusal (`is_safe_url`, `NEVER_ALLOW_SCHEMES`) never
//!   arises: no `.app` is launched and no handler runs.
//! * It names an EXISTING REGULAR FILE ([`resolve`]): symlinks are resolved
//!   (the tab opens the target, and the caption names the target), the
//!   target must be readable, and a directory, an `.app` bundle (or anything
//!   inside one), a FIFO or a device never qualifies.
//! * A relative citation is resolved against the session's own directories
//!   only: the foreground program's OS working directory (`agent_cwd`, the
//!   directory a Codex or Claude Code shortened its citation against), then
//!   the shell's OSC 7 report. Never aterm's own cwd: a session that names
//!   no directory gets no relative link.
//! * A citation resolves only while the session is known to run HERE
//!   (`App::resolve_citation`), read when it is asked. The program in front
//!   must be one aterm has named for the group in front right now and knows
//!   runs on this machine ([`runs_here`]: a shell, or an agent it identifies);
//!   anything else in front (`ssh`, `mosh`, `tmux`, `sudo`, `docker exec`, a
//!   pager, a name not resolved yet) offers no citation, so a remote or
//!   multiplexed session needs no list of remote programs to be refused. The
//!   shell's running command (`status detail=`) may not be a remote shell
//!   either ([`is_remote_shell`]: the moment between a typed `exec ssh` and
//!   the sweep's next naming). And the shell's OSC 7 report, while it stands
//!   ([`report_stands`]: under the group in front when aterm first saw it),
//!   must not name another host (`tab_label::names_this_machine`). What stays
//!   open: text an earlier remote session left on the screen, read once a
//!   local shell is back in front, resolves against this machine.
//! * Text carrying a control, format or default-ignorable character never
//!   resolves ([`hides_text`]), nor does a file whose real (symlink resolved)
//!   name carries one or is not UTF-8: the caption could not show what it
//!   names. The one such character a name keeps is an emoji presentation
//!   selector (U+FE0F) right after a character it turns into an emoji (`☁️`),
//!   which changes what is drawn; a zero-width joiner (U+200D, the glue of an
//!   emoji ZWJ sequence) or a redundant selector may hide a character and is
//!   refused.
//! * A click opens only what a caption showed: a citation opens only when
//!   the caption the window last drew for that cell named that very file,
//!   line and column; a URL under a standing caption only when it is the URL
//!   the caption named. Otherwise nothing opens and the caption is asked
//!   again (`App::open_link_under_pointer`).
//! * The hover discloses where the click goes before the hand is used:
//!   `opens in aterm: <path>:<line>` (`link_target::path_caption_row`), the
//!   resolved path, so a symlink's real target is what the band names. It is
//!   named where it really is, `~/…` under home or else in full
//!   ([`display`]), never relative to a directory the band does not show: two
//!   captions that read alike name the same file.
//! * The filesystem is never looked at on the UI thread ([`Looks`]): a look
//!   runs on its own thread, the UI waits a moment for it, and an answer not
//!   back by then is no link yet. A hung or slow mount, an automount or a
//!   dataless file costs at most that moment, once per citation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The column marker a wide glyph's continuation cell carries in the row
/// [`find_path_span`] reads: part of the token, contributing no character.
pub(crate) const WIDE_CONTINUATION: char = '\0';

/// The longest token read as a path, in cells. A citation longer than this
/// is not one a person reads, and the bound keeps a hover's work fixed.
const MAX_TOKEN_CELLS: usize = 1024;

/// One citation found in a row ([`find_path_span`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathSpan {
    /// The path as printed (no line suffix).
    pub(crate) path: String,
    /// The 1-based line the citation names, if any.
    pub(crate) line: Option<u32>,
    /// The 1-based column the citation names, if any.
    pub(crate) col: Option<u32>,
    /// First column of the span (inclusive).
    pub(crate) start: usize,
    /// Last column of the span (inclusive).
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
/// [-L<m>[C<n>]]`. A suffix that does not parse stays part of the path.
fn split_suffix(token: &str) -> (&str, Option<u32>, Option<u32>) {
    if let Some(hash) = token.rfind("#L") {
        let frag = &token[hash + 2..];
        // `#L4-L9` and `#L4-9`: the range starts at its first line.
        let first = frag.split('-').next().unwrap_or("");
        let (line, col) = match first.split_once('C') {
            Some((l, c)) => (positive(l), positive(c)),
            None => (positive(first), None),
        };
        if line.is_some() {
            return (&token[..hash], line, col);
        }
    }
    // `path:12:3` or `path:12`.
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
/// extension, and a letter or digit somewhere. Keeps ordinary words from
/// costing a filesystem look on every hover.
fn looks_like_path(path: &str) -> bool {
    if path.is_empty() || !path.chars().any(char::is_alphanumeric) {
        return false;
    }
    if path.contains('/') {
        return true;
    }
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| !ext.is_empty() && ext.chars().all(char::is_alphanumeric))
}

/// Find a plain-text citation covering column `col` of a row of chars (one
/// char per column; [`WIDE_CONTINUATION`] for a wide glyph's second column).
/// The span runs between delimiters ([`is_delimiter`]), so a path never
/// spans a space; trailing sentence punctuation is trimmed
/// (`see src/a.rs:12.` yields `src/a.rs` line 12). A URL (`://`) is not a
/// citation: the URL reader owns it.
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
    let mut end = col; // exclusive bound below
    while end < chars.len() && in_token(chars[end]) {
        end += 1;
    }
    if end - start > MAX_TOKEN_CELLS {
        return None;
    }
    // A wide continuation that opens the token belongs to a glyph that is not
    // in it; skip it.
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

/// Why a citation opens nothing ([`resolve`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathRefusal {
    /// The text, or the real name of the file it names, carries a control,
    /// format or default-ignorable character ([`hides_text`]), or that real
    /// name is not UTF-8.
    Control,
    /// A relative path, and the session names no directory to resolve it in.
    NoBase,
    /// Nothing exists there (or `~user`, which is never expanded).
    Missing,
    /// A directory.
    Directory,
    /// An `.app` bundle, or a file inside one.
    AppBundle,
    /// It exists but is not a regular file (a FIFO, a socket, a device).
    NotRegular,
    /// A regular file this process cannot open for reading.
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
/// supplement, and the unassigned rest of the tag plane). Tables from Unicode
/// 16.0 (`DerivedGeneralCategory.txt`, `DerivedCoreProperties.txt`); a
/// character a later version adds to `Cf` is one more row here.
fn is_hostile_char(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            // Cf and Default_Ignorable_Code_Point, merged and sorted.
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

/// Whether `text` carries a character that may hide or impersonate what is
/// drawn ([`is_hostile_char`]), read in context: an emoji presentation
/// selector (U+FE0F) right after a character whose default is text
/// presentation ([`aterm_grapheme::is_vs16_emoji_capable`]: `☁`, `❤`, a
/// keycap's digit) turns it into an emoji, a visible change, and is kept.
/// After anything else (a letter, an emoji already drawn as one) it changes
/// nothing on screen and is refused, as is every zero-width joiner.
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

/// Whether the program in front of a session, as the status sweep published
/// it, is known to run on THIS machine, so the paths it prints name this
/// machine's files: a shell (`quit_safety::is_shell_name`), or an agent aterm
/// identifies (`harness_host::identified_agent`: by its own name, or a
/// runtime like `node` by its screen). An allow-list by construction:
/// anything else (a remote shell, a multiplexer whose panes may be remote, a
/// wrapper that opens its own terminal, a program not named yet) is not
/// known to, and offers no citation (`App::resolve_citation`).
pub(crate) fn runs_here(front: &crate::session_timeline::AgentPublication) -> bool {
    front.program.as_deref().is_some_and(|program| {
        crate::quit_safety::is_shell_name(program)
            || crate::harness_host::identified_agent(front).is_some()
    })
}

/// Whether the engine's cwd report `seq` (`Terminal::reported_cwd_seq`)
/// still speaks for the session with group `front` in front, given the
/// status sweep's `witness` (`SessionTimeline::cwd_witness`: the report and
/// the group in front when the sweep first saw it). A report stands under
/// the group it was made under, so an OSC 7 an ssh's remote shell sent says
/// nothing once the local shell is back in front, and neither its host nor
/// its directory is read. A report newer than the sweep's last look stands
/// (it may be the remote's own: its host is read). A cwd a checkpoint
/// carried (`None`) was made under a group this process never saw, and
/// does not stand.
pub(crate) fn report_stands(seq: Option<u64>, witness: Option<(u64, i32)>, front: i32) -> bool {
    seq.is_some_and(|seq| witness.is_none_or(|(seen, group)| seen != seq || group == front))
}

/// The remote shells: a program in front by one of these names runs its
/// session on another machine, so the directory every local reading names
/// for it (its own OS cwd, a stale OSC 7) is not where its output's paths
/// live. Matched on the program's word as `status program=` and `detail=`
/// spell it (the `argv[0]` basename; the running command's program).
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
    // Readable, asked of what the open reached: never blocking (a FIFO swapped in
    // after the look above opens at once instead of waiting for a writer), never
    // through a link swapped in, and still a regular file once open.
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

/// Resolve a printed path to the canonical regular file it names: `~/…`
/// against `home`, an absolute path as itself, a relative one against each of
/// `bases` in order (the session's `agent_cwd`, then its OSC 7 cwd). The first
/// candidate that is an admissible file wins; else the refusal of the first
/// candidate that exists (a directory says so), else [`PathRefusal::Missing`].
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
    let candidates: Vec<PathBuf> = if raw == "~" || raw.starts_with("~/") {
        let home = home.ok_or(PathRefusal::Missing)?;
        vec![home.join(raw.trim_start_matches('~').trim_start_matches('/'))]
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
            // The caption names the real file; a name it could not show
            // honestly is no link (a symlink's target may carry what the
            // printed text does not).
            // A name that is not UTF-8 would be drawn with replacement
            // characters, which two different names can share.
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

/// A citation resolved to a file aterm may open ([`resolve`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathTarget {
    /// The canonical regular file.
    pub(crate) path: PathBuf,
    /// The 1-based line to reveal.
    pub(crate) line: Option<u32>,
    /// The 1-based column to reveal.
    pub(crate) col: Option<u32>,
    /// What the caption names ([`display`]).
    pub(crate) shown: String,
}

impl PathTarget {
    /// Resolve `span` against the session's `bases` ([`resolve`]), on this
    /// thread: the filesystem is looked at here. The UI asks through [`Looks`].
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
/// session's directories, which the band does not show. A citation printed
/// relative names a different file in each project that has one; two
/// captions that read alike must name one file, so a press refused because
/// the target moved under the pointer is seen to have moved.
fn look(question: &Question) -> Found {
    let path = resolve(&question.raw, &question.bases, question.home.as_deref())?;
    let name = display(&path, None, None, None, question.home.as_deref());
    Ok((path, name))
}

/// How long the UI waits for a look: far past a local file's (well under a
/// millisecond), short enough that a hung mount is a hitch, not a freeze.
pub(crate) const UI_WAIT: Duration = Duration::from_millis(25);

/// How long a known answer is reused: a pointer resting on a citation while
/// output streams asks once, not on every frame.
const LOOK_FRESH: Duration = Duration::from_secs(2);

/// Looks in flight at once, at most: a hung mount strands no more threads than
/// this, and a citation asked past it is no link until one comes back.
const LOOKS_IN_FLIGHT: usize = 4;

/// Answers kept, at most; the stale ones go first.
const LOOKS_KEPT: usize = 256;

/// THE UI'S WAY TO THE FILESYSTEM for a citation. Each new question is looked
/// at on its own thread; the asker waits up to `wait` for it, and an answer
/// not back by then is `None` (no link yet: the next pointer motion or frame
/// asks again and finds it). A question already in flight is never waited on
/// twice. Answers are reused for [`LOOK_FRESH`].
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
    /// Looks the UI waits on for at most `wait` each.
    pub(crate) fn new(wait: Duration) -> Self {
        Self {
            book: Arc::new((Mutex::new(Book::default()), Condvar::new())),
            wait,
        }
    }

    /// `span` resolved against `bases`: `Some` once the filesystem has
    /// answered, `None` while it has not (see [`Looks`]).
    pub(crate) fn target(
        &self,
        span: &PathSpan,
        bases: &[PathBuf],
        home: Option<&Path>,
    ) -> Option<Result<PathTarget, PathRefusal>> {
        let question = Question::new(&span.path, bases, home);
        // The text alone refuses without a look.
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

/// The caption's name for a resolved file: relative to `base` when it lies
/// under it (canonically, so `/tmp` and `/private/tmp` agree), else `~/…` under
/// `home`, else absolute; then `:line[:col]`.
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
        .or_else(|| home.and_then(under).map(|rel| format!("~/{rel}")))
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    with_line(text, line, col)
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

/// What a link click does ([`decide`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkAction {
    /// Hand an allowlisted URL to the OS (`open_url_external`).
    External(String),
    /// Open aterm's own editor tab on a resolved citation.
    Document(PathTarget),
    /// Nothing opens; the press goes on as an ordinary press.
    Nothing,
}

/// THE ONE PLACE A LINK CLICK BECOMES AN ACTION. The URL under the pointer (an
/// OSC 8 hyperlink or a plain-text URL) wins, and only an `is_safe_url` one
/// opens, through the OS. Only with no URL there is a resolved citation
/// opened, and only ever in aterm's own editor: no arm turns a [`PathTarget`]
/// into [`LinkAction::External`].
#[must_use]
pub(crate) fn decide(url: Option<String>, path: Option<PathTarget>) -> LinkAction {
    match (url, path) {
        (Some(url), _) if crate::is_safe_url(&url) => LinkAction::External(url),
        (Some(_), _) => LinkAction::Nothing,
        (None, Some(target)) => LinkAction::Document(target),
        (None, None) => LinkAction::Nothing,
    }
}

/// The 1-based `(line, col)` as a byte offset into `text`: the line's start
/// (the last line when `line` is past the end), then `col - 1` characters
/// into it, never past its end. Pure; the editor's caret lands here.
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
