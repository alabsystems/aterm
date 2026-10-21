// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The terminal face (design §5.4, §5.7; slice T3): `amacs FILE` over ssh,
//! in tmux or screen, in any terminal that is not an aterm window, and with
//! `-nw`, edits in the terminal itself, with the engine the native view runs
//! and its terminal keymap ([`Profile::Terminal`]: `C-s` is isearch).
//!
//! One loop: wait on the terminal (its bytes and the signal wake, which
//! [`DevTty`] polls together), decode, hand each key to the engine
//! ([`EditorWorkspace::handle_input`]), carry out what it asks
//! ([`EditorEffect`]), then draw the frame ([`crate::render`]) as the cells
//! that changed. A file is read and written only through amacs-files, with
//! the GUI's admission rules: the realpath, the size limit, UTF-8 only, a
//! regular file, its byte order mark and CRLF kept, a file that does not exist
//! yet admitted empty and created by its first save, and an atomic save that
//! refuses when the disk changed since the read. A second `C-x C-s` straight
//! after that refusal overwrites the disk's version; `M-x revert-buffer`
//! takes it instead.
//!
//! `C-x C-c` (and `C-x k`) closes the file, asking `Save FILE? (y, n, q)`
//! when it has unsaved changes; `C-c C-c` saves and closes, `C-c C-k` closes
//! without saving and ends the run with status 1, as the native face's done
//! and abort do (design §6.5). Several files are edited one after another,
//! sharing the kill ring and nothing else. `C-z` stops the process as a
//! shell's job control expects, and a resize, or the continue after a stop,
//! repaints at the terminal's size. A kill also goes to the terminal's
//! clipboard as an OSC 52 write (never a read), when that is on.
//!
//! The terminal is put back exactly however the run ends: on return, a
//! panic, SIGTERM or a hangup ([`DevTty`]'s guard, slice T2). There is no
//! draft journal yet (slice T5), so a TUI that is killed loses unsaved edits.
//!
//! The loop runs on any [`Tty`], so its tests drive it headless on an
//! in-memory terminal (`tests/tui_loop.rs`).
//!
//! The rules small enough to prove are scalar fns with Trust contracts
//! ([`saves_on`], [`discards_on`], [`cancels_on`], [`fits_osc52`]), each
//! `ensures` citing a Clean theorem about the compiled body (SPECS.md).
//!
//! [`DevTty`]: aterm_tty::tty::DevTty

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use amacs::editor::{
    CloseWhy, EditorBufferView, EditorEffect, EditorError, EditorLines, EditorWorkspace, Keymap,
    Profile, Selection, line_starts_capped,
};
use amacs::input::{EditorInput, Follow};
use amacs::language::{Language, LexCache};
use amacs::store::{
    DocumentError, DocumentId, DocumentStore, DocumentTxnOutcome, DocumentViewId, TextEdit,
};
use amacs_files::host::{
    self as files, AdmitOptions, DEFAULT_DOCUMENT_LIMIT, DocumentGrantStore, DocumentHostError,
    DocumentPersistenceStore, GrantAccess, PersistenceError,
};
use amacs_files::io::{SaveError, SaveReduction};
use amacs_files::save_words::{self, Failure, Speaker};
use aterm_buffer::{Seq, WriteCap};
use aterm_tty::decode::{Decoder, Event, MouseKind};
use aterm_tty::probe::{self, Caps};
use aterm_tty::screen::{self, Screen};
use aterm_tty::signals::Signals;
use aterm_tty::tty::{Mode, Readiness, Size, Tty};
use aterm_types::keyboard::{Key, Modifiers, NamedKey};
use aterm_types::mouse::{MouseButton, WheelDir};
use aterm_types::text_shaping::AmbiguousWidth;

use crate::client::{self, Target};
use crate::decide;
use crate::render;

/// The kitty keyboard flags the face pushes: disambiguate (1) and report
/// alternate keys (4), which keep an input method's composition safe (D-20).
pub const KITTY_FLAGS: u8 = 1 | 4;

/// The most text one OSC 52 write carries: aterm's own cap on what a
/// program may put on its clipboard (64 KiB of decoded text). A longer kill
/// stays in the kill ring only.
pub const OSC52_LIMIT: usize = 64 * 1024;
// `fits_osc52`'s clause and theorem spell the limit as the literal 64 KiB, and
// no landing check runs the Trust lane that would reject them once the
// constant moved: an ordinary build fails here instead.
const _: () = assert!(OSC52_LIMIT == 65_536);

/// How many lines one wheel notch scrolls.
const WHEEL_LINES: i32 = 3;

/// What the echo area says when a file opens.
const HINT: &str = "C-x C-s saves · C-x C-c exits · C-g cancels";

/// A development seam (`aterm_types::dev_seam!`, read in a debug or
/// `dev-seams` build only, never in a shipped one): the key that types this
/// character panics instead, so a test can watch the `amacs` binary put the
/// terminal back on a panic (`crates/aterm/tests/amacs_pty.rs`).
const INJECT_PANIC_SEAM: &str = "ATERM_AMACS_INJECT_PANIC";

/// The settings the face runs with. `[amacs]` (design §9) fills them once
/// the config parser lands (slice C1); until then they are its defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// `[amacs] osc52`: a kill also goes to the terminal's clipboard.
    pub osc52: bool,
    /// `[amacs] mouse`: a press sets point, a drag the region, the wheel
    /// scrolls.
    pub mouse: bool,
    /// The program the edit is for (`for=`), named on the modeline.
    pub for_word: Option<String>,
    /// aterm's `ambiguous_width`: how wide an East Asian ambiguous character
    /// is drawn.
    pub ambiguous: AmbiguousWidth,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            osc52: true,
            mouse: true,
            for_word: None,
            ambiguous: AmbiguousWidth::default(),
        }
    }
}

/// How a run of the face ended: its exit status, and the lines to say on
/// stderr once the terminal is put back (without the `amacs: ` lead).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub status: u8,
    pub said: Vec<String>,
}

/// How one file's edit ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Close {
    /// The file was closed (`C-x C-c`, `C-x k`), saved or not as the person
    /// answered: the run goes on to the next file.
    Closed,
    /// `C-c C-c`: saved, then closed.
    Finished,
    /// `C-c C-k`: closed without saving; the run ends with status 1.
    Aborted,
    /// A read found the terminal's end (EOF, or EIO with SIGHUP ignored, as
    /// under `nohup`). The kernel's SIGHUP itself never gets here: under
    /// [`DevTty`] it ends the process as SIGHUP does, the terminal put back
    /// first, with nothing said.
    ///
    /// [`DevTty`]: aterm_tty::tty::DevTty
    Hangup,
}

impl Close {
    /// The `decide::WAIT_*` this ending is, so a run's status is the native
    /// face's for the same ending ([`decide::wait_status`]).
    const fn wait_end(self) -> u8 {
        match self {
            Self::Closed => decide::WAIT_CLOSED,
            Self::Finished => decide::WAIT_FINISHED,
            Self::Aborted => decide::WAIT_ABORTED,
            Self::Hangup => decide::WAIT_FAILED,
        }
    }
}

// The answers to `Save FILE? (y, n, q)`, read as Emacs's `save-some-buffers`
// reads them, from the byte a key types (`answer_byte`). Trust's kernel
// import takes scalars only, so each class is its own bool fn.

trust_contract_fn! {
    /// The keys that answer "save it": `y`, `Y`, SPC and `!`. Proved for
    /// every byte.
    #[must_use]
    pub fn saves_on(byte: u8) -> bool
    clauses [ensures result == (byte == 32 || (byte == 33 || (byte == 89 || byte == 121))) by saves_on_is_the_class]
    {
        matches!(byte, b' ' | b'!' | b'Y' | b'y')
    }
}

trust_contract_fn! {
    /// The keys that answer "close it without saving": `n`, `N` and DEL.
    /// Proved for every byte.
    #[must_use]
    pub fn discards_on(byte: u8) -> bool
    clauses [ensures result == (byte == 78 || (byte == 110 || byte == 127)) by discards_on_is_the_class]
    {
        matches!(byte, b'N' | b'n' | 0x7f)
    }
}

trust_contract_fn! {
    /// The keys that take the question back and go on editing: `q`, `Q`,
    /// ESC and `C-g`. Proved for every byte.
    #[must_use]
    pub fn cancels_on(byte: u8) -> bool
    clauses [ensures result == (byte == 7 || (byte == 27 || (byte == 81 || byte == 113))) by cancels_on_is_the_class]
    {
        matches!(byte, 0x07 | 0x1b | b'Q' | b'q')
    }
}

trust_contract_fn! {
    /// Whether a kill of `len` bytes goes to the terminal's clipboard: at
    /// most [`OSC52_LIMIT`], the most aterm takes from one OSC 52 write.
    /// Proved for every length.
    #[must_use]
    pub fn fits_osc52(len: usize) -> bool
    clauses [ensures result == (len <= 65536) by fits_osc52_is_the_limit]
    {
        len <= OSC52_LIMIT
    }
}

// Specs `save-answer-classes` and `osc52-limit` (SPECS.md): each class is
// exactly its bytes, and the limit is aterm's 64 KiB, kernel-checked against
// the compiled bodies. The three classes are disjoint by their statements.
verify_island! {
    clean {
        theorem saves_on_is_the_class (b : UInt8) :
            trust_import_amacs_term__tui__saves_on b =
              (decide (b = 32) || (decide (b = 33) || (decide (b = 89) || decide (b = 121)))) := by
          simp only [trust_import_amacs_term__tui__saves_on]

        theorem discards_on_is_the_class (b : UInt8) :
            trust_import_amacs_term__tui__discards_on b =
              (decide (b = 78) || (decide (b = 110) || decide (b = 127))) := by
          simp only [trust_import_amacs_term__tui__discards_on]

        theorem cancels_on_is_the_class (b : UInt8) :
            trust_import_amacs_term__tui__cancels_on b =
              (decide (b = 7) || (decide (b = 27) || (decide (b = 81) || decide (b = 113)))) := by
          simp only [trust_import_amacs_term__tui__cancels_on]

        theorem fits_osc52_is_the_limit (n : UInt64) :
            trust_import_amacs_term__tui__fits_osc52 n = decide (n <= 65536) := by
          simp only [trust_import_amacs_term__tui__fits_osc52]
    }
}

/// The byte a key answers a question with: what it types, or DEL for
/// Backspace, ESC for Escape and BEL for `C-g`; `None` for a key that types
/// no ASCII.
fn answer_byte(key: &Key, mods: Modifiers, text: Option<char>) -> Option<u8> {
    let chord = mods.difference(Modifiers::SHIFT);
    match key {
        Key::Named(NamedKey::Backspace) if chord.is_empty() => Some(0x7f),
        Key::Named(NamedKey::Escape) if chord.is_empty() => Some(0x1b),
        Key::Character('g') if chord == Modifiers::CTRL => Some(0x07),
        _ if chord.is_empty() => text
            .filter(char::is_ascii)
            .and_then(|c| u8::try_from(c).ok()),
        _ => None,
    }
}

/// The editor in a terminal: the engine's workspace (one kill ring for
/// every file of the run), the terminal's capabilities, the decoder and what
/// the terminal shows.
pub struct Tui {
    workspace: EditorWorkspace,
    write: WriteCap,
    options: Options,
    caps: Caps,
    decoder: Decoder,
    /// Events read and not yet handled: the probe's typeahead, and the rest
    /// of a read whose earlier event closed a file.
    queued: VecDeque<Event>,
    size: Size,
    /// The frame the terminal shows, once the screen is entered.
    shown: Option<Screen>,
    /// The buffer rows of that frame, for a press to find its byte, and how
    /// many rows it gave the buffer.
    rows: Vec<render::DrawnRow>,
    body: u16,
    /// The next frame is drawn whole: the first, and after a resize or a
    /// suspension.
    repaint: bool,
    /// The screen is entered: the probe has run and the modes are set.
    started: bool,
    /// The echo area's message, until the next key.
    status: Option<String>,
    /// The face's own question (`Save FILE? (y, n, q)`), which takes the next
    /// key.
    asking: Option<String>,
    /// The view id the next file gets: one per file, so nothing the
    /// workspace remembers of a view carries to the next.
    next_view: u64,
    /// [`INJECT_PANIC_SEAM`]'s character, in a build that reads seams.
    inject_panic: Option<char>,
}

/// One file open in the face.
pub struct Buffer {
    store: DocumentStore,
    grants: DocumentGrantStore,
    persistence: DocumentPersistenceStore,
    document: DocumentId,
    view: EditorBufferView,
    path: PathBuf,
    name: String,
    language: Language,
    /// The syntax of `lexed`, the text the frame last drew.
    lex: LexCache,
    lexed: Arc<str>,
    /// The line index of `lexed` (`None` for a document too line-dense to
    /// index, which is scanned instead).
    starts: Option<Vec<usize>>,
    synced: Seq,
    /// The file did not exist when it was read.
    missing: bool,
    /// The last save was refused because the disk changed: a save straight
    /// after it overwrites the disk's version.
    overwrite: bool,
}

impl Buffer {
    /// The document's text now, with the line index and the syntax brought
    /// up to it.
    fn text(&mut self) -> Arc<str> {
        let Some(snapshot) = self.store.snapshot(self.document) else {
            return Arc::clone(&self.lexed);
        };
        if snapshot.seq != self.synced {
            self.starts = line_starts_capped(&snapshot.text);
            relex(&mut self.lex, &self.lexed, &snapshot.text);
            self.lexed = Arc::clone(&snapshot.text);
            self.synced = snapshot.seq;
        }
        snapshot.text
    }

    fn dirty(&self) -> bool {
        self.store.dirty(self.document).unwrap_or(false)
    }

    /// The file's path as a person reads it.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The lines of `text`, through `starts` when it indexes them.
fn lines_of<'a>(text: &'a str, starts: Option<&'a [usize]>) -> EditorLines<'a> {
    match starts {
        Some(starts) => EditorLines::indexed(text, starts),
        None => EditorLines::scanning(text),
    }
}

/// Bring `cache`, the syntax of `old`, up to `new`: the lines between the
/// texts' common start and common end are lexed again, and the lines after
/// them only as far as their starting state changed ([`LexCache::splice`]).
fn relex(cache: &mut LexCache, old: &str, new: &str) {
    let (old, new_bytes) = (old.as_bytes(), new.as_bytes());
    let prefix = old
        .iter()
        .zip(new_bytes)
        .take_while(|(a, b)| a == b)
        .count();
    let room = old.len().min(new_bytes.len()).saturating_sub(prefix);
    let suffix = old
        .iter()
        .rev()
        .zip(new_bytes.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    let breaks = |bytes: Option<&[u8]>| {
        bytes
            .unwrap_or_default()
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
    };
    let first = breaks(old.get(..prefix));
    let removed = breaks(old.get(prefix..old.len().saturating_sub(suffix))).saturating_add(1);
    let inserted =
        breaks(new_bytes.get(prefix..new_bytes.len().saturating_sub(suffix))).saturating_add(1);
    cache.splice(new, first, removed, inserted);
}

/// The byte `line` (1-based) and `column` (1-based, in characters) name in
/// `text`, clamped to the line and to the text, as aterm's path links place
/// a caret.
fn line_column_offset(text: &str, line: u32, column: Option<u32>) -> usize {
    let lines = EditorLines::scanning(text);
    let index = usize::try_from(line)
        .unwrap_or(usize::MAX)
        .saturating_sub(1)
        .min(lines.total().saturating_sub(1));
    let start = lines.byte_of(index).min(text.len());
    let rest = text.get(start..).unwrap_or_default();
    let line_text = rest
        .get(..rest.find('\n').unwrap_or(rest.len()))
        .unwrap_or_default();
    let chars = column.map_or(0, |column| {
        usize::try_from(column)
            .unwrap_or(usize::MAX)
            .saturating_sub(1)
    });
    let within = line_text
        .char_indices()
        .nth(chars)
        .map_or(line_text.len(), |(at, _)| at);
    start.saturating_add(within)
}

/// Why the file host refused to read `path` to open it (or to load it from
/// disk again), in the save words (`amacs_files::save_words`) aterm says
/// too: never the host's stage or the system's error text.
pub(crate) fn host_error(path: &Path, error: &DocumentHostError) -> String {
    save_words::words(Speaker::Amacs, path, &Failure::Open(error))
}

/// Why the buffer's store refused a step on the buffer, in a person's words.
fn store_words(error: &DocumentError) -> &'static str {
    match error {
        DocumentError::UnknownDocument | DocumentError::UnknownView => "the buffer is gone",
        DocumentError::Closing | DocumentError::CloseNotReady => "the buffer is closing",
        DocumentError::InvalidRange | DocumentError::OverlappingEdits => {
            "the buffer refused the edit"
        }
        DocumentError::CheckpointAheadOfHead => "the buffer changed while it was saved",
    }
}

/// What an engine error says in the echo area.
fn error_text(error: &EditorError) -> &'static str {
    match error {
        EditorError::NothingToUndo => "No further undo information",
        EditorError::NothingToRedo => "No further redo information",
        EditorError::NoKill => "Kill ring is empty",
        EditorError::MacroRecursion => "A keyboard macro cannot run itself",
        EditorError::HistoryDiverged { .. } => "The undo history no longer matches the buffer",
        EditorError::StaleView { .. } | EditorError::TransactionConflict { .. } => {
            "The buffer changed under that command; try it again"
        }
        EditorError::TransactionRejected => "That edit was refused",
        EditorError::InvalidSelections => "The selection is not in the buffer",
        EditorError::UnknownDocument => "The buffer is gone",
    }
}

/// `text` with every line break LF: a terminal pastes CR (or CRLF) breaks.
fn paste_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The OSC 52 write that puts `text` on the terminal's clipboard.
fn osc52(text: &str) -> Option<Vec<u8>> {
    let encoded = aterm_codec::base64::encode(text.as_bytes()).ok()?;
    Some(format!("\x1b]52;c;{encoded}\x07").into_bytes())
}

impl Tui {
    /// A face that edits with `write`, the run's one buffer-write capability
    /// (minted by the launcher, `aterm_cli::amacs_launch`).
    #[must_use]
    pub fn new(write: WriteCap, options: Options) -> Self {
        Self {
            workspace: EditorWorkspace::with_keymap(Keymap::emacs(Profile::Terminal)),
            write,
            options,
            caps: Caps::default(),
            decoder: Decoder::new(),
            queued: VecDeque::new(),
            size: Size { cols: 80, rows: 24 },
            shown: None,
            rows: Vec::new(),
            body: 0,
            repaint: true,
            started: false,
            status: None,
            asking: None,
            next_view: 1,
            inject_panic: None,
        }
    }

    /// Edit each of `targets` in turn on `tty`, and put the terminal back.
    /// A file that cannot be opened ends the run with status 1, as an abort
    /// does; the rest are not opened.
    pub fn edit<T: Tty + ?Sized>(&mut self, tty: &mut T, targets: &[Target]) -> Outcome {
        let mut said = Vec::new();
        let mut status = 0;
        for target in targets {
            let mut buffer = match self.open(target) {
                Ok(buffer) => buffer,
                Err(why) => {
                    said.push(why);
                    status = 1;
                    break;
                }
            };
            let ended = self.run(tty, &mut buffer);
            match ended {
                Ok(close) => {
                    if close == Close::Hangup && buffer.dirty() {
                        said.push(format!(
                            "{}: the terminal hung up; the unsaved changes were lost",
                            buffer.path.display()
                        ));
                    }
                    status = decide::wait_status(close.wait_end());
                }
                Err(error) => {
                    said.push(format!("the terminal failed: {error}"));
                    status = 1;
                }
            }
            if status != 0 {
                break;
            }
        }
        if let Err(error) = self.finish(tty) {
            said.push(format!("the terminal failed: {error}"));
            status = 1;
        }
        Outcome { status, said }
    }

    /// Open `target` as amacs-files admits a document: its realpath, the
    /// size limit, UTF-8, a regular file, a missing one as empty.
    ///
    /// # Errors
    ///
    /// Why the file cannot be edited, worded for a person.
    pub fn open(&mut self, target: &Target) -> Result<Buffer, String> {
        let named = |why: String| format!("{}: {why}", target.path.display());
        let refused = |e: DocumentHostError| named(host_error(&target.path, &e));
        let (path, uri) = files::open_path(&target.path).map_err(refused)?;
        let admitted = files::admit_local_file_opts(
            &uri,
            AdmitOptions {
                limit: DEFAULT_DOCUMENT_LIMIT,
                allow_config_symlinks: false,
                allow_missing: true,
            },
        )
        .map_err(refused)?;
        let missing = admitted.is_missing();
        let mut grants = DocumentGrantStore::new();
        let granted = grants
            .grant_admitted(admitted, GrantAccess::ReadWrite)
            .map_err(refused)?;
        let mut store = DocumentStore::new(self.write);
        let document = store.open(uri, granted.text.clone());
        let mut persistence = DocumentPersistenceStore::default();
        persistence
            .register(document, granted.grant.id, granted.observed, &granted.text)
            .map_err(|_| named("it is already open in this run".to_string()))?;
        // Each file has a store of its own, and a store numbers its documents
        // from one, so this file's id is the one every earlier file of the run
        // had. What the workspace keeps by document (the undo history, the
        // global marks, the yank and commit records) is that earlier file's:
        // dropped here, or an undo here would replay its edits on this text.
        // The kill ring is the run's, and stays.
        self.workspace.forget(document);
        let view_id = DocumentViewId(self.next_view);
        self.next_view = self.next_view.saturating_add(1);
        let mut view = self
            .workspace
            .attach(&mut store, document, view_id)
            .map_err(|e| named(error_text(&e).to_string()))?;
        let text: Arc<str> = Arc::from(granted.text.as_str());
        if let Some((line, column)) = target.line {
            view.selections = vec![Selection::caret(line_column_offset(&text, line, column))];
            view.primary = 0;
        }
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let first_line = text.split('\n').next().unwrap_or_default();
        let language = Language::detect(&path.to_string_lossy(), first_line);
        let synced = store
            .snapshot(document)
            .map_or(Seq(0), |snapshot| snapshot.seq);
        // M-q fills to the language's column: 72 in a commit message.
        self.workspace.options.fill_column = language.fill_column();
        self.asking = None;
        self.status = Some(if missing {
            format!("(New file) · {HINT}")
        } else {
            HINT.to_string()
        });
        Ok(Buffer {
            store,
            grants,
            persistence,
            document,
            view,
            starts: line_starts_capped(&text),
            lex: LexCache::new(language, &text),
            lexed: text,
            synced,
            path,
            name,
            language,
            missing,
            overwrite: false,
        })
    }

    /// Edit `buffer` until it is closed or the terminal hangs up, entering
    /// the screen first if this is the run's first file. A run that ended in
    /// a hangup may be run again on a terminal that has more to read: the
    /// file is where it was, an open question still asked.
    ///
    /// # Errors
    ///
    /// The terminal's OS error.
    pub fn run<T: Tty + ?Sized>(&mut self, tty: &mut T, buffer: &mut Buffer) -> io::Result<Close> {
        if !self.started {
            let signals = self.start(tty)?;
            self.signals(tty, buffer, signals)?;
        }
        self.fit(buffer, true);
        let mut buf = [0u8; 4096];
        loop {
            while let Some(event) = self.queued.pop_front() {
                if let Some(close) = self.event(tty, buffer, event)? {
                    return Ok(close);
                }
            }
            self.draw(tty, buffer)?;
            match tty.read(&mut buf, self.decoder.deadline())? {
                Readiness::Bytes(n) => {
                    let read = buf.get(..n).unwrap_or(&buf);
                    let events = self.decoder.feed(read, tty.now());
                    self.queued.extend(events);
                }
                Readiness::Timeout => {
                    let events = self.decoder.flush(tty.now());
                    self.queued.extend(events);
                }
                Readiness::Signal => {
                    let signals = tty.take_signals()?;
                    self.signals(tty, buffer, signals)?;
                }
                Readiness::Closed => return Ok(Close::Hangup),
            }
        }
    }

    /// Enter the screen: ask the terminal what it can do, then set the modes
    /// the face uses, each put back however the run ends. Returns the
    /// signals that arrived during the probe, for the loop to handle.
    fn start<T: Tty + ?Sized>(&mut self, tty: &mut T) -> io::Result<Signals> {
        self.size = tty.size()?;
        let probed = probe::probe(tty, &mut self.decoder)?;
        self.caps = probed.caps;
        self.started = true;
        tty.enable(Mode::AltScreen)?;
        if self.caps.kitty {
            tty.enable(Mode::KittyKeys(KITTY_FLAGS))?;
        }
        if self.options.mouse {
            tty.enable(Mode::Mouse)?;
        }
        // A terminal that does not know 2004 ignores the request; one that
        // does but answered no DECRQM still brackets its pastes, so each
        // inserts as one transaction instead of being typed.
        tty.enable(Mode::Paste)?;
        if self.caps.focus {
            tty.enable(Mode::Focus)?;
        }
        if self.caps.resize_2048 {
            tty.enable(Mode::InBandResize)?;
        }
        tty.enable(Mode::Cursor)?;
        self.repaint = true;
        self.queued.extend(probed.events);
        Ok(probed.signals)
    }

    /// Put back every mode the screen was entered with, in the one order
    /// every restore uses (the drawing state first, the alternate screen
    /// last). [`Self::edit`] ends with it.
    ///
    /// # Errors
    ///
    /// The terminal's OS error.
    pub fn finish<T: Tty + ?Sized>(&mut self, tty: &mut T) -> io::Result<()> {
        if !std::mem::take(&mut self.started) {
            return Ok(());
        }
        self.shown = None;
        for mode in [
            Mode::Cursor,
            Mode::InBandResize,
            Mode::Focus,
            Mode::Paste,
            Mode::Mouse,
            Mode::KittyKeys(KITTY_FLAGS),
            Mode::AltScreen,
        ] {
            tty.disable(mode)?;
        }
        Ok(())
    }

    /// What the echo area shows now, most pressing first: the face's
    /// question, a key sequence being typed, the minibuffer, a message.
    fn echo(&self, buffer: &Buffer) -> render::EchoLine {
        let echo = if let Some(question) = &self.asking {
            render::Echo::Question(question)
        } else if let Some(hud) = &buffer.view.prefix_hud {
            render::Echo::Hud(hud)
        } else if buffer.view.minibuffer_active() {
            render::Echo::Minibuffer {
                minibuffer: &buffer.view.minibuffer,
                status: self.status.as_deref(),
            }
        } else if let Some(status) = &self.status {
            render::Echo::Message(status)
        } else {
            render::Echo::Empty
        };
        render::EchoLine::of(&echo)
    }

    /// The next frame's layout, and its echo area.
    fn layout(&self, buffer: &Buffer) -> (render::Layout, render::EchoLine) {
        let echo = self.echo(buffer);
        let rows = render::echo_rows(self.size, self.options.ambiguous, &echo);
        (render::layout(self.size.rows, rows), echo)
    }

    /// Fit `buffer`'s view to the rows the next frame gives it: a new count
    /// reveals the caret; `reveal` reveals it anyway.
    fn fit(&mut self, buffer: &mut Buffer, reveal: bool) {
        let body = usize::from(self.layout(buffer).0.body);
        let text = buffer.text();
        let lines = lines_of(&text, buffer.starts.as_deref());
        if reveal {
            buffer.view.ensure_primary_visible_with(&lines, body);
        } else {
            buffer.view.reconcile_viewport_with(&lines, body);
        }
    }

    fn resize(&mut self, buffer: &mut Buffer, size: Size) {
        self.size = size;
        self.repaint = true;
        self.fit(buffer, false);
    }

    /// Read the terminal's size again and draw the next frame whole at it.
    fn refit<T: Tty + ?Sized>(&mut self, tty: &mut T, buffer: &mut Buffer) -> io::Result<()> {
        let size = tty.size()?;
        self.resize(buffer, size);
        Ok(())
    }

    fn signals<T: Tty + ?Sized>(
        &mut self,
        tty: &mut T,
        buffer: &mut Buffer,
        signals: Signals,
    ) -> io::Result<()> {
        if signals.contains(Signals::TSTP) {
            // A stop asked from outside (`kill -TSTP`): stop as C-z does.
            return self.suspend(tty, buffer);
        }
        // A continue is also the end of a stop this process did not make
        // (SIGSTOP, then `fg`), during which the window may have changed size:
        // read like a resize.
        if signals.contains(Signals::WINCH) || signals.contains(Signals::CONT) {
            self.refit(tty, buffer)?;
        }
        Ok(())
    }

    /// `suspend-frame` (`C-z`, or a SIGTSTP from outside): the shell gets the
    /// terminal back until `fg`, then the whole frame is drawn again, once,
    /// at the size the terminal has then. A resize while stopped sent its
    /// SIGWINCH to the shell, which had the terminal, so nothing else says
    /// the size changed.
    fn suspend<T: Tty + ?Sized>(&mut self, tty: &mut T, buffer: &mut Buffer) -> io::Result<()> {
        loop {
            tty.suspend()?;
            // The SIGCONT that continued the process is pending: taken with
            // the stop, so it draws no second frame. A stop that arrived
            // meanwhile stops again.
            if !tty.take_signals()?.contains(Signals::TSTP) {
                break;
            }
        }
        self.refit(tty, buffer)
    }

    /// Handle one decoded event; `Some` when it closed the file.
    fn event<T: Tty + ?Sized>(
        &mut self,
        tty: &mut T,
        buffer: &mut Buffer,
        event: Event,
    ) -> io::Result<Option<Close>> {
        match event {
            Event::Key { key, mods, text } => {
                if text.is_some() && text == self.inject_panic {
                    panic!("{INJECT_PANIC_SEAM}: a panic asked for by a test");
                }
                if self.asking.is_some() {
                    return self.answer(tty, buffer, answer_byte(&key, mods, text));
                }
                if key == Key::Character('z')
                    && mods == Modifiers::CTRL
                    && !buffer.view.chord_pending()
                {
                    self.suspend(tty, buffer)?;
                    return Ok(None);
                }
                self.input(
                    tty,
                    buffer,
                    EditorInput::Key {
                        key,
                        mods,
                        text: text.map(String::from),
                    },
                )
            }
            Event::Paste(text) if self.asking.is_none() => {
                self.input(tty, buffer, EditorInput::Paste(paste_text(&text)))
            }
            Event::Mouse {
                kind,
                button,
                col,
                row,
                mods,
            } if self.asking.is_none() => self.mouse(tty, buffer, kind, button, (col, row), mods),
            Event::Focus(focused) => self.input(tty, buffer, EditorInput::Focus(focused)),
            Event::Resize { cols, rows } => {
                self.resize(buffer, Size { cols, rows });
                Ok(None)
            }
            Event::Paste(_) | Event::Mouse { .. } | Event::Reply(_) | Event::Unknown(_) => Ok(None),
        }
    }

    fn mouse<T: Tty + ?Sized>(
        &mut self,
        tty: &mut T,
        buffer: &mut Buffer,
        kind: MouseKind,
        button: Option<MouseButton>,
        (col, row): (u16, u16),
        mods: Modifiers,
    ) -> io::Result<Option<Close>> {
        let body = usize::from(self.body);
        match kind {
            MouseKind::Wheel(direction) => {
                let delta = match direction {
                    WheelDir::Up => -WHEEL_LINES,
                    WheelDir::Down => WHEEL_LINES,
                    WheelDir::Left | WheelDir::Right => return Ok(None),
                };
                let text = buffer.text();
                let lines = lines_of(&text, buffer.starts.as_deref());
                buffer.view.scroll_lines_with(&lines, delta);
                Ok(None)
            }
            MouseKind::Press | MouseKind::Motion
                if button == Some(MouseButton::Left) && usize::from(row) < body =>
            {
                let byte = match self.rows.get(usize::from(row)) {
                    Some(drawn) => drawn.byte_at(col),
                    // Below the last line: its end.
                    None => self
                        .rows
                        .last()
                        .map_or_else(|| buffer.text().len(), |drawn| drawn.byte_at(u16::MAX)),
                };
                let extend = kind == MouseKind::Motion || mods.contains(Modifiers::SHIFT);
                self.input(
                    tty,
                    buffer,
                    EditorInput::Pointer {
                        byte,
                        extend,
                        visible_lines: body,
                    },
                )
            }
            MouseKind::Press | MouseKind::Motion | MouseKind::Release => Ok(None),
        }
    }

    /// Hand one input to the engine and carry out what it asks.
    fn input<T: Tty + ?Sized>(
        &mut self,
        tty: &mut T,
        buffer: &mut Buffer,
        input: EditorInput,
    ) -> io::Result<Option<Close>> {
        // Emacs clears the echo area at the next key; a focus change is no
        // key, and leaves an offer to overwrite standing.
        let pressed = !matches!(input, EditorInput::Focus(_));
        if pressed {
            self.status = None;
        }
        let reduction =
            match self
                .workspace
                .handle_input(&mut buffer.store, &mut buffer.view, input)
            {
                Ok(reduction) => reduction,
                Err(error) => {
                    self.status = Some(error_text(&error).to_string());
                    return Ok(None);
                }
            };
        let saving = reduction
            .effects
            .iter()
            .any(|effect| matches!(effect, EditorEffect::SaveDocument { .. }));
        // "C-x C-s again" means the next command: anything else between
        // takes the offer back.
        if pressed && !saving && !buffer.view.chord_pending() {
            buffer.overwrite = false;
        }
        for effect in reduction.effects {
            if let Some(close) = self.effect(tty, buffer, effect)? {
                return Ok(Some(close));
            }
        }
        if reduction.follow == Follow::Reveal {
            self.fit(buffer, true);
        }
        Ok(None)
    }

    fn effect<T: Tty + ?Sized>(
        &mut self,
        tty: &mut T,
        buffer: &mut Buffer,
        effect: EditorEffect,
    ) -> io::Result<Option<Close>> {
        match effect {
            EditorEffect::SaveDocument { .. } => {
                self.save(buffer);
            }
            EditorEffect::RevertDocument { .. } => self.revert(buffer),
            EditorEffect::CloseView { why } => return Ok(self.close(buffer, why)),
            EditorEffect::KillBuffer { query } => {
                let query = query.trim();
                if query.is_empty() || query == buffer.name {
                    return Ok(self.close(buffer, CloseWhy::Plain));
                }
                self.status = Some(format!("No buffer named {query}"));
            }
            EditorEffect::SwitchBuffer { query } => {
                let query = query.trim();
                if !query.is_empty() && query != buffer.name {
                    self.status = Some(format!(
                        "No buffer named {query}: this terminal edits {} alone",
                        buffer.name
                    ));
                }
            }
            EditorEffect::ShowCommands => {
                self.status = Some("Type a command name".to_string());
            }
            EditorEffect::KillToClipboard { text } => {
                if self.options.osc52
                    && fits_osc52(text.len())
                    && let Some(bytes) = osc52(&text)
                {
                    tty.write_all(&bytes)?;
                }
            }
            EditorEffect::Status(status) => self.status = Some(status),
            EditorEffect::Bell => tty.write_all(b"\x07")?,
        }
        Ok(None)
    }

    /// Close the file the way `why` asks: a plain close of a changed file
    /// asks first, a finish saves first.
    fn close(&mut self, buffer: &mut Buffer, why: CloseWhy) -> Option<Close> {
        match why {
            CloseWhy::Plain if buffer.dirty() => {
                self.asking = Some(format!("Save {}? (y, n, q) ", buffer.path.display()));
                None
            }
            CloseWhy::Plain => Some(Close::Closed),
            CloseWhy::Finish => (!buffer.dirty() || self.save(buffer)).then_some(Close::Finished),
            CloseWhy::Abort => Some(Close::Aborted),
        }
    }

    /// Read the answer to `Save FILE? (y, n, q)`.
    fn answer<T: Tty + ?Sized>(
        &mut self,
        tty: &mut T,
        buffer: &mut Buffer,
        byte: Option<u8>,
    ) -> io::Result<Option<Close>> {
        let Some(byte) = byte else {
            tty.write_all(b"\x07")?;
            self.status = Some("Please answer y, n or q".to_string());
            return Ok(None);
        };
        if saves_on(byte) {
            self.asking = None;
            return Ok(self.save(buffer).then_some(Close::Closed));
        }
        if discards_on(byte) {
            self.asking = None;
            return Ok(Some(Close::Closed));
        }
        if cancels_on(byte) {
            self.asking = None;
            self.status = Some("Quit".to_string());
            return Ok(None);
        }
        tty.write_all(b"\x07")?;
        self.status = Some("Please answer y, n or q".to_string());
        Ok(None)
    }

    /// Save `buffer` to its file, atomically, refusing when the disk changed
    /// since it was read (unless this save takes up the offer to overwrite).
    /// Answers whether the file now holds the buffer. A save that does not
    /// land says why in the save words aterm says too
    /// ([`amacs_files::save_words`]), never a step's name, a reducer's state
    /// or the system's error text.
    fn save(&mut self, buffer: &mut Buffer) -> bool {
        let name = buffer.name.clone();
        let path = buffer.path.clone();
        let failed = |failure: &Failure<'_>| {
            format!(
                "Could not save {name}: {}",
                save_words::words(Speaker::Amacs, &path, failure)
            )
        };
        let overwrite = std::mem::take(&mut buffer.overwrite);
        if !buffer.dirty() && !overwrite {
            self.status = Some("(No changes need to be saved)".to_string());
            return true;
        }
        if overwrite && let Err(why) = take_disk_version(buffer) {
            self.status = Some(format!("Could not save {name}: {why}"));
            return false;
        }
        // A save whose preflight found the same bytes under a new identity
        // rebinds and is offered again at once: one retry.
        for _ in 0..2 {
            let Some(snapshot) = buffer.store.snapshot(buffer.document) else {
                self.status = Some(format!("Could not save {name}: the buffer is gone"));
                return false;
            };
            let pending = match buffer.persistence.begin(&snapshot) {
                Ok(pending) => pending,
                Err(PersistenceError::Save(SaveError::ConflictRequiresReconcile)) => {
                    self.offer_overwrite(buffer, None);
                    return false;
                }
                Err(error @ PersistenceError::Save(SaveError::ReconcileRequired)) => {
                    let why = save_words::words(Speaker::Amacs, &path, &Failure::Begin(&error));
                    self.offer_overwrite(buffer, Some(&why));
                    return false;
                }
                Err(error) => {
                    self.status = Some(failed(&Failure::Begin(&error)));
                    return false;
                }
            };
            let Some(grant) = buffer.grants.get(pending.grant) else {
                self.status = Some(failed(&Failure::Begin(&PersistenceError::UnknownDocument)));
                return false;
            };
            let result = files::execute_granted_save(grant, &pending.plan);
            let reduction =
                buffer
                    .persistence
                    .complete(buffer.document, pending.plan.generation, result);
            match reduction {
                Ok(SaveReduction::Durable(checkpoint)) => {
                    if let Err(error) = buffer.store.checkpoint_ack(buffer.document, checkpoint.seq)
                    {
                        self.status = Some(format!(
                            "Wrote {}, but could not mark it saved: {}",
                            path.display(),
                            store_words(&error)
                        ));
                        return false;
                    }
                    buffer.missing = false;
                    self.status = Some(format!("Wrote {}", path.display()));
                    return true;
                }
                Ok(SaveReduction::ReboundEquivalent(_)) => {}
                Ok(SaveReduction::Conflict(_)) => {
                    self.offer_overwrite(buffer, None);
                    return false;
                }
                Ok(SaveReduction::ReconcileRequired { stage, cause, .. }) => {
                    let why = save_words::words(
                        Speaker::Amacs,
                        &path,
                        &Failure::Unconfirmed { stage, cause },
                    );
                    self.offer_overwrite(buffer, Some(&why));
                    return false;
                }
                Ok(SaveReduction::Failed { stage, cause, .. }) => {
                    self.status = Some(failed(&Failure::Write { stage, cause }));
                    return false;
                }
                Ok(SaveReduction::Cancelled | SaveReduction::Stale) => {
                    self.status = Some(format!("The save of {name} did not finish"));
                    return false;
                }
                Err(error) => {
                    self.status = Some(failed(&Failure::Begin(&error)));
                    return false;
                }
            }
        }
        self.status = Some(format!(
            "{name} keeps changing on disk; nothing was written"
        ));
        false
    }

    /// The disk changed since the read, or a save was written but its proof
    /// failed (`unconfirmed`, why): say so, and let a save straight after this
    /// one overwrite the disk's version.
    fn offer_overwrite(&mut self, buffer: &mut Buffer, unconfirmed: Option<&str>) {
        buffer.overwrite = true;
        let name = &buffer.name;
        self.status = Some(match unconfirmed {
            None => format!(
                "{name} changed on disk; C-x C-s again to overwrite, M-x revert-buffer to reload"
            ),
            Some(why) => format!(
                "Could not confirm {name} was saved ({why}); C-x C-s again to write it, \
                 M-x revert-buffer to reload"
            ),
        });
    }

    /// `revert-buffer`: the buffer becomes the file as it is on disk now.
    fn revert(&mut self, buffer: &mut Buffer) {
        match reload(buffer) {
            Ok(()) => {
                buffer.overwrite = false;
                self.status = Some(format!("Reverted {}", buffer.path.display()));
            }
            Err(why) => self.status = Some(format!("Could not revert {}: {why}", buffer.name)),
        }
        self.fit(buffer, true);
    }

    /// Draw the frame: the cells that changed, or the whole screen when it
    /// must be.
    fn draw<T: Tty + ?Sized>(&mut self, tty: &mut T, buffer: &mut Buffer) -> io::Result<()> {
        let (layout, echo) = self.layout(buffer);
        let text = buffer.text();
        let lines = lines_of(&text, buffer.starts.as_deref());
        // An echo area that grew or shrank changes the rows the buffer has:
        // a new count reveals the caret, as a resize does.
        buffer
            .view
            .reconcile_viewport_with(&lines, usize::from(layout.body));
        let (line, column) = render::caret_line_column(&lines, &buffer.view);
        let parts = render::FrameParts {
            lines: &lines,
            view: &buffer.view,
            syntax: Some(render::Syntax {
                cache: &buffer.lex,
                lines: &lines,
            }),
            modeline: render::Modeline {
                name: &buffer.name,
                dirty: buffer.store.dirty(buffer.document).unwrap_or(false),
                language: buffer.language,
                line,
                column,
                for_word: self.options.for_word.as_deref(),
            },
            echo: &echo,
        };
        let (next, rows) = render::frame(self.size, self.options.ambiguous, &layout, &parts);
        let bytes = match &self.shown {
            Some(shown) if !self.repaint => screen::diff(shown, &next, &self.caps),
            _ => screen::repaint(&next, &self.caps),
        };
        if !bytes.is_empty() {
            tty.write_all(&bytes)?;
        }
        self.shown = Some(next);
        self.rows = rows;
        self.body = layout.body;
        self.repaint = false;
        Ok(())
    }
}

/// Read `buffer`'s file again through its grant: a file that is gone reads
/// as missing and empty.
fn read_disk(buffer: &Buffer, saving: bool) -> Result<files::GrantedDocument, String> {
    let words = |failure: &Failure<'_>| save_words::words(Speaker::Amacs, &buffer.path, failure);
    let grant = buffer
        .persistence
        .grant(buffer.document)
        .ok_or_else(|| words(&Failure::Begin(&PersistenceError::UnknownDocument)))?;
    buffer
        .grants
        .refresh_local_allowing_missing(grant, DEFAULT_DOCUMENT_LIMIT)
        .map_err(|error| {
            words(&if saving {
                Failure::Read(&error)
            } else {
                Failure::Open(&error)
            })
        })
}

/// Take the disk's current version as the one a save replaces, keeping the
/// buffer's text and the way it maps to bytes: the overwrite a person asked
/// for with a second `C-x C-s`.
fn take_disk_version(buffer: &mut Buffer) -> Result<(), String> {
    let fresh = read_disk(buffer, true)?;
    buffer
        .persistence
        .accept_overwrite(buffer.document, fresh.observed)
        .map_err(|error| save_words::words(Speaker::Amacs, &buffer.path, &Failure::Begin(&error)))
}

/// Make `buffer` the file as it is on disk now (its unsaved edits are gone,
/// as the native view's discard drops them), and the disk's version the one
/// the next save replaces.
fn reload(buffer: &mut Buffer) -> Result<(), String> {
    let fresh = read_disk(buffer, false)?;
    let before = buffer
        .store
        .snapshot(buffer.document)
        .ok_or_else(|| "the buffer is gone".to_string())?;
    let (seq, deltas) = if *before.text == *fresh.text {
        (before.seq, Vec::new())
    } else {
        match buffer.store.transact(
            buffer.document,
            before.seq,
            vec![TextEdit {
                range: 0..before.text.len(),
                insert: fresh.text.clone(),
            }],
        ) {
            DocumentTxnOutcome::Committed { seq, deltas, .. } => (seq, deltas),
            DocumentTxnOutcome::Conflict { .. } => {
                return Err("the buffer changed while it was reloaded".to_string());
            }
            DocumentTxnOutcome::Rejected(error) => return Err(store_words(&error).to_string()),
        }
    };
    buffer
        .persistence
        .accept_reloaded_observation(buffer.document, fresh.observed, &fresh.text)
        .map_err(|error| {
            save_words::words(Speaker::Amacs, &buffer.path, &Failure::Begin(&error))
        })?;
    buffer
        .store
        .checkpoint_ack(buffer.document, seq)
        .map_err(|error| store_words(&error).to_string())?;
    buffer.view.observe_external(seq, &deltas);
    buffer.missing = !fresh.observed.exists;
    Ok(())
}

/// The terminal face over `/dev/tty`, to its exit status: what
/// [`crate::run_client`] runs when the edit is this terminal's. `why`, when
/// given, is why the aterm window did not take the files, said first.
#[must_use]
pub fn run_tui(
    write: WriteCap,
    targets: &[Target],
    options: Options,
    why: Option<&str>,
) -> ExitCode {
    let mut stderr = io::stderr();
    if let Some(why) = why {
        client::say(&mut stderr, why);
    }
    #[cfg(unix)]
    {
        let mut tty = match aterm_tty::tty::DevTty::open() {
            Ok(tty) => tty,
            Err(error) => {
                client::say(
                    &mut stderr,
                    &format!("no terminal to edit in (/dev/tty: {error})"),
                );
                return ExitCode::from(decide::EXIT_USAGE);
            }
        };
        let mut tui = Tui::new(write, options);
        tui.inject_panic = aterm_types::dev_seam!(INJECT_PANIC_SEAM)
            .and_then(|seam| seam.to_str().and_then(|seam| seam.chars().next()));
        let outcome = tui.edit(&mut tty, targets);
        // The terminal is put back before anything is said on it.
        drop(tty);
        for line in &outcome.said {
            client::say(&mut stderr, line);
        }
        ExitCode::from(outcome.status)
    }
    #[cfg(not(unix))]
    {
        // The Windows console (slice T4) is not built yet.
        let _ = (write, targets, options);
        client::say(
            &mut stderr,
            "the terminal editor is not built for this system yet; run amacs inside an aterm window",
        );
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caret placed by `+LINE:COL` lands on that line's character, clamped
    /// to the line and to the text.
    #[test]
    fn a_position_names_a_line_and_a_character_on_it() {
        let text = "one\ntwo λx\nthree";
        assert_eq!(line_column_offset(text, 1, None), 0);
        assert_eq!(line_column_offset(text, 2, None), 4);
        assert_eq!(line_column_offset(text, 2, Some(5)), 4 + "two ".len());
        assert_eq!(line_column_offset(text, 2, Some(6)), 4 + "two λ".len());
        assert_eq!(line_column_offset(text, 2, Some(99)), 4 + "two λx".len());
        assert_eq!(line_column_offset(text, 99, None), "one\ntwo λx\n".len());
        assert_eq!(line_column_offset("", 3, Some(3)), 0);
    }

    /// The syntax after an edit is the syntax of the edited text lexed from
    /// scratch, whatever the edit joined, split or replaced.
    #[test]
    fn relexing_an_edit_matches_a_fresh_lex() {
        let texts = [
            "# Title\n\nsome *text*\n```\ncode\n```\n",
            "# Title\nsome *text*\n```\ncode\n```\n",
            "# Title\n\nsome *text*\n``\ncode\n```\n",
            "",
            "```\n",
            "a\nb\nc",
            "a\nX\nc",
            "ab",
            "a\nb",
        ];
        for old in texts {
            for new in texts {
                let mut cache = LexCache::new(Language::Markdown, old);
                relex(&mut cache, old, new);
                assert_eq!(
                    cache,
                    LexCache::new(Language::Markdown, new),
                    "{old:?} -> {new:?}"
                );
            }
        }
    }

    /// A question is answered by the byte a key types; Backspace, Escape and
    /// C-g answer too, and a chord or a key that types nothing does not.
    #[test]
    fn a_key_answers_with_the_byte_it_types() {
        let none = Modifiers::empty();
        assert_eq!(
            answer_byte(&Key::Character('y'), none, Some('y')),
            Some(b'y')
        );
        assert_eq!(
            answer_byte(&Key::Character('y'), Modifiers::SHIFT, Some('Y')),
            Some(b'Y')
        );
        assert_eq!(
            answer_byte(&Key::Named(NamedKey::Space), none, Some(' ')),
            Some(b' ')
        );
        assert_eq!(
            answer_byte(&Key::Named(NamedKey::Backspace), none, None),
            Some(0x7f)
        );
        assert_eq!(
            answer_byte(&Key::Named(NamedKey::Escape), none, None),
            Some(0x1b)
        );
        assert_eq!(
            answer_byte(&Key::Character('g'), Modifiers::CTRL, None),
            Some(0x07)
        );
        assert_eq!(
            answer_byte(&Key::Character('y'), Modifiers::ALT, None),
            None
        );
        assert_eq!(answer_byte(&Key::Character('λ'), none, Some('λ')), None);
        assert_eq!(
            answer_byte(&Key::Named(NamedKey::ArrowUp), none, None),
            None
        );
        for byte in 0..=u8::MAX {
            let classes = [saves_on(byte), discards_on(byte), cancels_on(byte)];
            assert!(
                classes.iter().filter(|class| **class).count() <= 1,
                "{byte:#04x}"
            );
        }
    }

    /// A paste's CR and CRLF breaks are line feeds in the buffer.
    #[test]
    fn a_paste_breaks_lines_with_line_feeds() {
        assert_eq!(paste_text("a\rb\r\nc\nd"), "a\nb\nc\nd");
    }
}
