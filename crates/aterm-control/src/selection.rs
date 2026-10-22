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
use aterm_core::terminal::{Terminal, smart_word_in_cells};

use crate::host::SessionHost;
use crate::wire::{
    VisibleTrim, WAIT_DEFAULT_MS, json_ok, json_str_field, pct_encode_into, push_visible,
    trimmed_len, wait_ms,
};

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
/// unknown or the block has no output yet. [`blocktext`] with no trim.
pub fn cmd_blocktext(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    let Ok(id) = rest.trim().parse::<u64>() else {
        return "ERR usage: blocktext <id>\n".to_string();
    };
    blocktext(host, sid, id, false)
}

/// Block `id`'s output as `blocktext` answers it: `OK <n>\n` and each output
/// line with its control chars collapsed and its tail trimmed
/// ([`VisibleTrim::Whitespace`]); with `trim`, `OK <sent> trimmed=<k>\n` and only
/// the rows before the blank tail ([`trimmed_len`]). The terminal is held only to
/// copy the block's text out; the collapse, the count and the framing run after
/// it is released, so a block of thousands of rows does not stall the PTY
/// reader while it is framed.
pub fn blocktext(host: &impl SessionHost, sid: u64, id: u64, trim: bool) -> String {
    match host.with_terminal(sid, |t: &Terminal| block_text(t, id)) {
        None => NO_SESSION.to_string(),
        Some(Err(refusal)) => refusal.to_string(),
        Some(Ok(text)) => frame_block_text(&text, trim),
    }
}

/// Block `id`'s raw output text, or the reply line that refuses it. The enum
/// `block_output_text` answers is an explicit signal for an evicted block,
/// never silently shifted or empty text.
fn block_text(t: &Terminal, id: u64) -> Result<String, &'static str> {
    use aterm_core::terminal::BlockText;
    let block = t.block_by_id(id).ok_or("ERR no such block\n")?;
    match t.block_output_text(block) {
        BlockText::Text(text) => Ok(text),
        BlockText::Evicted => Err("ERR block output evicted from scrollback\n"),
        BlockText::NotAvailable => Err("ERR block has no output yet\n"),
    }
}

/// Frame block output `text` (see [`blocktext`]). Runs with no lock held. The
/// blank tail is counted on the raw lines ([`trimmed_len`] reads a raw line as
/// its shown form), so the header is written first and every row once.
fn frame_block_text(text: &str, trim: bool) -> String {
    use std::fmt::Write as _;
    #[cfg(test)]
    blocktext_tests::FRAMED.with(|n| n.set(n.get() + 1));
    let rows = text.lines().count();
    let sent = if trim {
        trimmed_len(text.lines())
    } else {
        rows
    };
    // The framed rows are never longer than `text` and a closing '\n'.
    let mut out = String::with_capacity(text.len() + 64);
    let _ = write!(out, "OK {sent}");
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
/// (default 30 000 ms, capped at 600 000; any other tail is the usage line, per
/// [`wait_ms`]). Run a command, `wait`, then read with `blocktext`. Needs shell
/// integration (OSC 133); without it, times out.
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
/// A session that ends first answers `ERR exited`, like every await.
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
        // The session ended: nothing more can complete. A completion it made
        // first was answered just above.
        if host.session_ended(sid) {
            return "ERR exited\n".to_string();
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
/// [`Terminal::line_range_cells`](aterm_core::terminal::Terminal::line_range_cells),
/// soft-wrapped rows joined with the cells each was read from, so a word
/// straddling the wrap is one word and the returned cells may sit on
/// different rows. A hard newline still bounds the run. The click and the
/// word are mapped through those cells
/// ([`smart_word_in_cells`](aterm_core::terminal::smart_word_in_cells)), never
/// by re-measuring the text, so the word is selected in the cells the grid
/// drew it in.
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
        // A click right of the text is no cell: `None`, as on whitespace.
        let (bytes, columns) = smart_word_in_cells(smart_rules(), &text, &cells, global)?;
        // A match touching the window edge may continue past it. Widen and re-ask, unless
        // the edge IS the logical line's end, where there is nothing more to read.
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
        highlighted_text_from(term, 0)
    }

    /// [`highlighted_text`] from selection-space row `first_row` (negative:
    /// scrollback) down to the screen's last row.
    fn highlighted_text_from(term: &Terminal, first_row: i32) -> String {
        let cols = term.cols();
        let rows = i32::from(term.rows());
        let mut out = String::new();
        let mut first = true;
        for row in first_row..rows {
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

    /// A double-click on a word after an Indic conjunct selects that word. The
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

    /// With `ambiguous_width = "wide"` the grid draws an East Asian Ambiguous
    /// character (`→`) in two cells, and a double-click measures the line the
    /// same way: a click on any cell of `foo` (cells 4-6) selects `foo`, on the
    /// screen and from scrollback.
    #[test]
    fn a_double_click_measures_ambiguous_characters_as_the_grid_draws_them() {
        let mut term = Terminal::new(4, 20);
        term.apply_config(&aterm_core::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        term.process("→→foo bar\r\n".as_bytes());
        assert!(
            term.grid().is_wide_continuation_at(0, 3),
            "precondition: the second arrow takes cells 2-3"
        );
        for col in 4..=6 {
            assert_eq!(
                select_word(&mut term, 0, col),
                ((0, 4), (0, 6)),
                "col {col}"
            );
            assert_eq!(term.selection_to_string().as_deref(), Some("foo"));
        }
        assert_eq!(select_word(&mut term, 0, 8), ((0, 8), (0, 10)));
        assert_eq!(term.selection_to_string().as_deref(), Some("bar"));

        term.process(b"1\r\n2\r\n3\r\n4");
        let row = (1..=4)
            .map(|n| -n)
            .find(|&r| {
                term.get_line_text(r, None)
                    .is_some_and(|t| t.ends_with("foo bar"))
            })
            .expect("the line reached scrollback");
        assert_eq!(select_word(&mut term, row, 5), ((row, 4), (row, 6)));
        assert_eq!(term.selection_to_string().as_deref(), Some("foo"));

        // A word straddling a wrap after ambiguous characters: four arrows and
        // `ab` fill row 0's ten cells, so `abcd` is one word across the wrap.
        let mut term = Terminal::new(4, 10);
        term.apply_config(&aterm_core::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        term.process("→→→→abcd efgh".as_bytes());
        assert!(term.row_continues_previous(1), "precondition: a soft wrap");
        assert_eq!(select_word(&mut term, 1, 0), ((0, 8), (1, 1)));
        assert_eq!(term.selection_to_string().as_deref(), Some("abcd"));
    }

    /// A double-click on a wrapped run selects the word in the cells the grid
    /// drew it in when a row holds a VS16 base the grid kept one cell wide in
    /// its last column (it has no room for the spacer), on the screen and from
    /// scrollback. Re-measuring the row's text would count that base as two
    /// columns and shift every later word one cell left.
    #[test]
    fn a_word_click_after_a_last_column_vs16_base_selects_the_word_drawn_there() {
        let mut term = Terminal::new(4, 10);
        term.process("abcd efgh\u{2714}\u{fe0f}ij klm".as_bytes());
        assert_eq!(
            term.cell_grapheme(0, 9).as_deref(),
            Some("\u{2714}\u{fe0f}"),
            "precondition: the base and its VS16 are in the last column"
        );
        assert!(term.row_continues_previous(1), "precondition: a soft wrap");
        assert_eq!(term.cell_grapheme(1, 0).as_deref(), Some("i"));
        assert_eq!(term.cell_grapheme(1, 3).as_deref(), Some("k"));
        let words = |row: i32| {
            [
                ((row - 1, 5), ((row - 1, 5), (row - 1, 8)), "efgh"),
                ((row, 0), ((row, 0), (row, 1)), "ij"),
                ((row, 1), ((row, 0), (row, 1)), "ij"),
                ((row, 3), ((row, 3), (row, 5)), "klm"),
                ((row, 5), ((row, 3), (row, 5)), "klm"),
            ]
        };
        for ((row, col), span, word) in words(1) {
            assert_eq!(
                select_word(&mut term, row, col),
                span,
                "click at ({row}, {col})"
            );
            assert_eq!(term.selection_to_string().as_deref(), Some(word));
            assert_eq!(highlighted_text(&term), word);
        }
        // The same run from scrollback, where a row is drawn from its stored
        // line: the base is still one cell in the last column.
        term.process(b"\r\n1\r\n2\r\n3\r\n4");
        assert_eq!(term.get_line_text(-1, None).as_deref(), Some("ij klm"));
        assert!(term.row_continues_previous(-1), "precondition: a soft wrap");
        for ((row, col), span, word) in words(-1) {
            assert_eq!(
                select_word(&mut term, row, col),
                span,
                "click at ({row}, {col})"
            );
            assert_eq!(term.selection_to_string().as_deref(), Some(word));
        }
    }

    /// A double-click selects the word in the cells the grid drew it in when
    /// two regional indicators were drawn into neighbouring cells out of order:
    /// the grid folds a flag half only into the cell before the cursor, so they
    /// stay two cells, two columns each, where a walk of the row's text pairs
    /// them into one two-column flag.
    #[test]
    fn a_word_click_after_regional_indicators_drawn_apart_selects_the_word_drawn_there() {
        let mut term = Terminal::new(4, 10);
        term.process("\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5Hwx yzpqr st".as_bytes());
        assert_eq!(term.cell_grapheme(0, 0).as_deref(), Some("\u{1f1e6}"));
        assert_eq!(
            term.cell_grapheme(0, 2).as_deref(),
            Some("\u{1f1e7}"),
            "precondition: the flag halves stay two cells"
        );
        assert_eq!(term.cell_grapheme(0, 4).as_deref(), Some("w"));
        assert_eq!(term.cell_grapheme(0, 9).as_deref(), Some("p"));
        assert!(term.row_continues_previous(1), "precondition: a soft wrap");
        assert_eq!(term.cell_grapheme(1, 0).as_deref(), Some("q"));
        for ((row, col), span, word) in [
            ((0, 4), ((0, 4), (0, 5)), "wx"),
            ((0, 5), ((0, 4), (0, 5)), "wx"),
            ((0, 7), ((0, 7), (1, 1)), "yzpqr"),
            ((0, 9), ((0, 7), (1, 1)), "yzpqr"),
            ((1, 0), ((0, 7), (1, 1)), "yzpqr"),
            ((1, 4), ((1, 3), (1, 4)), "st"),
        ] {
            assert_eq!(
                select_word(&mut term, row, col),
                span,
                "click at ({row}, {col})"
            );
            assert_eq!(term.selection_to_string().as_deref(), Some(word));
            assert_eq!(highlighted_text(&term), word);
        }
        // A click on a flag half is no word: just that cell.
        assert!(word_span(&term, 0, 2).is_none());

        // The same run from scrollback, on the ring tier, which draws a row
        // from its own cells: the halves are still two cells there.
        term.process(b"\r\n1\r\n2\r\n3\r\n4");
        assert_eq!(term.get_line_text(-1, None).as_deref(), Some("qr st"));
        assert!(term.row_continues_previous(-1), "precondition: a soft wrap");
        term.scroll_display(2);
        assert_eq!(
            term.display_cell_grapheme(0, 2).as_deref(),
            Some("\u{1f1e7}"),
            "precondition: the ring tier draws the halves as two cells"
        );
        assert_eq!(term.display_cell_grapheme(0, 4).as_deref(), Some("w"));
        term.scroll_to_bottom();
        for ((row, col), span, word) in [
            ((-2, 4), ((-2, 4), (-2, 5)), "wx"),
            ((-2, 5), ((-2, 4), (-2, 5)), "wx"),
            ((-2, 7), ((-2, 7), (-1, 1)), "yzpqr"),
            ((-2, 9), ((-2, 7), (-1, 1)), "yzpqr"),
            ((-1, 0), ((-2, 7), (-1, 1)), "yzpqr"),
            ((-1, 4), ((-1, 3), (-1, 4)), "st"),
        ] {
            assert_eq!(
                select_word(&mut term, row, col),
                span,
                "scrollback click at ({row}, {col})"
            );
            assert_eq!(term.selection_to_string().as_deref(), Some(word));
        }
        assert!(word_span(&term, -2, 2).is_none());
    }

    /// A word double-clicked in a history row written before a live switch
    /// to `ambiguous_width = "wide"` copies the word drawn there. The row
    /// keeps the cells it was drawn in (two one-cell arrows, `foo` in cells
    /// 2-4).
    #[test]
    fn a_word_copied_from_history_after_a_live_ambiguous_switch_is_the_word_drawn_there() {
        let mut term = Terminal::new(4, 20);
        term.process("→→foo bar".as_bytes());
        term.apply_config(&aterm_core::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        });
        term.process(b"\r\n1\r\n2\r\n3\r\n4");
        assert_eq!(term.get_line_text(-1, None).as_deref(), Some("→→foo bar"));
        term.scroll_display(1);
        assert_eq!(
            term.display_cell_grapheme(0, 2).as_deref(),
            Some("f"),
            "precondition: the row keeps the cells it was drawn in"
        );
        term.scroll_to_bottom();
        for col in 2..=4 {
            assert_eq!(
                select_word(&mut term, -1, col),
                ((-1, 2), (-1, 4)),
                "col {col}"
            );
            assert_eq!(term.selection_to_string().as_deref(), Some("foo"));
            assert_eq!(highlighted_text_from(&term, -1), "foo");
        }
        assert_eq!(select_word(&mut term, -1, 6), ((-1, 6), (-1, 8)));
        assert_eq!(term.selection_to_string().as_deref(), Some("bar"));
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

/// `blocktext`'s framing, and where it runs relative to the terminal lock.
#[cfg(test)]
mod blocktext_tests {
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

    /// Two-step reference framing: every line collapsed and `trim_end`ed, and
    /// `trim` drops rows that are blank by a spaces-only test.
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

    /// Each row is collapsed and trimmed, `trim` drops exactly the blank tail,
    /// and every reply is the one the two-step framing gave, byte for byte.
    /// Negative control: counting the tail on the raw lines with the spaces-only
    /// test would keep rows that show nothing.
    #[test]
    fn blocktext_frames_each_row_and_trims_the_blank_tail() {
        let text = "out\x01\n\u{a0}\n\x01 \t\n\n";
        assert_eq!(frame_block_text(text, false), "OK 4\nout\n\n\n\n");
        assert_eq!(frame_block_text(text, true), "OK 1 trimmed=3\nout\n");
        assert_eq!(frame_block_text("", false), "OK 0\n");
        assert_eq!(frame_block_text("", true), "OK 0 trimmed=0\n");
        assert_eq!(
            frame_block_text("a\r\nb \u{9b} \r\n", false),
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
                    frame_block_text(sample, trim),
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

    /// [`MemoryHost`] that records whether a block reply was framed while it
    /// held the terminal.
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

    /// `blocktext` frames its reply after the host has released the terminal,
    /// with or without `trim`; a refusal frames nothing. Negative control: the
    /// same framing called inside the host's closure is seen inside.
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
            frame_block_text(&block_text(t, 0).expect("the block has output"), false)
        });
        assert_eq!(inside.as_deref(), Some(whole.as_str()));
        assert_eq!(
            host.framed_inside.get(),
            1,
            "control: framing in the closure is seen"
        );
    }
}
