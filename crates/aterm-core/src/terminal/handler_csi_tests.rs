// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#[test]
fn sd_with_a_zero_or_several_parameters_is_mouse_tracking_not_a_scroll() {
    use crate::terminal::Terminal;

    let row0 = |bytes: &[u8]| {
        let mut term = Terminal::new(3, 4);
        term.process(bytes);
        term.grid().row_text(0).unwrap_or_default()
    };
    assert_eq!(row0(b"a\x1b[0T").trim_end(), "a", "CSI 0 T");
    assert_eq!(row0(b"a\x1b[1;2;3;4;5T").trim_end(), "a", "five params");
    assert_eq!(row0(b"a\x1b[2;1T").trim_end(), "a", "two params");
    assert_eq!(row0(b"a\x1b[T").trim_end(), "", "CSI T scrolls one line");
    assert_eq!(row0(b"a\x1b[1T").trim_end(), "", "CSI 1 T");
    assert_eq!(row0(b"a\x1b[0^").trim_end(), "", "CSI 0 ^ is plain SD");
}

#[test]
fn decslrm_invalid_left_ge_right_is_noop() {
    use crate::terminal::Terminal;

    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?69h\x1b[11;51s");
    assert_eq!(
        term.grid().horizontal_margins(),
        aterm_grid::HorizontalMargins {
            left: 10,
            right: 50,
        },
        "valid DECSLRM sets [10,50]"
    );

    term.process(b"\x1b[6;31H");
    assert_eq!((term.cursor().row, term.cursor().col), (5, 30));

    term.process(b"\x1b[41;31s");

    assert_eq!(
        term.grid().horizontal_margins(),
        aterm_grid::HorizontalMargins {
            left: 10,
            right: 50,
        },
        "invalid DECSLRM must NOT clobber the prior margins"
    );
    assert_eq!(
        (term.cursor().row, term.cursor().col),
        (5, 30),
        "invalid DECSLRM must NOT home the cursor"
    );
}

#[test]
fn decslrm_default_params_reset_full_and_home() {
    use crate::terminal::Terminal;

    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?69h\x1b[11;51s\x1b[6;31H");
    term.process(b"\x1b[s");
    assert_eq!(
        term.grid().horizontal_margins(),
        aterm_grid::HorizontalMargins { left: 0, right: 79 },
        "default DECSLRM resets to full width"
    );
    assert_eq!((term.cursor().row, term.cursor().col), (0, 0));
}
