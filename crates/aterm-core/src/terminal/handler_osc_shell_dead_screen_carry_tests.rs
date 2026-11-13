// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::Terminal;

const ROWS: u16 = 6;

struct Tab {
    ours: Terminal,
    host: Terminal,
}

impl Tab {
    fn killed(used: u16, app: &[u8], prompt: &[u8]) -> Self {
        let mut ours = Terminal::new(ROWS, 40);
        ours.set_host_resyncs_alt_screen(true);
        let mut tab = Self {
            ours,
            host: Terminal::new(ROWS, 40),
        };
        for i in 0..used {
            tab.both(format!("out {i}\r\n").as_bytes());
        }
        tab.both(b"\x1b[?1049h\x1b[H\x1b[2J");
        tab.both(app);
        tab.both(b"\x1b]133;D;-1\x07\x1b]133;A\x07");
        tab.both(prompt);
        tab.both(b"\x1b]133;B\x07");
        tab
    }

    fn both(&mut self, bytes: &[u8]) {
        self.ours.process(bytes);
        self.host.process(&without_marks(bytes));
    }

    fn leave(&mut self, carry: bool) -> bool {
        let text = self.ours.dead_alt_screen_carry();
        let mut bytes = b"\x1b[?1049l".to_vec();
        if carry && let Some(text) = &text {
            bytes.extend_from_slice(text.as_bytes());
        }
        self.both(&bytes);
        text.is_some()
    }

    fn rows(t: &Terminal) -> Vec<String> {
        (0..usize::from(ROWS))
            .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
            .collect()
    }

    fn history(t: &Terminal) -> Vec<String> {
        let grid = t.grid();
        (0..grid.scrollback_lines())
            .map(|i| {
                grid.get_history_line(i)
                    .as_ref()
                    .and_then(|l| l.as_str())
                    .map(|s| s.trim_end().to_string())
                    .unwrap_or_default()
            })
            .collect()
    }

    fn in_step(&self) -> Vec<String> {
        let (a, b) = (self.ours.cursor(), self.host.cursor());
        assert_eq!(Self::rows(&self.ours), Self::rows(&self.host));
        assert_eq!(Self::history(&self.ours), Self::history(&self.host));
        assert_eq!((a.row, a.col), (b.row, b.col), "cursors");
        assert!(!self.ours.is_alternate_screen() && !self.host.is_alternate_screen());
        Self::rows(&self.ours)
    }

    fn cursor(&self) -> (u16, u16) {
        let c = self.ours.cursor();
        (c.row, c.col)
    }
}

fn without_marks(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\x1b]133;") {
            i += bytes[i..]
                .iter()
                .position(|&b| b == 0x07)
                .map_or(bytes.len(), |p| p + 1);
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[test]
fn the_prompt_comes_back_at_the_row_and_column_it_was_drawn_at() {
    let mut tab = Tab::killed(3, b"line 1 of the file\r\nline 2\x1b[H", b"PS> ");
    assert!(tab.leave(true), "a prompt to carry");
    let rows = tab.in_step();
    assert_eq!(rows, ["PS>", "", "", "", "", ""]);
    assert_eq!(tab.cursor(), (0, 4), "where the line editor anchored");
    assert_eq!(Tab::history(&tab.ours), ["out 0", "out 1", "out 2"]);
    tab.both(b"\x1b[1;5Hecho after");
    assert_eq!(Tab::rows(&tab.ours)[0], "PS> echo after");
}

#[test]
fn without_the_carry_the_anchored_redraw_overwrites_an_earlier_row() {
    let mut tab = Tab::killed(3, b"line 1 of the file\r\nline 2\x1b[H", b"PS> ");
    tab.leave(false);
    tab.in_step();
    assert_eq!(tab.cursor(), (3, 0));
    tab.both(b"\x1b[1;5Hecho after");
    assert_eq!(
        Tab::rows(&tab.ours)[..3],
        ["out echo after", "out 1", "out 2"]
    );
}

#[test]
fn a_prompt_below_the_used_rows_scrolls_nothing() {
    let mut tab = Tab::killed(2, b"a\r\nb\r\nc\r\nd\x1b[5;1H", b"PS> ");
    assert!(tab.leave(true));
    let rows = tab.in_step();
    assert_eq!(rows, ["out 0", "out 1", "", "", "PS>", ""]);
    assert_eq!(tab.cursor(), (4, 4));
    assert!(Tab::history(&tab.ours).is_empty());
}

#[test]
fn the_carry_is_the_prompt_alone_and_all_of_it() {
    let mut tab = Tab::killed(1, b"\x1b[2;1Hdead app text that runs on\x1b[2;10H", b"PS> ");
    assert!(tab.leave(true));
    assert_eq!(tab.in_step(), ["out 0", "         PS>", "", "", "", ""]);
    assert_eq!(tab.cursor(), (1, 13));

    let mut tab = Tab::killed(4, b"x\r\ny\r\nz\x1b[2;1H", b"~\\dir\r\nPS> ");
    assert!(tab.leave(true));
    assert_eq!(tab.in_step(), ["out 3", "~\\dir", "PS>", "", "", ""]);
    assert_eq!(tab.cursor(), (2, 4));
    assert_eq!(Tab::history(&tab.ours), ["out 0", "out 1", "out 2"]);
}

#[test]
fn the_prompt_keeps_its_colours() {
    let mut tab = Tab::killed(1, b"\x1b[H", b"\x1b[32mPS\x1b[0m> ");
    assert!(tab.leave(true));
    tab.in_step();
    let text = tab.ours.grid().row_ansi_text_screen(0).unwrap_or_default();
    assert!(text.contains("\x1b[0;32mPS"), "{text:?}");
}

#[test]
fn the_prompt_comes_back_in_the_pen_the_leave_restores() {
    let mut tab = Tab::killed(1, b"\x1b[0;1;7mruler\x1b[H", b"PS> ");
    assert!(tab.leave(true));
    assert_eq!(tab.in_step()[0], "PS>");
    for t in [&tab.ours, &tab.host] {
        let text = t.grid().row_ansi_text_screen(0).unwrap_or_default();
        assert!(!text.contains('\x1b'), "{text:?}");
    }
}

#[test]
fn nothing_drawn_after_the_mark_carries_nothing() {
    let tab = Tab::killed(2, b"pager\x1b[H", b"");
    assert_eq!(tab.ours.dead_alt_screen_carry(), None);

    let mut term = Terminal::new(ROWS, 40);
    term.set_host_resyncs_alt_screen(true);
    term.process(b"\x1b[?1049hpager\x1b[H");
    assert_eq!(term.dead_alt_screen_carry(), None, "no prompt on it");
    term.process(b"\x1b]133;A\x07PS> ");
    assert!(term.dead_alt_screen_carry().is_some());
    term.process(b"\x1b[?1049l");
    assert_eq!(term.dead_alt_screen_carry(), None, "left");

    let mut local = Terminal::new(ROWS, 40);
    local.process(b"\x1b[?1049hpager\x1b[H\x1b]133;A\x07PS> ");
    assert!(!local.is_alternate_screen(), "the local leave");
    assert_eq!(local.dead_alt_screen_carry(), None);
    assert_eq!(
        super::TerminalHandler::unescape_vscode_string("a\\x+ab\\x41"),
        "a\\x+abA"
    );
    let mut alt = Terminal::new(ROWS, 40);
    alt.process(b"\x1b[?1049hpager\x1b]133;Aextra\x07\x1b]633;Eextra;echo hi\x07");
    assert!(alt.is_alternate_screen(), "multi-byte subcommand is not A");
    assert_eq!(alt.take_osc_event(), None);
}
