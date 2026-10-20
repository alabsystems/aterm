// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Predictive echo never paints a keystroke the program will not echo, nor a
//! guess or a cursor where the echo will not land.
//!
//! Driven end to end on a headless App: presses go through [`App::input`] (the
//! press path that arms guesses), the program's output goes into the focused
//! pane's terminal, and the composed present (`redraw_compose`) reconciles and
//! paints. The echo latency the Adaptive gate measures is injected through the
//! frame instant handed to the present, never slept.
//!
//! Each scenario first shows the rig can paint a ghost (an echoing line on a
//! slow link), so "the secret is not on glass" is not vacuous.

use crate::input::{InputEvent, Source};
use crate::{App, WindowId, term_lock};
use aterm_session::sink::SinkWriter;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
/// A link slow enough for Adaptive to paint: two such confirmations latch it.
const RTT: Duration = Duration::from_millis(60);
/// What the remote shell prints for `sudo`: the password prompt.
const PROMPT: &str = "[sudo] password for me: ";

fn press(key: Key, mods: Modifiers) -> InputEvent {
    InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

fn glyph(c: char) -> InputEvent {
    press(Key::Character(c), Modifiers::empty())
}

/// A focused headless split in `mode`; the caret sits on the focused pane's
/// bottom row (a terminal in use is scrolled to the bottom), after `$ `.
struct Rig {
    app: App,
    wid: WindowId,
    focus: u64,
    other: u64,
    bottom: usize,
}

impl Rig {
    fn new(mode: &str) -> Self {
        Self::with_sink(mode, Arc::new(SinkWriter::new(-1)))
    }

    /// [`Self::new`] whose focused leaf writes into `sink`.
    fn with_sink(mode: &str, sink: Arc<SinkWriter>) -> Self {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let _ = app.split_active_stub_tab_dir_with_sink(wid, crate::pane::SplitDir::Vertical, sink);
        app.windows.get_mut(&wid).expect("window").focused = true;
        app.config.predictive_echo = Some(mode.to_string());
        app.predict_mode_cache = None;
        let (focus, other, rows) = {
            let ws = app.windows.get(&wid).expect("window");
            let tree = &ws.layouts[ws.tabs.active];
            let focus = tree.focus();
            let layout = tree.compute_layout(ws.rows, ws.cols);
            let rect = layout
                .iter()
                .find(|r| r.session == focus)
                .expect("the focused leaf is laid out");
            let other = layout
                .iter()
                .map(|r| r.session)
                .find(|s| *s != focus)
                .expect("the background leaf is laid out");
            (focus, other, usize::from(rect.rows))
        };
        let rig = Self {
            app,
            wid,
            focus,
            other,
            bottom: rows - 1,
        };
        rig.feed(&"\r\n".repeat(rows + 4));
        rig.feed("$ ");
        assert_eq!(
            rig.cursor(),
            (rig.bottom, 2),
            "fixture: the caret is on the bottom row"
        );
        assert_eq!(
            rig.app.front_terminal(wid).map(|t| t.session),
            Some(focus),
            "fixture: the keyboard drives the focused leaf"
        );
        rig
    }

    /// The program writes `text` (an echo, a prompt, a newline).
    fn feed(&self, text: &str) {
        let term = self.app.pool.get(self.focus).expect("focus").term.clone();
        term_lock(&term).process(text.as_bytes());
    }

    fn cursor(&self) -> (usize, usize) {
        let term = self.app.pool.get(self.focus).expect("focus").term.clone();
        let c = term_lock(&term).cursor();
        (usize::from(c.row), usize::from(c.col))
    }

    fn key(&mut self, ev: InputEvent) {
        let _ = self.app.input(self.wid, ev, Source::Human);
    }

    /// One presented frame at `now`. The background pane is damaged so a frame
    /// presents even when the focused pane did not change.
    fn present(&mut self, now: Instant) {
        let term = self.app.pool.get(self.other).expect("other").term.clone();
        term_lock(&term).process(b".");
        assert!(
            self.app
                .redraw_compose(self.wid, ROWS, COLS, false, false, None, 0, now)
                .is_some(),
            "the frame presents"
        );
    }

    /// The glyph the presented composite carries at a focused-pane cell.
    fn on_glass(&self, row: usize, col: usize) -> char {
        let ws = self.app.windows.get(&self.wid).expect("window");
        let tree = &ws.layouts[ws.tabs.active];
        let rect = *tree
            .compute_layout(ws.rows, ws.cols)
            .iter()
            .find(|r| r.session == self.focus)
            .expect("focused leaf");
        ws.input_scratch
            .cells
            .get(usize::from(rect.row_off) + row)
            .and_then(|r| r.get(usize::from(rect.col_off) + col))
            .map_or('\u{0}', |c| c.ch)
    }

    fn displaying(&self) -> bool {
        self.app.windows[&self.wid]
            .predictor
            .is_displaying(Instant::now())
    }

    /// Type `c`; the program echoes it one slow RTT later.
    fn type_echoed(&mut self, c: char) {
        self.key(glyph(c));
        self.feed(&c.to_string());
        self.present(Instant::now() + RTT);
    }

    /// Type a command on the slow link until Adaptive paints, and show it does:
    /// the last glyph is on glass before its echo (the rig's positive control).
    fn type_command_on_a_slow_link(&mut self, cmd: &str) {
        let (head, last) = cmd.split_at(cmd.len() - 1);
        for c in head.chars() {
            self.type_echoed(c);
        }
        let last = last.chars().next().expect("a non-empty command");
        let (row, col) = self.cursor();
        self.key(glyph(last));
        self.present(Instant::now());
        assert!(self.displaying(), "control: a slow, echoing line paints");
        assert_eq!(
            self.on_glass(row, col),
            last,
            "control: the rig paints a ghost before the echo"
        );
        self.feed(&last.to_string());
        self.present(Instant::now() + RTT);
    }

    /// The person types one secret glyph at a prompt that never echoes; return
    /// whether it reached glass (ghost cell or a displaying predictor).
    fn secret_is_painted(&mut self, secret: char) -> bool {
        let (row, col) = self.cursor();
        self.key(glyph(secret));
        let displaying = self.displaying();
        self.present(Instant::now());
        displaying || self.on_glass(row, col) == secret
    }
}

/// A line the program ended (`read -n1`'s answer needs no Enter) scrolls the
/// next prompt onto the same physical bottom row. The echo proof belongs to the
/// line that scrolled away: the password typed at the new prompt is not painted.
#[test]
fn a_prompt_scrolled_onto_the_echoing_row_does_not_inherit_its_proof() {
    let mut rig = Rig::new("adaptive");
    rig.type_command_on_a_slow_link("./go");
    rig.key(press(Key::Named(NamedKey::Enter), Modifiers::empty()));
    rig.feed("\r\nProceed? [y/N] ");
    rig.present(Instant::now() + RTT);
    // `read -n1`: the answer echoes and ends the line by itself.
    rig.type_echoed('y');
    rig.feed(&format!("\r\n{PROMPT}"));
    rig.present(Instant::now() + RTT);
    assert_eq!(
        rig.cursor(),
        (rig.bottom, PROMPT.len()),
        "fixture: the password prompt reuses the bottom row"
    );
    assert!(
        !rig.secret_is_painted('h'),
        "an unechoed password glyph was painted on a prompt that inherited \
         the previous line's echo proof"
    );
}

/// Over ssh no tty verdict reaches the host (the local pty is the ssh client's,
/// raw), so a remote `read -n1 -p 'Proceed? '; read -s -p ' Password: '` is all
/// one row: the answer echoes and proves the line, then the remote prints the
/// password prompt beside it. That print moved the cursor without us; the
/// password typed where it left the cursor must not be painted.
#[test]
fn a_prompt_printed_beside_an_echoed_answer_does_not_inherit_its_proof() {
    let mut rig = Rig::new("adaptive");
    rig.type_command_on_a_slow_link("./go");
    rig.key(press(Key::Named(NamedKey::Enter), Modifiers::empty()));
    rig.feed("\r\nProceed? ");
    rig.present(Instant::now() + RTT);
    rig.type_echoed('y');
    assert!(
        rig.app.windows[&rig.wid].predictor.idle(),
        "fixture: the answer's echo was confirmed"
    );
    rig.feed(" Password: ");
    assert_eq!(rig.cursor().0, rig.bottom, "fixture: one row, no scroll");
    assert!(
        !rig.secret_is_painted('h'),
        "a password glyph typed after a same-row prompt was painted"
    );
}

/// A command submitted by a key other than a plain Enter — Shift+Enter (aterm
/// sends LF, readline's accept-line), Ctrl+Enter (CR), Ctrl+J, Ctrl+M — and
/// the password typed ahead of the prompt on a slow link. The submit is input
/// the predictor did not model; what is typed after it has no echo proof.
#[test]
fn a_password_typed_ahead_after_any_submit_key_is_not_painted() {
    let submits: [(&str, InputEvent); 4] = [
        (
            "shift+enter",
            press(Key::Named(NamedKey::Enter), Modifiers::SHIFT),
        ),
        (
            "ctrl+enter",
            press(Key::Named(NamedKey::Enter), Modifiers::CTRL),
        ),
        ("ctrl+j", press(Key::Character('j'), Modifiers::CTRL)),
        ("ctrl+m", press(Key::Character('m'), Modifiers::CTRL)),
    ];
    for (label, submit) in submits {
        let mut rig = Rig::new("adaptive");
        rig.type_command_on_a_slow_link("sudo");
        rig.key(submit);
        // Typed ahead: the remote has not answered the submit yet.
        assert!(
            !rig.secret_is_painted('h'),
            "{label}: a password glyph typed after the submit was painted"
        );
    }
}

/// A word typed ahead of its echo on a slow link (`echo foo`) keeps painting
/// across an unmodelled line terminator, which cannot erase or move it. Any other
/// unmodelled key may erase it (Ctrl+W, Ctrl+H, or a `[key_sequences]` remap the
/// host cannot list); once the erase echoes, the cursor sits over a blank cell
/// the reconcile cannot tell from "not echoed yet", so the guesses must go.
#[test]
fn a_typed_ahead_word_outlives_only_a_line_terminator() {
    fn typed_ahead_foo(rig: &mut Rig) -> (usize, usize) {
        rig.type_command_on_a_slow_link("echo");
        rig.type_echoed(' ');
        let (row, col) = rig.cursor();
        for c in "foo".chars() {
            rig.key(glyph(c));
        }
        rig.present(Instant::now());
        assert_eq!(
            rig.on_glass(row, col),
            'f',
            "control: the typed-ahead word paints"
        );
        (row, col)
    }
    let ctrl = |c| press(Key::Character(c), Modifiers::CTRL);
    let bytes = |b: &[u8]| InputEvent::KeySequence(b.to_vec());

    let terminators: [(&str, InputEvent); 4] = [
        (
            "enter",
            press(Key::Named(NamedKey::Enter), Modifiers::empty()),
        ),
        (
            "shift+enter",
            press(Key::Named(NamedKey::Enter), Modifiers::SHIFT),
        ),
        ("ctrl+j", ctrl('j')),
        ("a remapped CR", bytes(b"\r")),
    ];
    for (label, submit) in terminators {
        let mut rig = Rig::new("adaptive");
        let (row, col) = typed_ahead_foo(&mut rig);
        rig.key(submit);
        rig.present(Instant::now());
        let tail: String = (col..col + 3).map(|c| rig.on_glass(row, c)).collect();
        assert_eq!(
            tail, "foo",
            "{label}: the submitted line's tail blinked out"
        );
    }

    // (label, the key, what the program echoes for "foo" and then the key, how
    // many cells from the word's start the cursor ends)
    let kill_word = "foo\x08\x08\x08\x1b[K";
    let delete_char = "foo\x08 \x08";
    let erases: [(&str, InputEvent, &str, usize); 4] = [
        ("ctrl+w", ctrl('w'), kill_word, 0),
        ("ctrl+h", ctrl('h'), delete_char, 2),
        ("a remapped ctrl+w", bytes(b"\x17"), kill_word, 0),
        ("a remapped BS", bytes(b"\x08"), delete_char, 2),
    ];
    for (label, erase, echo, end) in erases {
        let mut rig = Rig::new("adaptive");
        let (row, col) = typed_ahead_foo(&mut rig);
        rig.key(erase);
        rig.feed(echo);
        assert_eq!(rig.cursor(), (row, col + end), "{label}: fixture");
        rig.present(Instant::now() + RTT);
        let erased: String = (col + end..col + 3).map(|c| rig.on_glass(row, c)).collect();
        assert!(
            !erased.contains(['f', 'o']),
            "{label}: the erased text is still on glass as a ghost: {erased:?}"
        );
    }
}

/// Set the slave's local modes, as the program reading it would.
#[cfg(unix)]
fn set_lflag(slave: i32, edit: impl FnOnce(&mut libc::tcflag_t)) {
    // SAFETY: `libc::termios` is plain integer fields plus a byte array;
    // zeroed is a valid value, and `tcgetattr` fills it on the open slave.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `slave` is an open tty descriptor this test owns.
    assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0, "tcgetattr");
    edit(&mut t.c_lflag);
    // SAFETY: `t` is a valid termios for the open slave.
    assert_eq!(
        unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) },
        0,
        "tcsetattr"
    );
}

/// A local prompt that reads in canonical mode with ECHO clear (`read -s`,
/// `sudo`, `ssh`'s passphrase, `getpass`) will never echo the next byte, and
/// the host reads exactly that off the pty at the key. No mode paints there —
/// not `always`, and not `adaptive` on a line that already proved it echoes
/// (`read -n1 -p 'Proceed? '; read -s -p ' Password: '` shares one row).
#[cfg(unix)]
#[test]
fn a_tty_that_will_not_echo_is_never_predicted() {
    type Edit = fn(&mut libc::tcflag_t);
    // `always`: the first keystroke paints unless the tty's verdict stops it.
    let cases: [(&str, Edit, bool); 3] = [
        ("cooked (the kernel echoes)", |_| {}, true),
        (
            "canonical no-echo (read -s, sudo, getpass)",
            |l| *l &= !libc::ECHO,
            false,
        ),
        (
            "raw (the program echoes for itself)",
            |l| *l &= !(libc::ECHO | libc::ICANON),
            true,
        ),
    ];
    for (label, edit, paints) in cases {
        let (master, slave) = super::typed_kitty_summon_tests::pty_pair_with_lflag(edit);
        let sink = Arc::new(SinkWriter::new(master));
        let mut rig = Rig::with_sink("always", sink.clone());
        assert_eq!(rig.secret_is_painted('h'), paints, "{label}");
        drop(rig);
        drop(sink);
        // SAFETY: both fds are ours and open; the borrowed-fd sink did not
        // close the master.
        unsafe {
            // The master first: a slave closed over output its master never read
            // waits out the tty's drain (0.6 s on macOS).
            libc::close(master);
            libc::close(slave);
        }
    }

    // `adaptive` on a slow link, one row: the shell line, `read -n1`'s echoed
    // answer, then `read -s` on the same row.
    let (master, slave) = super::typed_kitty_summon_tests::pty_pair_with_lflag(|l| {
        *l &= !(libc::ECHO | libc::ICANON)
    });
    let sink = Arc::new(SinkWriter::new(master));
    let mut rig = Rig::with_sink("adaptive", sink.clone());
    rig.type_command_on_a_slow_link("./go");
    rig.key(press(Key::Named(NamedKey::Enter), Modifiers::empty()));
    rig.feed("\r\nProceed? ");
    rig.present(Instant::now() + RTT);
    set_lflag(slave, |l| *l = (*l | libc::ECHO) & !libc::ICANON); // read -n1
    rig.type_echoed('y');
    rig.feed(" Password: ");
    set_lflag(slave, |l| *l = (*l | libc::ICANON) & !libc::ECHO); // read -s
    assert!(
        !rig.secret_is_painted('h'),
        "adaptive: a canonical no-echo read on an echo-proven row was painted"
    );
    drop(rig);
    drop(sink);
    // SAFETY: as above.
    unsafe {
        // The master first: a slave closed over output its master never read
        // waits out the tty's drain (0.6 s on macOS).
        libc::close(master);
        libc::close(slave);
    }
}

// --- Where a guess is placed and where the cursor is drawn ---------------------

/// Under `ambiguous_width = "wide"` the grid writes `é` two cells wide, so the
/// press path tells the predictor (`set_ambiguous_width_double`, read off the
/// focused terminal at the key) and `é` is declined like CJK rather than guessed
/// one cell where its echo takes two. Under the narrow policy the same press is
/// a guess (the control), so the wide refusal is the call's doing.
#[test]
fn the_press_path_measures_a_glyph_with_the_terminals_ambiguous_width() {
    for wide in [false, true] {
        let mut rig = Rig::new("always");
        if wide {
            let config = aterm_core::config::TerminalConfig {
                ambiguous_width_double: true,
                ..aterm_core::config::TerminalConfig::default()
            };
            let term = rig.app.pool.get(rig.focus).expect("focus").term.clone();
            term_lock(&term).apply_config(&config);
        }
        rig.key(glyph('é'));
        let pending = rig.app.windows[&rig.wid].predictor.pending().count();
        assert_eq!(
            pending,
            usize::from(!wide),
            "wide={wide}: `é` is a guess exactly when the grid writes it one cell wide"
        );
    }
}

/// The composed present draws the cursor one past the newest guess it PAINTED.
/// The program left `" 日"` after the prompt and the cursor back on the blank;
/// `b` is guessed on the blank, `c` on `日`'s lead, which the painter refuses.
/// One past `c` would be `日`'s right half, under a block cursor; the cursor goes
/// one past `b` instead, onto the cell `c` will echo on.
#[test]
fn the_composed_cursor_never_covers_half_of_a_wide_glyph() {
    let mut rig = Rig::new("always");
    rig.feed(" 日\x1b[3D");
    assert_eq!(rig.cursor(), (rig.bottom, 2), "fixture: back on the blank");
    rig.key(glyph('b'));
    rig.key(glyph('c'));
    assert_eq!(
        rig.app.windows[&rig.wid].predictor.pending().count(),
        2,
        "fixture: both glyphs are guessed"
    );
    rig.present(Instant::now());
    assert_eq!(
        rig.on_glass(rig.bottom, 2),
        'b',
        "the blank takes its ghost"
    );
    assert_eq!(
        rig.on_glass(rig.bottom, 3),
        '日',
        "the wide glyph refuses one"
    );
    let ws = rig.app.windows.get(&rig.wid).expect("window");
    let tree = &ws.layouts[ws.tabs.active];
    let rect = *tree
        .compute_layout(ws.rows, ws.cols)
        .iter()
        .find(|r| r.session == rig.focus)
        .expect("focused leaf");
    assert_eq!(
        (ws.input_scratch.cursor_row, ws.input_scratch.cursor_col),
        (
            usize::from(rect.row_off) + rig.bottom,
            usize::from(rect.col_off) + 3
        ),
        "one past `b`, on `日`'s lead — not one past the refused `c`, on its right half"
    );
}
