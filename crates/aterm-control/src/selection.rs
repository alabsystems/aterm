// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selection, copy, and command-block control verbs (`select`, `selection`, `copy`, `blocks`,
//! `blocktext`, `wait`).

use std::sync::OnceLock;

use aterm_core::selection::{SelectionSide, SelectionType, SmartSelection};
use aterm_core::terminal::{Terminal, smart_word_in_cells};

use crate::host::SessionHost;
use crate::wire::{
    VisibleTrim, WAIT_DEFAULT_MS, frame_open_tail, frame_unfinished_now, json_ok, json_str_field,
    pct_encode_into, push_visible, trimmed_len, wait_ms,
};

/// The host does not resolve the target sid.
const NO_SESSION: &str = "ERR no such session\n";

/// The `blocks [N]` argument: the last `N` blocks, or all of them when `N` is
/// absent, unparsable or not smaller than the count.
fn last_n<'a, T>(all: &'a [T], rest: &str) -> &'a [T] {
    let trimmed = rest.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return all;
    }
    match trimmed.parse::<usize>() {
        Ok(n) if n < all.len() => &all[all.len() - n..],
        _ => all,
    }
}

/// A block state's wire word.
#[inline]
const fn state_word(state: aterm_core::terminal::BlockState) -> &'static str {
    use aterm_core::terminal::BlockState;
    match state {
        BlockState::PromptOnly => "prompt",
        BlockState::EnteringCommand => "entering",
        BlockState::Executing => "executing",
        BlockState::Complete => "complete",
        _ => "unknown",
    }
}

/// `blocks [N] --json`: return OSC 133/633 command blocks as JSON.
pub fn cmd_blocks_json(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    let Some(items) = host.with_terminal_as_it_stands(sid, |t: &Terminal| {
        let all: Vec<_> = t.all_blocks().collect();
        let slice = last_n(&all, rest);
        let opt_row = |r: Option<u64>| r.map_or_else(|| "null".to_string(), |v| v.to_string());
        let mut items: Vec<String> = Vec::with_capacity(slice.len());
        for b in slice {
            let exit = b
                .exit_code
                .map_or_else(|| "null".to_string(), |c| c.to_string());
            items.push(format!(
                "{{\"id\":{},{},\"exit\":{exit},\"prompt\":{},\"cmd\":{},\"cmdcol\":{},\"out\":{},\"end\":{},{},{}}}",
                b.id,
                json_str_field("state", state_word(b.state)),
                b.prompt_start_row,
                opt_row(b.command_start_row),
                opt_row(b.command_start_col.map(u64::from)),
                opt_row(b.output_start_row),
                opt_row(b.end_row),
                json_str_field(
                    "cwd",
                    &host.native_cwd(b.working_directory.as_deref().unwrap_or("")),
                ),
                json_str_field("cmdline", b.commandline.as_deref().unwrap_or("")),
            ));
        }
        items
    }) else {
        return NO_SESSION.to_string();
    };
    json_ok(&format!("{{\"blocks\":[{}]}}", items.join(",")))
}

/// `blocks [N]`: return OSC 133/633 command blocks, oldest first.
pub fn cmd_blocks(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    use std::fmt::Write as _;
    host.with_terminal_as_it_stands(sid, |t: &Terminal| {
        let all: Vec<_> = t.all_blocks().collect();
        let slice = last_n(&all, rest);
        let mut out = format!("OK {}\n", slice.len());
        let opt_row = |r: Option<u64>| r.map_or_else(|| "-".to_string(), |v| v.to_string());
        for b in slice {
            let exit = b
                .exit_code
                .map_or_else(|| "-".to_string(), |c| c.to_string());
            let _ = write!(
                out,
                "block {} {} exit={} prompt={} cmd={} out={} end={} cwd=",
                b.id,
                state_word(b.state),
                exit,
                b.prompt_start_row,
                opt_row(b.command_start_row),
                opt_row(b.output_start_row),
                opt_row(b.end_row),
            );
            pct_encode_into(
                &mut out,
                &host.native_cwd(b.working_directory.as_deref().unwrap_or("")),
            );
            out.push_str(" cmdline=");
            pct_encode_into(&mut out, b.commandline.as_deref().unwrap_or(""));
            out.push('\n');
        }
        out
    })
    .unwrap_or_else(|| NO_SESSION.to_string())
}

fn parse_ascii_u64(token: &str) -> Option<u64> {
    (!token.is_empty() && token.bytes().all(|b| b.is_ascii_digit()))
        .then(|| token.parse::<u64>().ok())
        .flatten()
}

fn parse_ascii_u16(token: &str) -> Option<u16> {
    (!token.is_empty() && token.bytes().all(|b| b.is_ascii_digit()))
        .then(|| token.parse::<u16>().ok())
        .flatten()
}

fn parse_row_i32(token: &str) -> Option<i32> {
    let digits = token.strip_prefix('-').unwrap_or(token);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if token.starts_with('-') && digits.bytes().all(|b| b == b'0') {
        return None;
    }
    token.parse::<i32>().ok()
}

/// `blocktext <id>`: return the output text of command block `<id>`.
pub fn cmd_blocktext(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    let Some(id) = parse_ascii_u64(rest.trim()) else {
        return "ERR usage: blocktext <id>\n".to_string();
    };
    blocktext(host, sid, id, false)
}

/// Format command block `id`'s output for `blocktext`.
pub fn blocktext(host: &impl SessionHost, sid: u64, id: u64, trim: bool) -> String {
    match host.with_terminal(sid, |t: &Terminal| (block_text(t, id), frame_open(t))) {
        None => NO_SESSION.to_string(),
        Some((Err(refusal), _)) => refusal.to_string(),
        Some((Ok(text), open)) => frame_block_text(&text, trim, open),
    }
}

/// ` frame=open` when the program's DEC 2026 frame is unfinished under the host's hold.
fn frame_open(t: &Terminal) -> &'static str {
    frame_open_tail(frame_unfinished_now(t))
}

/// Block `id`'s raw output text, or the reply line that refuses it.
fn block_text(t: &Terminal, id: u64) -> Result<String, &'static str> {
    use aterm_core::terminal::BlockText;
    let block = t.block_by_id(id).ok_or("ERR no such block\n")?;
    match t.block_output_text(block) {
        BlockText::Text(text) => Ok(text),
        BlockText::Evicted => Err("ERR block output evicted from scrollback\n"),
        BlockText::NotAvailable => Err("ERR block has no output yet\n"),
    }
}

/// Frame block output `text` (see [`blocktext`]).
fn frame_block_text(text: &str, trim: bool, open: &str) -> String {
    use std::fmt::Write as _;
    #[cfg(test)]
    blocktext_tests::FRAMED.with(|n| n.set(n.get() + 1));
    let rows = text.lines().count();
    let sent = if trim {
        trimmed_len(text.lines())
    } else {
        rows
    };
    let mut out = String::with_capacity(text.len() + 64);
    let _ = write!(out, "OK {sent}{open}");
    if trim {
        let _ = write!(out, " trimmed={}", rows - sent);
    }
    out.push('\n');
    for line in text.lines().take(sent) {
        push_visible(&mut out, line, VisibleTrim::Whitespace);
        out.push('\n');
    }
    out
}

/// Newest completed block id and whether any command block is still in flight.
fn scan_blocks(t: &Terminal) -> (Option<(u64, Option<i32>)>, bool) {
    use aterm_core::terminal::BlockState;
    let mut newest_done: Option<(u64, Option<i32>)> = None;
    let mut in_flight = false;
    for b in t.all_blocks() {
        match b.state {
            BlockState::Complete => newest_done = Some((b.id, b.exit_code)),
            BlockState::Executing => in_flight = true,
            _ => {}
        }
    }
    (newest_done, in_flight)
}

/// `wait [timeout_ms]`: wait for the newest completed command block.
pub fn cmd_wait(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    let timeout_ms = match rest.trim() {
        "" => WAIT_DEFAULT_MS,
        ms => match wait_ms(ms) {
            Some(ms) => ms,
            None => return "ERR usage: wait [<ms>]\n".to_string(),
        },
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let reply = |id: u64, exit: Option<i32>| {
        let exit = exit.map_or_else(|| "-".to_string(), |c| c.to_string());
        format!("OK complete {id} exit={exit}\n")
    };
    let Some((entry_done, in_flight)) = host.with_terminal_as_it_stands(sid, scan_blocks) else {
        return NO_SESSION.to_string();
    };
    if !in_flight && let Some((id, exit)) = entry_done {
        return reply(id, exit);
    }
    let baseline = entry_done.map(|(id, _)| id);
    let sub = host.subscribe(sid);
    loop {
        let Some(newest) = host.with_terminal_as_it_stands(sid, |t: &Terminal| {
            t.newest_completed_block().map(|b| (b.id, b.exit_code))
        }) else {
            return NO_SESSION.to_string();
        };
        if let Some((id, exit)) = newest
            && baseline.is_none_or(|b| id > b)
        {
            return reply(id, exit);
        }
        if host.session_ended(sid) {
            return "ERR exited\n".to_string();
        }
        if sub.caller_gone() {
            return "ERR hangup\n".to_string();
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return "OK timeout\n".to_string();
        }
        let dur = deadline
            .saturating_duration_since(now)
            .max(std::time::Duration::from_millis(1));
        let _ = sub.wait(dur);
    }
}

/// Process-wide smart-selection rules, built lazily once.
static SMART_RULES: OnceLock<SmartSelection> = OnceLock::new();

/// The engine's builtin smart-selection rules (lazy singleton).
fn smart_rules() -> &'static SmartSelection {
    SMART_RULES.get_or_init(SmartSelection::with_builtin_rules)
}

/// Initial wrapped-line rows joined around a click before word-selection rules run.
const WORD_WINDOW_ROWS: usize = 2;

/// The widest a double-click looks, in rows of the wrapped line.
const WORD_WINDOW_MAX_ROWS: usize = 64;

/// Inclusive word cell bounds `((start_row, start_col), (end_row, end_col))` at `(row, col)`.
#[must_use]
pub fn word_span(t: &Terminal, row: i32, col: u16) -> Option<((i32, u16), (i32, u16))> {
    let cols = usize::from(t.cols());
    if cols == 0 {
        return None;
    }
    let (line_first, line_last) = t.logical_line_span(row);
    let mut back = WORD_WINDOW_ROWS;
    let mut fwd = WORD_WINDOW_ROWS;
    let (first_row, (start, end)) = loop {
        let win_first = row
            .saturating_sub(i32::try_from(back).unwrap_or(i32::MAX))
            .max(line_first);
        let win_last = row
            .saturating_add(i32::try_from(fwd).unwrap_or(i32::MAX))
            .min(line_last);
        let (text, cells) = t.line_range_cells(win_first, win_last)?;
        let offset = usize::try_from(row.checked_sub(win_first)?).ok()?;
        let global = offset.checked_mul(cols)?.checked_add(usize::from(col))?;
        let (bytes, columns) = smart_word_in_cells(smart_rules(), &text, &cells, global)?;
        let open_up = bytes.start == 0 && win_first > line_first;
        let open_down = bytes.end >= text.len() && win_last < line_last;
        if !open_up && !open_down {
            break (win_first, columns);
        }
        if back.saturating_add(fwd) >= WORD_WINDOW_MAX_ROWS {
            break (win_first, columns);
        }
        if open_up {
            back = back.saturating_mul(2);
        }
        if open_down {
            fwd = fwd.saturating_mul(2);
        }
    };
    let last = end.saturating_sub(1).max(start);
    let cell = |g: usize| -> (i32, u16) {
        let dr = i32::try_from(g / cols).unwrap_or(i32::MAX);
        let c = u16::try_from(g % cols).unwrap_or(u16::MAX);
        (first_row.saturating_add(dr), c)
    };
    Some((cell(start), cell(last)))
}

/// Word-select at live-screen `(row, col)` and return inclusive cell bounds.
pub fn select_word(t: &mut Terminal, row: i32, col: u16) -> ((i32, u16), (i32, u16)) {
    let (start, end) = word_span(t, row, col).unwrap_or(((row, col), (row, col)));
    let sel = t.text_selection_mut();
    sel.start_selection(
        start.0,
        start.1,
        SelectionSide::Left,
        SelectionType::Semantic,
    );
    sel.update_selection(end.0, end.1, SelectionSide::Right);
    sel.complete_selection();
    (start, end)
}

/// Line-select the logical line at live-screen row `row` and return `(start_row, end_row)`.
pub fn select_line(t: &mut Terminal, row: i32) -> (i32, i32) {
    let (first, last) = t.logical_line_span(row);
    let max_col = t.cols().saturating_sub(1);
    let sel = t.text_selection_mut();
    sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Lines);
    sel.update_selection(last, max_col, SelectionSide::Right);
    sel.complete_selection();
    (first, last)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectCmd {
    Clear,
    Word(i32, u16),
    Line(i32),
    Block(i32, u16, i32, u16),
    Extend(i32, u16),
    Range(i32, u16, i32, u16),
}

impl SelectCmd {
    fn parse(rest: &str) -> Result<Self, &'static str> {
        const USAGE: &str = "ERR usage: select <r1> <c1> <r2> <c2> | select word <r> <c> | \
                             select line <r> | select block <r1> <c1> <r2> <c2> | \
                             select extend <r> <c> | select clear\n";
        let mut it = rest.split_whitespace();
        match it.next() {
            None => Err(USAGE),
            Some("clear") => match it.next() {
                None => Ok(Self::Clear),
                Some(_) => Err(USAGE),
            },
            Some("word") => {
                let (Some(r), Some(c), None) = (
                    it.next().and_then(parse_row_i32),
                    it.next().and_then(parse_ascii_u16),
                    it.next(),
                ) else {
                    return Err("ERR usage: select word <r> <c>\n");
                };
                Ok(Self::Word(r, c))
            }
            Some("line") => {
                let (Some(r), None) = (it.next().and_then(parse_row_i32), it.next()) else {
                    return Err("ERR usage: select line <r>\n");
                };
                Ok(Self::Line(r))
            }
            Some("block") => {
                let (Some(r1), Some(c1), Some(r2), Some(c2), None) = (
                    it.next().and_then(parse_row_i32),
                    it.next().and_then(parse_ascii_u16),
                    it.next().and_then(parse_row_i32),
                    it.next().and_then(parse_ascii_u16),
                    it.next(),
                ) else {
                    return Err("ERR usage: select block <r1> <c1> <r2> <c2>\n");
                };
                Ok(Self::Block(r1, c1, r2, c2))
            }
            Some("extend") => {
                let (Some(r), Some(c), None) = (
                    it.next().and_then(parse_row_i32),
                    it.next().and_then(parse_ascii_u16),
                    it.next(),
                ) else {
                    return Err("ERR usage: select extend <r> <c>\n");
                };
                Ok(Self::Extend(r, c))
            }
            Some(r1s) => {
                let (Some(c1s), Some(r2s), Some(c2s), None) =
                    (it.next(), it.next(), it.next(), it.next())
                else {
                    return Err(USAGE);
                };
                let (Some(r1), Some(c1), Some(r2), Some(c2)) = (
                    parse_row_i32(r1s),
                    parse_ascii_u16(c1s),
                    parse_row_i32(r2s),
                    parse_ascii_u16(c2s),
                ) else {
                    return Err("ERR bad args\n");
                };
                Ok(Self::Range(r1, c1, r2, c2))
            }
        }
    }
}

/// `select ...`: drive the engine's text selection.
pub fn cmd_select(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    let cmd = match SelectCmd::parse(rest) {
        Ok(cmd) => cmd,
        Err(usage) => return usage.to_string(),
    };
    let open = match cmd {
        SelectCmd::Clear => host.with_terminal_mut(sid, |t| {
            t.text_selection_mut().clear();
            frame_open(t)
        }),
        SelectCmd::Word(r, c) => host.with_terminal_mut(sid, |t| {
            select_word(t, r, c);
            frame_open(t)
        }),
        SelectCmd::Line(r) => host.with_terminal_mut(sid, |t| {
            select_line(t, r);
            frame_open(t)
        }),
        SelectCmd::Block(r1, c1, r2, c2) => host.with_terminal_mut(sid, |t| {
            let sel = t.text_selection_mut();
            sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Block);
            sel.update_selection(r2, c2, SelectionSide::Right);
            sel.complete_selection();
            frame_open(t)
        }),
        SelectCmd::Extend(r, c) => {
            let extended = host.with_terminal_mut(sid, |t| {
                let sel = t.text_selection_mut();
                if !sel.has_selection() || sel.is_empty() {
                    return None;
                }
                let st = sel.start();
                let side = if (r, c) < (st.row, st.col) {
                    SelectionSide::Left
                } else {
                    SelectionSide::Right
                };
                sel.extend_selection(r, c, side);
                sel.complete_selection();
                Some(frame_open(t))
            });
            match extended {
                Some(None) => return "ERR no selection\n".to_string(),
                extended => extended.flatten(),
            }
        }
        SelectCmd::Range(r1, c1, r2, c2) => {
            let ((sr, sc), (er, ec)) = if (r2, c2) < (r1, c1) {
                ((r2, c2), (r1, c1))
            } else {
                ((r1, c1), (r2, c2))
            };
            host.with_terminal_mut(sid, |t| {
                let sel = t.text_selection_mut();
                sel.start_selection(sr, sc, SelectionSide::Left, SelectionType::Simple);
                sel.update_selection(er, ec, SelectionSide::Right);
                sel.complete_selection();
                frame_open(t)
            })
        }
    };
    let Some(open) = open else {
        return NO_SESSION.to_string();
    };
    host.request_redraw(sid);
    format!("OK{open}\n")
}

/// `selection`: return the selected text.
pub fn cmd_selection(host: &impl SessionHost, sid: u64) -> String {
    let Some(((text, truncated), open)) = host.with_terminal(sid, |t: &Terminal| {
        (t.selection_to_string_bounded(), frame_open(t))
    }) else {
        return NO_SESSION.to_string();
    };
    match text {
        Some(text) if !text.is_empty() => {
            let lines: Vec<&str> = text.split('\n').collect();
            let incomplete = if truncated { " incomplete" } else { "" };
            let mut out = format!("OK {}{incomplete}{open}\n", lines.len());
            for l in lines {
                out.push_str(l);
                out.push('\n');
            }
            out
        }
        _ => format!("OK 0{open}\n"),
    }
}

/// `copy`: copy the selected text to the system clipboard.
pub fn cmd_copy(host: &impl SessionHost, sid: u64) -> String {
    if !host.capabilities().clipboard {
        return "ERR unsupported\n".to_string();
    }
    let Some(((text, truncated), open)) = host.with_terminal(sid, |t: &Terminal| {
        (t.selection_to_string_bounded(), frame_open(t))
    }) else {
        return NO_SESSION.to_string();
    };
    match text {
        Some(t) if !t.is_empty() => {
            if host.clipboard_set(&t) {
                let incomplete = if truncated { " incomplete" } else { "" };
                format!("OK {}{incomplete}{open}\n", t.len())
            } else {
                "ERR pbcopy failed\n".to_string()
            }
        }
        _ => format!("OK 0{open}\n"),
    }
}

/// Click gestures against soft wraps. A soft-wrapped line is one logical line
/// to the copy path, so a gesture bound to the physical row would highlight
/// edge-to-edge while copying a prefix.
#[cfg(test)]
#[path = "selection_gesture_tests.rs"]
mod gesture_tests;

/// `blocktext`'s framing, and where it runs relative to the terminal lock.
#[cfg(test)]
#[path = "selection_blocktext_tests.rs"]
mod blocktext_tests;
