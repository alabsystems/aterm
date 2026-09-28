// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A COPY THAT APPEARED BESIDE A LINE IS NOT WHERE THE LINE WENT**
//! (2026-09-25).
//!
//! The follow pass carries a band onto the row its text moved to
//! (`rk::witness::Witness::follow_runs`), and counts a glyph found there as
//! evidence only if it ARRIVED. A copy of the line that APPEARS beside it
//! after its first keys — while the typed text still stands — did not
//! arrive, and must not carry the band when the line is erased: the band
//! would light the copy, which no key wrote, for ~1.3 s. The shapes are
//! everyday ones: fzf's list landing after the query's first keys (its
//! input still loading), a completion popup that opens after two keys, a
//! line drawn above the prompt that echoes it, vim's `yyp`, a streamed
//! answer quoting the composer's words back, a job notice re-echoing the
//! command line.
//!
//! The witness's evidence is ACCUMULATED (`rk::witness`, its module doc):
//! on every frame the witness has rows, every standing record reads what
//! its sampled neighbour rows hold at its column, and a copy of the glyph
//! there is a `twin`, sticky — at once on the arming frame, otherwise once
//! it has stood there `TWIN_MIN` (40 ms of wall time, seen on every sampled
//! frame between: time, not frames, because two frames are 16 ms while a
//! key's light is live and a quarter of a second under the no-pet style's
//! idle pacing). A glyph found at a move ARRIVED only if its record is not
//! a twin there; otherwise it is neutral. A copy that stood beside the line
//! makes every record it matches a twin, and the erase is no move. Two
//! places the witness knows it has not seen are read accordingly: the row a
//! band move opens beside a line is not clear until a frame shows it
//! without the glyph (`yyp`), and a row a scroll brings in dates a copy on
//! it from the last walk before the scroll.
//!
//! Every law runs at the HOST seam, GUI-faithfully (`trail_host::Core`): a
//! real `aterm_core::terminal::Terminal` driven byte for byte; the
//! content-scroll seam first (`sync_cursor_effect_scroll` — a frame that
//! scrolled or moved a band samples nothing, as `app_render.rs` does); then
//! the frame hold's rows (`row_cols_into` after the batch, fed to
//! `CursorGlow::observe_row` / `observe_ribbon_row` / `ribbon_rows`); then
//! `CursorGlow::tick`. Frames come at 16 ms, the cadence a fading ribbon
//! keeps. A RED reading is `(followed, frames the copy's row was lit out of
//! 90)`, against `(0, 0)`; each law names its reading on 0.93.0.
//!
//! THE LIMIT. A copy that stood less than `TWIN_MIN` is not yet a twin, on
//! purpose: a program that repaints without a synchronized-update bracket
//! can draw the text's new row up to three frames before it erases the old
//! — a torn repaint, which must follow (`tests/moves_are_followed.rs`). A
//! copy that appears that shortly before the line is erased is not told
//! from that, and neither is a copy written in the SAME batch as the erase
//! onto a row the witness had seen empty, or never seen: glyph for glyph,
//! that is the line relocated one row, which follows.
//! `the_limit_a_copy_that_stood_less_than_twin_min_is_a_move`
//! states both; `tests/copies_are_not_moves.rs` sweeps every family of
//! these shapes over grids of timing and pacing.

#[macro_use]
mod trail_host;

use std::time::Duration;
use trail_host::{Core, Opts, Theme};

const ROWS: usize = 24;
const COLS: usize = 80;

/// The GUI's frame seam (`trail_host::Core`): the content-scroll seam first
/// (a scroll or band move is applied to the engine and the frame samples
/// nothing), the alt-screen re-baseline, then the frame hold — the caret row's
/// probe, which is also its witness sample, and the rows the witness names,
/// all read AFTER the last `process` — then the tick.
const SEAM: Opts = Opts {
    scroll_seam: true,
    alt_rebaseline: true,
    skip_caret: true,
    honour_sync: false,
    pane_columns: false,
};

/// The host: `trail_host`'s frame seam on a 24×80 grid of 8×16 cells, and
/// this file's input gestures.
struct Host {
    c: Core,
}
core_deref!(Host);

impl Host {
    /// A fresh host with the caret parked at 0-based `(row, 0)` and the
    /// anchor seeded by one frame.
    fn at_row(row: u16) -> Self {
        Self::in_pane(ROWS, row)
    }

    /// A host whose terminal is the TOP PANE of a stacked split: `pane_rows`
    /// rows of the 24-row window, the glow's coordinate space the window's
    /// (as `app_render.rs`'s composed path hands it over, with
    /// `note_pane_rows`). The caret parked at 0-based `(row, 0)`.
    fn in_pane(pane_rows: usize, row: u16) -> Self {
        let mut c = Core::new(pane_rows, COLS, 8, 16, Theme::Tokyo, SEAM);
        c.g.rows = ROWS;
        c.g.win_h = u16::try_from(ROWS * 16).expect("a small window");
        if pane_rows < ROWS {
            c.glow.note_pane_rows(0, pane_rows);
        }
        c.term.process(format!("\x1b[{};1H", row + 1).as_bytes());
        c.frame();
        Self { c }
    }

    /// Idle frames at 16 ms until `ms` have passed.
    fn idle(&mut self, ms: u64) {
        let end = self.now + Duration::from_millis(ms);
        while self.now < end {
            self.now += Duration::from_millis(16);
            self.frame();
        }
    }

    /// Type `s` a key at a time, 90 ms apart, each key's echo — followed by
    /// `suffix`, the program's own redraw in the same write — on one frame.
    fn type_with(&mut self, s: &str, suffix: &str) {
        for ch in s.chars() {
            self.now += Duration::from_millis(90);
            let now = self.now;
            self.glow.note_typed_cells(now, 1);
            self.term.process(format!("{ch}{suffix}").as_bytes());
            self.frame();
        }
    }

    fn type_str(&mut self, s: &str) {
        self.type_with(s, "");
    }

    /// A PTY batch with no key behind it, presented on one frame.
    fn program(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        self.term.process(bytes);
        self.frame();
    }

    /// The kill key (Ctrl-U, Ctrl-W): the hand's kill hint, then the line
    /// editor's erase `bytes`, on the next frame.
    fn kill(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(16);
        let now = self.now;
        self.glow.note_kill(now, true);
        self.term.process(bytes);
        self.frame();
    }

    /// `(followed since f0, frames `row` was lit over the next ~1.4 s)`.
    fn verdict(&mut self, f0: u64, row: u16) -> (u64, usize) {
        let followed = self.followed() - f0;
        let mut lit = 0;
        for _ in 0..90 {
            self.idle(16);
            lit += usize::from(self.lit(row));
        }
        (followed, lit)
    }
}

/// fzf with `--info=hidden` whose input is still loading when the hand
/// starts typing: the list rows are blank while the query's first `k0` keys
/// echo, then the input lands and fzf draws its list, the best match
/// `src/lib.rs` beside the prompt — the pointer `> ` at column 0, the item
/// at column 2, the query's own columns — redrawn with every later key.
/// `reverse` is `--layout=reverse`: the prompt on top, the best match BELOW
/// it. Ctrl-U clears the query and fzf redraws `redraw_after`; the query
/// went nowhere. Returns the verdict on the best match's row.
fn fzf_list_lands_late(k0: usize, reverse: bool, redraw_after: Option<&str>) -> (u64, usize) {
    let q = "src/li";
    let (query_row, item_row) = if reverse { (10u16, 11u16) } else { (21, 20) };
    let mut h = Host::at_row(query_row);
    h.program(format!("\x1b[{};1H> ", query_row + 1).as_bytes());
    let list = if reverse {
        "\x1b7\x1b[12;1H> src/lib.rs\x1b[K\x1b[13;1H  src/main.rs\x1b[K\x1b[14;1H  tests/a.rs\x1b[K\x1b8"
    } else {
        "\x1b7\x1b[19;1H  tests/a.rs\x1b[K\x1b[20;1H  src/main.rs\x1b[K\x1b[21;1H> src/lib.rs\x1b[K\x1b8"
    };
    h.type_str(&q[..k0]);
    h.program(list.as_bytes());
    h.type_with(&q[k0..], list);
    h.idle(64);
    assert_eq!(
        h.live(query_row),
        (2..8).collect::<Vec<u16>>(),
        "fixture: the band is under the query"
    );
    let f0 = h.followed();
    let after = redraw_after.unwrap_or(list);
    h.kill(format!("\r\x1b[2C\x1b[K{after}").as_bytes());
    h.verdict(f0, item_row)
}

/// **FZF'S LIST LANDS AFTER THE QUERY'S FIRST KEYS, THEN CTRL-U** — the
/// best match above the prompt (default layout) or below it (`--reverse`),
/// landing after two or three query keys. The records of the keys typed
/// before the list were armed over blank rows, CLEAR there; they learn the
/// list item as a twin once it has stood beside them `TWIN_MIN`. Read as
/// arrived, those records carry the band onto the list item. RED on 0.93.0
/// after three keys, `(3, 80)` and `(3, 81)`; GREEN after two.
#[test]
fn an_fzf_list_that_lands_after_the_query_s_first_keys_is_not_where_the_query_went() {
    for (k0, reverse) in [(2, false), (2, true), (3, false), (3, true)] {
        assert_eq!(
            fzf_list_lands_late(k0, reverse, None),
            (0, 0),
            "the list landed after {k0} keys, reverse={reverse}: (followed, frames the best \
             match's row was lit) — the query went nowhere"
        );
    }
}

/// **…AND RE-SORTED AS THE QUERY IS CLEARED.** The same late list; after
/// Ctrl-U fzf shows its unfiltered order, `src/main.rs` beside the prompt
/// and `src/lib.rs` a row farther off. One row up, the query's `src/`
/// stands as twins of the old best match and `li` is not found: nothing
/// arrived there. No row two away is named for its own sake
/// (`rk::Engine::ribbon_rows_for`), so the list item two rows up is never
/// sampled beside the query, and the Ctrl-U carries nothing. Were it
/// sampled, the query's six glyphs would all stand there, never seen there
/// before, and the band would go onto the list item. GREEN on 0.93.0.
#[test]
fn an_fzf_list_re_sorted_as_the_query_is_cleared_is_not_where_the_query_went() {
    let full = "\x1b7\x1b[19;1H  tests/a.rs\x1b[K\x1b[20;1H  src/lib.rs\x1b[K\x1b[21;1H> src/main.rs\x1b[K\x1b8";
    let (followed, lit) = fzf_list_lands_late(2, false, Some(full));
    assert_eq!(
        (followed, lit),
        (0, 0),
        "fzf re-sorted under a cleared query: (followed, frames the best match's row was lit)"
    );
}

/// **FZF'S INFO LINE, OVER A REMOTE LINK.** fzf's default info line
/// (`  3/3`) stands between the list and the prompt, so the best match is
/// TWO rows above the query. The list landed after the query's first two
/// keys; Ctrl-U and the next query key `s` are pressed 60 ms apart and
/// their echoes arrive in ONE batch (the SSH round trip), with fzf's redraw
/// for the query `s` — the best match still `src/lib.rs`. The best match
/// two rows up is not a row the host names, and nothing is carried there;
/// named on every key's frame, it read "seen, no twin" and the band went
/// onto it, `(6, 77)`. GREEN on 0.93.0.
#[test]
fn an_fzf_best_match_two_rows_up_is_not_where_the_query_went_over_a_remote_link() {
    let q = "src/li";
    let mut h = Host::at_row(21);
    h.program(b"\x1b[22;1H> ");
    let list = "\x1b7\x1b[18;1H  tests/a.rs\x1b[K\x1b[19;1H  src/main.rs\x1b[K\x1b[20;1H> src/lib.rs\x1b[K\x1b[21;1H  3/3\x1b[K\x1b8";
    h.type_str(&q[..2]);
    h.program(list.as_bytes());
    h.type_with(&q[2..], list);
    h.idle(64);
    assert_eq!(h.live(21), (2..8).collect::<Vec<u16>>(), "fixture");
    let f0 = h.followed();
    h.now += Duration::from_millis(90);
    let t_kill = h.now;
    h.glow.note_kill(t_kill, true);
    h.glow
        .note_typed_cells(t_kill + Duration::from_millis(60), 1);
    h.now = t_kill + Duration::from_millis(120);
    h.term.process(format!("\r\x1b[2C\x1b[Ks{list}").as_bytes());
    h.frame();
    assert_eq!(
        h.verdict(f0, 19),
        (0, 0),
        "Ctrl-U and `s` in one batch: (followed, frames the best match's row was lit)"
    );
}

/// **A COMPLETION POPUP OPENED AFTER TWO KEYS, THEN CTRL-W.** An editor's
/// completion menu (an async LSP reply, or nvim-cmp's `keyword_length = 2`)
/// opens directly BELOW the word after its first two keys, its first item's
/// text at the word's column, refiltered each key. Ctrl-W in insert mode
/// deletes the word; the popup is still on glass on that frame (its close
/// is debounced) and gone on the next. Nothing moved onto the popup's row:
/// the popup stood beside the word `TWIN_MIN` and more, and its item is a
/// twin of every record it matches. GREEN on 0.93.0.
#[test]
fn a_completion_popup_opened_after_two_keys_is_not_where_a_ctrl_w_word_went() {
    let word = "println";
    let mut h = Host::at_row(5);
    h.program(b"\x1b[5;1Hfn main() {\x1b[6;1H    \x1b[7;1H}\x1b[6;5H");
    let menu = "\x1b7\x1b[7;5Hprintln!      Macro\x1b[K\x1b8";
    h.type_str(&word[..2]);
    h.type_with(&word[2..], menu);
    h.idle(48);
    assert_eq!(h.live(5), (4..11).collect::<Vec<u16>>(), "fixture");
    let f0 = h.followed();
    h.kill(b"\x1b[6;5H\x1b[K");
    h.program(b"\x1b7\x1b[7;1H}\x1b[K\x1b8");
    assert_eq!(
        h.verdict(f0, 6),
        (0, 0),
        "Ctrl-W under an open popup: (followed, frames the popup's row was lit)"
    );
}

/// **THE MECHANISM, BARE: A COPY OF THE LINE DRAWN BESIDE IT AFTER ITS FIRST
/// TWO KEYS.** A program writes `$ git status` on the row above the prompt
/// after the hand has typed `gi` (a status line that echoes the command, a
/// TUI's "last command" header), the hand types on, and the line editor's
/// redraw erases the line. Two records saw that row NOT hold their glyph
/// and learn the copy as a twin once it has stood `TWIN_MIN`; every later
/// one was born beside it, a twin at once. GREEN on 0.93.0.
#[test]
fn a_copy_of_the_line_drawn_after_its_first_two_keys_is_not_where_it_went() {
    let cmd = "git status";
    let mut h = Host::at_row(4);
    h.program(b"\r\n$ ");
    h.type_str(&cmd[..2]);
    h.program(format!("\x1b7\x1b[5;1H$ {cmd}\x1b8").as_bytes());
    h.type_str(&cmd[2..]);
    h.idle(64);
    assert_eq!(h.live(5), (2..12).collect::<Vec<u16>>(), "fixture");
    let f0 = h.followed();
    h.program(b"\r$ \x1b[K");
    assert_eq!(
        h.verdict(f0, 4),
        (0, 0),
        "the redraw's erase: (followed, frames the copy's row was lit)"
    );
}

/// **VIM'S `yyp`, THEN `S` ON THE ORIGINAL.** A statement typed on row 2;
/// Esc, `yyp` PUTS a copy under it (vim opens the line with `IL`, a band
/// move of the rows below, and writes the copy), `k` `S` clears the
/// original. The copy was put, not typed, and the original went nowhere.
/// The records were CLEAR one row down (they were armed over the blank
/// below the statement); the band move forgets what was seen of the rows it
/// moved, and a row it opened beside the line is not clear until a frame
/// shows it without the glyph (`Witness::translate_band`) — `yyp`'s copy
/// never is, however few frames sample it before `S`
/// (`tests/copies_are_not_moves.rs`). RED on 0.93.0: `(14, 89)`.
#[test]
fn vim_s_yyp_copy_is_not_where_the_original_went_when_s_clears_it() {
    let mut h = Host::at_row(3);
    h.program(
        b"\x1b[1;1Hfn f() {\x1b[2;1H    let a = 1;\x1b[3;1H    \x1b[4;1H}\x1b[5;1H~\x1b[6;1H~\x1b[3;5H",
    );
    h.type_str("let total = 0;");
    h.idle(64);
    assert_eq!(h.live(2), (4..18).collect::<Vec<u16>>(), "fixture");
    let f0 = h.followed();
    h.program(b"\x1b[3;18H");
    h.program(b"\x1b[4;1H\x1b[L    let total = 0;\x1b[4;5H");
    h.idle(32);
    h.program(b"\x1b[3;5H\x1b[K");
    assert_eq!(
        h.verdict(f0, 3),
        (0, 0),
        "`S` on the original: (followed, frames the put copy's row was lit)"
    );
}

/// **A STREAMED ROW QUOTING THE COMPOSER.** A composer under a streaming
/// region whose last line is rewritten with each chunk; one chunk quotes
/// the user's words back at the composer's own columns — one or two rows
/// above it — and 90 ms later the hand clears the composer with Ctrl-U. The
/// frames between are the fading ribbon's own 16 ms cadence. The composer's
/// text went nowhere. RED on 0.93.0: `(20, 80)` with the quote one row up.
#[test]
fn a_streamed_row_quoting_the_composer_is_not_where_the_composer_s_text_went() {
    let text = "please fix the tests";
    for gap in [1u16, 2] {
        let mut h = Host::at_row(20);
        let top = 20 - gap;
        h.program(b"\x1b[21;1H> ");
        let stream = |i: usize| {
            format!(
                "\x1b7\x1b[{};1H\x1b[2K  thinking {}\x1b8",
                top + 1,
                ".".repeat(i % 7)
            )
        };
        for (i, ch) in text.chars().enumerate() {
            h.type_with(&ch.to_string(), &stream(i));
        }
        h.program(stream(text.len()).as_bytes());
        assert_eq!(h.live(20), (2..22).collect::<Vec<u16>>(), "fixture");
        h.program(format!("\x1b7\x1b[{};1H\x1b[2K  {text}\x1b8", top + 1).as_bytes());
        h.idle(90);
        let f0 = h.followed();
        h.kill(b"\r\x1b[2C\x1b[K");
        assert_eq!(
            h.verdict(f0, top),
            (0, 0),
            "the quote {gap} row(s) up, then Ctrl-U: (followed, frames the quote's row was lit)"
        );
    }
}

/// A command line typed on the last row of a host's scrolling region, then
/// output that scrolls the region one row and, in the same batch, writes a
/// copy of the line on the new last row (a job-notice redraw that re-echoes
/// the buffer); 64 ms later the original is erased. Returns the verdict on
/// the copy's row.
fn copy_written_by_a_scroll(mut h: Host, last: u16) -> (u64, usize) {
    let cmd = "git status";
    h.program(b"$ ");
    h.type_str(cmd);
    h.idle(48);
    assert_eq!(h.live(last), (2..12).collect::<Vec<u16>>(), "fixture");
    let f0 = h.followed();
    h.program(format!("\r\n$ {cmd}").as_bytes());
    assert_eq!(
        h.live(last - 1),
        (2..12).collect::<Vec<u16>>(),
        "the scroll carried the band up with its line"
    );
    h.idle(48);
    h.program(format!("\x1b7\x1b[{};1H\x1b[K\x1b8", last).as_bytes());
    h.verdict(f0, last)
}

/// **A COPY WRITTEN ON THE NEW LAST ROW BY THE SCROLL THAT MADE IT.** A
/// command line typed on the grid's LAST row; output then scrolls the screen
/// one row and, in the same batch, writes a copy of the line on the new last
/// row (a job-notice redraw that re-echoes the buffer); a moment later the
/// original is erased. The line's records were armed with no row below them,
/// and the scroll made that row real. The scroll keeps what WAS seen, and the
/// copy that came in with the row it brought in is dated from the last walk
/// before the scroll (`Witness::translate` starts the offset's
/// `Seen::pending` clock at `Witness::last_walk`): the look 48 ms on finds
/// it has stood `TWIN_MIN`, a twin. RED on 0.93.0: `(10, 90)`.
#[test]
fn a_copy_written_on_the_new_last_row_by_a_scroll_is_not_where_the_line_went() {
    let last = (ROWS - 1) as u16;
    assert_eq!(
        copy_written_by_a_scroll(Host::at_row(last), last),
        (0, 0),
        "the original erased above its copy: (followed, frames the copy's row was lit)"
    );
}

/// **…AND IN A STACKED SPLIT'S TOP PANE.** The same, in a 12-row pane at the
/// top of the 24-row window: the rows the pane's scroll brings in are at the
/// PANE's bottom, row 11, not the window's (`Engine::translate_scroll` dates
/// the rows above the focused pane's bottom). Dated from the window's
/// bottom, row 11's copy was clocked only from its first look, still short
/// of `TWIN_MIN` when the original was erased, and the band went onto it:
/// `(10, 90)`.
#[test]
fn a_copy_written_on_a_split_pane_s_new_last_row_by_a_scroll_is_not_where_the_line_went() {
    let last = 11;
    assert_eq!(
        copy_written_by_a_scroll(Host::in_pane(12, last), last),
        (0, 0),
        "the original erased above its copy in the top pane: (followed, frames the copy's row \
         was lit)"
    );
}

/// **THE LIMIT: A COPY THAT STOOD LESS THAN `TWIN_MIN` IS A MOVE.** Two
/// shapes the accumulated evidence cannot tell from a real move, stated so
/// that a change to either is seen:
///
/// * a copy drawn 16 ms before the line is erased — seen on the one frame
///   before the erase, well short of the 40 ms of wall time
///   (`rk::witness::TWIN_MIN`) a copy must stand to be a twin — is, glyph
///   for glyph, a program that repaints without a synchronized-update
///   bracket drawing the text's new row a frame before it erases the old (a
///   torn repaint, `tests/moved_again_without_a_key.rs`, which must follow);
/// * a copy written in the SAME batch as the erase, onto a row the witness
///   had seen empty — the typed line scrolled up, the new last row blank for
///   a while, then a program writes the line there and erases the original
///   — is the line relocated one row down, which follows
///   (`a_line_relocated_twice_without_a_key_follows_both_times`).
///
/// Both follow here, as on 0.93.0.
#[test]
fn the_limit_a_copy_that_stood_less_than_twin_min_is_a_move() {
    // The quote one frame before the erase.
    let text = "please fix the tests";
    let mut h = Host::at_row(20);
    h.program(b"\x1b[21;1H> ");
    h.type_str(text);
    h.idle(64);
    h.program(format!("\x1b7\x1b[20;1H\x1b[2K  {text}\x1b8").as_bytes());
    let f0 = h.followed();
    h.kill(b"\r\x1b[2C\x1b[K");
    assert_eq!(
        h.followed() - f0,
        20,
        "a copy seen on one frame before the erase is a torn repaint's new row"
    );
    // The copy and the erase in one batch, onto a row seen empty.
    let cmd = "git status";
    let last = (ROWS - 1) as u16;
    let mut h = Host::at_row(last);
    h.program(b"$ ");
    h.type_str(cmd);
    h.idle(48);
    h.program(b"\r\n");
    h.idle(32);
    let f0 = h.followed();
    h.program(
        format!(
            "$ {cmd}\x1b[{};1H\x1b[K\x1b[{};{}H",
            last,
            last + 1,
            cmd.len() + 3
        )
        .as_bytes(),
    );
    assert_eq!(
        h.followed() - f0,
        10,
        "a copy written with the erase onto a row seen empty is the line moved"
    );
}
