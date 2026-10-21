// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The client's face, row by row of design §6.1's table, in its order: each
//! row decides when every row above it does not, whatever the inputs below
//! it say.

use amacs_term::decide::{
    EXIT_USAGE, Face, Handoff, Inputs, NO_TERMINAL, OpenIn, after_handoff, decide,
};

/// Inside an aterm tab, at a terminal, nothing asked: the native face.
fn in_a_tab() -> Inputs {
    Inputs {
        parent_session: true,
        stdin_tty: true,
        stdout_tty: true,
        ..Inputs::default()
    }
}

/// Every combination of the inputs, so each row is checked against all the
/// others' values.
fn every_input() -> Vec<Inputs> {
    let mut all = Vec::new();
    for bits in 0u32..(1 << 9) {
        let bit = |n: u32| bits & (1 << n) != 0;
        all.push(Inputs {
            tty_flag: bit(0),
            wait: bit(1),
            no_wait: bit(2),
            parent_session: bit(3),
            tmux: bit(4),
            sty: bit(5),
            stdin_tty: bit(6),
            stdout_tty: bit(7),
            open_in: if bit(8) {
                OpenIn::Terminal
            } else {
                OpenIn::Window
            },
        });
    }
    all
}

#[test]
fn every_row_of_the_design_table() {
    assert_eq!(decide(&in_a_tab()), Face::Native);
    let no_terminal = Face::Exit(EXIT_USAGE, NO_TERMINAL.to_string());

    // Row 1: `-nw`/`--tty`, or `[amacs] open_in = "terminal"`: the terminal,
    // even inside a tab at a terminal.
    for inputs in [
        Inputs {
            tty_flag: true,
            ..in_a_tab()
        },
        Inputs {
            open_in: OpenIn::Terminal,
            ..in_a_tab()
        },
    ] {
        assert_eq!(decide(&inputs), Face::Terminal, "{inputs:?}");
    }
    // Row 2: no aterm window hosts this terminal, or a multiplexer sits
    // between: the terminal.
    for inputs in [
        Inputs {
            parent_session: false,
            ..in_a_tab()
        },
        Inputs {
            tmux: true,
            ..in_a_tab()
        },
        Inputs {
            sty: true,
            ..in_a_tab()
        },
    ] {
        assert_eq!(decide(&inputs), Face::Terminal, "{inputs:?}");
    }
    // Row 3: stdin and stdout not both terminals, and neither wait flag:
    // exit 2 with the no-terminal line; either flag opens the view.
    for (stdin_tty, stdout_tty) in [(false, true), (true, false), (false, false)] {
        let inputs = Inputs {
            stdin_tty,
            stdout_tty,
            ..in_a_tab()
        };
        assert_eq!(decide(&inputs), no_terminal, "{inputs:?}");
        for asked in [
            Inputs {
                wait: true,
                ..inputs
            },
            Inputs {
                no_wait: true,
                ..inputs
            },
        ] {
            assert_eq!(decide(&asked), Face::Native, "{asked:?}");
        }
    }
    assert!(NO_TERMINAL.starts_with("amacs: no terminal to edit in"));
    // Rows 4-6, after the handoff: an `ERR` or a failed connection is the
    // terminal; the window taking the request and not answering exits 1,
    // never a second editor; `OK edit` is the native view.
    assert_eq!(after_handoff(Handoff::Refused), Face::Terminal);
    assert_eq!(after_handoff(Handoff::Unreachable), Face::Terminal);
    assert!(matches!(after_handoff(Handoff::TimedOut), Face::Exit(1, _)));
    assert_eq!(after_handoff(Handoff::Opened), Face::Native);

    // The order, over every combination: each row's verdict stands whatever
    // the inputs the rows below it read.
    for inputs in every_input() {
        let row1 = inputs.tty_flag || inputs.open_in == OpenIn::Terminal;
        let row2 = !inputs.parent_session || inputs.tmux || inputs.sty;
        let row3 = !(inputs.stdin_tty && inputs.stdout_tty) && !inputs.wait && !inputs.no_wait;
        let want = if row1 || row2 {
            Face::Terminal
        } else if row3 {
            no_terminal.clone()
        } else {
            Face::Native
        };
        assert_eq!(decide(&inputs), want, "{inputs:?}");
    }
}
