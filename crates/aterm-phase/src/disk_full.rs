// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code disk-full notice detection by screen placement.

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
    /// Pinned disk-full notification.
    Notification,
    /// Failed command output or pre-start error.
    ToolOutput,
    /// Goal cleared by disk full.
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
    let room = message_room(rows, frame);
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
                .or_else(|| pre_start(rows, i, room))
                .or_else(|| tool_output(rows, i, true))
                .or_else(|| goal_cleared(rows, i, true))
        })
        .or_else(|| {
            let end = block.status.unwrap_or(frame.top).min(frame.top);
            (0..end)
                .rev()
                .take(TOOL_ROWS)
                .take_while(|&i| !rows[i].starts_with('❯'))
                .find_map(|i| {
                    pre_start(rows, i, room)
                        .or_else(|| tool_output(rows, i, true))
                        .or_else(|| goal_cleared(rows, i, true))
                })
        })
}

const TOOL_ROWS: usize = 40;

/// Width a result's message wraps at: the composer rule's width (the pane's)
/// less the message indent.
fn message_room(rows: &[String], frame: crate::phase::Frame) -> usize {
    crate::question::cells(crate::text::trim_end(&rows[frame.top]))
        .saturating_sub(RESULT_BODY.len())
}

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

const GLYPH: char = '\u{26A0}';

fn after_glyph(seg: &str) -> &str {
    seg.strip_prefix(GLYPH)
        .map(|r| r.strip_prefix('\u{FE0F}').unwrap_or(r))
        .and_then(|r| r.strip_prefix(' '))
        .unwrap_or(seg)
}

fn code_in(text: &str) -> Option<Code> {
    match text.rsplit([' ', '(']).next()? {
        "ENOSPC" => Some(Code::Enospc),
        "EDQUOT" => Some(Code::Edquot),
        _ => None,
    }
}

/// Prefix drawn before 2_1_309 on a failed command's lost output.
pub const TOOL_HEAD_BEFORE_2_1_309: &str = "Command output was lost: the temp filesystem at";

const RESULT_GLYPH: &str = "  ⎿";

const RESULT_BODY: &str = "     ";

const FAILED_RESULT: &str = "Error: ";

const TOOL_NOTICE_ROWS: usize = 6;

fn result_row(row: &str) -> Option<&str> {
    let rest = row.strip_prefix(RESULT_GLYPH)?;
    let text = crate::text::trim_start(rest);
    (rest.is_empty() || text.len() < rest.len()).then_some(text)
}

fn exit_row(row: &str) -> bool {
    let exit = const { crate::anchors::anchor_text("wall.disk_full.tool_exit") };
    result_row(crate::text::trim_end(row))
        .and_then(|t| t.strip_prefix(FAILED_RESULT))
        .and_then(|t| t.strip_prefix(exit))
        .is_some_and(|code| code.bytes().all(|b| b.is_ascii_digit()))
}

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

fn tool_output(rows: &[String], i: usize, vendor: bool) -> Option<DiskFull> {
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

fn message_row(row: &str) -> bool {
    row.strip_prefix(RESULT_BODY)
        .is_some_and(|text| !text.is_empty() && !text.starts_with(' '))
}

/// Pre-start notice on the `⎿` row: the whole template, word-wrapped at
/// `room`. A printed copy (wrapped by column) or a cut notice is refused.
fn pre_start(rows: &[String], i: usize, room: usize) -> Option<DiskFull> {
    let head = result_row(crate::text::trim_end(&rows[i]))?;
    let seg = head.strip_prefix(FAILED_RESULT)?;
    let drawn: Vec<&str> = std::iter::once(head)
        .chain(
            rows[i + 1..]
                .iter()
                .take_while(|r| message_row(r))
                .map(|r| crate::text::trim(r)),
        )
        .collect();
    let joined = drawn.join(" ");
    let said = joined
        .strip_prefix(FAILED_RESULT)?
        .strip_prefix(const { crate::anchors::anchor_text("wall.disk_full.pre_start") })?;
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
    (!dir.is_empty() && whole && word_wrapped(&drawn, room)).then(|| DiskFull {
        code,
        source: Source::ToolOutput,
        row: i,
        message: seg.to_string(),
    })
}

/// Whether each row break is a word wrap's at `room`. A row that fills
/// `room` carries its trailing space onto the next row, as
/// `prompt::fixtures::ink_wrap` models Ink's wrap.
fn word_wrapped(drawn: &[&str], room: usize) -> bool {
    let mut carried = 0;
    drawn.windows(2).all(|pair| {
        let used = carried + crate::question::cells(pair[0]);
        let next = pair[1].split(' ').next().unwrap_or_default();
        carried = usize::from(used >= room);
        used + 1 + crate::question::cells(next) > room
    })
}

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
