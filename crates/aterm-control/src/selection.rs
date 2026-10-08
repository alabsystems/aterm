// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selection / copy / block-aware verbs: `select` (plain ranges plus the
//! `word`/`line`/`block`/`extend` gestures), `selection`, `copy`, the OSC-133
//! command-block verbs (`blocks`/`blocktext`) and `wait`, over [`SessionHost`].
//!
//! The gesture helpers (`word_span`/`select_word`/`select_line`) take a bare
//! `&mut Terminal`: the GUI's double/triple-click calls them directly off its
//! lock guard, sharing one rule set with the `select word` verb. They work in
//! logical lines (soft-wrapped rows joined), so a gesture never stops at the
//! window width, just as the copy path inserts no newline there.

use std::sync::OnceLock;

use aterm_core::selection::{SelectionSide, SelectionType, SmartSelection};
use aterm_core::terminal::Terminal;

use crate::host::SessionHost;
use crate::wire::{json_ok, json_str_field, pct_encode_into, visible_char};

/// The host does not resolve the target sid. Unreachable from a dispatcher that
/// resolves the session before dispatch (aterm-gui's does); it exists so a
/// multi-session host cannot answer a stale sid with another session's state.
const NO_SESSION: &str = "ERR no such session\n";

/// The `blocks [N]` argument: the last `N` blocks, or all of them when `N` is
/// absent, unparsable or not smaller than the count.
fn last_n<'a, T>(all: &'a [T], rest: &str) -> &'a [T] {
    match rest.trim().parse::<usize>() {
        Ok(n) if n < all.len() => &all[all.len() - n..],
        _ => all,
    }
}

/// A block state's wire word.
fn state_word(state: aterm_core::terminal::BlockState) -> &'static str {
    use aterm_core::terminal::BlockState;
    match state {
        BlockState::PromptOnly => "prompt",
        BlockState::EnteringCommand => "entering",
        BlockState::Executing => "executing",
        BlockState::Complete => "complete",
        _ => "unknown",
    }
}

/// `blocks [N] --json` -> `{"blocks":[{...}]}`: the same OSC 133/633 command
/// blocks as `cmd_blocks` (oldest-first, optional last-N), one object per block
/// with absolute rows, exit code, state, cwd and commandline. Absent rows are
/// `null`; cwd/commandline are plain JSON strings, the cwd in the host's native
/// form ([`SessionHost::native_cwd`]). `"cmdcol"` (after `"cmd"`, `null` with
/// it) is the column the shell's `133;B` marked as the input start, so a reader
/// can tell the prompt from typed text (used by `harness::upgrade_drive` to
/// avoid a person's typeahead).
pub fn cmd_blocks_json(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    let Some(items) = host.with_terminal(sid, |t: &Terminal| {
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

/// `blocks [N]` -> the shell-integration command blocks (OSC 133/633), oldest
/// first (or the last `N`): exit codes, absolute output row ranges, command
/// text and cwd, so an AI navigates by command instead of scraping the screen.
///
/// Every `prompt`/`cmd`/`out`/`end` row is a monotonic absolute row, the one
/// read coordinate this socket uses; feed it to `line <abs_row>` or `text`. For
/// a block's full output prefer `blocktext <id>`, which reads the range itself
/// and answers an explicit `ERR` when rows were evicted from scrollback.
///
/// Header `OK <shown>\n`, then one line per block: `block <id> <state>
/// exit=<code|-> prompt=<row> cmd=<row|-> out=<row|-> end=<row|-> cwd=<pct>
/// cmdline=<pct>`. `state` is prompt|entering|executing|complete; cwd/cmdline
/// are percent-encoded single tokens, the cwd the host's native path
/// ([`SessionHost::native_cwd`]). Needs a shell emitting OSC 133 (see the
/// `shell_integration` injection); empty otherwise.
pub fn cmd_blocks(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    use std::fmt::Write as _;
    host.with_terminal(sid, |t: &Terminal| {
        let all: Vec<_> = t.all_blocks().collect();
        let slice = last_n(&all, rest);
        let mut out = format!("OK {}\n", slice.len());
        let opt_row = |r: Option<u64>| r.map_or_else(|| "-".to_string(), |v| v.to_string());
        for b in slice {
            let exit = b
                .exit_code
                .map_or_else(|| "-".to_string(), |c| c.to_string());
            // Written directly into the response buffer, with no per-block
            // `String`: this loop runs up to `OUTPUT_BLOCKS_MAX` (1000) times
            // under the terminal lock the render path contends for.
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

/// `blocktext <id>` -> the output text of command block `<id>` (from
/// `blocks`), one row per line after `OK <n>`. The engine reads the block's
/// absolute row range itself, across scrollback and screen. `ERR` if the id is
/// unknown or the block has no output yet.
pub fn cmd_blocktext(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    let Ok(id) = rest.trim().parse::<u64>() else {
        return "ERR usage: blocktext <id>\n".to_string();
    };
    host.with_terminal(sid, |t: &Terminal| {
        let Some(block) = t.block_by_id(id).cloned() else {
            return "ERR no such block\n".to_string();
        };
        // The enum form returns an explicit signal for an evicted block instead
        // of silently-shifted or empty text.
        let text = match t.block_output_text(&block) {
            aterm_core::terminal::BlockText::Text(s) => s,
            aterm_core::terminal::BlockText::Evicted => {
                return "ERR block output evicted from scrollback\n".to_string();
            }
            aterm_core::terminal::BlockText::NotAvailable => {
                return "ERR block has no output yet\n".to_string();
            }
        };
        let lines: Vec<&str> = text.lines().collect();
        let mut out = format!("OK {}\n", lines.len());
        for line in lines {
            let s: String = line.chars().map(visible_char).collect();
            out.push_str(s.trim_end());
            out.push('\n');
        }
        out
    })
    .unwrap_or_else(|| NO_SESSION.to_string())
}

/// The newest completed block (ids are monotonic and blocks iterate
/// oldest-first; an id baseline, unlike a count, survives eviction) plus
/// whether any block is still in flight.
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

/// `wait [timeout_ms]` -> the newest completed command block as
/// `OK complete <id> exit=<code|->`; `OK timeout` if nothing completes in time
/// (default 30 000 ms, capped at 600 000). Run a command, `wait`, then read
/// with `blocktext`. Needs shell integration (OSC 133); without it, times out.
///
/// Entry race: a fast command can complete between the submit and this call.
/// So with no block executing and a completed block present, the newest is
/// returned immediately; with one executing, `wait` waits for a completion
/// newer than the entry snapshot. In-flight means `Executing` only: the shell
/// emits OSC 133;B at the end of every prompt, so an idle prompt sits in
/// `EnteringCommand`, and counting it would make the immediate arm
/// unreachable.
///
/// Event-driven: registers a subscriber and parks on its wake (every output
/// burst notifies), re-checking on each wake; the deadline is the backstop.
pub fn cmd_wait(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    let timeout_ms = rest.trim().parse::<u64>().unwrap_or(30_000).min(600_000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let reply = |id: u64, exit: Option<i32>| {
        let exit = exit.map_or_else(|| "-".to_string(), |c| c.to_string());
        format!("OK complete {id} exit={exit}\n")
    };
    let Some((entry_done, in_flight)) = host.with_terminal(sid, scan_blocks) else {
        return NO_SESSION.to_string();
    };
    if !in_flight && let Some((id, exit)) = entry_done {
        return reply(id, exit);
    }
    let baseline = entry_done.map(|(id, _)| id);
    // Register before re-checking: a completion landing in this gap still leaves
    // its single-slot notify pending, so the first `wait` below cannot miss it.
    let sub = host.subscribe(sid);
    loop {
        // After the entry check only the newest completion matters. This runs
        // once per output burst under the terminal lock, so ask the engine for
        // the newest `Complete` directly rather than walking every block; it is
        // the same block `scan_blocks` would return.
        let Some(newest) = host.with_terminal(sid, |t: &Terminal| {
            t.newest_completed_block().map(|b| (b.id, b.exit_code))
        }) else {
            return NO_SESSION.to_string();
        };
        if let Some((id, exit)) = newest
            && baseline.is_none_or(|b| id > b)
        {
            return reply(id, exit);
        }
        // The caller hung up: nobody can read the completion, so stop now.
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

/// Process-wide smart-selection rules, built lazily once (the builtin rules
/// compile regexes). Shared by the GUI's double-click and `select word`.
static SMART_RULES: OnceLock<SmartSelection> = OnceLock::new();

/// The engine's builtin smart-selection rules (lazy singleton).
fn smart_rules() -> &'static SmartSelection {
    SMART_RULES.get_or_init(SmartSelection::with_builtin_rules)
}

/// Rows of a wrapped line joined on each side of a click before the word rules
/// run, doubling while the match still reaches a window edge. Two rows each way
/// holds nearly every word a person double-clicks, so the common case is not
/// linear in the whole logical line (see [`word_span`]).
const WORD_WINDOW_ROWS: usize = 2;

/// The widest a double-click looks, in rows of the wrapped line.
///
/// A line can be one unbroken token (a minified bundle), and unbounded growth
/// would read all of it twice per pointer move under the terminal mutex. 64
/// rows (~5 kB at 80 columns) exceeds any identifier, URL, path or hash and
/// scans in under a millisecond; a longer token is selected up to the bound.
/// Drag and triple-click still reach the whole logical line.
const WORD_WINDOW_MAX_ROWS: usize = 64;

/// Inclusive word cell bounds `((start_row, start_col), (end_row, end_col))` at
/// live-screen `(row, col)`, from the engine's builtin smart-selection rules
/// (URL/path/email/... patterns, falling back to alphanumeric+underscore
/// words). `None` on whitespace or right of the line's text; the caller then
/// selects just the clicked cell.
///
/// The rules run over
/// [`Terminal::logical_line_text`](aterm_core::terminal::Terminal::logical_line_text),
/// soft-wrapped rows joined and column-aligned, so a word straddling the wrap
/// is one word and the returned cells may sit on different rows. A hard
/// newline still bounds the run.
#[must_use]
pub fn word_span(t: &Terminal, row: i32, col: u16) -> Option<((i32, u16), (i32, u16))> {
    let cols = usize::from(t.cols());
    if cols == 0 {
        return None;
    }
    // Windowed, not whole-line: the rules scan is linear in its input, and a
    // logical line can be huge while the drag arm calls this twice per pointer
    // move under the terminal mutex. A word is delimited by whitespace, so read
    // a small window and widen only when the match reaches an edge that is not
    // the line's own end.
    let (line_first, line_last) = t.logical_line_span(row);
    let mut back = WORD_WINDOW_ROWS;
    let mut fwd = WORD_WINDOW_ROWS;
    let (first_row, start, end) = loop {
        let win_first = row
            .saturating_sub(i32::try_from(back).unwrap_or(i32::MAX))
            .max(line_first);
        let win_last = row
            .saturating_add(i32::try_from(fwd).unwrap_or(i32::MAX))
            .min(line_last);
        let text = t.line_range_text(win_first, win_last)?;
        let offset = usize::try_from(row.checked_sub(win_first)?).ok()?;
        let global = offset.checked_mul(cols)?.checked_add(usize::from(col))?;
        // A click right of the text is an empty cell: `None` from the rules.
        let (start, end) = smart_rules().word_boundaries_at_column(&text, global)?;
        // A match touching the window edge may continue past it. Widen and re-ask, unless
        // the edge IS the logical line's end, where there is nothing more to read.
        let open_up = start == 0 && win_first > line_first;
        let open_down =
            end >= aterm_core::grapheme::byte_to_column(&text, text.len()) && win_last < line_last;
        if !open_up && !open_down {
            break (win_first, start, end);
        }
        if back.saturating_add(fwd) >= WORD_WINDOW_MAX_ROWS {
            break (win_first, start, end);
        }
        if open_up {
            back = back.saturating_mul(2);
        }
        if open_down {
            fwd = fwd.saturating_mul(2);
        }
    };
    // The returned end column is exclusive; selection anchors are inclusive cells.
    let last = end.saturating_sub(1).max(start);
    let cell = |g: usize| -> (i32, u16) {
        let dr = i32::try_from(g / cols).unwrap_or(i32::MAX);
        let c = u16::try_from(g % cols).unwrap_or(u16::MAX);
        (first_row.saturating_add(dr), c)
    };
    Some((cell(start), cell(last)))
}

/// Word-select at live-screen `(row, col)` (double-click / `select word`): a
/// `Semantic` selection over the word's cells (inclusive, Left/Right anchor
/// sides), or just the clicked cell on whitespace. Completes the selection and
/// returns the inclusive bounds selected, which span two rows when the word
/// straddles a soft wrap.
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

/// Line-select the logical line at live-screen row `row` (triple-click /
/// `select line`): a `Lines` selection from column 0 of its first physical row
/// to the last column of its last, so a soft-wrapped line is selected whole.
/// Completes the selection and returns the inclusive `(first_row, last_row)`.
///
/// The span comes from
/// [`Terminal::logical_line_span`](aterm_core::terminal::Terminal::logical_line_span),
/// the same soft-wrap bit the copy walk uses, so the text is the whole logical
/// line with no newline at the wrap, and a hard newline still ends it.
pub fn select_line(t: &mut Terminal, row: i32) -> (i32, i32) {
    let (first, last) = t.logical_line_span(row);
    let max_col = t.cols().saturating_sub(1);
    let sel = t.text_selection_mut();
    sel.start_selection(first, 0, SelectionSide::Left, SelectionType::Lines);
    sel.update_selection(last, max_col, SelectionSide::Right);
    sel.complete_selection();
    (first, last)
}

/// `select ...` -> drive the engine's text selection. Forms:
///
/// * `select <r1> <c1> <r2> <c2>` — simple range from cell `(r1,c1)` to
///   `(r2,c2)`, both endpoint cells inclusive (normalized to reading order, so
///   either order works).
/// * `select word <r> <c>` — semantic (word/URL/path) selection via the
///   builtin smart-selection rules; a whitespace cell selects itself. Same code
///   as the GUI's double-click, so a word straddling a soft wrap is selected
///   whole.
/// * `select line <r>` — logical-line selection at row `r` (triple-click): a
///   soft-wrapped line from its first physical row through its last; a hard
///   newline ends it.
/// * `select block <r1> <c1> <r2> <c2>` — rectangular (block) selection with
///   the two cells as inclusive corners (any corner order).
/// * `select extend <r> <c>` — extend the existing selection so `(r,c)` is its
///   new inclusive endpoint (shift-click); `ERR no selection` when nothing is
///   selected.
/// * `select clear` — clear the selection.
///
/// Rows are live-screen coords as signed integers: `0..rows` is the visible
/// screen and negative rows address scrollback (`-1` = most recently
/// scrolled-off). All forms nudge a windowed session to repaint and reply
/// `OK\n`.
///
/// `select` mutates `text_selection_mut()` directly rather than through the
/// host's input seam: it sets absolute coordinates and produces no PTY bytes,
/// and no human event produces an absolute-coordinate selection (the human path
/// is press → drag → release through the seam's gesture arms).
pub fn cmd_select(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    const USAGE: &str = "ERR usage: select <r1> <c1> <r2> <c2> | select word <r> <c> | \
                         select line <r> | select block <r1> <c1> <r2> <c2> | \
                         select extend <r> <c> | select clear\n";
    let rest = rest.trim();
    if rest == "clear" {
        if host
            .with_terminal_mut(sid, |t| t.text_selection_mut().clear())
            .is_none()
        {
            return NO_SESSION.to_string();
        }
        host.request_redraw(sid);
        return "OK\n".to_string();
    }
    let mut it = rest.split_whitespace();
    let Some(head) = it.next() else {
        return USAGE.to_string();
    };
    match head {
        "word" => {
            let (Some(Ok(r)), Some(Ok(c))) = (
                it.next().map(str::parse::<i32>),
                it.next().map(str::parse::<u16>),
            ) else {
                return "ERR usage: select word <r> <c>\n".to_string();
            };
            if host
                .with_terminal_mut(sid, |t| {
                    select_word(t, r, c);
                })
                .is_none()
            {
                return NO_SESSION.to_string();
            }
        }
        "line" => {
            let Some(Ok(r)) = it.next().map(str::parse::<i32>) else {
                return "ERR usage: select line <r>\n".to_string();
            };
            if host.with_terminal_mut(sid, |t| select_line(t, r)).is_none() {
                return NO_SESSION.to_string();
            }
        }
        "block" => {
            let (Some(Ok(r1)), Some(Ok(c1)), Some(Ok(r2)), Some(Ok(c2))) = (
                it.next().map(str::parse::<i32>),
                it.next().map(str::parse::<u16>),
                it.next().map(str::parse::<i32>),
                it.next().map(str::parse::<u16>),
            ) else {
                return "ERR usage: select block <r1> <c1> <r2> <c2>\n".to_string();
            };
            // Block normalization is corner-order agnostic (min/max per axis)
            // and forces Left/Right sides on the normalized corners, so both
            // given cells are inclusive whichever corners they are.
            if host
                .with_terminal_mut(sid, |t| {
                    let sel = t.text_selection_mut();
                    sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Block);
                    sel.update_selection(r2, c2, SelectionSide::Right);
                    sel.complete_selection();
                })
                .is_none()
            {
                return NO_SESSION.to_string();
            }
        }
        "extend" => {
            let (Some(Ok(r)), Some(Ok(c))) = (
                it.next().map(str::parse::<i32>),
                it.next().map(str::parse::<u16>),
            ) else {
                return "ERR usage: select extend <r> <c>\n".to_string();
            };
            let extended = host.with_terminal_mut(sid, |t| {
                let sel = t.text_selection_mut();
                if !sel.has_selection() || sel.is_empty() {
                    return false;
                }
                // Side by direction so the clicked cell is included: extending
                // backward the moving anchor is the start (Left includes its
                // cell), forward it is the end (Right includes its cell).
                let st = sel.start();
                let side = if (r, c) < (st.row, st.col) {
                    SelectionSide::Left
                } else {
                    SelectionSide::Right
                };
                sel.extend_selection(r, c, side);
                sel.complete_selection();
                true
            });
            match extended {
                None => return NO_SESSION.to_string(),
                Some(false) => return "ERR no selection\n".to_string(),
                Some(true) => {}
            }
        }
        r1s => {
            let (Some(c1s), Some(r2s), Some(c2s)) = (it.next(), it.next(), it.next()) else {
                return USAGE.to_string();
            };
            let (Ok(r1), Ok(c1), Ok(r2), Ok(c2)) = (
                r1s.parse::<i32>(),
                c1s.parse::<u16>(),
                r2s.parse::<i32>(),
                c2s.parse::<u16>(),
            ) else {
                return "ERR bad args\n".to_string();
            };
            // Normalize to reading order so the Left/Right anchor sides make
            // both endpoint cells inclusive.
            let ((sr, sc), (er, ec)) = if (r2, c2) < (r1, c1) {
                ((r2, c2), (r1, c1))
            } else {
                ((r1, c1), (r2, c2))
            };
            if host
                .with_terminal_mut(sid, |t| {
                    let sel = t.text_selection_mut();
                    sel.start_selection(sr, sc, SelectionSide::Left, SelectionType::Simple);
                    sel.update_selection(er, ec, SelectionSide::Right);
                    sel.complete_selection();
                })
                .is_none()
            {
                return NO_SESSION.to_string();
            }
        }
    }
    host.request_redraw(sid);
    "OK\n".to_string()
}

/// `selection` -> the selected text as `OK <n>[ incomplete]\n` + `n` data lines
/// (split on newlines, like `text`). No or empty selection -> `OK 0\n`.
///
/// A trailing ` incomplete` (as in `cmd_search`) marks selected content missing
/// from the reply: the copy caps (`MAX_SELECTION_ROWS`/`MAX_SELECTION_BYTES`)
/// cut the tail, or scrollback eviction clamped the head.
pub fn cmd_selection(host: &impl SessionHost, sid: u64) -> String {
    let Some((text, truncated)) = host.with_terminal(sid, Terminal::selection_to_string_bounded)
    else {
        return NO_SESSION.to_string();
    };
    match text {
        Some(text) if !text.is_empty() => {
            let lines: Vec<&str> = text.split('\n').collect();
            let incomplete = if truncated { " incomplete" } else { "" };
            let mut out = format!("OK {}{incomplete}\n", lines.len());
            for l in lines {
                out.push_str(l);
                out.push('\n');
            }
            out
        }
        _ => "OK 0\n".to_string(),
    }
}

/// `copy` -> copy the selected text to the system clipboard and reply
/// `OK <byte-count>[ incomplete]\n`; no or empty selection -> `OK 0\n` (the
/// clipboard is untouched). The selection is not cleared.
///
/// A host with no clipboard ([`HostCapabilities::clipboard`] unset) answers
/// `ERR unsupported` before reading the selection.
///
/// A trailing ` incomplete` means the clipboard holds part of the selection: a
/// prefix when the copy caps cut the tail, a suffix when eviction clamped the
/// head.
///
/// [`HostCapabilities::clipboard`]: crate::HostCapabilities::clipboard
pub fn cmd_copy(host: &impl SessionHost, sid: u64) -> String {
    if !host.capabilities().clipboard {
        return "ERR unsupported\n".to_string();
    }
    let Some((text, truncated)) = host.with_terminal(sid, Terminal::selection_to_string_bounded)
    else {
        return NO_SESSION.to_string();
    };
    match text {
        Some(t) if !t.is_empty() => {
            if host.clipboard_set(&t) {
                let incomplete = if truncated { " incomplete" } else { "" };
                format!("OK {}{incomplete}\n", t.len())
            } else {
                "ERR pbcopy failed\n".to_string()
            }
        }
        _ => "OK 0\n".to_string(),
    }
}

/// Click gestures against soft wraps. A soft-wrapped line is one logical line
/// to the copy path, so a gesture bound to the physical row would highlight
/// edge-to-edge while copying a prefix. Every check is on bytes (length and
/// text), not anchors.
#[cfg(test)]
mod gesture_tests {
    use aterm_core::selection::SelectionType;
    use aterm_core::terminal::Terminal;

    use super::{select_line, select_word, word_span};

    /// A 130-char command auto-wrapped across two rows of an 80-column screen.
    /// The head fills row 0 exactly and ends `aterm-build rust`; the tail is
    /// the 50 bytes a row-bound gesture would drop.
    const HEAD: &str =
        "docker run --rm -v $PWD:/w -w /w --network host --name bld-boxy aterm-build rust";
    const TAIL: &str = ":1.85 cargo build --release --features gpu,wayland";

    /// That command on row 0/1, then a separate logical line after a hard
    /// newline (the boundary the join must never cross).
    fn wrapped_command() -> (Terminal, String) {
        let cmd = format!("{HEAD}{TAIL}");
        assert_eq!(HEAD.len(), 80, "the head must fill the row exactly");
        let mut term = Terminal::new(6, 80);
        term.process(cmd.as_bytes());
        term.process(b"\r\necho AFTER");
        (term, cmd)
    }

    /// A word straddling the wrap: `STRADDLEWORD` starts at column 74 of row 0
    /// and finishes on row 1.
    fn wrapped_word() -> Terminal {
        let mut term = Terminal::new(6, 80);
        let filler = "word ".repeat(14) + "abc ";
        assert_eq!(filler.len(), 74);
        term.process(format!("{filler}STRADDLEWORD tail").as_bytes());
        term
    }

    /// Rebuild the text the highlight covers from the predicate the renderer
    /// paints with (`TextSelection::contains`), joining rows by the copy's
    /// soft-wrap rule. Every gesture is checked against this as well as the
    /// clipboard, since the paint is what the user trusts.
    fn highlighted_text(term: &Terminal) -> String {
        let cols = term.cols();
        let rows = i32::from(term.rows());
        let mut out = String::new();
        let mut first = true;
        for row in 0..rows {
            let sel = term.text_selection();
            let lo = (0..cols).find(|&c| sel.contains(row, c));
            let hi = (0..cols).rev().find(|&c| sel.contains(row, c));
            let (Some(lo), Some(hi)) = (lo, hi) else {
                continue;
            };
            if !first && !term.row_continues_previous(row) {
                out.push('\n');
            }
            first = false;
            out.push_str(&term.get_line_text(row, Some((lo, hi))).unwrap_or_default());
        }
        out
    }

    /// A triple-click on the head of a soft-wrapped command must
    /// copy the whole command, not the 80 bytes that fit on the row.
    #[test]
    fn a_line_click_selects_the_whole_soft_wrapped_logical_line() {
        let (mut term, cmd) = wrapped_command();
        let span = select_line(&mut term, 0);
        assert_eq!(span, (0, 1), "the gesture must cover both physical rows");

        let copied = term.selection_to_string().expect("a selection was made");
        assert_eq!(
            copied.len(),
            cmd.len(),
            "the clipboard must carry all {} bytes, not the {} that fit on the row",
            cmd.len(),
            HEAD.len()
        );
        assert_eq!(copied, cmd);
        assert!(
            !copied.contains('\n'),
            "a SOFT wrap must not invent a newline mid-line: {copied:?}"
        );
        assert_eq!(
            highlighted_text(&term),
            copied,
            "the highlight the user sees must be byte-for-byte what they copy"
        );
    }

    /// Clicking the continuation row reaches back to the head, so where the
    /// user clicks in a wrapped line does not change the result.
    #[test]
    fn a_line_click_on_the_continuation_row_selects_the_same_logical_line() {
        let (mut term, cmd) = wrapped_command();
        assert_eq!(select_line(&mut term, 1), (0, 1));
        let copied = term.selection_to_string().expect("a selection was made");
        assert_eq!(copied, cmd);
        assert_eq!(highlighted_text(&term), copied);
    }

    /// A hard newline still ends the selection in both directions.
    #[test]
    fn a_hard_newline_still_bounds_a_line_click() {
        let (mut term, cmd) = wrapped_command();
        select_line(&mut term, 0);
        let copied = term.selection_to_string().expect("a selection was made");
        assert!(
            !copied.contains("echo AFTER"),
            "the next LOGICAL line must not be swallowed: {copied:?}"
        );
        assert_eq!(copied, cmd);

        assert_eq!(
            select_line(&mut term, 2),
            (2, 2),
            "an unwrapped row is its own logical line"
        );
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("echo AFTER"),
            "and selecting it must not reach back over the hard newline"
        );
    }

    /// A double-click on a word straddling the wrap must select the whole word,
    /// not the `STRADD` fragment on the clicked row.
    #[test]
    fn a_word_click_selects_a_word_straddling_the_wrap_whole() {
        let mut term = wrapped_word();
        // Column 76 is inside the fragment on row 0.
        let (start, end) = select_word(&mut term, 0, 76);
        assert_eq!(
            (start, end),
            ((0, 74), (1, 5)),
            "the word's cells must span both rows"
        );
        let copied = term.selection_to_string().expect("a selection was made");
        assert_eq!(copied, "STRADDLEWORD");
        assert_eq!(highlighted_text(&term), copied);

        // Clicking the tail fragment answers the same word.
        let (start2, end2) = select_word(&mut term, 1, 2);
        assert_eq!((start2, end2), (start, end));
        assert_eq!(term.selection_to_string().as_deref(), Some("STRADDLEWORD"));
    }

    /// A double-click on a word AFTER an Indic conjunct selects that word. The
    /// conjunct `न्दी` is one cluster the grid lays out in three cells, so the
    /// window join and the past-the-text guard must charge it three cells, not
    /// its display width of one.
    #[test]
    fn a_word_click_after_an_indic_conjunct_selects_the_word() {
        let c = "\u{0928}\u{094D}\u{0926}\u{0940}";
        // Five conjuncts fill columns 0..15, a space, then `tail` at 16..20.
        let mut term = Terminal::new(6, 40);
        term.process(format!("{c}{c}{c}{c}{c} tail").as_bytes());
        assert_eq!(select_word(&mut term, 0, 17), ((0, 16), (0, 19)));
        assert_eq!(term.selection_to_string().as_deref(), Some("tail"));
        assert_eq!(highlighted_text(&term), "tail");

        // Four conjunct words fill 16 of 20 columns; `WORDXYZWORD` straddles
        // the wrap, four cells on row 0 and seven on row 1.
        let mut term = Terminal::new(6, 20);
        term.process(format!("{c} {c} {c} {c} WORDXYZWORD tail").as_bytes());
        for (row, col) in [(0, 17), (1, 2)] {
            assert_eq!(
                select_word(&mut term, row, col),
                ((0, 16), (1, 6)),
                "click at ({row}, {col})"
            );
            assert_eq!(term.selection_to_string().as_deref(), Some("WORDXYZWORD"));
            assert_eq!(highlighted_text(&term), "WORDXYZWORD");
        }
    }

    /// The same after Sinhala `ශ්‍රී`: a ZWJ ends its first UAX #29 cluster and
    /// the grid folds the next cluster's letter into the same cell, so five of
    /// them fill five cells, not the ten a per-cluster count gives.
    #[test]
    fn a_word_click_after_a_sinhala_zwj_conjunct_selects_the_word() {
        let sri = "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}";
        let mut term = Terminal::new(6, 40);
        term.process(format!("{sri}{sri}{sri}{sri}{sri} tail end").as_bytes());
        assert_eq!(select_word(&mut term, 0, 7), ((0, 6), (0, 9)));
        assert_eq!(term.selection_to_string().as_deref(), Some("tail"));
        assert_eq!(highlighted_text(&term), "tail");
    }

    /// A double-click on a word that a format control spells selects all of it:
    /// the Persian half space, the ZWJ of Bengali ya-phala, the Mongolian vowel
    /// separator and the Egyptian hieroglyph joiners sit between letters the grid
    /// draws in separate cells, and must not end the word.
    #[test]
    fn a_word_click_on_a_word_a_joiner_spells_selects_it_whole() {
        // Each word, and the last cell the grid draws it in (it starts in cell 2).
        for (word, last) in [
            (
                "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
                8,
            ),
            ("\u{09B0}\u{200D}\u{09CD}\u{09AF}\u{09BE}\u{09AC}", 5),
            ("\u{182C}\u{1820}\u{1837}\u{180E}\u{1820}", 5),
            ("\u{13000}\u{13431}\u{13001}\u{13430}\u{13002}", 4),
        ] {
            let mut term = Terminal::new(6, 40);
            term.process(format!("x {word} y").as_bytes());
            for col in 2..=last {
                assert_eq!(
                    select_word(&mut term, 0, col),
                    ((0, 2), (0, last)),
                    "{word:?} clicked at column {col}"
                );
                assert_eq!(term.selection_to_string().as_deref(), Some(word));
            }
        }
    }

    /// A word wider than `WORD_WINDOW_ROWS` rows, clicked in its middle, comes
    /// back whole: the match runs off both edges of the first window and both
    /// arms widen.
    #[test]
    fn a_word_wider_than_the_window_is_still_selected_whole() {
        let cols = 80usize;
        let rows = super::WORD_WINDOW_ROWS * 4 + 3;
        let token = "x".repeat(cols * rows);
        // The terminal must be taller than the token, or most of it scrolls into
        // history and the click row addresses something else entirely.
        let mut term = Terminal::new(
            u16::try_from(rows + 3).unwrap(),
            u16::try_from(cols).unwrap(),
        );
        term.process(format!("{token} tail").as_bytes());
        // Click in the middle row of the token, so the window opens upward and downward.
        let mid = i32::try_from(rows / 2).unwrap();
        select_word(&mut term, mid, 10);
        let picked = term.selection_to_string().expect("a selection was made");
        assert_eq!(
            picked.len(),
            token.len(),
            "a token spanning {rows} rows came back {} bytes instead of {} — the window \
             widened too little",
            picked.len(),
            token.len()
        );
        assert!(
            picked.chars().all(|c| c == 'x'),
            "the widened span picked up neighbours"
        );
    }

    /// Widening stops at the logical line's own end: a token ending exactly on
    /// the last row of a wrapped line does not reach into the next line.
    #[test]
    fn widening_stops_at_the_end_of_the_logical_line() {
        let mut term = Terminal::new(6, 80);
        // 80-col head + a tail token, then a hard newline and another word.
        term.process(format!("{}ENDTOKEN", "y".repeat(80)).as_bytes());
        term.process(b"\r\nNEXTWORD");
        select_word(&mut term, 1, 2);
        let picked = term.selection_to_string().expect("a selection was made");
        assert!(
            !picked.contains("NEXTWORD"),
            "widening walked past the hard newline into the next logical line: {picked:?}"
        );
    }

    /// An ordinary unwrapped line still selects exactly itself, word and line
    /// alike, with the same anchors.
    #[test]
    fn unwrapped_lines_and_words_are_unchanged() {
        let mut term = Terminal::new(6, 80);
        term.process(b"alpha beta gamma\r\ndelta epsilon");
        assert_eq!(select_line(&mut term, 0), (0, 0));
        assert_eq!(
            term.selection_to_string().as_deref(),
            Some("alpha beta gamma")
        );
        assert_eq!(term.text_selection().selection_type(), SelectionType::Lines);

        assert_eq!(select_word(&mut term, 0, 7), ((0, 6), (0, 9)));
        assert_eq!(term.selection_to_string().as_deref(), Some("beta"));
        assert_eq!(
            term.text_selection().selection_type(),
            SelectionType::Semantic
        );

        // A click past the end of the text still selects just that cell.
        assert_eq!(select_word(&mut term, 0, 40), ((0, 40), (0, 40)));
        assert!(word_span(&term, 0, 40).is_none());
    }
}
