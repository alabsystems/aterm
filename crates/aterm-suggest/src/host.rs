// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host adapter: the two impedance-matching functions between a live
//! `Terminal`-shaped host and the pure engine.
//!
//! Deliberately takes PLAIN DATA rather than an `aterm-core` handle. The engine
//! stays dependency-free (and therefore trivially testable and cheap to pull
//! into the wasm embedders), while the host keeps a mapping so small it cannot
//! hide a bug: read the fields off `OutputBlock`, call these.

/// Reconstruct the command line being typed, from the screen.
///
/// This is the load-bearing trick of the whole feature. aterm does **not**
/// model readline: it reads what is actually on the glass between the
/// OSC 133;B anchor (`command_start_row` / `command_start_col`) and the cursor.
/// Whatever the line editor did to get there — history recall with the up
/// arrow, `Ctrl-R`, `Ctrl-W`, a paste, a vi-mode `daw` — the result is
/// displayed, so reading it is correct by construction where modelling would be
/// a permanent game of catch-up across four shells.
///
/// # Arguments
///
/// * `rows` — the text of each row of the command region, in order, starting
///   with the row the command begins on. The host supplies full row texts.
/// * `start_col` — character offset into `rows[0]` where the command begins.
///   Everything before it is the prompt and is excluded.
/// * `cursor` — `(row index within `rows`, character offset within that row)`.
///   Text at or after the cursor is excluded: it is either nothing, or the tail
///   of a line the user has moved back into, and completing from the middle of
///   a line is not what this feature does.
///
/// # Wide characters
///
/// Offsets are in **characters**, not grid cells. A host whose grid contains
/// double-width glyphs must pass character offsets (its row-text extraction
/// already produces a `String`, so it is converting anyway). Getting this wrong
/// truncates the buffer, which yields a wrong-but-safe suggestion, never a
/// wrong edit — nothing here is ever written to the PTY without an explicit
/// accept.
#[must_use]
pub fn line_buffer(rows: &[&str], start_col: usize, cursor: (usize, usize)) -> String {
    let (cur_row, cur_col) = cursor;
    if rows.is_empty() || cur_row >= rows.len() {
        return String::new();
    }
    // One reservation up front. `String::new()` grew 8→16→32→…, so a wrapped
    // 100-column line cost five reallocations per keystroke; byte length is a
    // safe upper bound on the char count we are about to push.
    let mut out = String::with_capacity(rows.iter().take(cur_row + 1).map(|r| r.len()).sum());
    for (i, row) in rows.iter().enumerate().take(cur_row + 1) {
        // Character-indexed slicing: byte offsets would panic mid-codepoint on
        // any non-ASCII prompt or command.
        let from = if i == 0 { start_col } else { 0 };
        if i == cur_row {
            if cur_col > from {
                out.extend(row.chars().skip(from).take(cur_col - from));
            }
        } else {
            // A whole intermediate row of a wrapped line — no `chars().count()`
            // needed just to bound the take.
            out.extend(row.chars().skip(from));
        }
    }
    // A wrapped command line is stored as separate rows but is ONE logical
    // line; the visual break is not part of what the user typed. Trailing
    // blanks are grid padding, not input. Truncated in place rather than
    // reallocating a trimmed copy.
    out.truncate(out.trim_end().len());
    out
}

/// One completed command, as read off an `OutputBlock`.
///
/// Mirrors the block fields exactly so the host's mapping is a field-for-field
/// copy with no interpretation:
///
/// ```text
/// BlockRecord {
///     command:   block.commandline (preferred) or the scraped command text,
///     cwd:       block.working_directory,
///     exit_code: block.exit_code,
///     at_ms:     block.command_exec_start_time_ms or prompt_time_ms,
/// }
/// ```
#[derive(Clone, Copy, Debug)]
pub struct BlockRecord<'a> {
    /// The command text. Prefer OSC 633;E (`commandline`) — it is clean text
    /// with no prompt decoration.
    pub command: &'a str,
    /// The working directory the command ran in (OSC 7 / OSC 633;P).
    pub cwd: Option<&'a str>,
    /// The exit code from OSC 133;D, if the block completed.
    pub exit_code: Option<i32>,
    /// When the command ran, epoch-ms.
    pub at_ms: u64,
}

impl crate::Engine {
    /// Record one completed block. The host calls this once per block
    /// transition to `Complete`.
    pub fn record_block(&mut self, r: &BlockRecord<'_>) {
        self.record(r.command, r.cwd, r.exit_code, r.at_ms);
    }
}

#[cfg(test)]
mod tests;
