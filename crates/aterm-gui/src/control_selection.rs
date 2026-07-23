// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Selection / copy / block-aware verbs: `select` (plain ranges plus the
//! `word`/`line`/`block`/`extend` gestures), `selection`, `copy`, and the
//! OSC-133 command-block verbs (`blocks`/`blocktext`) plus `wait`. Moved
//! verbatim from `control.rs` (behavior-preserving). The shared JSON/encode
//! helpers stay in `control.rs` and are reached via `super::`.

use std::sync::{Arc, Mutex, OnceLock};

use aterm_core::selection::{SelectionSide, SelectionType, SmartSelection};
use aterm_core::terminal::Terminal;
use winit::event_loop::EventLoopProxy;

use super::{json_ok, json_str_field, pct_encode, visible_char};
use crate::{Wake, term_lock};

/// `blocks [N] --json` -> `{"blocks":[{...}]}`: the SAME OSC 133/633 command
/// blocks `cmd_blocks` reports (oldest-first, optional last-N), one JSON object
/// per block with the absolute rows, exit code, state, cwd and commandline. An
/// absent optional row is JSON `null`; the cwd/commandline are JSON strings (not
/// percent-encoded — JSON carries spaces natively).
pub(crate) fn cmd_blocks_json(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    use aterm_core::terminal::BlockState;
    let t = term_lock(term);
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
pub(crate) fn cmd_blocks(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    use aterm_core::terminal::BlockState;
    let t = term_lock(term);
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
        out.push_str(&format!(
            "block {} {} exit={} prompt={} cmd={} out={} end={} cwd={} cmdline={}\n",
            b.id,
            state,
            exit,
            b.prompt_start_row,
            opt_row(b.command_start_row),
            opt_row(b.output_start_row),
            opt_row(b.end_row),
            pct_encode(b.working_directory.as_deref().unwrap_or("")),
            pct_encode(b.commandline.as_deref().unwrap_or("")),
        ));
    }
    out
}

/// `blocktext <id>` -> the OUTPUT text of command block `<id>` (from `blocks`),
/// one row per line after `OK <n>`. The engine reads the block's absolute row
/// range itself (across scrollback AND the visible screen), so the caller does
/// NOT juggle coordinate spaces — an AI reads a specific command's output (e.g.
/// the failed one's error) directly. `ERR` if the id is unknown or the block has
/// not produced output yet.
pub(crate) fn cmd_blocktext(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let Ok(id) = rest.trim().parse::<u64>() else {
        return "ERR usage: blocktext <id>\n".to_string();
    };
    let t = term_lock(term);
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
/// `notify`s (`Wake::Output`, the same machinery as `ready`/`await`) — re-
/// checking the entry snapshot on each wake. The deadline is the backstop.
pub(crate) fn cmd_wait(
    term: &Arc<Mutex<Terminal>>,
    session: u64,
    rest: &str,
    subscribers: &crate::subscribe::Subscribers,
) -> String {
    use aterm_core::terminal::BlockState;

    use crate::subscribe::SubscriberSet;
    let timeout_ms = rest.trim().parse::<u64>().unwrap_or(30_000).min(600_000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let reply = |id: u64, exit: Option<i32>| {
        let exit = exit.map_or_else(|| "-".to_string(), |c| c.to_string());
        format!("OK complete {id} exit={exit}\n")
    };
    // One pass under the lock: the newest completed block (ids are monotonic and
    // blocks iterate oldest-first, so the last Complete seen is the newest — and
    // an id baseline, unlike a count, survives old-block eviction) plus whether
    // any block is still in flight.
    let scan = |t: &Terminal| {
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
    };
    let (entry_done, in_flight) = scan(&term_lock(term));
    if !in_flight && let Some((id, exit)) = entry_done {
        return reply(id, exit);
    }
    let baseline = entry_done.map(|(id, _)| id);
    // Register BEFORE re-checking: a completion landing in this gap still leaves
    // its single-slot notify pending, so the first `wait` below cannot miss it.
    let sub = SubscriberSet::register(subscribers, &[session]);
    loop {
        if let (Some((id, exit)), _) = scan(&term_lock(term))
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
pub(crate) fn smart_rules() -> &'static SmartSelection {
    SMART_RULES.get_or_init(SmartSelection::with_builtin_rules)
}

/// Inclusive word-column bounds at live-screen `(row, col)`, from the engine's
/// builtin smart-selection rules (URL/path/email/... patterns, falling back to
/// plain alphanumeric+underscore words). `None` when the cell is whitespace or
/// to the right of the row's text — the caller selects just the clicked cell.
pub(crate) fn word_cols(t: &Terminal, row: i32, col: u16) -> Option<(u16, u16)> {
    let text = t.get_line_text(row, None)?;
    // `word_boundaries_at_column` clamps a past-the-text column INTO the text
    // (it would snap to the LAST word); a click right of the text is whitespace.
    if usize::from(col) >= aterm_core::grapheme::byte_to_column(&text, text.len()) {
        return None;
    }
    let (start, end) = smart_rules().word_boundaries_at_column(&text, usize::from(col))?;
    // The returned end column is EXCLUSIVE; selection anchors are inclusive cells.
    let last = end.saturating_sub(1).max(start);
    let clamp = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
    Some((clamp(start), clamp(last)))
}

/// Word-select at live-screen `(row, col)` — the double-click / `select word`
/// gesture: a `Semantic` selection spanning the word's cells (both boundary
/// cells inclusive, Left/Right anchor sides), or just the clicked cell when on
/// whitespace. Completes the selection and returns the inclusive
/// `(start_col, end_col)` actually selected.
pub(crate) fn select_word(t: &mut Terminal, row: i32, col: u16) -> (u16, u16) {
    let (start, end) = word_cols(t, row, col).unwrap_or((col, col));
    let sel = t.text_selection_mut();
    sel.start_selection(row, col, SelectionSide::Left, SelectionType::Semantic);
    sel.expand_semantic(start, end);
    sel.complete_selection();
    (start, end)
}

/// Line-select live-screen row `row` — the triple-click / `select line`
/// gesture: a `Lines` selection expanded to the full row width (the extracted
/// text is the whole row, trailing blanks trimmed). Completes the selection.
pub(crate) fn select_line(t: &mut Terminal, row: i32) {
    let max_col = t.cols().saturating_sub(1);
    let sel = t.text_selection_mut();
    sel.start_selection(row, 0, SelectionSide::Left, SelectionType::Lines);
    sel.expand_lines(max_col);
    sel.complete_selection();
}

/// `select ...` -> drive the engine's text selection. Forms:
///
/// * `select <r1> <c1> <r2> <c2>` — simple range from cell `(r1,c1)` to
///   `(r2,c2)`, BOTH endpoint cells INCLUSIVE (the two points are normalized
///   to reading order first, so either order works).
/// * `select word <r> <c>` — semantic (word/URL/path) selection at the cell
///   via the engine's builtin smart-selection rules; a whitespace cell selects
///   just itself. Same code path as the GUI's double-click.
/// * `select line <r>` — full-line selection of row `r` (triple-click).
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
/// rather than through [`App::input`](crate::App::input). This is DELIBERATE and
/// NOT a convergence gap: `select` produces NO PTY bytes (it sets ABSOLUTE
/// coordinates, not a press/drag GESTURE), so it has no byte-indistinguishability
/// stake. It is the controller analogue of an external "set the selection here"
/// command — there is no human winit event that produces an absolute-coordinate
/// selection (the human path is press → drag → release, which DOES go through the
/// seam's `MouseButton`/`MouseMove` gesture arms). Keeping it out of the seam
/// avoids inventing a synthetic gesture; the seam's "sole selection-mutation"
/// claim is about the GESTURE path, which both sources share.
pub(crate) fn cmd_select(
    term: &Arc<Mutex<Terminal>>,
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    rest: &str,
) -> String {
    const USAGE: &str = "ERR usage: select <r1> <c1> <r2> <c2> | select word <r> <c> | \
                         select line <r> | select block <r1> <c1> <r2> <c2> | \
                         select extend <r> <c> | select clear\n";
    let rest = rest.trim();
    if rest == "clear" {
        term_lock(term).text_selection_mut().clear();
        let _ = proxy.send_event(Wake::redraw(session));
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
            select_word(&mut term_lock(term), r, c);
        }
        "line" => {
            let Some(Ok(r)) = it.next().map(str::parse::<i32>) else {
                return "ERR usage: select line <r>\n".to_string();
            };
            select_line(&mut term_lock(term), r);
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
            let mut t = term_lock(term);
            let sel = t.text_selection_mut();
            sel.start_selection(r1, c1, SelectionSide::Left, SelectionType::Block);
            sel.update_selection(r2, c2, SelectionSide::Right);
            sel.complete_selection();
        }
        "extend" => {
            let (Some(Ok(r)), Some(Ok(c))) = (
                it.next().map(str::parse::<i32>),
                it.next().map(str::parse::<u16>),
            ) else {
                return "ERR usage: select extend <r> <c>\n".to_string();
            };
            let mut t = term_lock(term);
            let sel = t.text_selection_mut();
            if !sel.has_selection() || sel.is_empty() {
                return "ERR no selection\n".to_string();
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
            let mut t = term_lock(term);
            let sel = t.text_selection_mut();
            sel.start_selection(sr, sc, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(er, ec, SelectionSide::Right);
            sel.complete_selection();
        }
    }
    let _ = proxy.send_event(Wake::redraw(session));
    "OK\n".to_string()
}

/// `selection` -> the currently selected text as `OK <n>[ incomplete]\n` + `n`
/// data lines (the text split on newlines, same framing as `text`). No or empty
/// selection -> `OK 0\n`.
///
/// INCOMPLETE: if the selection is so large it hits the engine's copy caps
/// (`MAX_SELECTION_ROWS`/`MAX_SELECTION_BYTES`), the header carries a trailing
/// ` incomplete` token — mirroring `cmd_search` — so a client knows the text was
/// truncated rather than trusting a short list silently.
pub(crate) fn cmd_selection(term: &Arc<Mutex<Terminal>>) -> String {
    let (text, truncated) = term_lock(term).selection_to_string_bounded();
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

/// `copy` -> copy the currently selected text to the macOS system clipboard
/// (`pbcopy`) and reply `OK <byte-count>[ incomplete]\n`; no or empty selection ->
/// `OK 0\n` (the clipboard is left untouched). The selection is NOT cleared.
///
/// INCOMPLETE: as with `selection`, a copy clipped by the engine's copy caps
/// carries a trailing ` incomplete` token so the client knows the clipboard holds
/// a truncated prefix, not the whole selection.
pub(crate) fn cmd_copy(term: &Arc<Mutex<Terminal>>) -> String {
    let (text, truncated) = term_lock(term).selection_to_string_bounded();
    match text {
        Some(t) if !t.is_empty() => {
            if pbcopy(&t) {
                let incomplete = if truncated { " incomplete" } else { "" };
                format!("OK {}{incomplete}\n", t.len())
            } else {
                "ERR pbcopy failed\n".to_string()
            }
        }
        _ => "OK 0\n".to_string(),
    }
}

/// Place `text` on the system CLIPBOARD. macOS writes the general `NSPasteboard`
/// IN-PROCESS (a subprocess `pbcopy` would cost a fork/exec + wait — tens of ms —
/// on every Cmd-C / copy-on-select on the winit event-loop thread); Linux/X11
/// takes ownership of the CLIPBOARD selection via the native x11rb backend
/// ([`crate::clipboard_x11`]) — so no external helper (`xclip`/`wl-copy`) is
/// required; Windows goes through the Win32 clipboard
/// ([`crate::clipboard_win`]). Shared by the `copy` verb, the GUI copy shortcut, copy-on-select,
/// and OSC 52. Returns whether the text was placed. (Named `pbcopy` for historical
/// continuity across the stable `crate::control::pbcopy` path.)
///
/// LOCALE: the old subprocess path had to pin `LC_ALL`/`LC_CTYPE` to UTF-8 (a
/// Finder/.app launch hands the process a non-UTF-8 locale and `pbcopy`/`pbpaste`
/// transcode against the C codeset — mojibake). The in-process path has NO
/// locale-sensitive transcode: `NSString` ⇄ Rust `String` is a direct UTF-8
/// conversion, so multibyte text round-trips regardless of the launch locale.
///
/// THREADING: called off the main thread by the OSC 52 worker
/// ([`crate::spawn`]) and the control-server `copy` verb. `NSPasteboard`
/// string get/set from a non-main thread is established AppKit practice
/// (Alacritty/copypasta ship exactly this) and the class is not on Apple's
/// main-thread-only list.
pub(crate) fn pbcopy(text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::NSString;
        // SAFETY: generalPasteboard is a valid singleton on any thread;
        // clearContents/setString:forType: take ownership of the pasteboard
        // before writing (the documented write protocol) and the NSString
        // argument outlives the call.
        unsafe {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            pb.setString_forType(&NSString::from_str(text), NSPasteboardTypeString)
        }
    }
    #[cfg(target_os = "linux")]
    {
        crate::clipboard_x11::X11Clipboard::get_handle()
            .is_some_and(|c| c.set(crate::clipboard_x11::Sel::Clipboard, text))
    }
    #[cfg(windows)]
    {
        crate::clipboard_win::set(text)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = text;
        false
    }
}

/// Read the system CLIPBOARD as UTF-8 text, or `None` when empty / unavailable.
/// macOS reads the general `NSPasteboard` in-process (see [`pbcopy`] for the
/// locale + threading notes); Linux/X11 reads the CLIPBOARD selection via the
/// native x11rb backend. The platform twin of [`pbcopy`], used by the GUI paste
/// shortcut and the menu Paste. An empty pasteboard string maps to `None` so no
/// Paste event fires on an empty clipboard.
pub(crate) fn pbpaste() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        // SAFETY: generalPasteboard is a valid singleton on any thread and
        // stringForType: only reads it; the returned NSString is retained.
        let s = unsafe {
            NSPasteboard::generalPasteboard()
                .stringForType(NSPasteboardTypeString)?
                .to_string()
        };
        (!s.is_empty()).then_some(s)
    }
    #[cfg(target_os = "linux")]
    {
        crate::clipboard_x11::X11Clipboard::get_handle()
            .and_then(|c| c.get(crate::clipboard_x11::Sel::Clipboard))
    }
    #[cfg(windows)]
    {
        crate::clipboard_win::get()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        None
    }
}

/// The non-blocking twin of [`pbpaste`] for X11: return the CLIPBOARD text only when
/// we OWN the selection (the stored slot, instant — `X11Clipboard::get_owned`),
/// `None` when a FOREIGN client owns it. Lets the GUI paste deliver the own-selection
/// case synchronously and offload only the blocking foreign read off the UI thread.
/// Linux-only: macOS / Windows reads are already in-process, so those paths call
/// [`pbpaste`] directly and never need this fast-path (an unused import off Linux).
#[cfg(target_os = "linux")]
pub(crate) fn pbpaste_owned() -> Option<String> {
    crate::clipboard_x11::X11Clipboard::get_handle()
        .and_then(|c| c.get_owned(crate::clipboard_x11::Sel::Clipboard))
}

/// Set the X11 PRIMARY selection (the select-to-copy / middle-click-paste buffer)
/// to `text`. X11-only — PRIMARY has no macOS/Wayland-headless analogue, so this is
/// a no-op (returns `false`) elsewhere. Distinct from the CLIPBOARD ([`pbcopy`]) so
/// a drag-select never clobbers an explicit Ctrl+Shift+C copy.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn primary_set(text: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::clipboard_x11::X11Clipboard::get_handle()
            .is_some_and(|c| c.set(crate::clipboard_x11::Sel::Primary, text))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = text;
        false
    }
}

/// Read the X11 PRIMARY selection as UTF-8 text (the middle-click-paste source), or
/// `None` when empty / off X11.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn primary_get() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        crate::clipboard_x11::X11Clipboard::get_handle()
            .and_then(|c| c.get(crate::clipboard_x11::Sel::Primary))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
