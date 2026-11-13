// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seat rule beside the brain, replayed in a host's order: bytes into a real terminal, the
//! effects pipeline's frame over it (the pet's world read from the frame's cells, then the
//! pet's tick, then the word pass), and the rule's answer read after each frame. Every frame
//! holds the shadow seat to the cells that frame drew.

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Rgb, Terminal};
use aterm_time::{Duration, Instant};

use super::{SeatArt, SeatShadow, SeatWire, WorldPage};
use crate::cursor_fx::TrailColor;
use crate::kitty_pet::{PetBrain, PetSense, PetSpecies};
use crate::pet_seat::{Body, Edge, Page, Path, Rule, Seat, Size, Why};
use crate::pet_world::{PetPane, PetWorldFacts};
use crate::pipeline::EffectsPipeline;

const ROWS: u16 = 14;
/// The trail gallery's cells (px).
const CELL: (u16, u16) = (14, 26);
const FRAME_MS: u64 = 16;
/// The cat, and the dog.
/// The pets the rule seats: each animal beside the rainbow theme.
const STYLES: [PetSpecies; 2] = [PetSpecies::Cat, PetSpecies::Dog];

/// A terminal of [`ROWS`] rows in the trail gallery's dark colours.
fn terminal(cols: u16) -> Terminal {
    let mut term = Terminal::new(ROWS, cols);
    let rgb = |c: u32| Rgb {
        r: (c >> 16) as u8,
        g: (c >> 8) as u8,
        b: c as u8,
    };
    term.set_default_foreground(rgb(0x00D0_D0D0));
    term.set_default_background(rgb(0x0011_1318));
    term.set_default_cursor_color(Some(rgb(0x0050_FA7B)));
    term
}

/// A terminal and the effects over it, a frame every 16 ms.
struct Desk {
    term: Terminal,
    fx: EffectsPipeline,
    grid: (u16, u16),
    ms: u64,
    /// Every answer that was not `Kept`, with the ms it came at.
    changes: Vec<(u64, SeatShadow)>,
    /// How many frames the rule was asked on, and how many of them had the seat on the page.
    asked: usize,
    on_page: usize,
}

impl Desk {
    fn new(style: PetSpecies, cols: u16) -> Self {
        Self::with_words(style, cols, true)
    }

    /// A desk with Sparkle Words on or off.
    fn with_words(style: PetSpecies, cols: u16, words: bool) -> Self {
        let term = terminal(cols);
        let mut fx = EffectsPipeline::new();
        fx.set_focused(true);
        fx.set_cursor_glow(
            true,
            "rainbow",
            TrailColor::Automatic,
            None,
            260,
            24,
            1.0,
            0.6,
            true,
            0x0050_FA7B,
        );
        fx.set_cursor_pet(true, 0x5EED);
        fx.set_cursor_pet_species(style);
        fx.set_sparkle_enabled(words);
        Self {
            term,
            fx,
            grid: (ROWS, cols),
            ms: 0,
            changes: Vec::new(),
            asked: 0,
            on_page: 0,
        }
    }

    /// The program's bytes, now.
    fn out(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.fx.now(),
            wall_ms: None,
        };
        self.term.process_at(bytes, at);
    }

    /// The pane is `rows` by `cols` cells from the next frame.
    fn resize(&mut self, rows: u16, cols: u16) {
        self.term.resize(rows, cols);
        self.grid = (rows, cols);
    }

    /// The seat's box: its rows (first, last) and its columns (first, end).
    fn boxed(&self, seat: Seat) -> Option<((u16, u16), (u16, u16))> {
        let Seat::On { row, col, small } = seat else {
            return None;
        };
        let body = self
            .fx
            .companion()
            .brain()
            .species()
            .seat_body(CELL.0, CELL.1);
        let size = if small {
            body.small.expect("a small seat of a body drawn small")
        } else {
            body.whole
        };
        Some((
            ((row + 1).saturating_sub(size.rows), row),
            (col, col + size.cols),
        ))
    }

    /// One frame, 16 ms on: the shadow seat covers no cell this frame draws a glyph in, not the
    /// caret's cell, and nothing past the pane.
    fn frame(&mut self) -> SeatShadow {
        self.fx.advance(FRAME_MS as f64);
        self.ms += FRAME_MS;
        let (rows, cols) = (usize::from(self.grid.0), usize::from(self.grid.1));
        let mut input: RenderInput = self.term.cell_frame(rows, cols);
        // Read before the effects draw into the frame.
        let glyph: Vec<Vec<bool>> = input
            .cells
            .iter()
            .map(|line| {
                line.iter()
                    .map(|c| c.ch != ' ' && c.ch != '\0' && !c.ch.is_control())
                    .collect()
            })
            .collect();
        let caret = (input.cursor_row, input.cursor_col);
        self.fx.apply(
            &mut self.term,
            &mut input,
            usize::from(CELL.0),
            usize::from(CELL.1),
        );
        let shadow = self.fx.companion().brain().seat_shadow();
        if !shadow.asked || shadow.unread {
            return shadow;
        }
        self.asked += 1;
        self.on_page += usize::from(shadow.seat.on_page());
        if let Some(((r0, r1), (c0, c1))) = self.boxed(shadow.seat) {
            assert!(
                usize::from(c1) <= cols && usize::from(r1) < rows,
                "{} ms: {shadow} is past the pane of {rows} by {cols}",
                self.ms
            );
            for r in usize::from(r0)..=usize::from(r1) {
                for c in usize::from(c0)..usize::from(c1) {
                    let lettered = glyph.get(r).and_then(|l| l.get(c)).copied() == Some(true);
                    assert!(
                        !lettered && (r, c) != caret,
                        "{} ms: {shadow} covers ({r}, {c}); caret {caret:?}",
                        self.ms
                    );
                }
            }
        }
        if shadow.why != Why::Kept {
            self.changes.push((self.ms, shadow));
        }
        shadow
    }

    fn rest(&mut self, ms: u64) -> SeatShadow {
        let mut last = self.frame();
        for _ in 1..ms / FRAME_MS {
            last = self.frame();
        }
        last
    }

    /// One key at the window, its echo, then `gap_ms` of frames.
    fn key(&mut self, c: char, gap_ms: u64) -> SeatShadow {
        let (rows, cols) = (usize::from(self.grid.0), usize::from(self.grid.1));
        let input = self.term.cell_frame(rows, cols);
        let _ = self.fx.note_committed_char(&mut self.term, &input, c);
        let mut b = [0u8; 4];
        self.out(c.encode_utf8(&mut b).as_bytes());
        self.rest(gap_ms)
    }

    fn caret(&mut self) -> (u16, u16) {
        let input = self
            .term
            .cell_frame(usize::from(self.grid.0), usize::from(self.grid.1));
        (input.cursor_row as u16, input.cursor_col as u16)
    }

    /// The changes as one line each, for a failure's message.
    fn told(&self) -> String {
        self.changes
            .iter()
            .map(|(ms, s)| format!("\n  {ms} {s}"))
            .collect()
    }
}

/// A shell's full page of `cols` columns: every row over the last lettered, the prompt on the
/// last row.
fn full_page(cols: u16) -> Vec<u8> {
    let line = "test result: ok. 3 passed; 0 failed; 0 ignored; finished in 0.42s";
    let mut page = String::from("\x1b[2J\x1b[H");
    for _ in 1..ROWS {
        page.push_str(&line[..line.len().min(usize::from(cols) - 1)]);
        page.push_str("\r\n");
    }
    page.push_str("$ ");
    page.into_bytes()
}

/// A pet that is neither the cat nor the dog: a wide, low animal with no small drawing.
struct Snake;

impl SeatArt for Snake {
    fn seat_art_cols(&self, _: u16, _: u16) -> f32 {
        8.4
    }

    fn seat_art_rows(&self) -> f32 {
        0.9
    }

    fn seat_small_fit(&self) -> f32 {
        0.0
    }
}

/// A blank page of a size.
struct Blank(u16, u16);

impl Page for Blank {
    fn rows(&self) -> u16 {
        self.0
    }

    fn cols(&self) -> u16 {
        self.1
    }

    fn lettered(&self, _: u16, _: u16) -> bool {
        false
    }
}

#[test]
fn a_pet_s_art_is_all_the_seat_rule_reads_of_it() {
    // The cat and the dog on the trail gallery's cells: six cells by two, three by one small.
    for species in [PetSpecies::Cat, PetSpecies::Dog] {
        let body = species.seat_body(CELL.0, CELL.1);
        assert_eq!(body.whole, Size { cols: 6, rows: 2 }, "{species:?}");
        assert_eq!(body.small, Some(Size { cols: 3, rows: 1 }), "{species:?}");
    }
    // Another pet gives its own three numbers and is seated by the same rule: its box, with no
    // small one, at the far end of the free lead on a blank page.
    let body = Snake.seat_body(CELL.0, CELL.1);
    assert_eq!(
        body,
        Body {
            whole: Size { cols: 9, rows: 1 },
            small: None
        }
    );
    let page = Blank(ROWS, 80);
    let scene = crate::pet_seat::Scene {
        page: &page,
        caret: (5, 10),
        body,
        ahead: 0,
        below_is_next: true,
        quiet_ms: 60_000,
        hold: false,
        new_page: false,
        scrolled: false,
    };
    let (seat, _) = crate::pet_seat::best(&Rule::DEFAULT, &scene);
    assert_eq!(
        seat,
        Seat::On {
            row: 5,
            col: 15,
            small: false
        }
    );
}

/// The rule's page is the pet's own reading of the frame's cells: a glyph is lettered, a blank
/// cell is not (beside the caret too, whose ring the brain's own laws protect), a rule drawn
/// across the pane is a fence, and a page of another grid than the frame's is no page.
#[test]
fn the_pet_s_world_is_the_rule_s_page() {
    let mut desk = Desk::new(STYLES[0], 40);
    desk.out("\x1b[2J\x1b[Hab cd\r\n".as_bytes());
    desk.out("\u{2500}".repeat(40).as_bytes());
    desk.out(b"\r\n$ ");
    let _ = desk.frame();
    let brain = desk.fx.companion().brain();
    let world = brain.console.world.as_deref().expect("the frame read it");
    let (page, stamp) = WorldPage::read(ROWS, 40, world).expect("a page of the frame's grid");
    assert_eq!((page.rows(), page.cols()), (ROWS, 40));
    assert!(!stamp.surface.alt_screen);
    assert!(page.lettered(0, 0) && page.lettered(0, 1) && !page.lettered(0, 2));
    assert!(page.lettered(0, 3) && !page.lettered(0, 5) && !page.lettered(5, 20));
    // The prompt's row: `$`, a blank, the caret's blank cell and the blank cells about it.
    assert!(page.lettered(2, 0) && !page.lettered(2, 1));
    assert!(!page.lettered(2, 2) && !page.lettered(2, 3) && !page.lettered(3, 2));
    // The rule's row is lettered and a fence; no other row is one.
    assert!(page.lettered(1, 0) && page.lettered(1, 39));
    assert!(page.fence(1) && !page.fence(0) && !page.fence(2));
    // Another grid than the world read: no page.
    assert!(WorldPage::read(ROWS, 41, world).is_none());
    assert!(WorldPage::read(ROWS + 1, 40, world).is_none());
}

#[test]
fn the_shadow_is_one_word_a_status_line_can_carry() {
    assert_eq!(SeatShadow::NONE.to_string(), "none");
    let on = SeatShadow {
        seat: Seat::On {
            row: 10,
            col: 15,
            small: false,
        },
        why: Why::GaveWay,
        path: Path::Walk,
        ahead: 3,
        quiet_ms: 0,
        hold: true,
        caret_clamped: true,
        unread: false,
        asked: true,
    };
    assert_eq!(on.to_string(), "r10c15w/gave-way/walk/a3/q0/held/clamped");
    let off = SeatShadow {
        seat: Seat::Off(Edge::Right),
        why: Why::Yielded,
        path: Path::Cut,
        ahead: 0,
        quiet_ms: 1504,
        hold: false,
        caret_clamped: false,
        unread: true,
        asked: true,
    };
    assert_eq!(off.to_string(), "off-right/yielded/cut/a0/q1504/unread");
    assert!(!on.to_string().contains(' ') && !off.to_string().contains(' '));
}

#[test]
fn a_writing_hand_is_three_keys_ahead_of_its_caret() {
    let rule = Rule::DEFAULT;
    let mut wire = SeatWire::new();
    // No run open: nothing ahead.
    assert_eq!(wire.hand_ahead(&rule), 0);
    // A key: three keys ahead, inside the rule's free lead.
    wire.since_step_ms = 0;
    assert_eq!(wire.hand_ahead(&rule), 3);
    assert!(wire.hand_ahead(&rule) < rule.lead);
    // Wide glyphs step two cells: three keys are six cells. The latest step says how wide.
    wire.step_cells = 2;
    assert_eq!(wire.hand_ahead(&rule), 6);
    wire.step_cells = 1;
    assert_eq!(wire.hand_ahead(&rule), 3);
    // 300 ms after the last step the hand is still writing; later it rests.
    wire.since_step_ms = 300;
    assert_eq!(wire.hand_ahead(&rule), 3);
    wire.since_step_ms = 301;
    assert_eq!(wire.hand_ahead(&rule), 0);
    // A step is one or two cells along the row, or the wrap; nothing else.
    assert_eq!(SeatWire::hand_step((4, 10), (4, 11), 80), Some(1));
    assert_eq!(SeatWire::hand_step((4, 10), (4, 12), 80), Some(2));
    assert_eq!(SeatWire::hand_step((4, 10), (4, 13), 80), None);
    assert_eq!(SeatWire::hand_step((4, 10), (4, 9), 80), None);
    assert_eq!(SeatWire::hand_step((4, 79), (5, 0), 80), Some(1));
    assert_eq!(SeatWire::hand_step((13, 79), (13, 0), 80), Some(1));
    assert_eq!(SeatWire::hand_step((4, 40), (5, 0), 80), None);
}

/// A typed line on a blank page, for the cat and the dog: the shadow comes on from an edge on
/// its feet, keeps ahead of the hand by walks along the row, is never on a letter or the caret
/// (held on every frame by [`Desk::frame`]), and rests inside the free lead past the caret.
#[test]
fn beside_a_typed_line_the_shadow_walks_ahead_and_rests_inside_the_lead() {
    for style in STYLES {
        let mut desk = Desk::new(style, 80);
        desk.out(b"\x1b[2J\x1b[6H$ ");
        let came = desk.rest(2_000);
        assert!(came.seat.on_page(), "{style:?}: {came}");
        let (_, first) = desk.changes[0];
        assert_eq!(first.why, Why::Returned, "{style:?}");
        assert!(matches!(first.path, Path::Enter(_)), "{style:?}: {first}");
        desk.changes.clear();
        for c in "echo the quick brown fox".chars() {
            let _ = desk.key(c, 48);
        }
        assert!(
            desk.changes.iter().all(|(_, s)| matches!(
                s.seat,
                Seat::On {
                    row: 5,
                    small: false,
                    ..
                }
            ) && matches!(s.why, Why::GaveWay | Why::Moved)
                && s.path == Path::Walk),
            "{style:?}: typing sends it on along its row by walks alone:{}",
            desk.told()
        );
        let gave_way = desk
            .changes
            .iter()
            .filter(|(_, s)| s.why == Why::GaveWay)
            .count();
        assert!(
            (6..=13).contains(&gave_way),
            "{style:?}: a seat or two at a step, not one a key: {gave_way} for 24 keys"
        );
        // It rests where the writing left it, inside the free lead: no seat there is better
        // than another by the margin, so a pause pulls it nowhere.
        let rest = desk.rest(2_000);
        let caret = desk.caret();
        let Seat::On {
            row: 5,
            col,
            small: false,
        } = rest.seat
        else {
            panic!("{style:?}: {rest}");
        };
        assert!(
            col > caret.1 && col <= caret.1 + 1 + Rule::DEFAULT.lead,
            "{style:?}: {rest}, caret at {}",
            caret.1
        );
        assert_eq!((rest.ahead, rest.why), (0, Why::Kept), "{style:?}");
        assert!(rest.quiet_ms >= 1_900, "{style:?}: {rest}");
    }
}

/// KI-1107-4's page: a line typed to the right wall of a full page's last row. The shadow
/// leaves the typed row before the words reach it, on its feet or by a leap, never by a cut and
/// never for a letter under it; where the wall leaves it no seat it walks off through the edge
/// and walks back in once the line is entered and the rows about the caret have been quiet.
#[test]
fn at_the_wall_the_shadow_is_never_cut_and_never_under_a_letter() {
    for style in STYLES {
        for cols in [70u16, 71] {
            let mut desk = Desk::new(style, cols);
            desk.out(&full_page(cols));
            let _ = desk.rest(2_000);
            desk.changes.clear();
            for _ in 2..cols - 1 {
                let _ = desk.key('x', 48);
            }
            assert!(
                desk.changes
                    .iter()
                    .all(|(_, s)| s.why != Why::Yielded && s.path != Path::Cut),
                "{style:?} at {cols}:{}",
                desk.told()
            );
            let was_off = !desk.fx.companion().brain().seat_shadow().seat.on_page();
            if let Some((_, left)) = desk.changes.iter().find(|(_, s)| !s.seat.on_page()) {
                assert!(
                    matches!(left.path, Path::Exit(_)) && left.why == Why::GaveWay,
                    "{style:?} at {cols}: {left}"
                );
            }
            // Enter: the page scrolls one row and the prompt is back at the row's start.
            desk.changes.clear();
            desk.out(b"\r\n$ ");
            let entered = desk.ms;
            let back = desk.rest(2_400);
            assert!(back.seat.on_page(), "{style:?} at {cols}: {back}");
            assert!(
                desk.changes
                    .iter()
                    .all(|(_, s)| s.why != Why::Yielded && s.path != Path::Cut),
                "{style:?} at {cols}:{}",
                desk.told()
            );
            if was_off {
                let (at, came) = desk.changes[0];
                assert!(
                    came.why == Why::Returned
                        && matches!(came.path, Path::Enter(_))
                        && at - entered >= 1_500,
                    "{style:?} at {cols}: back at {at} ms, entered at {entered}: {came}"
                );
            }
        }
    }
}

/// A SCROLL UNDER A CARET THAT STAYED LEAVES THE SEAT BESIDE IT. A bare Enter at a full page's
/// last row scrolls every row up one and the prompt is on the last row again: the seat is
/// where it was, beside the caret, and it was no change of seat.
#[test]
fn a_scroll_under_a_caret_that_stayed_leaves_the_seat_beside_it() {
    let mut desk = Desk::new(STYLES[0], 80);
    desk.out(&full_page(80));
    let seated = desk.rest(3_000);
    assert!(
        matches!(seated.seat, Seat::On { row, .. } if row == ROWS - 1),
        "{seated}"
    );
    desk.changes.clear();
    for _ in 0..3 {
        desk.out(b"\r\n$ ");
        let after = desk.frame();
        assert_eq!((after.seat, after.why), (seated.seat, Why::Kept), "{after}");
        assert_eq!(after.quiet_ms, 0, "a scroll is no quiet");
        let _ = desk.rest(160);
    }
    assert!(desk.changes.is_empty(), "{}", desk.told());
}

/// The trail film's `long-build`: a program prints a line every 50 ms on a full page's last
/// row (20 lines a second, slower than most builds). The seat was carried up with the page at
/// each line, the line was printed under it, and it leapt back to the seat it had: 11 leaps in
/// place in the film, one a line; and four lines inside one dwell filled the rule's four bars
/// and sent the pet off the glass. Beside a caret that stays, the seat stays: no change of seat
/// for a line of output, and the pet is on the glass throughout.
#[test]
fn a_build_printing_under_the_pet_does_not_move_it_a_line() {
    for style in STYLES {
        let mut desk = Desk::new(style, 72);
        desk.out(&full_page(72));
        let seated = desk.rest(3_000);
        assert!(seated.seat.on_page(), "{style:?}: {seated}");
        desk.out(b"cargo build\r\n");
        let _ = desk.rest(320);
        desk.changes.clear();
        let (asked, on_page) = (desk.asked, desk.on_page);
        for k in 0..60 {
            let line =
                format!("   Compiling aterm-crate-{k:02} v0.{k}.0 (crates/aterm-crate-{k:02})\r\n");
            desk.out(line.as_bytes());
            let _ = desk.rest(48);
        }
        let lines = 60;
        assert_eq!(
            desk.asked - asked,
            desk.on_page - on_page,
            "{style:?}: off the glass while the build printed:{}",
            desk.told()
        );
        assert!(
            desk.changes
                .iter()
                .all(|(_, s)| s.why != Why::Yielded && s.path != Path::Cut),
            "{style:?}:{}",
            desk.told()
        );
        assert!(
            desk.changes.len() <= 6,
            "{style:?}: {} changes of seat for {lines} lines:{}",
            desk.changes.len(),
            desk.told()
        );
        let leaps = desk
            .changes
            .iter()
            .filter(|(_, s)| s.path == Path::Leap)
            .count();
        assert!(leaps <= 2, "{style:?}: {leaps} leaps:{}", desk.told());
    }
}

/// ANOTHER SCREEN IS MET ON ITS OWN FRAME. A pager takes the screen under a seated pet: the
/// frame that first draws the other screen asks the rule on that screen's page, told it is
/// another page. The pet is on the new page's best seat that frame or keeps a seat that is
/// still one; no letter landed on it, so it is no yield and no strike, and it is not sent off
/// the glass for 1.5 s. Leaving the pager is the same.
#[test]
fn another_screen_is_another_page_on_the_frame_that_draws_it() {
    for style in STYLES {
        let mut desk = Desk::new(style, 80);
        desk.out(b"\x1b[2J\x1b[6H$ ");
        let seated = desk.rest(3_000);
        assert!(seated.seat.on_page(), "{style:?}: {seated}");
        desk.changes.clear();
        // The pager: every row lettered to the margin but the last, the caret on the last.
        let mut pager = String::from("\x1b[?1049h\x1b[2J\x1b[H");
        for _ in 1..ROWS {
            pager.push_str(&"m".repeat(80));
        }
        pager.push(':');
        desk.out(pager.as_bytes());
        let switched = desk.frame();
        assert_eq!(switched.why, Why::Switched, "{style:?}: {switched}");
        let _ = desk.rest(800);
        // And back.
        desk.out(b"\x1b[?1049l");
        let back = desk.frame();
        assert!(
            matches!(back.why, Why::Switched | Why::Kept),
            "{style:?}: {back}"
        );
        let rest = desk.rest(800);
        assert!(rest.seat.on_page(), "{style:?}: {rest}");
        assert!(
            desk.changes.iter().all(|(_, s)| s.why != Why::Yielded),
            "{style:?}:{}",
            desk.told()
        );
    }
}

/// A RESIZE IS MET ON ITS OWN FRAME. A window dragged narrower and wider, a column a frame,
/// over a full page: each frame's page is the reflowed one and the rule is told it is another
/// page. No frame has the pet on a letter of the grid it is drawn on (held by
/// [`Desk::frame`]), none is a yield, and the pet is on the glass when the drag ends: a letter
/// "landing" at each step would have been a strike a step, 12 s off the glass at the cap.
#[test]
fn a_drag_resize_is_another_page_each_step_and_never_a_letter_under_the_pet() {
    for style in STYLES {
        let mut desk = Desk::new(style, 80);
        desk.out(&full_page(80));
        let seated = desk.rest(3_000);
        assert!(seated.seat.on_page(), "{style:?}: {seated}");
        desk.changes.clear();
        for cols in (60..80).rev().chain(60..=90) {
            desk.resize(ROWS, cols);
            let _ = desk.frame();
        }
        for rows in [ROWS + 4, ROWS - 3, ROWS] {
            desk.resize(rows, 90);
            let _ = desk.frame();
        }
        assert!(
            desk.changes.iter().all(|(_, s)| s.why != Why::Yielded),
            "{style:?}:{}",
            desk.told()
        );
        let rest = desk.rest(600);
        assert!(rest.seat.on_page(), "{style:?}: {rest}:{}", desk.told());
    }
}

/// THE PET'S PAGE IS ITS OWN. With Sparkle Words off the rule reads the same page: on a full
/// page the shadow is never on a letter (held on every frame), comes on the glass and answers
/// as it does with the words on. Read from the word pass's ink probe, the page was blank with
/// the words off and the pet was seated on letters.
#[test]
fn with_sparkle_words_off_the_rule_reads_the_same_page() {
    let run = |words: bool| {
        let mut desk = Desk::with_words(STYLES[0], 72, words);
        desk.out(&full_page(72));
        let seated = desk.rest(3_000);
        assert!(seated.seat.on_page(), "words {words}: {seated}");
        for c in "git status".chars() {
            let _ = desk.key(c, 64);
        }
        desk.out(b"\r\n M crates/aterm-effects/src/pet_seat.rs\r\n$ ");
        let _ = desk.rest(2_400);
        assert!(desk.asked > 300 && desk.on_page * 10 >= desk.asked * 9);
        desk.changes
    };
    let (on, off) = (run(true), run(false));
    assert!(!off.is_empty());
    assert_eq!(
        on.iter().map(|(ms, s)| (*ms, s.seat)).collect::<Vec<_>>(),
        off.iter().map(|(ms, s)| (*ms, s.seat)).collect::<Vec<_>>()
    );
}

/// A shell's grey suggestion printed past the caret, on the seat: the pet goes on past its end
/// that frame, stays on the glass while the line is typed into it, and is beside the caret
/// again when the suggestion is taken.
#[test]
fn a_suggestion_past_the_caret_never_takes_the_shadow_off_the_glass() {
    for style in STYLES {
        let mut desk = Desk::new(style, 72);
        desk.out(&full_page(72));
        let seated = desk.rest(3_000);
        assert!(seated.seat.on_page(), "{style:?}: {seated}");
        desk.changes.clear();
        let (asked, on_page) = (desk.asked, desk.on_page);
        let line = "git commit -m 'quick brown fox jumps'";
        // The first key, and fish's suggestion in grey after it, the caret put back.
        let mut typed = 0;
        for c in "git c".chars() {
            typed += 1;
            let rest = &line[typed..];
            let _ = desk.key(c, 0);
            desk.out(format!("\x1b[90m{rest}\x1b[0m\x1b[{}D", rest.len()).as_bytes());
            let _ = desk.rest(96);
        }
        let shown = desk.rest(600);
        let end = 2 + line.len() as u16;
        assert!(
            matches!(shown.seat, Seat::On { row, col, .. } if row == ROWS - 1 && col >= end),
            "{style:?}: {shown}, the suggestion ends at {end}"
        );
        // The suggestion is taken: the caret is at the line's end.
        desk.out(format!("\x1b[{}C", line.len() - typed).as_bytes());
        let _ = desk.rest(600);
        assert_eq!(
            desk.asked - asked,
            desk.on_page - on_page,
            "{style:?}: off the glass for a suggestion:{}",
            desk.told()
        );
        assert!(
            desk.changes
                .iter()
                .all(|(_, s)| s.why != Why::Yielded && s.path != Path::Cut),
            "{style:?}:{}",
            desk.told()
        );
    }
}

/// Claude Code's input box: a rule over the typed row and one under it, the program's footer
/// under that. The shadow keeps to the typed row, drawn small past the caret, while a line is
/// typed and after; it is never seated in the footer.
#[test]
fn in_an_input_box_the_shadow_keeps_to_the_typed_row() {
    for style in STYLES {
        let mut desk = Desk::new(style, 72);
        let rule = "\u{2500}".repeat(72);
        desk.out(
            format!(
                "\x1b[2J\x1b[5H  this box. Type a line into either to judge the trail on it.\
                 \x1b[7H* Worked for 42s\x1b[10H{rule}\x1b[12H{rule}\x1b[13H  ? for shortcuts\
                 \x1b[14H  >> accept edits on\x1b[11H> "
            )
            .as_bytes(),
        );
        let seated = desk.rest(3_000);
        assert!(
            matches!(
                seated.seat,
                Seat::On {
                    row: 10,
                    small: true,
                    ..
                }
            ),
            "{style:?}: {seated}"
        );
        desk.changes.clear();
        let (asked, on_page) = (desk.asked, desk.on_page);
        for c in "echo the quick brown fox jumps".chars() {
            let _ = desk.key(c, 64);
        }
        let rest = desk.rest(2_000);
        let caret = desk.caret();
        assert!(
            matches!(rest.seat, Seat::On { row: 10, col, small: true }
                if col > caret.1 && col <= caret.1 + 1 + Rule::DEFAULT.lead),
            "{style:?}: {rest}, caret {caret:?}"
        );
        assert!(
            desk.changes
                .iter()
                .all(|(_, s)| matches!(s.seat, Seat::On { row: 10, .. }) && s.path == Path::Walk),
            "{style:?}:{}",
            desk.told()
        );
        assert_eq!(desk.asked - asked, desk.on_page - on_page, "{style:?}");
    }
}

/// A caret a frame shows past the last column (a line typed to its end, the wrap pending) is
/// read at the last column, and the shadow says so.
#[test]
fn a_caret_past_the_last_column_is_read_at_the_last() {
    let mut brain = Brain::new(40);
    brain.out(b"\x1b[2J\x1b[6H$ ");
    let _ = brain.frame(16);
    brain.caret = Some((5, 40));
    let shadow = brain.frame(16);
    assert!(shadow.caret_clamped, "{shadow}");
    brain.caret = Some((5, 12));
    let shadow = brain.frame(16);
    assert!(!shadow.caret_clamped, "{shadow}");
}

/// A brain alone over a terminal, in a host's order: the world read from the frame's cells,
/// then the tick. The caret is the terminal's unless a test names one.
struct Brain {
    term: Terminal,
    pet: PetBrain,
    now: Instant,
    cols: u16,
    caret: Option<(u16, u16)>,
    /// Whether the next frames read the page for the pet.
    reads: bool,
}

impl Brain {
    fn new(cols: u16) -> Self {
        Self {
            term: terminal(cols),
            pet: PetBrain::new(),
            now: Instant::now(),
            cols,
            caret: None,
            reads: true,
        }
    }

    fn out(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
    }

    /// A frame `ms` after the last, of a pane `rows` tall.
    fn frame_of(&mut self, ms: u64, rows: u16) -> SeatShadow {
        self.now += Duration::from_millis(ms);
        let input = self
            .term
            .cell_frame(usize::from(rows), usize::from(self.cols));
        if self.reads {
            let facts = PetWorldFacts::read(&self.term, 7);
            self.pet
                .observe_console(&input, &facts, PetPane::full(&input));
        }
        let caret = self
            .caret
            .unwrap_or((input.cursor_row as u16, input.cursor_col as u16));
        let _ = self.pet.tick(PetSense {
            caret_drawn: true,
            now: self.now,
            caret: Some(caret),
            wrapped: false,
            rows,
            cols: self.cols,
            cell_w: CELL.0,
            cell_h: CELL.1,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        });
        self.pet.seat_shadow()
    }

    fn frame(&mut self, ms: u64) -> SeatShadow {
        self.frame_of(ms, ROWS)
    }
}

/// NO PAGE, NO ASKING. A brain no host reads a page for has no pet seated by the rule. A frame
/// of another grid than the page last read asks nothing: the seat stands, the shadow says
/// `unread`, no clock of the rule's asks for a wake, and the next page read is another page,
/// so letters found under the seat then are no letter's landing.
#[test]
fn a_frame_with_no_page_asks_nothing_and_the_next_page_is_another_page() {
    let mut brain = Brain::new(60);
    brain.reads = false;
    brain.out(b"\x1b[2J\x1b[6H$ ");
    for _ in 0..40 {
        let shadow = brain.frame(16);
        assert!(!shadow.asked, "{shadow}");
        assert_eq!(shadow.to_string(), "none");
    }
    assert!(brain.pet.seat_due().is_none());
    brain.reads = true;
    let mut seated = brain.frame(16);
    for _ in 0..80 {
        seated = brain.frame(16);
    }
    assert!(seated.asked && seated.seat.on_page(), "{seated}");
    // The pane grows four rows and the host's frame is of the new grid, read by nobody yet.
    brain.reads = false;
    brain.term.resize(ROWS + 4, 60);
    let unread = brain.frame_of(16, ROWS + 4);
    assert!(unread.unread && unread.seat == seated.seat, "{unread}");
    assert_eq!(unread.why, Why::Kept);
    assert!(unread.to_string().ends_with("/unread"), "{unread}");
    assert!(brain.pet.seat_due().is_none());
    // The page is read again, with a line printed across the seat meanwhile.
    let Seat::On { row, .. } = seated.seat else {
        unreachable!()
    };
    brain.out(format!("\x1b7\x1b[{};1H{}\x1b8", row + 1, "w".repeat(58)).as_bytes());
    brain.reads = true;
    let read = brain.frame_of(16, ROWS + 4);
    assert!(!read.unread, "{read}");
    assert_eq!(read.why, Why::Switched, "{read}");
}

/// THE RULE'S CLOCKS NAME THEIR OWN DEADLINE, for a host that draws on demand. Ticked only when
/// the rule's deadline says, a pet off the glass comes on and rests; every deadline is after
/// the asking that set it, and once the pet rests the rule names none.
#[test]
fn a_host_that_draws_on_demand_is_woken_for_the_rule_s_clocks() {
    let mut brain = Brain::new(60);
    brain.out(b"\x1b[2J\x1b[6H$ ");
    let first = brain.frame(16);
    assert!(first.asked && !first.seat.on_page(), "{first}");
    let mut wakes = 0;
    let mut came = None;
    while let Some(due) = brain.pet.seat_due() {
        assert!(due > brain.now, "a deadline at or before its own asking");
        let ms = due.saturating_duration_since(brain.now).as_millis() as u64;
        let shadow = brain.frame(ms);
        if shadow.why == Why::Returned {
            came = Some(shadow);
        }
        wakes += 1;
        assert!(wakes <= 8, "the rule keeps asking for wakes: {shadow}");
    }
    let came = came.expect("woken, it came on the glass");
    assert!(matches!(came.path, Path::Enter(_)), "{came}");
    // It is on the typed row: at the end of the free lead, or where it came in while the
    // brain holds it there (asleep), whose waking is the brain's own frame.
    let rest = brain.pet.seat_shadow();
    assert!(
        matches!(rest.seat, Seat::On { row: 5, col, small: false }
            if rest.hold || col == 3 + Rule::DEFAULT.lead),
        "{rest}"
    );
    // A page that changes nowhere near the caret sets no clock.
    let still = brain.frame(1_000);
    assert_eq!(still.why, Why::Kept);
    assert!(brain.pet.seat_due().is_none());
}
