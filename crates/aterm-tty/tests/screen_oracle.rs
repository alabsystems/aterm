// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests comparing the screen writer against `aterm_core::terminal::Terminal`.

use aterm_core::terminal::Terminal;
use aterm_tty::probe::Caps;
use aterm_tty::screen::{self, Attrs, Color, CursorShape, Pos, Screen, Style};
use aterm_tty::tty::Size;
use aterm_types::CursorStyle;
use aterm_types::text_shaping::AmbiguousWidth;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).expect("small")).expect("small")
    }

    fn below_u16(&mut self, n: u16) -> u16 {
        u16::try_from(self.below(usize::from(n))).expect("below a u16")
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const PIECES: &[&str] = &[
    "a",
    "b",
    "Z",
    "0",
    "~",
    " ",
    "hello",
    "中",
    "文",
    "字",
    "한",
    "😀",
    "👍",
    "👨\u{200D}👩\u{200D}👧",
    "❤\u{200D}🔥",
    "👍🏽",
    "\u{1F3FD}",
    "🇺🇸",
    "🇯🇵",
    "\u{1F1FA}",
    "\u{1F1F8}",
    "\u{231A}",
    "\u{231A}\u{FE0E}",
    "😀\u{FE0E}",
    "❤\u{FE0E}",
    "❤\u{FE0F}",
    "©\u{FE0F}",
    "1\u{FE0F}\u{20E3}",
    "\u{FE0E}",
    "\u{FE0F}",
    "e\u{301}",
    "\u{301}",
    "\u{200D}",
    "\u{200B}",
    "e\u{301}\u{302}\u{303}\u{304}\u{301}\u{302}\u{303}\u{304}\u{301}",
    "\u{2764}\u{301}\u{302}\u{303}\u{304}\u{301}\u{302}\u{303}\u{304}",
    "\u{301}\u{302}\u{303}\u{304}\u{301}\u{302}\u{303}\u{304}",
    "→",
    "─",
    "°",
    "α",
    "ක\u{DCA}\u{200D}ය",
    "न\u{94D}दी",
    "\t",
    "\x1b",
    "\u{85}",
    "\x7f",
];

const COLORS: &[Color] = &[
    Color::Default,
    Color::Default,
    Color::Indexed(1),
    Color::Indexed(4),
    Color::Indexed(9),
    Color::Indexed(200),
];

const SHAPES: &[CursorShape] = &[
    CursorShape::Default,
    CursorShape::BlinkingBlock,
    CursorShape::SteadyBlock,
    CursorShape::BlinkingUnderline,
    CursorShape::SteadyUnderline,
    CursorShape::BlinkingBar,
    CursorShape::SteadyBar,
];

fn style(rng: &mut Rng) -> Style {
    if rng.chance(40) {
        return Style::default();
    }
    let attrs = if rng.chance(50) {
        Attrs::NONE
    } else {
        Attrs::from_bits(u8::try_from(rng.below(64)).expect("6 bits"))
    };
    Style {
        fg: *rng.pick(COLORS),
        bg: *rng.pick(COLORS),
        attrs,
    }
}

fn text(rng: &mut Rng) -> String {
    let n = 1 + rng.below(6);
    (0..n).map(|_| *rng.pick(PIECES)).collect()
}

fn mutate(rng: &mut Rng, screen: &mut Screen) {
    let Size { cols, rows } = screen.size();
    let row = rng.below_u16(rows);
    match rng.below(100) {
        0..=54 => {
            let col = rng.below_u16(cols);
            let style = style(rng);
            screen.put(row, col, &text(rng), style);
        }
        55..=69 => {
            let from = rng.below_u16(cols);
            let to = from + 1 + rng.below_u16(cols - from);
            let style = style(rng);
            screen.fill(row, from, to, style);
        }
        70..=79 => {
            let from = rng.below_u16(cols);
            let to = from + 1 + rng.below_u16(cols - from);
            let style = style(rng);
            screen.set_style(row, from, to, style);
        }
        80..=81 => screen.clear(),
        82..=93 => {
            let cursor = rng.chance(80).then(|| Pos {
                row: rng.below_u16(rows),
                col: rng.below_u16(cols),
            });
            screen.set_cursor(cursor);
        }
        _ => screen.set_cursor_shape(*rng.pick(SHAPES)),
    }
}

fn terminal(size: Size, ambiguous: AmbiguousWidth) -> Terminal {
    let mut term = Terminal::new(size.rows, size.cols);
    term.apply_config(&aterm_core::config::TerminalConfig {
        ambiguous_width_double: ambiguous == AmbiguousWidth::Double,
        ..Default::default()
    });
    term
}

fn color(default: bool, indexed: bool, index: u8) -> Option<Color> {
    if default {
        Some(Color::Default)
    } else if indexed {
        Some(Color::Indexed(index))
    } else {
        None
    }
}

const FLAG_BITS: [(Attrs, u16); 6] = [
    (Attrs::BOLD, 1),
    (Attrs::DIM, 1 << 1),
    (Attrs::ITALIC, 1 << 2),
    (Attrs::UNDERLINE, 1 << 3),
    (Attrs::REVERSE, 1 << 5),
    (Attrs::STRIKE, 1 << 7),
];

fn mismatch(term: &Terminal, screen: &Screen) -> Option<String> {
    let Size { cols, rows } = screen.size();
    for row in 0..rows {
        let grid = term
            .display_row_grapheme_cells(usize::from(row))
            .expect("a row");
        for col in 0..cols {
            let cell = screen.cell(row, col).expect("in the frame");
            let (text, wide) = &grid[usize::from(col)];
            let at = format!("row {row} col {col}");
            if cell.is_continuation() {
                if !term.grid().is_wide_continuation_at(row, col) {
                    return Some(format!(
                        "{at}: expected a wide cell's right half, grid has {text:?}"
                    ));
                }
                continue;
            }
            let shown = if text.is_empty() { " " } else { text.as_str() };
            if shown != cell.grapheme() || *wide != (cell.width() == 2) {
                return Some(format!(
                    "{at}: expected {:?} width {}, grid has {shown:?} wide={wide}",
                    cell.grapheme(),
                    cell.width()
                ));
            }
            let g = term.grid().cell(row, col).expect("a grid cell");
            let flags = g.flags().0;
            let attrs = FLAG_BITS
                .iter()
                .filter(|(_, bit)| flags & bit != 0)
                .fold(Attrs::NONE, |all, (attr, _)| all.union(*attr));
            let colors = g.colors();
            let fg = color(
                colors.fg_is_default(),
                colors.fg_is_indexed(),
                colors.fg_index(),
            );
            let bg = color(
                colors.bg_is_default(),
                colors.bg_is_indexed(),
                colors.bg_index(),
            );
            let want = cell.style();
            if (Some(want.fg), Some(want.bg), want.attrs) != (fg, bg, attrs) {
                return Some(format!(
                    "{at} {:?}: expected {want:?}, grid has fg {fg:?} bg {bg:?} {attrs:?}",
                    cell.grapheme()
                ));
            }
        }
    }
    match screen.cursor() {
        Some(pos) => {
            let c = term.cursor();
            if !term.cursor_visible() || (c.row, c.col) != (pos.row, pos.col) {
                return Some(format!(
                    "cursor: expected {pos:?}, grid has ({}, {}) visible={}",
                    c.row,
                    c.col,
                    term.cursor_visible()
                ));
            }
        }
        None if term.cursor_visible() => return Some("cursor: expected hidden".to_owned()),
        None => {}
    }
    let shape = match screen.cursor_shape() {
        CursorShape::Default | CursorShape::BlinkingBlock => CursorStyle::BlinkingBlock,
        CursorShape::SteadyBlock => CursorStyle::SteadyBlock,
        CursorShape::BlinkingUnderline => CursorStyle::BlinkingUnderline,
        CursorShape::SteadyUnderline => CursorStyle::SteadyUnderline,
        CursorShape::BlinkingBar => CursorStyle::BlinkingBar,
        CursorShape::SteadyBar => CursorStyle::SteadyBar,
    };
    (term.cursor_style() != shape).then(|| {
        format!(
            "cursor shape: expected {shape:?}, grid has {:?}",
            term.cursor_style()
        )
    })
}

fn dirty_state(rng: &mut Rng, size: Size) -> Vec<u8> {
    let mut out = Vec::new();
    let margins = format!("\x1b[?69h\x1b[1;{}s", (size.cols - 1).max(2));
    for dirt in [
        "\x1b[1;7;31m",
        "\x1b(0",
        "\x1b)0\x0e",
        "\x1b[4h",
        "\x1b[?7l",
        "\x1b[?6h",
        "\x1b[2;3r",
        margins.as_str(),
    ] {
        if rng.chance(50) {
            out.extend_from_slice(dirt.as_bytes());
        }
    }
    out.extend_from_slice(b"\x1b[3;2H");
    if rng.chance(50) {
        out.extend_from_slice(b"\x1b#6");
    }
    out
}

fn rows_of(screen: &Screen) -> String {
    (0..screen.size().rows)
        .map(|row| format!("  {:?}\n", screen.row_text(row)))
        .collect()
}

const SEQUENCES: u64 = 50;
const FRAMES: usize = 100;

#[test]
fn diff_render_matches_terminal_oracle() {
    let mut frames = 0;
    for ambiguous in [AmbiguousWidth::Single, AmbiguousWidth::Double] {
        for seed in 0..SEQUENCES {
            let mut rng = Rng(seed);
            let size = Size {
                cols: 2 + rng.below_u16(39),
                rows: 3 + rng.below_u16(8),
            };
            let caps = Caps {
                sync_2026: rng.chance(50),
                ..Caps::default()
            };
            let mut term = terminal(size, ambiguous);
            let mut prev = Screen::new(size, ambiguous);
            for frame in 0..FRAMES {
                let mut next = prev.clone();
                for _ in 0..1 + rng.below(6) {
                    mutate(&mut rng, &mut next);
                }
                let bytes = if frame == 0 || rng.chance(3) {
                    term.process(&dirty_state(&mut rng, size));
                    screen::repaint(&next, &caps)
                } else {
                    screen::diff(&prev, &next, &caps)
                };
                term.process(&bytes);
                let _ = term.take_response();
                if let Some(wrong) = mismatch(&term, &next) {
                    panic!(
                        "{ambiguous:?} seed {seed} frame {frame} ({}x{}, {caps:?}): {wrong}\n\
                         bytes {:?}\nbefore:\n{}after:\n{}",
                        size.cols,
                        size.rows,
                        String::from_utf8_lossy(&bytes),
                        rows_of(&prev),
                        rows_of(&next)
                    );
                }
                let mut wrong = next.clone();
                let (row, col) = (rng.below_u16(size.rows), rng.below_u16(size.cols));
                wrong.put(row, col, "Q", Style::default());
                if wrong != next {
                    assert!(
                        mismatch(&term, &wrong).is_some(),
                        "{ambiguous:?} seed {seed} frame {frame}: a changed cell went unseen"
                    );
                }
                prev = next;
                frames += 1;
            }
        }
    }
    assert_eq!(frames, 10_000);
}

#[test]
fn single_cell_change_is_minimal() {
    let size = Size { cols: 80, rows: 24 };
    let plain = Caps::default();
    let sync = Caps {
        sync_2026: true,
        ..Caps::default()
    };
    let mut prev = Screen::new(size, AmbiguousWidth::Single);
    for row in 0..size.rows {
        prev.put(
            row,
            0,
            "The quick brown fox jumps over the lazy dog",
            Style::default(),
        );
    }
    let one = |prev: &Screen, text: &str, style: Style| {
        let mut next = prev.clone();
        next.put(5, 10, text, style);
        next
    };
    let bold_red = Style {
        fg: Color::Indexed(1),
        attrs: Attrs::BOLD,
        ..Style::default()
    };

    assert_eq!(screen::diff(&prev, &prev, &plain), b"");
    let next = one(&prev, "x", Style::default());
    assert_eq!(screen::diff(&prev, &next, &plain), b"\x1b[6;11Hx");
    let next = one(&prev, "x", bold_red);
    assert_eq!(
        screen::diff(&prev, &next, &plain),
        b"\x1b[6;11H\x1b[1;31mx\x1b[m"
    );
    let next = one(&prev, "中", Style::default());
    assert_eq!(
        screen::diff(&prev, &next, &plain),
        "\x1b[6;11H中".as_bytes()
    );
    let next = one(&prev, "x", Style::default());
    assert_eq!(
        screen::diff(&prev, &next, &sync),
        b"\x1b[?2026h\x1b[6;11Hx\x1b[?2026l"
    );

    prev.set_cursor(Some(Pos { row: 5, col: 10 }));
    let mut next = one(&prev, "x", Style::default());
    next.set_cursor(Some(Pos { row: 5, col: 11 }));
    assert_eq!(screen::diff(&prev, &next, &plain), b"\x1b[?25lx\x1b[?25h");
    assert_eq!(
        screen::diff(&prev, &next, &sync),
        b"\x1b[?2026hx\x1b[?2026l"
    );
    let mut elsewhere = prev.clone();
    elsewhere.put(20, 60, "x", Style::default());
    assert_eq!(
        screen::diff(&prev, &elsewhere, &sync),
        b"\x1b[?2026h\x1b[21;61Hx\x1b[6;11H\x1b[?2026l"
    );
    let mut moved = prev.clone();
    moved.set_cursor(Some(Pos { row: 5, col: 14 }));
    assert_eq!(screen::diff(&prev, &moved, &plain), b"\x1b[4C");
    moved.set_cursor(Some(Pos { row: 9, col: 0 }));
    assert_eq!(screen::diff(&prev, &moved, &plain), b"\x1b[10H");
    let mut shaped = prev.clone();
    shaped.set_cursor_shape(CursorShape::SteadyBar);
    assert_eq!(screen::diff(&prev, &shaped, &plain), b"\x1b[6 q");

    let longest_sgr = b"\x1b[0;1;2;3;4;7;9;38;5;200;48;5;200m".len();
    let mut rng = Rng(7);
    for _ in 0..1000 {
        let row = rng.below_u16(size.rows);
        let col = rng.below_u16(size.cols - 1);
        let mut next = prev.clone();
        let s = style(&mut rng);
        let piece = *rng.pick(&["x", "中", "é", "😀"]);
        next.put(row, col, piece, s);
        let bytes = screen::diff(&prev, &next, &plain);
        let budget = 6 + 8 + longest_sgr + piece.len() + 3 + 8 + 6;
        assert!(
            bytes.len() <= budget,
            "{piece:?} at {row},{col} in {s:?}: {} bytes {:?}",
            bytes.len(),
            String::from_utf8_lossy(&bytes)
        );
    }
}
