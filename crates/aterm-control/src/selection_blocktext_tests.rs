// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

use aterm_core::terminal::Terminal;

use super::{NO_SESSION, block_text, blocktext, frame_block_text};
use crate::conformance::MemoryHost;
use crate::host::{ChangeWait, HostCapabilities, Selector, SessionEntry, SessionHost};
use crate::wire::visible_char;

thread_local! {
    /// How many block replies [`frame_block_text`] framed on this thread.
    pub(super) static FRAMED: Cell<usize> = const { Cell::new(0) };
}

fn framed() -> usize {
    FRAMED.with(Cell::get)
}

fn two_step_reply(text: &str, trim: bool) -> String {
    let rows: Vec<String> = text
        .lines()
        .map(|l| {
            l.chars()
                .map(visible_char)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    let sent = if trim {
        rows.iter()
            .rposition(|r| !r.trim_matches(' ').is_empty())
            .map_or(0, |i| i + 1)
    } else {
        rows.len()
    };
    let mut out = format!("OK {sent}");
    if trim {
        out.push_str(&format!(" trimmed={}", rows.len() - sent));
    }
    out.push('\n');
    for row in &rows[..sent] {
        out.push_str(row);
        out.push('\n');
    }
    out
}

#[test]
fn blocktext_frames_each_row_and_trims_the_blank_tail() {
    let text = "out\x01\n\u{a0}\n\x01 \t\n\n";
    assert_eq!(frame_block_text(text, false, ""), "OK 4\nout\n\n\n\n");
    assert_eq!(frame_block_text(text, true, ""), "OK 1 trimmed=3\nout\n");
    assert_eq!(
        frame_block_text(text, true, " frame=open"),
        "OK 1 frame=open trimmed=3\nout\n"
    );
    assert_eq!(frame_block_text("", false, ""), "OK 0\n");
    assert_eq!(frame_block_text("", true, ""), "OK 0 trimmed=0\n");
    assert_eq!(
        frame_block_text("a\r\nb \u{9b} \r\n", false, ""),
        "OK 2\na\nb\n"
    );
    for sample in [
        text,
        "",
        "\n\n",
        "a\r\nb \u{9b} \r\n",
        "head\n\nmid  \n\u{3000}\n",
        "no newline at the end",
        "\0\t\x1b[0m\nlast\x7f",
    ] {
        for trim in [false, true] {
            assert_eq!(
                frame_block_text(sample, trim, ""),
                two_step_reply(sample, trim),
                "{sample:?} trim={trim}"
            );
        }
    }
    let raw_spaces_only = text
        .lines()
        .collect::<Vec<_>>()
        .iter()
        .rposition(|l| !l.trim_matches(' ').is_empty())
        .map_or(0, |i| i + 1);
    assert_eq!(
        raw_spaces_only, 3,
        "control: the raw spaces-only count keeps two blank rows"
    );
}

struct Watching {
    inner: MemoryHost,
    framed_inside: Cell<usize>,
}

impl SessionHost for Watching {
    fn capabilities(&self) -> HostCapabilities {
        self.inner.capabilities()
    }
    fn sessions(&self) -> Vec<SessionEntry> {
        self.inner.sessions()
    }
    fn resolve(&self, selector: Selector<'_>) -> Option<u64> {
        self.inner.resolve(selector)
    }
    fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        let before = framed();
        let out = self.inner.with_terminal(sid, f);
        self.framed_inside
            .set(self.framed_inside.get() + framed() - before);
        out
    }
    fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
        self.inner.with_terminal_mut(sid, f)
    }
    fn request_redraw(&self, sid: u64) {
        self.inner.request_redraw(sid);
    }
    fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_> {
        self.inner.subscribe(sid)
    }
    fn clipboard_set(&self, text: &str) -> bool {
        self.inner.clipboard_set(text)
    }
}

#[test]
fn blocktext_frames_after_the_terminal_lock() {
    let host = Watching {
        inner: MemoryHost::new(0),
        framed_inside: Cell::new(0),
    };
    host.inner.feed(
        b"\x1b]133;A\x07$ ls\x1b]133;B\x07\r\n\x1b]133;C\x07a\r\nb\r\n\r\n\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ",
    );
    let start = framed();
    let whole = blocktext(&host, 0, 0, false);
    assert!(
        whole.starts_with("OK ") && whole.contains("\na\nb\n"),
        "{whole}"
    );
    let trimmed = blocktext(&host, 0, 0, true);
    assert!(
        trimmed.contains(" trimmed=")
            && !trimmed.contains(" trimmed=0")
            && trimmed.ends_with("\na\nb\n"),
        "{trimmed}"
    );
    assert_eq!(framed() - start, 2, "both replies were framed");
    assert_eq!(
        host.framed_inside.get(),
        0,
        "neither was framed under the lock"
    );

    assert_eq!(blocktext(&host, 0, 99, true), "ERR no such block\n");
    assert_eq!(blocktext(&host, 5, 0, true), NO_SESSION);
    assert_eq!(framed() - start, 2, "a refusal frames nothing");

    let inside = host.with_terminal(0, |t| {
        frame_block_text(&block_text(t, 0).expect("the block has output"), false, "")
    });
    assert_eq!(inside.as_deref(), Some(whole.as_str()));
    assert_eq!(
        host.framed_inside.get(),
        1,
        "control: framing in the closure is seen"
    );
}
