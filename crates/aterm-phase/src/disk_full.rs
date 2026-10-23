// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK IS FULL, BY CLAUDE CODE'S OWN WORD — read by PLACE, never by the words alone.

/// The error code a disk-full notice names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// `ENOSPC`: no space (or no inodes) left on the device.
    Enospc,
    /// `EDQUOT`: the user's disk quota is exceeded.
    Edquot,
}

impl Code {
    /// The code as the notice spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::Enospc => "ENOSPC",
            Code::Edquot => "EDQUOT",
        }
    }
}

/// Which of Claude Code's texts said it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The pinned notification (`wall.disk_full`).
    Notification,
    /// A failed command's output (`wall.disk_full.tool` and its kin, or
    /// [`TOOL_HEAD_BEFORE_2_1_309`]), or a command not started
    /// (`wall.disk_full.pre_start`).
    ToolOutput,
    /// A goal cleared by it (`wall.disk_full.goal`).
    GoalCleared,
}

/// One disk-full notice read from a screen ([`disk_full`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFull {
    pub code: Code,
    pub source: Source,
    /// The row it opens on.
    pub row: usize,
    /// The notice as drawn on that row, from its anchor on (the vendor's glyph before it left off).
    pub message: String,
}

/// The disk-full notice on a Claude Code screen, if one stands where Claude Code draws it (the
/// module's rules); the newest (lowest) first.
#[must_use]
pub fn disk_full(rows: &[String]) -> Option<DiskFull> {
    let frame = crate::phase::composer_frame(rows)?;
    let under = (frame.bottom + 1..rows.len()).rev().find_map(|i| {
        notification(rows, i)
            .or_else(|| goal_cleared(rows, i, false))
            .or_else(|| tool_output(rows, i, false))
    });
    if under.is_some() {
        return under;
    }
    let block = crate::phase::status_block(rows, frame.top);
    let from = block.status.map_or(block.from, |s| s + 1);
    (from..frame.top)
        .rev()
        .find_map(|i| {
            if block
                .status
                .is_some_and(|s| crate::wall::in_queued_message(rows, s, i))
            {
                return None;
            }
            notification(rows, i)
                .or_else(|| tool_output(rows, i, true))
                .or_else(|| goal_cleared(rows, i, true))
        })
        .or_else(|| {
            // The vendor's `⎿` rows of the LAST TURN, above the status row
            // (the rows under it were read above): up to the last message a
            // person sent (a `❯` row at the edge), never past it — a row of
            // an earlier turn is an incident since answered.
            let end = block.status.unwrap_or(frame.top).min(frame.top);
            (0..end)
                .rev()
                .take(TOOL_ROWS)
                .take_while(|&i| !rows[i].starts_with('❯'))
                .find_map(|i| tool_output(rows, i, true).or_else(|| goal_cleared(rows, i, true)))
        })
}

/// How far above the composer a tool's output row is looked for: the last
/// screenful's tail, where the turn that failed drew it.
const TOOL_ROWS: usize = 40;

/// Segments of a row: its text after its indent, and the text after each run
/// of two or more spaces — where a footer item, a right-aligned notice or a
/// banner beside the spinner starts.
fn segments(row: &str) -> impl Iterator<Item = &str> {
    let trimmed = row.trim_start();
    let mut starts = vec![trimmed];
    let mut rest = trimmed;
    while let Some(at) = rest.find("  ") {
        let after = rest[at..].trim_start_matches(' ');
        if after.is_empty() {
            break;
        }
        starts.push(after);
        rest = after;
    }
    starts.into_iter()
}

/// The pinned notification on row `i`: a segment that opens with the anchor,
/// or with the vendor's glyph and the anchor ([`after_glyph`]), and names
/// `— ENOSPC)` or `— EDQUOT)` (a code alone when the vendor maps none:
/// `(ENOSPC)`).
fn notification(rows: &[String], i: usize) -> Option<DiskFull> {
    let anchor = const { crate::anchors::anchor_text("wall.disk_full") };
    let row = crate::text::trim_end(&rows[i]);
    segments(row).find_map(|seg| {
        let words = after_glyph(seg);
        let rest = words.strip_prefix(anchor)?;
        let inside = rest.split(')').next()?;
        let code = code_in(inside)?;
        Some(DiskFull {
            code,
            source: Source::Notification,
            row: i,
            message: words.to_string(),
        })
    })
}

/// The warning glyph Claude Code draws before a pinned notice's words
/// (2.1.304's notice row: `⚠`, then one space; MEASURED, the module's
/// fixture), U+26A0 with or without the emoji selector U+FE0F.
const GLYPH: char = '\u{26A0}';

/// `seg` less the vendor's glyph and the ONE space after it, where it opens
/// with them; `seg` itself otherwise. Nothing else before the words is
/// taken off: a draft, a quote or a label in front of them is no notice.
fn after_glyph(seg: &str) -> &str {
    seg.strip_prefix(GLYPH)
        .map(|r| r.strip_prefix('\u{FE0F}').unwrap_or(r))
        .and_then(|r| r.strip_prefix(' '))
        .unwrap_or(seg)
}

/// The code a notice's words name: the last word of `text`, or `None` when
/// it is neither code.
fn code_in(text: &str) -> Option<Code> {
    match text.rsplit([' ', '(']).next()? {
        "ENOSPC" => Some(Code::Enospc),
        "EDQUOT" => Some(Code::Edquot),
        _ => None,
    }
}

/// The head 2.1.302 through 2.1.308 drew on a failed command's lost output
/// (`Command output was lost: the temp filesystem at <dir> is full …`);
/// 2.1.309 draws the `wall.disk_full.tool*` heads instead. Kept off the
/// anchor table, which holds the store build to every literal it lists.
pub const TOOL_HEAD_BEFORE_2_1_309: &str = "Command output was lost: the temp filesystem at";

/// The vendor's result row opens with `⎿` at column two; its text follows
/// after white space (2.1.309 draws a space and a no-break space, U+00A0,
/// MEASURED: every `⎿` row of `prompt::fixtures::TOOL_DISK_FULL_2_1_309`).
const RESULT_GLYPH: &str = "  ⎿";

/// The indent of a result's rows under its `⎿` row: five columns.
const RESULT_BODY: &str = "     ";

/// What a failed command's `⎿` row opens with before its exit line
/// (`⎿  Error: Exit code 1`, MEASURED: `prompt::fixtures::TOOL_DISK_FULL_2_1_309`);
/// the exit line itself is the bundle's `wall.disk_full.tool_exit`, then
/// the code.
const FAILED_RESULT: &str = "Error: ";

/// The most rows a tool's notice is joined from: its head row and the rows
/// it wraps onto. 2.1.309's statfs form takes four at 144 columns
/// (MEASURED); rewrapped as Ink wraps it, each of the four forms with a
/// directory as long as the measured one (`/private/tmp/claude-` and a
/// three-digit uid) reaches its directory phrase within these rows down to
/// a 40-column pane, where the quota form (the longest before its phrase)
/// ends it on its fifth row (`a_narrow_panes_notice_reads_across_its_wrap`).
const TOOL_NOTICE_ROWS: usize = 6;

/// The text of a vendor's result row ([`RESULT_GLYPH`], then white space),
/// or `None` for any other row.
fn result_row(row: &str) -> Option<&str> {
    let rest = row.strip_prefix(RESULT_GLYPH)?;
    let text = crate::text::trim_start(rest);
    (rest.is_empty() || text.len() < rest.len()).then_some(text)
}

/// Whether `row` is a failed command's exit line: the vendor's `⎿` row
/// reading `Error: Exit code <n>` and nothing more (the row trimmed, the
/// anchor's closing space leaves no empty code).
fn exit_row(row: &str) -> bool {
    let exit = const { crate::anchors::anchor_text("wall.disk_full.tool_exit") };
    result_row(crate::text::trim_end(row))
        .and_then(|t| t.strip_prefix(FAILED_RESULT))
        .and_then(|t| t.strip_prefix(exit))
        .is_some_and(|code| code.bytes().all(|b| b.is_ascii_digit()))
}

/// The text a tool's result opens with on row `i`: the vendor's `⎿` row at
/// column two, or the row right under a failed command's exit line
/// ([`exit_row`]), five columns in: that command's first line of output.
/// What the text must open with is the caller's (`tool_output`'s heads,
/// none of which opens with white space).
fn result_head(rows: &[String], i: usize) -> Option<&str> {
    let row = crate::text::trim_end(&rows[i]);
    if let Some(text) = result_row(row) {
        return Some(text);
    }
    let text = row.strip_prefix(RESULT_BODY)?;
    i.checked_sub(1)
        .is_some_and(|above| exit_row(&rows[above]))
        .then_some(text)
}

/// A tool's disk-full notice opening row `i`: where a tool's result opens
/// (`vendor`: above the composer, [`result_head`]), or opening a segment of
/// a footer row under it. A 2.1.309 head counts only with its directory
/// phrase in the notice's rows.
fn tool_output(rows: &[String], i: usize, vendor: bool) -> Option<DiskFull> {
    if vendor && let Some(found) = pre_start(rows, i) {
        return Some(found);
    }
    let heads = [
        const { crate::anchors::anchor_text("wall.disk_full.tool") },
        const { crate::anchors::anchor_text("wall.disk_full.tool_inodes") },
        const { crate::anchors::anchor_text("wall.disk_full.tool_quota") },
        TOOL_HEAD_BEFORE_2_1_309,
    ];
    let row = crate::text::trim_end(&rows[i]);
    let seg = if vendor {
        result_head(rows, i)?
    } else {
        segments(row).find(|s| heads.iter().any(|h| s.starts_with(h)))?
    };
    let head = heads.into_iter().find(|h| seg.starts_with(h))?;
    // The rest of the notice is wrapped onto the rows under it: a result's
    // rows (five columns in) above the composer, any text under it.
    let joined: String = std::iter::once(seg)
        .chain(
            rows.iter()
                .skip(i + 1)
                .take(TOOL_NOTICE_ROWS - 1)
                .take_while(|r| !vendor || r.starts_with(RESULT_BODY))
                .map(|r| r.trim())
                .take_while(|r| !r.is_empty()),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let code = if head == TOOL_HEAD_BEFORE_2_1_309 {
        if joined.contains("EDQUOT") || joined.contains("disk quota") {
            Code::Edquot
        } else {
            Code::Enospc
        }
    } else {
        if !joined.contains(const { crate::anchors::anchor_text("wall.disk_full.tool_dir") }) {
            return None;
        }
        if head == const { crate::anchors::anchor_text("wall.disk_full.tool_quota") } {
            Code::Edquot
        } else {
            Code::Enospc
        }
    };
    Some(DiskFull {
        code,
        source: Source::ToolOutput,
        row: i,
        message: seg.to_string(),
    })
}

/// A row of an error result's message under its `⎿` row: exactly five
/// columns in. Claude Code wraps an error result's message by word and
/// opens each row after the first on a word (MEASURED:
/// `prompt::fixtures::TOOL_ERROR_AUTO_MODE_2_1_309`), so a row that opens on
/// a space is no row of a message.
fn message_row(row: &str) -> bool {
    row.strip_prefix(RESULT_BODY)
        .is_some_and(|text| !text.is_empty() && !text.starts_with(' '))
}

/// The notice of a command Claude Code did not start (2.1.309): the result
/// of a call that threw before it spawned, drawn on the vendor's `⎿` row as
/// `Error: ` and the notice, the rest of it on the [`message_row`]s under
/// it. Joined across those rows with one space each, it must be the
/// bundle's template, only the code and the directory free:
/// `wall.disk_full.pre_start`, then `wall.disk_full.pre_start_full` and
/// ENOSPC or `wall.disk_full.pre_start_quota` and EDQUOT (the bundle draws
/// no other pair), `)`, `wall.disk_full.pre_start_volume`, the directory,
/// `wall.disk_full.tool_dir`, `wall.disk_full.pre_start_ran`, ` ` and
/// `wall.disk_full.free_space`, all of it (the measured error result is
/// drawn whole, every row of its message).
///
/// A command that prints the same words gets them wrapped by column, not by
/// word (MEASURED: `prompt::fixtures::TOOL_PRESTART_PRINTED_2_1_309`): a row
/// broken inside a word joins with a space the template lacks, and a row
/// that opens on the space ends the join short, so either leaves the
/// template unmatched. What still reads is a column wrap whose every break
/// falls just after a space, or inside the directory: rows a word wrap
/// would also draw, told apart only by the pane's wrap width, which this
/// reader does not model.
fn pre_start(rows: &[String], i: usize) -> Option<DiskFull> {
    let seg = result_row(crate::text::trim_end(&rows[i]))?.strip_prefix(FAILED_RESULT)?;
    let joined = std::iter::once(seg)
        .chain(
            rows[i + 1..]
                .iter()
                .take_while(|r| message_row(r))
                .map(|r| crate::text::trim(r)),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let said =
        joined.strip_prefix(const { crate::anchors::anchor_text("wall.disk_full.pre_start") })?;
    let (code, rest) = [
        (
            const { crate::anchors::anchor_text("wall.disk_full.pre_start_full") },
            Code::Enospc,
        ),
        (
            const { crate::anchors::anchor_text("wall.disk_full.pre_start_quota") },
            Code::Edquot,
        ),
    ]
    .into_iter()
    .find_map(|(words, code)| {
        let rest = said
            .strip_prefix(words)?
            .strip_prefix(code.as_str())?
            .strip_prefix(')')?;
        Some((code, rest))
    })?;
    let rest = rest
        .strip_prefix(const { crate::anchors::anchor_text("wall.disk_full.pre_start_volume") })?;
    let (dir, rest) =
        rest.split_once(const { crate::anchors::anchor_text("wall.disk_full.tool_dir") })?;
    let advice = rest
        .strip_prefix(const { crate::anchors::anchor_text("wall.disk_full.pre_start_ran") })?
        .strip_prefix(' ')?;
    let whole = advice == const { crate::anchors::anchor_text("wall.disk_full.free_space") };
    (!dir.is_empty() && whole).then(|| DiskFull {
        code,
        source: Source::ToolOutput,
        row: i,
        message: seg.to_string(),
    })
}

/// A goal the vendor ended on a full disk: its notice opening a segment of
/// a footer row, or (`vendor`: above the composer) a vendor row, `⎿` or
/// `⏺` at its head.
fn goal_cleared(rows: &[String], i: usize, vendor: bool) -> Option<DiskFull> {
    let anchor = const { crate::anchors::anchor_text("wall.disk_full.goal") };
    let row = crate::text::trim_end(&rows[i]);
    let seg = if vendor {
        let t = row.trim_start();
        t.strip_prefix('⎿')
            .or_else(|| t.strip_prefix('⏺'))?
            .trim_start()
    } else {
        segments(row).find(|s| s.starts_with(anchor))?
    };
    seg.strip_prefix(anchor)
        .is_some_and(|rest| rest.starts_with("disk full)"))
        .then(|| DiskFull {
            code: Code::Enospc,
            source: Source::GoalCleared,
            row: i,
            message: seg.to_string(),
        })
}

#[cfg(test)]
#[path = "disk_full_tests.rs"]
mod tests;
