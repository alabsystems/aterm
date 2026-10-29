// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Restore paints carry real edge wraps under margins, origin mode and saved cursors.

use super::Terminal;
use aterm_types::{CharacterSetState, TerminalModes};

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    rows: Vec<String>,
    cursor: (u16, u16, bool),
    modes: TerminalModes,
    region: aterm_grid::ScrollRegion,
    margins: aterm_grid::HorizontalMargins,
    charset: CharacterSetState,
    pen: String,
    protected: bool,
}

fn observe(t: &Terminal) -> Observation {
    let c = t.cursor();
    Observation {
        rows: (0..t.rows())
            .map(|r| t.grid().row_ansi_text_screen(r).unwrap_or_default())
            .collect(),
        cursor: (c.row, c.col, t.grid().pending_wrap()),
        modes: *t.modes(),
        region: t.grid().scroll_region(),
        margins: t.grid().horizontal_margins(),
        charset: t.charset,
        pen: super::super::handler_decrqss::sgr_of(&t.style, t.transient.current_underline_color),
        protected: t.style.protected,
    }
}

struct Case {
    name: &'static str,
    input: &'static str,
    cursor: (u16, u16, bool),
    next: &'static str,
}

const CASES: &[Case] = &[
    Case {
        name: "left and right margins",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd",
        cursor: (0, 4, true),
        next: "X",
    },
    Case {
        name: "origin mode in a scroll region",
        input: "\x1b[2;5r\x1b[?6h\x1b[1;1H0123456789",
        cursor: (1, 9, true),
        next: "X",
    },
    Case {
        name: "origin mode and horizontal margins",
        input: "\x1b[?69h\x1b[2;7s\x1b[2;5r\x1b[?6h\x1b[1;1Habcdef",
        cursor: (1, 6, true),
        next: "X",
    },
    Case {
        name: "origin wrap at the bottom of the scroll region",
        input: "TOP\x1b[6;1HBOTTOM\x1b[2;5r\x1b[?6h\x1b[4;1H0123456789",
        cursor: (4, 9, true),
        next: "X",
    },
    Case {
        name: "wide glyph and colours at the margin",
        input: "L\x1b[1;8HRGT\x1b[2;1HNEIGHBOR\x1b[?69h\x1b[2;7s\x1b[1;2H\x1b[31mab\x1b[44mcd\x1b[38;2;1;2;3m中",
        cursor: (0, 6, true),
        next: "\x1b[4:3;58:2::4:5:6mZ",
    },
    Case {
        name: "a shifted line-drawing character set",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2H\x1b)0\x0eqqqq",
        cursor: (0, 4, true),
        next: "q",
    },
    Case {
        name: "an opaque 94-set under a 96-set override",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b*0\x1b.A",
        cursor: (0, 4, true),
        next: "\x1bnA",
    },
    Case {
        name: "protection enabled after the wrap was armed",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b[1\"q",
        cursor: (0, 4, true),
        next: "X\x1b[?2K",
    },
    Case {
        name: "a saved protected pen before the current pen is unprotected",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b[1\"q\x1b7\x1b[0\"q\x1b[3;1H.",
        cursor: (2, 1, false),
        next: "\x1b8X\x1b[?2K",
    },
    Case {
        name: "a protected main save does not protect the alternate-screen paint",
        input: "0123456789\x1b[1\"q\x1b[?1049h\x1b[0\"q\x1b[1;1Halt",
        cursor: (0, 3, false),
        next: "\x1b[?2K",
    },
    Case {
        name: "saved origin wrap",
        input: "\x1b[2;5r\x1b[?6h\x1b[1;1H0123456789\x1b7\x1b[?6l\x1b[6;1H.",
        cursor: (5, 1, false),
        next: "\x1b8X",
    },
    Case {
        name: "a saved origin coordinate survives moving and resetting the scroll region",
        input: "\x1b[2;5r\x1b[?6h\x1b[1;1H0123456789\x1b7\x1b[?6l\x1b[3;5r\x1b[6;1H.",
        cursor: (5, 1, false),
        next: "\x1b[r\x1b8X",
    },
    Case {
        name: "saved horizontal-margin wrap",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b7\x1b[3;1H.",
        cursor: (2, 1, false),
        next: "\x1b8X",
    },
    Case {
        name: "saved wrap whose horizontal margins moved aside",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b7\x1b[?69l\x1b[3;1H.",
        cursor: (2, 1, false),
        next: "\x1b8X",
    },
    Case {
        name: "active wrap whose horizontal margins moved aside",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b[?69l",
        cursor: (0, 4, true),
        next: "X",
    },
    Case {
        name: "an armed wrap while auto-wrap is temporarily off",
        input: "\x1b[2;5r\x1b[?6h\x1b[1;1H0123456789\x1b[?7l",
        cursor: (1, 9, true),
        next: "\x1b[?7hX",
    },
    Case {
        name: "a saved origin and margin wrap with its pen and character set",
        input: "\x1b[?69h\x1b[2;7s\x1b[2;5r\x1b[?6h\x1b[1;1H\x1b[32;44m\x1b)0\x0eqqqqqq\x1b7\x1b[?6l\x0f\x1b[m\x1b[6;1H.",
        cursor: (5, 1, false),
        next: "\x1b8q",
    },
    Case {
        name: "an active DECRC wrap short of today's right margin",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b7\x1b[2;8s\x1b8",
        cursor: (0, 4, true),
        next: "X",
    },
    Case {
        name: "an active DECRC wrap whose charset was changed after restore",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2H\x1b)0\x0eqqqq\x1b7\x1b[2;8s\x1b8\x0f",
        cursor: (0, 4, true),
        next: "q",
    },
    Case {
        name: "an active wrap with full-width margins enabled and no saved cursor",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b[?69l\x1b[?69h",
        cursor: (0, 4, true),
        next: "X",
    },
    Case {
        name: "an active wrap with full-width margins enabled and a different saved cursor",
        input: "\x1b[4;4H\x1b[32m\x1b7\x1b[m\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b[?69l\x1b[?69h",
        cursor: (0, 4, true),
        next: "X",
    },
    Case {
        name: "a raw switch carries a main saved wrap onto alt with no alt saved cursor",
        input: "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b7\x1b[2;8s\x1b8\x1b[?47h",
        cursor: (0, 4, true),
        next: "X\x1b[?47l\x1b8Y",
    },
    Case {
        name: "a raw switch carries an alt saved wrap onto main with no main saved cursor",
        input: "\x1b[?47h\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b7\x1b[2;8s\x1b8\x1b[?47l",
        cursor: (0, 4, true),
        next: "X\x1b8Y",
    },
    Case {
        name: "a main-screen origin and margin wrap saved behind the alternate screen",
        input: "\x1b[?69h\x1b[2;7s\x1b[2;5r\x1b[?6h\x1b[1;1Habcdef\x1b[?1049h\x1b[?6l\x1b[?69l\x1b[1;1Halt",
        cursor: (0, 3, false),
        next: "\x1b[?1049lX",
    },
    Case {
        name: "a saved origin and margin wrap on the alternate screen",
        input: "\x1b[?1049h\x1b[?69h\x1b[2;7s\x1b[2;5r\x1b[?6h\x1b[1;1Habcdef\x1b7\x1b[?6l\x1b[6;1H.",
        cursor: (5, 1, false),
        next: "\x1b8X",
    },
];

fn source(case: &Case) -> Terminal {
    let mut t = Terminal::new(6, 10);
    t.process(case.input.as_bytes());
    let c = t.cursor();
    assert_eq!(
        (c.row, c.col, t.grid().pending_wrap()),
        case.cursor,
        "fixture premise: {}",
        case.name
    );
    t
}

#[test]
fn a_restore_paint_preserves_live_margin_and_origin_wraps() {
    let mut wrong = Vec::new();
    for case in CASES {
        let mut live = source(case);
        let mut restored = Terminal::new(6, 10);
        restored.process(live.serialize_restore_ansi().as_bytes());
        let (got, want) = (observe(&restored), observe(&live));
        if got != want {
            wrong.push(format!(
                "{} before next: restored {got:?}, live {want:?}",
                case.name
            ));
        }
        live.process(case.next.as_bytes());
        restored.process(case.next.as_bytes());
        let (got, want) = (observe(&restored), observe(&live));
        if got != want {
            wrong.push(format!(
                "{} after next: restored {got:?}, live {want:?}",
                case.name
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn cursor_position_alone_cannot_restore_an_armed_margin_or_origin_wrap() {
    for case in CASES.iter().filter(|case| case.cursor.2) {
        let mut live = source(case);
        let mut without_wrap = source(case);
        let c = live.cursor();
        let r = live.grid().scroll_region();
        let m = live.grid().horizontal_margins();
        let row = if live.modes().origin_mode {
            c.row - r.top
        } else {
            c.row
        };
        let col = if live.modes().origin_mode && live.modes().left_right_margin_mode {
            c.col - m.left
        } else {
            c.col
        };
        without_wrap.process(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
        assert!(!without_wrap.grid().pending_wrap(), "{}", case.name);
        live.process(b"\x1b[?7hX");
        without_wrap.process(b"\x1b[?7hX");
        assert_ne!(observe(&live), observe(&without_wrap), "{}", case.name);
    }
}

#[test]
fn a_restore_paint_keeps_saved_wraps_after_the_edge_is_erased_or_replaced() {
    let mut wrong = Vec::new();
    for (input, next, erased, active_wrap) in [
        (
            "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b7\x1b[1;2H\x1b[K",
            "\x1b8X",
            true,
            false,
        ),
        (
            "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b7\x1b[?69l\x1b[1;5H中",
            "\x1b8X",
            false,
            false,
        ),
        (
            "\x1b[2;5r\x1b[?6h\x1b[1;1H0123456789\x1b7\x1b[?6l\x1b[2;1H\x1b[K",
            "\x1b8X",
            true,
            false,
        ),
        (
            "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b7\x1b[1;2H\x1b[K\x1b8",
            "X",
            true,
            true,
        ),
        (
            "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b7\x1b[1;2H\x1b[K\x1b8\x1b[?1049h\x1b[1;1Halt",
            "\x1b[?1049lX",
            true,
            false,
        ),
        ("0123456789\x1b7\x1b[1;1H\x1b[K\x1b8", "X", true, true),
        (
            "\x1b[2;5r\x1b[?6h\x1b[1;1H0123456789\x1b7\x1b[?6l\x1b[2;1H\x1b[K\x1b8",
            "X",
            true,
            true,
        ),
    ] {
        let mut live = Terminal::new(6, 10);
        live.process(input.as_bytes());
        let saved = live
            .cursor_save
            .main
            .as_ref()
            .expect("actual DECSC or 1049 save");
        assert!(saved.pending_wrap, "source slot is armed: {input:?}");
        assert_eq!(live.grid().pending_wrap(), active_wrap, "{input:?}");
        let grid = live.main_grid();
        if erased {
            assert!(
                grid.row_at_screen(saved.cursor.row)
                    .is_none_or(|r| r.len() <= saved.cursor.col)
            );
        } else {
            assert!(
                grid.row_at_screen(saved.cursor.row)
                    .unwrap()
                    .is_cell_wide_continuation(saved.cursor.col + 1)
            );
        }
        let mut restored = Terminal::new(6, 10);
        restored.process(live.serialize_restore_ansi().as_bytes());
        let (got, want) = (observe(&restored), observe(&live));
        if got != want {
            wrong.push(format!(
                "{input:?} before next: restored {got:?}, live {want:?}"
            ));
        }
        live.process(next.as_bytes());
        restored.process(next.as_bytes());
        let (got, want) = (observe(&restored), observe(&live));
        if got != want {
            wrong.push(format!(
                "{input:?} after {next:?}: restored {got:?}, live {want:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_restore_paint_does_not_invent_a_wrap_after_cursor_movement() {
    for case in CASES.iter().filter(|case| case.cursor.2) {
        let mut live = source(case);
        let c = live.cursor();
        let r = live.grid().scroll_region();
        let m = live.grid().horizontal_margins();
        let row = if live.modes().origin_mode {
            c.row - r.top
        } else {
            c.row
        };
        let col = if live.modes().origin_mode && live.modes().left_right_margin_mode {
            c.col - m.left
        } else {
            c.col
        };
        live.process(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
        assert!(!live.grid().pending_wrap(), "{}", case.name);
        let mut restored = Terminal::new(6, 10);
        restored.process(live.serialize_restore_ansi().as_bytes());
        assert_eq!(
            observe(&restored),
            observe(&live),
            "{} before next",
            case.name
        );
        live.process(case.next.as_bytes());
        restored.process(case.next.as_bytes());
        assert_eq!(
            observe(&restored),
            observe(&live),
            "{} after next",
            case.name
        );
    }
}

#[test]
fn a_restore_paint_keeps_a_wrap_after_rectangle_erasure_without_saving_a_cursor() {
    let mut wrong = Vec::new();
    for (input, row, col) in [
        ("0123456789\x1b[2;1;2;10;1;1;1;1$v", 0, 9),
        (
            "\x1b[2;5r\x1b[?6h\x1b[1;1H0123456789\x1b[2;1;2;10;1;1;1;1$v",
            1,
            9,
        ),
        (
            "\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b[2;1;2;10;1;1;1;1$v",
            0,
            4,
        ),
        (
            "\x1b[?69h\x1b[2;5s\x1b[2;5r\x1b[?6h\x1b[1;1Habcd\x1b[2;1;2;4;1;1;1;1$v",
            1,
            4,
        ),
        (
            "\x1b[38;2;1;2;3m中\x1b[44m12345678\x1b[2;9;2;10;1;1;9;1$v",
            0,
            9,
        ),
        (
            "\x1b[35m中L\x1b[m\x1b[?69h\x1b[4;7s\x1b[1;4H\x1b[42mabcd\x1b[2;4;2;7;1;1;4;1$v",
            0,
            6,
        ),
    ] {
        let mut live = Terminal::new(6, 10);
        live.process(input.as_bytes());
        assert_eq!(
            (live.cursor().row, live.cursor().col),
            (row, col),
            "{input:?}"
        );
        assert!(
            live.grid().pending_wrap(),
            "real rectangle operation retained the flag: {input:?}"
        );
        assert!(
            live.cursor_save.main.is_none() && live.cursor_save.alt.is_none(),
            "{input:?}"
        );
        assert!(
            live.grid()
                .row_at_screen(row)
                .is_none_or(|r| r.len() <= col),
            "the copied blank tail includes the pending edge: {input:?}"
        );
        let mut restored = Terminal::new(6, 10);
        restored.process(live.serialize_restore_ansi().as_bytes());
        for next in ["", "X", "\x1b8Y"] {
            live.process(next.as_bytes());
            restored.process(next.as_bytes());
            let (got, want) = (observe(&restored), observe(&live));
            if got != want {
                wrong.push(format!(
                    "{input:?} after {next:?}: restored {got:?}, live {want:?}"
                ));
            }
        }
        let mut plain = Terminal::new(6, 10);
        plain.process(input.as_bytes());
        plain.process(format!("\x1b[?6l\x1b[{};{}H", row + 1, col + 1).as_bytes());
        assert!(
            !plain.grid().pending_wrap(),
            "real CUP clears the erased edge's flag"
        );
        let mut plain_replay = Terminal::new(6, 10);
        plain_replay.process(plain.serialize_restore_ansi().as_bytes());
        for next in ["", "X"] {
            plain.process(next.as_bytes());
            plain_replay.process(next.as_bytes());
            let (got, want) = (observe(&plain_replay), observe(&plain));
            if got != want {
                wrong.push(format!(
                    "plain CUP after {input:?}, then {next:?}: restored {got:?}, live {want:?}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn restoring_an_active_wrap_preserves_the_existing_saved_cursor_or_its_absence() {
    for case in CASES {
        let mut live = source(case);
        let mut restored = Terminal::new(6, 10);
        restored.process(live.serialize_restore_ansi().as_bytes());
        for next in ["X", "\x1b8Y"] {
            live.process(next.as_bytes());
            restored.process(next.as_bytes());
            assert_eq!(
                observe(&restored),
                observe(&live),
                "{} after {next:?}",
                case.name
            );
        }
    }
}

#[test]
fn a_restore_paint_keeps_a_wrap_when_a_rectangle_puts_a_wide_glyph_across_it() {
    let mut wrong = Vec::new();
    for (input, row, col) in [
        (
            "\x1b[2;1H中\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b[?69l\x1b[2;1;2;2;1;1;5;1$v",
            0,
            4,
        ),
        (
            "\x1b[2;1H中\x1b[?69h\x1b[2;3s\x1b[1;2Hab\x1b[?69l\x1b[2;1;2;2;1;1;3;1$v",
            0,
            2,
        ),
        (
            "\x1b[2;1H中\x1b[?69h\x1b[2;5s\x1b[1;2Habcd\x1b[2;1;2;2;1;1;5;1$v",
            0,
            4,
        ),
        (
            "\x1b[3;2H中\x1b[?69h\x1b[2;5s\x1b[2;5r\x1b[?6h\x1b[1;1Habcd\x1b[2;1;2;2;1;1;4;1$v",
            1,
            4,
        ),
    ] {
        let mut live = Terminal::new(6, 10);
        live.process(input.as_bytes());
        assert_eq!(
            (live.cursor().row, live.cursor().col),
            (row, col),
            "{input:?}"
        );
        assert!(live.grid().pending_wrap(), "{input:?}");
        assert!(
            live.cursor_save.main.is_none() && live.cursor_save.alt.is_none(),
            "{input:?}"
        );
        assert_eq!(live.grid().resolved_char(row, col), Some('中'), "{input:?}");
        assert!(
            live.grid()
                .row_at_screen(row)
                .unwrap()
                .is_cell_wide_continuation(col + 1),
            "{input:?}"
        );
        let mut restored = Terminal::new(6, 10);
        restored.process(live.serialize_restore_ansi().as_bytes());
        for next in ["", "X", "\x1b8Y"] {
            live.process(next.as_bytes());
            restored.process(next.as_bytes());
            let (got, want) = (observe(&restored), observe(&live));
            if got != want {
                wrong.push(format!(
                    "{input:?} after {next:?}: restored {got:?}, live {want:?}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
