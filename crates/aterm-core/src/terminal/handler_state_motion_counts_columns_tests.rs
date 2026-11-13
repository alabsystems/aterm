// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::Terminal;

fn terminals() -> [(&'static str, Terminal); 3] {
    let plain = Terminal::new(2, 20);
    let mut asked = Terminal::new(2, 20);
    asked.process(b"\x1b[?2027h");
    let mut carried = Terminal::new(2, 20);
    carried.modes_mut().grapheme_cluster_mode = true;
    [("plain", plain), ("?2027h", asked), ("carried", carried)]
}

#[test]
fn cuf_cub_hpr_and_bs_move_by_columns_over_wide_text() {
    let moves: [(&str, &[u8], u16); 5] = [
        ("CR CUF CUF", b"\r\x1b[C\x1b[C", 2),
        ("CR CUF 3", b"\r\x1b[3C", 3),
        ("CR HPR 2", b"\r\x1b[2a", 2),
        ("col 5, BS x3", b"\x1b[6G\x08\x08\x08", 2),
        ("col 5, CUB 3", b"\x1b[6G\x1b[3D", 2),
    ];
    for (label, mut term) in terminals() {
        term.process("中文ab".as_bytes());
        for (what, bytes, col) in moves {
            term.process(bytes);
            assert_eq!(term.cursor().col, col, "{label}: {what}");
        }
        term.process(b"\r\x1b[C\x1b[CX");
        let grid = term.grid();
        let row: Vec<char> = (0..6)
            .map(|col| grid.cell(0, col).map_or('?', |c| c.char()))
            .collect();
        assert_eq!(row[0], '中', "{label}");
        assert_eq!(row[2], 'X', "{label}: X written at column 2");
        assert_eq!(row[3], ' ', "{label}: 文's second half is blanked");
        assert_eq!((row[4], row[5]), ('a', 'b'), "{label}");
        assert_eq!(term.cursor().col, 3, "{label}");
    }
}
