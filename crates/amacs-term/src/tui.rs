// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The terminal face (design §5.4, §5.7; slice T3).

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

/// The kitty keyboard flags the face pushes.
pub const KITTY_FLAGS: u8 = 1 | 4;

/// The most text one OSC 52 write carries.
pub const OSC52_LIMIT: usize = 64 * 1024;
const _: () = assert!(OSC52_LIMIT == 65_536);

const WHEEL_LINES: i32 = 3;

const HINT: &str = "C-x C-s saves · C-x C-c exits · C-g cancels";

const INJECT_PANIC_SEAM: &str = "ATERM_AMACS_INJECT_PANIC";

/// The settings the face runs with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// `[amacs] osc52`.
    pub osc52: bool,
    /// `[amacs] mouse`.
    pub mouse: bool,
    /// The program the edit is for (`for=`), named on the modeline.
    pub for_word: Option<String>,
    /// aterm's `ambiguous_width`.
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

/// How a run of the face ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub status: u8,
    pub said: Vec<String>,
}

/// How one file's edit ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Close {
    /// The file was closed (`C-x C-c`, `C-x k`), saved or not as the person answered.
    Closed,
    /// `C-c C-c`: saved, then closed.
    Finished,
    /// `C-c C-k`: closed without saving.
    Aborted,
    /// A read found the terminal's end (EOF, or EIO with SIGHUP ignored, as under `nohup`).
    Hangup,
}

impl Close {
    const fn wait_end(self) -> u8 {
        match self {
            Self::Closed => decide::WAIT_CLOSED,
            Self::Finished => decide::WAIT_FINISHED,
            Self::Aborted => decide::WAIT_ABORTED,
            Self::Hangup => decide::WAIT_FAILED,
        }
    }
}

trust_contract_fn! {
    /// The keys that answer "save it".
    #[must_use]
    pub fn saves_on(byte: u8) -> bool
    clauses [ensures result == (byte == 32 || (byte == 33 || (byte == 89 || byte == 121))) by saves_on_is_the_class]
    {
        matches!(byte, b' ' | b'!' | b'Y' | b'y')
    }
}

trust_contract_fn! {
    /// The keys that answer "close it without saving".
    #[must_use]
    pub fn discards_on(byte: u8) -> bool
    clauses [ensures result == (byte == 78 || (byte == 110 || byte == 127)) by discards_on_is_the_class]
    {
        matches!(byte, b'N' | b'n' | 0x7f)
    }
}

trust_contract_fn! {
    /// The keys that take the question back and go on editing.
    #[must_use]
    pub fn cancels_on(byte: u8) -> bool
    clauses [ensures result == (byte == 7 || (byte == 27 || (byte == 81 || byte == 113))) by cancels_on_is_the_class]
    {
        matches!(byte, 0x07 | 0x1b | b'Q' | b'q')
    }
}

trust_contract_fn! {
    /// Whether a kill of `len` bytes goes to the terminal's clipboard.
    #[must_use]
    pub fn fits_osc52(len: usize) -> bool
    clauses [ensures result == (len <= 65536) by fits_osc52_is_the_limit]
    {
        len <= OSC52_LIMIT
    }
}

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

/// The editor in a terminal.
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

const fn lines_of<'a>(text: &'a str, starts: Option<&'a [usize]>) -> EditorLines<'a> {
    match starts {
        Some(starts) => EditorLines::indexed(text, starts),
        None => EditorLines::scanning(text),
    }
}

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

/// Why the file host refused to read `path`, in `amacs_files::save_words` phrasing.
pub(crate) fn host_error(path: &Path, error: &DocumentHostError) -> String {
    save_words::words(Speaker::Amacs, path, &Failure::Open(error))
}

const fn store_words(error: &DocumentError) -> &'static str {
    match error {
        DocumentError::UnknownDocument | DocumentError::UnknownView => "the buffer is gone",
        DocumentError::Closing | DocumentError::CloseNotReady => "the buffer is closing",
        DocumentError::InvalidRange | DocumentError::OverlappingEdits => {
            "the buffer refused the edit"
        }
        DocumentError::CheckpointAheadOfHead => "the buffer changed while it was saved",
    }
}

const fn error_text(error: &EditorError) -> &'static str {
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

fn paste_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn osc52(text: &str) -> Option<Vec<u8>> {
    let encoded = aterm_codec::base64::encode(text.as_bytes()).ok()?;
    Some(format!("\x1b]52;c;{encoded}\x07").into_bytes())
}

impl Tui {
    /// A face that edits with `write`, the run's one buffer-write capability.
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

    /// Open `target` as amacs-files admits a document.
    /// # Errors
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

    /// Edit `buffer` until closed or the terminal hangs up, entering the screen first if needed.
    /// # Errors
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

    /// Put back every mode the screen was entered with, in the one order every restore uses.
    /// # Errors
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

    fn layout(&self, buffer: &Buffer) -> (render::Layout, render::EchoLine) {
        let echo = self.echo(buffer);
        let rows = render::echo_rows(self.size, self.options.ambiguous, &echo);
        (render::layout(self.size.rows, rows), echo)
    }

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
            return self.suspend(tty, buffer);
        }
        if signals.contains(Signals::WINCH) || signals.contains(Signals::CONT) {
            self.refit(tty, buffer)?;
        }
        Ok(())
    }

    fn suspend<T: Tty + ?Sized>(&mut self, tty: &mut T, buffer: &mut Buffer) -> io::Result<()> {
        loop {
            tty.suspend()?;
            if !tty.take_signals()?.contains(Signals::TSTP) {
                break;
            }
        }
        self.refit(tty, buffer)
    }

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

    fn input<T: Tty + ?Sized>(
        &mut self,
        tty: &mut T,
        buffer: &mut Buffer,
        input: EditorInput,
    ) -> io::Result<Option<Close>> {
        let pressed = !matches!(input, EditorInput::Focus(_));
        if pressed {
            self.status = None;
        }
        let document = buffer.document;
        let reduction = self
            .workspace
            .handle_input(&mut buffer.store, &mut buffer.view, input);
        // This face has one view, already advanced by the reducer. Release its
        // publication batch even when a macro committed before a later error.
        let _ = self.workspace.take_commits(document);
        let reduction = match reduction {
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

    fn draw<T: Tty + ?Sized>(&mut self, tty: &mut T, buffer: &mut Buffer) -> io::Result<()> {
        let (layout, echo) = self.layout(buffer);
        let text = buffer.text();
        let lines = lines_of(&text, buffer.starts.as_deref());
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

fn take_disk_version(buffer: &mut Buffer) -> Result<(), String> {
    let fresh = read_disk(buffer, true)?;
    buffer
        .persistence
        .accept_overwrite(buffer.document, fresh.observed)
        .map_err(|error| save_words::words(Speaker::Amacs, &buffer.path, &Failure::Begin(&error)))
}

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

/// The terminal face over `/dev/tty`, to its exit status.
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
        drop(tty);
        for line in &outcome.said {
            client::say(&mut stderr, line);
        }
        ExitCode::from(outcome.status)
    }
    #[cfg(not(unix))]
    {
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

    #[test]
    fn input_releases_commit_projections_even_after_a_partial_macro_error() {
        use amacs::editor::EditorCommand;
        use aterm_tty::tty::MemTty;

        // SAFETY: the test is a launcher with no untrusted input; its capability
        // is used only for this scratch document.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let mut tui = Tui::new(
            authority.grant(aterm_cap::Tier::Trusted),
            Options::default(),
        );
        let scratch = aterm_tempfile::TempDir::new().unwrap();
        let path = scratch.path().join("publication.txt");
        std::fs::write(&path, "base").unwrap();
        let mut buffer = tui.open(&Target { path, line: None }).unwrap();
        let (mut tty, _peer) = MemTty::pair(Size { cols: 80, rows: 24 });
        let key = |character, mods| EditorInput::Key {
            key: Key::Character(character),
            mods,
            text: None,
        };
        let mut send = |tui: &mut Tui, buffer: &mut Buffer, input| {
            assert!(tui.input(&mut tty, buffer, input).unwrap().is_none());
            assert!(tui.workspace.take_commits(buffer.document).is_empty());
        };

        send(
            &mut tui,
            &mut buffer,
            EditorInput::Text("seed ".to_string()),
        );
        assert_eq!(buffer.text().as_ref(), "seed base");
        tui.workspace
            .execute(
                &mut buffer.store,
                &mut buffer.view,
                EditorCommand::StartMacro,
            )
            .unwrap();
        send(&mut tui, &mut buffer, EditorInput::Text("x".to_string()));
        for _ in 0..2 {
            send(&mut tui, &mut buffer, key('/', Modifiers::CTRL));
        }
        tui.workspace
            .execute(&mut buffer.store, &mut buffer.view, EditorCommand::EndMacro)
            .unwrap();
        assert_eq!(buffer.text().as_ref(), "base");
        let before = buffer.store.snapshot(buffer.document).unwrap();

        send(&mut tui, &mut buffer, key('x', Modifiers::CTRL));
        send(&mut tui, &mut buffer, key('e', Modifiers::empty()));
        let after = buffer.store.snapshot(buffer.document).unwrap();
        assert_eq!(after.text, before.text);
        assert!(after.seq > before.seq);
        assert_eq!(
            tui.status.as_deref(),
            Some(error_text(&EditorError::NothingToUndo))
        );
        assert!(!tui.workspace.can_undo(buffer.document));
        assert!(tui.workspace.can_redo(buffer.document));
    }

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

    #[test]
    fn a_paste_breaks_lines_with_line_feeds() {
        assert_eq!(paste_text("a\rb\r\nc\nd"), "a\nb\nc\nd");
    }
}
