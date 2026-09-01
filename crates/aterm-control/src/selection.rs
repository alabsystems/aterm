// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selection / copy / block-aware verbs: `select` (plain ranges plus the
//! `word`/`line`/`block`/`extend` gestures), `selection`, `copy`, and the
//! OSC-133 command-block verbs (`blocks`/`blocktext`) plus `wait`. Moved from
//! `aterm-gui`'s `control_selection.rs` (behavior-preserving) and re-typed
//! against [`SessionHost`], so the wire bytes are unchanged.
//!
//! The three GESTURE helpers (`word_span`/`select_word`/`select_line`) keep
//! taking a bare `&mut Terminal`: the GUI's double/triple-click calls them
//! DIRECTLY off its lock guard, and they must share one rule set with the
//! `select word` verb. All three work in LOGICAL lines — soft-wrapped rows
//! joined — so a click gesture never stops at the window width the way the
//! copy path never inserts a newline there.

use std::sync::OnceLock;

use aterm_core::selection::{SelectionSide, SelectionType, SmartSelection};
use aterm_core::terminal::Terminal;

use crate::host::SessionHost;
use crate::wire::{json_ok, json_str_field, pct_encode_into, visible_char};

/// The host does not resolve the target sid. Unreachable from a dispatcher that
/// resolves the session before dispatch (aterm-gui's does); it exists so a
/// multi-session host cannot answer a stale sid with another session's state.
const NO_SESSION: &str = "ERR no such session\n";

/// `blocks [N] --json` -> `{"blocks":[{...}]}`: the SAME OSC 133/633 command
/// blocks `cmd_blocks` reports (oldest-first, optional last-N), one JSON object
/// per block with the absolute rows, exit code, state, cwd and commandline. An
/// absent optional row is JSON `null`; the cwd/commandline are JSON strings (not
/// percent-encoded — JSON carries spaces natively).
pub fn cmd_blocks_json(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    use aterm_core::terminal::BlockState;
    let Some(items) = host.with_terminal(sid, |t: &Terminal| {
        let all: Vec<_> = t.all_blocks().collect();
        let slice: &[_] = match rest.trim().parse::<usize>() {
            Ok(n) if n < all.len() => &all[all.len() - n..],
            _ => &all,
        };
        let opt_row = |r: Option<u64>| r.map_or_else(|| "null".to_string(), |v| v.to_string());
        let mut items: Vec<String> = Vec::with_capacity(slice.len());
        for b in slice {
            let state = match b.state {
                BlockState::PromptOnly => "prompt",
                BlockState::EnteringCommand => "entering",
                BlockState::Executing => "executing",
                BlockState::Complete => "complete",
                _ => "unknown",
            };
            let exit = b
                .exit_code
                .map_or_else(|| "null".to_string(), |c| c.to_string());
            items.push(format!(
                "{{\"id\":{},{},\"exit\":{exit},\"prompt\":{},\"cmd\":{},\"out\":{},\"end\":{},{},{}}}",
                b.id,
                json_str_field("state", state),
                b.prompt_start_row,
                opt_row(b.command_start_row),
                opt_row(b.output_start_row),
                opt_row(b.end_row),
                json_str_field("cwd", b.working_directory.as_deref().unwrap_or("")),
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
/// first (or the last `N`). This is the project's point made concrete: an AI
/// driving the terminal navigates by COMMAND — exit codes, the output's absolute
/// row range, the command text and cwd — instead of scraping the screen.
///
/// COORDINATE SPACE (B-2): every `prompt`/`cmd`/`out`/`end` row is a MONOTONIC
/// ABSOLUTE row, the SINGLE read coordinate this socket uses. Feed any of them
/// DIRECTLY to `line <abs_row>` (one row) or `text` (the visible screen) — those
/// verbs accept absolute rows and convert at the read site. (Previously `line`
/// took a 0-based history index, so feeding it a block's absolute row read the
/// WRONG line; `line` now shares the absolute-row space.) For a block's full
/// output prefer `blocktext <id>`, which reads the absolute range itself and
/// reports an EXPLICIT `ERR` when those rows have been EVICTED from scrollback
/// (never silently-shifted text).
///
/// Header `OK <shown>\n`, then one line per block: `block <id> <state>
/// exit=<code|-> prompt=<row> cmd=<row|-> out=<row|-> end=<row|-> cwd=<pct>
/// cmdline=<pct>`. `state` is prompt|entering|executing|complete; cwd/cmdline
/// are percent-encoded (single tokens even with spaces). Needs a shell emitting
/// OSC 133 (see the `shell_integration` injection); empty otherwise.
pub fn cmd_blocks(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    use aterm_core::terminal::BlockState;
    use std::fmt::Write as _;
    host.with_terminal(sid, |t: &Terminal| {
        let all: Vec<_> = t.all_blocks().collect();
        let slice: &[_] = match rest.trim().parse::<usize>() {
            Ok(n) if n < all.len() => &all[all.len() - n..],
            _ => &all,
        };
        let mut out = format!("OK {}\n", slice.len());
        let opt_row = |r: Option<u64>| r.map_or_else(|| "-".to_string(), |v| v.to_string());
        for b in slice {
            let state = match b.state {
                BlockState::PromptOnly => "prompt",
                BlockState::EnteringCommand => "entering",
                BlockState::Executing => "executing",
                BlockState::Complete => "complete",
                _ => "unknown",
            };
            let exit = b
                .exit_code
                .map_or_else(|| "-".to_string(), |c| c.to_string());
            // Write the line DIRECTLY into the response buffer. The `format!`
            // this replaces allocated a whole line `String` per block — plus one
            // per percent-encoded field — purely to be copied in and dropped,
            // and this loop runs up to `OUTPUT_BLOCKS_MAX` (1000) times while
            // holding the terminal lock the render path contends for. The
            // format string is unchanged, merely split at the two encoded
            // fields, so the emitted bytes are identical.
            let _ = write!(
                out,
                "block {} {} exit={} prompt={} cmd={} out={} end={} cwd=",
                b.id,
                state,
                exit,
                b.prompt_start_row,
                opt_row(b.command_start_row),
                opt_row(b.output_start_row),
                opt_row(b.end_row),
            );
            pct_encode_into(&mut out, b.working_directory.as_deref().unwrap_or(""));
            out.push_str(" cmdline=");
            pct_encode_into(&mut out, b.commandline.as_deref().unwrap_or(""));
            out.push('\n');
        }
        out
    })
    .unwrap_or_else(|| NO_SESSION.to_string())
}

/// `blocktext <id>` -> the OUTPUT text of command block `<id>` (from `blocks`),
/// one row per line after `OK <n>`. The engine reads the block's absolute row
/// range itself (across scrollback AND the visible screen), so the caller does
/// NOT juggle coordinate spaces — an AI reads a specific command's output (e.g.
/// the failed one's error) directly. `ERR` if the id is unknown or the block has
/// not produced output yet.
pub fn cmd_blocktext(host: &impl SessionHost, sid: u64, rest: &str) -> String {
    let Ok(id) = rest.trim().parse::<u64>() else {
        return "ERR usage: blocktext <id>\n".to_string();
    };
    host.with_terminal(sid, |t: &Terminal| {
        let Some(block) = t.block_by_id(id).cloned() else {
            return "ERR no such block\n".to_string();
        };
        // Use the enum form so an EVICTED block returns an explicit signal instead
        // of silently-shifted or empty text (B-1 / DL-1).
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

/// The newest COMPLETED block (ids are monotonic and blocks iterate oldest-first,
/// so the last `Complete` seen is the newest — and an id baseline, unlike a
/// count, survives old-block eviction) plus whether any block is still in flight.
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

/// `wait [timeout_ms]` -> the newest COMPLETED command block as
/// `OK complete <id> exit=<code|->`; `OK timeout` if nothing completes in time
/// (default 30 000 ms, capped at 600 000). The AI runs a command then `wait`s
/// for it to finish before reading with `blocktext`. Needs shell integration
/// (OSC 133); with none it times out.
///
/// ENTRY RACE (the send→wait gap): a fast command can COMPLETE in the few ms
/// between the submit and this call, so a "new completion since entry" baseline
/// alone would wait past it and time out. Therefore: with NO block EXECUTING
/// and a completed block present, the NEWEST completed block is returned
/// immediately; with one executing, `wait` blocks for a completion NEWER than
/// the entry snapshot; only when neither happens does the deadline expire.
/// In-flight means `Executing` ONLY: the shell emits OSC 133;B at the END of
/// every prompt, before any typing, so an IDLE prompt sits in
/// `EnteringCommand` — counting it in-flight would make the immediate-return
/// arm unreachable and time `wait` out after every fast command (the exact
/// race this exists to close, verified live).
///
/// EVENT-DRIVEN (no fixed-interval poll): registers a subscriber and parks on
/// its wake — block-state transitions ride PTY output, and every output burst
/// notifies — re-checking the entry snapshot on each wake. The deadline is the
/// backstop.
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
    // Register BEFORE re-checking: a completion landing in this gap still leaves
    // its single-slot notify pending, so the first `wait` below cannot miss it.
    let sub = host.subscribe(sid);
    loop {
        // Only the ENTRY check above needs `in_flight`; from here on the loop
        // wants the newest completion and nothing else. `scan_blocks` walks
        // every block with no early exit, and this runs once per output BURST
        // (hundreds/sec under a flood) while holding the terminal lock the PTY
        // and render paths contend for — so ask the engine for the newest
        // `Complete` directly. Ids are monotonic and blocks iterate oldest-first
        // with `current_block` last, so the last `Complete` forward (what
        // `scan_blocks` returned) is the first `Complete` backward (what
        // `newest_completed_block` returns): the same block, hence the same
        // `OK complete <id> exit=...` bytes.
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

/// Process-wide smart-selection rules, built lazily ONCE (the builtin rules
/// compile a set of regexes). Shared by the GUI's double-click gesture and the
/// `select word` verb so both use identical word/URL/path boundaries.
static SMART_RULES: OnceLock<SmartSelection> = OnceLock::new();

/// The engine's builtin smart-selection rules (lazy singleton).
fn smart_rules() -> &'static SmartSelection {
    SMART_RULES.get_or_init(SmartSelection::with_builtin_rules)
}

/// Rows of a wrapped line joined on each side of a click before the word rules are asked,
/// doubling while the match still reaches a window edge.
///
/// Two rows each way is ~three screen widths of context, which contains every word a
/// person double-clicks; a token longer than that (a very long URL or path) simply costs
/// one more doubling. The point is that the COMMON case stops being linear in the whole
/// logical line — see the loop in [`word_span`].
const WORD_WINDOW_ROWS: usize = 2;

/// The widest a double-click will ever look, in rows of the wrapped line.
///
/// The window widens while the match still runs off an edge, which is right for a long
/// URL or path — but a line can be ONE unbroken token (`cat` of a minified bundle is
/// 320 kB with no break in it), and there the widening reads the whole thing on every
/// call, twice per pointer move, under the terminal mutex. So the widening STOPS here and
/// the gesture returns the bounded span it found.
///
/// 64 rows is ~5 kB at 80 columns: past any identifier, URL, path or hash a person
/// double-clicks to grab, and small enough that the scan stays under a millisecond. A
/// token longer than this is selected up to the bound rather than freezing the window —
/// the drag and the triple-click still reach the whole logical line, and this bound is
/// only on what ONE double-click will widen to.
const WORD_WINDOW_MAX_ROWS: usize = 64;

/// Inclusive word CELL bounds `((start_row, start_col), (end_row, end_col))` at
/// live-screen `(row, col)`, from the engine's builtin smart-selection rules
/// (URL/path/email/... patterns, falling back to plain alphanumeric+underscore
/// words). `None` when the cell is whitespace or to the right of the line's
/// text — the caller selects just the clicked cell.
///
/// LOGICAL, not physical: the rules run over
/// [`Terminal::logical_line_text`](aterm_core::terminal::Terminal::logical_line_text)
/// — the soft-wrapped rows joined and COLUMN-ALIGNED — so a word straddling the
/// wrap is ONE word and the returned cells may sit on different rows. Bound to
/// the physical row instead, a double-click on such a word yields only the
/// fragment on the clicked row ("…STR" or "ADDLEWORD", never the whole word).
///
/// A HARD newline still bounds the run: `logical_line_text` joins soft wraps
/// only, so a word can never reach into the following logical line.
#[must_use]
pub fn word_span(t: &Terminal, row: i32, col: u16) -> Option<((i32, u16), (i32, u16))> {
    let cols = usize::from(t.cols());
    if cols == 0 {
        return None;
    }
    // WINDOWED, not whole-line. The rules scan is linear in the text it is handed, and a
    // logical line can be enormous — one `cat` of a minified bundle is 320 kB across 4000
    // rows. Scanning all of it cost ~70 ms per call, and the double-click DRAG arm calls
    // this twice per pointer move under the terminal mutex, which is ~7 fps while dragging.
    // A word is delimited by whitespace, so the answer is almost always within a row or
    // two; read a small window and widen ONLY when the match reaches an edge that is not
    // the line's own end. Bounded by the line span, so the widest case is the old cost and
    // the common case is a few hundred bytes.
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
        // `word_boundaries_at_column` clamps a past-the-text column INTO the text
        // (it would snap to the LAST word); a click right of the text is whitespace.
        if global >= aterm_core::grapheme::byte_to_column(&text, text.len()) {
            return None;
        }
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
    // The returned end column is EXCLUSIVE; selection anchors are inclusive cells.
    let last = end.saturating_sub(1).max(start);
    let cell = |g: usize| -> (i32, u16) {
        let dr = i32::try_from(g / cols).unwrap_or(i32::MAX);
        let c = u16::try_from(g % cols).unwrap_or(u16::MAX);
        (first_row.saturating_add(dr), c)
    };
    Some((cell(start), cell(last)))
}

/// Word-select at live-screen `(row, col)` — the double-click / `select word`
/// gesture: a `Semantic` selection spanning the word's cells (both boundary
/// cells inclusive, Left/Right anchor sides), or just the clicked cell when on
/// whitespace. Completes the selection and returns the inclusive
/// `((start_row, start_col), (end_row, end_col))` actually selected — which
/// spans two rows when the word straddles a SOFT WRAP.
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

/// Line-select the LOGICAL line at live-screen row `row` — the triple-click /
/// `select line` gesture: a `Lines` selection from column 0 of the logical
/// line's FIRST physical row to the last column of its LAST, so a soft-wrapped
/// line is selected whole. Completes the selection and returns the inclusive
/// `(first_row, last_row)` span it covered.
///
/// The span comes from
/// [`Terminal::logical_line_span`](aterm_core::terminal::Terminal::logical_line_span),
/// the same soft-wrap bit the copy walk uses to decide where a newline goes —
/// so the extracted text is the whole logical line with NO newline invented at
/// the wrap, and a HARD newline still ends it (the following logical line is
/// never swallowed). Selecting the physical row alone truncated a wrapped line
/// at the window width while the highlight still looked edge-to-edge.
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
///   `(r2,c2)`, BOTH endpoint cells INCLUSIVE (the two points are normalized
///   to reading order first, so either order works).
/// * `select word <r> <c>` — semantic (word/URL/path) selection at the cell
///   via the engine's builtin smart-selection rules; a whitespace cell selects
///   just itself. Same code path as the GUI's double-click, so a word
///   straddling a SOFT WRAP is selected whole, across both rows.
/// * `select line <r>` — LOGICAL-line selection at row `r` (triple-click): a
///   soft-wrapped line is selected from its first physical row through its
///   last, and a hard newline ends it.
/// * `select block <r1> <c1> <r2> <c2>` — rectangular (block) selection with
///   the two cells as INCLUSIVE corners (any corner order).
/// * `select extend <r> <c>` — extend the EXISTING selection so cell `(r,c)`
///   becomes its new (inclusive) endpoint (shift-click); `ERR no selection`
///   when nothing is selected.
/// * `select clear` — clear the selection.
///
/// Rows are LIVE-screen coords as signed integers: `0..rows` is the visible
/// live screen and NEGATIVE rows address scrollback (`-1` = the most recently
/// scrolled-off line). All forms nudge a windowed session to repaint the
/// highlight and reply `OK\n`.
///
/// SEAM CARVE-OUT (Phase 0.5): `select` mutates `text_selection_mut()` directly
/// rather than through the host's input seam. This is DELIBERATE and NOT a
/// convergence gap: `select` produces NO PTY bytes (it sets ABSOLUTE
/// coordinates, not a press/drag GESTURE), so it has no byte-indistinguishability
/// stake. It is the controller analogue of an external "set the selection here"
/// command — there is no human winit event that produces an absolute-coordinate
/// selection (the human path is press → drag → release, which DOES go through the
/// seam's `MouseButton`/`MouseMove` gesture arms). Keeping it out of the seam
/// avoids inventing a synthetic gesture; the seam's "sole selection-mutation"
/// claim is about the GESTURE path, which both sources share.
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
                // Side by direction so the clicked cell is INCLUDED whichever way
                // the selection grows: extending backward the moving anchor is the
                // normalized START (Left side includes its cell), extending
                // forward it is the normalized END (Right side includes its cell).
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
            // Normalize to reading order so the Left/Right anchor sides below
            // always make BOTH endpoint cells inclusive (a Right-sided end
            // includes its cell; after normalization the end is never
            // side-flipped into an exclusion).
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

/// `selection` -> the currently selected text as `OK <n>[ incomplete]\n` + `n`
/// data lines (the text split on newlines, same framing as `text`). No or empty
/// selection -> `OK 0\n`.
///
/// INCOMPLETE: the header carries a trailing ` incomplete` token — mirroring
/// `cmd_search` — whenever selected content is MISSING from the reply, so a client
/// knows not to trust a short list silently. Two causes: the selection hit the
/// engine's copy caps (`MAX_SELECTION_ROWS`/`MAX_SELECTION_BYTES`) and lost its tail,
/// or scrollback eviction clamped its head to the history floor and it lost its
/// front.
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

/// `copy` -> copy the currently selected text to the system clipboard and reply
/// `OK <byte-count>[ incomplete]\n`; no or empty selection -> `OK 0\n` (the
/// clipboard is left untouched). The selection is NOT cleared.
///
/// UNSUPPORTED: a host with no clipboard ([`HostCapabilities::clipboard`] unset)
/// answers `ERR unsupported` BEFORE reading the selection — the verb genuinely
/// does not exist there, and saying so beats a write that silently goes nowhere.
///
/// INCOMPLETE: as with `selection`, a copy that is missing selected content carries
/// a trailing ` incomplete` token so the client knows the clipboard holds part of the
/// selection — a prefix when the copy caps clipped the tail, a suffix when scrollback
/// eviction clamped the head to the history floor.
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

/// The CLICK gestures against soft wraps — the defect this module's
/// `word_span`/`select_line` exist to close.
///
/// A soft-wrapped line is ONE logical line to the copy path (it emits no
/// newline before a continuation row), so a click gesture bound to the PHYSICAL
/// row selected a prefix while the highlight ran edge-to-edge: the user saw a
/// complete selection and middle-clicked a TRUNCATED command. Every check below
/// is on bytes — the length and the text — not on the anchors, because the
/// anchors were never the thing that was wrong.
#[cfg(test)]
mod gesture_tests {
    use aterm_core::selection::SelectionType;
    use aterm_core::terminal::Terminal;

    use super::{select_line, select_word, word_span};

    /// The reported command: 130 chars auto-wrapped across two rows of an
    /// 80-column screen. The head fills row 0 exactly and ends
    /// `aterm-build rust` — the point the truncated clipboard stopped at; the
    /// tail is the 50 bytes that were silently dropped.
    const HEAD: &str =
        "docker run --rm -v $PWD:/w -w /w --network host --name bld-boxy aterm-build rust";
    const TAIL: &str = ":1.85 cargo build --release --features gpu,wayland";

    /// That command on row 0/1, then a SEPARATE logical line after a hard
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

    /// Rebuild the text the HIGHLIGHT covers, straight from the predicate the
    /// renderer paints with (`TextSelection::contains`), joining rows by the
    /// same soft-wrap rule the copy uses.
    ///
    /// A highlight that disagrees with the clipboard is its own defect — the
    /// user's only evidence of what they are about to paste is the paint — so
    /// every gesture below is checked BOTH ways against this.
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

    /// THE DEFECT: a triple-click on the head of a soft-wrapped command must
    /// copy the WHOLE command, not the 80 bytes that fit on the row.
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

    /// The same logical line, clicked on its CONTINUATION row: the gesture
    /// reaches BACK to the head, so where in the wrapped line the user clicks
    /// cannot change what they get.
    #[test]
    fn a_line_click_on_the_continuation_row_selects_the_same_logical_line() {
        let (mut term, cmd) = wrapped_command();
        assert_eq!(select_line(&mut term, 1), (0, 1));
        let copied = term.selection_to_string().expect("a selection was made");
        assert_eq!(copied, cmd);
        assert_eq!(highlighted_text(&term), copied);
    }

    /// The boundary that is NOT a soft wrap: a HARD newline still ends the
    /// selection. The wrapped line must not swallow the line after it, and the
    /// line after it must not reach back.
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

    /// A double-click on a word straddling the wrap must select the WHOLE word,
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

        // Clicking the TAIL fragment answers the same word.
        let (start2, end2) = select_word(&mut term, 1, 2);
        assert_eq!((start2, end2), (start, end));
        assert_eq!(term.selection_to_string().as_deref(), Some("STRADDLEWORD"));
    }

    /// The gestures must not have grown a taste for joining everything: an
    /// ORDINARY unwrapped line still selects exactly itself, word and line
    /// alike, with the same anchors as before.
    /// THE WINDOW MUST NOT COST CORRECTNESS. `word_span` reads a small window around the
    /// click instead of the whole logical line; a word longer than that window must still
    /// come back whole, which is what the widening loop is for. This builds a token far
    /// wider than `WORD_WINDOW_ROWS` rows and clicks in its MIDDLE, so the match runs off
    /// BOTH edges of the first window and both arms have to widen.
    #[test]
    fn a_word_wider_than_the_window_is_still_selected_whole() {
        let cols = 80usize;
        let rows = super::WORD_WINDOW_ROWS * 4 + 3;
        let token = "x".repeat(cols * rows);
        // The terminal must be TALLER than the token, or most of it scrolls into
        // history and the click row addresses something else entirely.
        let mut term = Terminal::new(
            u16::try_from(rows + 3).unwrap(),
            u16::try_from(cols).unwrap(),
        );
        term.process(format!("{token} tail").as_bytes());
        // Click in the middle row of the token, so the window opens upward AND downward.
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

    /// The widening must STOP at the logical line's own end rather than walking into the
    /// next line. A token that ends exactly at the last row of a wrapped line touches the
    /// window edge on the way down, but there is nothing beyond it to take.
    #[test]
    fn widening_stops_at_the_end_of_the_logical_line() {
        let mut term = Terminal::new(6, 80);
        // 80-col head + a tail token, then a HARD newline and another word.
        term.process(format!("{}ENDTOKEN", "y".repeat(80)).as_bytes());
        term.process(b"\r\nNEXTWORD");
        select_word(&mut term, 1, 2);
        let picked = term.selection_to_string().expect("a selection was made");
        assert!(
            !picked.contains("NEXTWORD"),
            "widening walked past the hard newline into the next logical line: {picked:?}"
        );
    }

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
