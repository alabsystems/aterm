// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Gemini CLI screen grammar, measured on 0.46.0 and read from its bundle.

use crate::anchors::anchor_text;
use crate::phase::{Phase, leading_spaces, plain_rule_width};
use crate::prompt::{Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select};
use crate::text::blank;
use crate::wall::{ApiCause, Placement, Wall, WallKind};

/// Version of the Gemini CLI (`gemini`) harness, tracked independently per harness starting at `"0.1"`.
pub const HARNESS_VERSION: &str = "0.1";

const SPINNER: std::ops::RangeInclusive<char> = '\u{2801}'..='\u{28FF}';

/// The spinner row (`is_spinner_row`) as the wire's regex: a frame in column 1 (column 0 with the UI details hidden), a blank, a word — what [`crate::reader::GeminiReader`]'s busy guard waits on to leave.
pub(crate) fn busy_guard() -> String {
    format!("^\\s?[{}-{}]\\s\\S", SPINNER.start(), SPINNER.end())
}

fn is_spinner_row(row: &str) -> bool {
    let mut c = row.chars().peekable();
    if c.peek() == Some(&' ') {
        c.next();
    }
    c.next().is_some_and(|g| SPINNER.contains(&g))
        && c.next() == Some(' ')
        && c.next().is_some_and(|w| !w.is_whitespace())
}

const BAND_TOP: &str = "▄";
const BAND_BOTTOM: &str = "▀";

fn run_of(row: &str, glyph: &str) -> Option<usize> {
    let t = crate::text::trim_end(row).as_bytes();
    let g = glyph.as_bytes();
    (t.len() >= 10 * g.len() && t.chunks(g.len()).all(|c| c == g)).then_some(t.len() / g.len())
}

const CARET: u8 = b'>';
const CARET_YOLO: u8 = b'*';
const CARET_SHELL: u8 = b'!';
const CARET_SENT_SHELL: u8 = b'$';

fn caret_glyph(row: &str) -> Option<u8> {
    let b = row.as_bytes();
    if b.first() != Some(&b' ') || !b.get(2).is_none_or(|&c| c == b' ') {
        return None;
    }
    b.get(1).copied().filter(u8::is_ascii_graphic)
}

fn is_caret_row(row: &str) -> bool {
    caret_glyph(row).is_some_and(|g| [CARET, CARET_YOLO, CARET_SHELL].contains(&g))
}

fn is_sent_row(row: &str) -> bool {
    caret_glyph(row).is_some_and(|g| [CARET, CARET_SENT_SHELL].contains(&g))
}

fn is_band_text(row: &str) -> bool {
    blank(row) || leading_spaces(row) >= 3
}

/// The composer, read whole.
#[derive(Debug, Clone, Copy)]
struct Frame {
    /// The status row: right above the live zone's rule, or right above the
    /// band with the UI details hidden (no rule).
    status: Option<usize>,
    /// The live zone's first row: the status row, else the band's top.
    live: usize,
    /// The live zone's `─` rule over the band ([`frame_at`]'s, as wide as
    /// the band); `None` with the UI details hidden at the default approval
    /// mode, which draws no rule ([`details_hidden_frame`]).
    rule: Option<usize>,
    caret: usize,
    /// The caret's glyph ([`CARET`], [`CARET_YOLO`], [`CARET_SHELL`]).
    glyph: u8,
    /// The band's bottom row (`▀`).
    bottom: usize,
    /// Rows other than the footer are drawn under the band: the command
    /// menu, or a list this grammar has not measured.
    menu: bool,
}

const UNDER_RULE_ROWS: usize = 4;

fn frame(rows: &[String]) -> Option<Frame> {
    (1..rows.len())
        .rev()
        .filter(|&t| run_of(&rows[t], BAND_TOP).is_some())
        .find_map(|t| frame_at(rows, t))
        .or_else(|| details_hidden_frame(rows))
}

fn details_hidden_frame(rows: &[String]) -> Option<Frame> {
    let bottom = rows.iter().rposition(|r| !blank(r))?;
    let width = run_of(&rows[bottom], BAND_BOTTOM)?;
    let mut caret = bottom.checked_sub(1)?;
    while caret > 0 && !is_caret_row(&rows[caret]) && is_band_text(&rows[caret]) {
        caret -= 1;
    }
    if !is_caret_row(&rows[caret]) {
        return None;
    }
    let top = caret.checked_sub(1)?;
    let status = top.checked_sub(1)?;
    if run_of(&rows[top], BAND_TOP) != Some(width) || !is_status_row(&rows[status]) {
        return None;
    }
    Some(Frame {
        status: Some(status),
        live: status,
        rule: None,
        caret,
        glyph: caret_glyph(&rows[caret])?,
        bottom,
        menu: false,
    })
}

fn frame_at(rows: &[String], top: usize) -> Option<Frame> {
    let width = run_of(&rows[top], BAND_TOP)?;
    let rule = (top.saturating_sub(UNDER_RULE_ROWS + 1)..top)
        .rev()
        .find(|&r| plain_rule_width(&rows[r]).is_some())?;
    if plain_rule_width(&rows[rule]) != Some(width)
        || rows[rule + 1..top]
            .iter()
            .any(|r| run_of(r, BAND_TOP).is_some() || run_of(r, BAND_BOTTOM).is_some())
    {
        return None;
    }
    let caret = top + 1;
    if !rows.get(caret).is_some_and(|r| is_caret_row(r)) {
        return None;
    }
    let bottom = (caret + 1..rows.len()).find(|&i| !is_band_text(&rows[i]))?;
    if run_of(&rows[bottom], BAND_BOTTOM) != Some(width) {
        return None;
    }
    let menu = rows[bottom + 1..]
        .iter()
        .find(|r| !blank(r))
        .is_some_and(|r| {
            !crate::text::contains(r, const { anchor_text("gemini.footer.workspace") })
        });
    let status = rule.checked_sub(1);
    Some(Frame {
        status,
        live: status.unwrap_or(rule),
        rule: Some(rule),
        caret,
        glyph: caret_glyph(&rows[caret])?,
        bottom,
        menu,
    })
}

fn caret_text(row: &str) -> &str {
    let text = crate::text::trim_end(row).get(3..).unwrap_or("");
    if text.strip_prefix("  ") == Some(const { anchor_text("gemini.composer.placeholder") }) {
        ""
    } else {
        crate::text::trim(text)
    }
}

fn draft(rows: &[String], f: Frame) -> Vec<String> {
    std::iter::once(caret_text(&rows[f.caret]))
        .chain(
            rows[f.caret + 1..f.bottom]
                .iter()
                .map(|r| crate::text::trim(r)),
        )
        .map(str::to_string)
        .collect()
}

/// The composer's text as `(caret row, lines)`: the caret row's text first (empty under the placeholder: [`caret_text`]), then each row its draft wrapped onto.
#[must_use]
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let f = frame(rows).filter(|f| f.glyph != CARET_SHELL)?;
    Some((f.caret, draft(rows, f)))
}

/// The caret the composer on `rows` is drawn with — what a guard on text typed into it anchors to: `>`, or `*` while YOLO is on; `>` with no composer on the screen.
#[must_use]
pub fn caret_on(rows: &[String]) -> Option<char> {
    match frame(rows).map(|f| f.glyph) {
        Some(CARET_SHELL) => None,
        Some(glyph) => Some(char::from(glyph)),
        None => Some(char::from(CARET)),
    }
}

/// The row of the live zone's `─` rule ([`Frame`]): the plain rule over the approval mode and the composer's band, as wide as the band — Gemini CLI's top line, where aterm's glass draws the program's mark and the harness switch (presentation only: nothing reads a phase from it).
#[must_use]
pub fn top_rule(rows: &[String]) -> Option<usize> {
    frame(rows)?.rule
}

/// The most lines a paste is drawn as typed ([`folded_paste`]): the bundle's `LARGE_PASTE_LINE_THRESHOLD`.
pub const PASTE_FOLD_LINES: usize = 5;

/// The most UTF-16 code units a paste is drawn as typed ([`folded_paste`]): the bundle's `LARGE_PASTE_CHAR_THRESHOLD`, held to a JavaScript string's `length`.
pub const PASTE_FOLD_UNITS: usize = 500;

/// THE CHIP A LONG PASTE IS DRAWN AS: what the composer shows in place of `text` written into it as ONE paste (a plain `turn`).
#[must_use]
pub fn folded_paste(text: &str) -> Option<String> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines = text.split('\n').count();
    let units = text.encode_utf16().count();
    if lines > PASTE_FOLD_LINES {
        Some(format!("[Pasted Text: {lines} lines]"))
    } else if units > PASTE_FOLD_UNITS {
        Some(format!("[Pasted Text: {units} chars]"))
    } else {
        None
    }
}

/// What the status row says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// ` ⠧ … (esc to cancel, 3s)`: a turn runs.
    Spinner,
    /// `? for shortcuts` alone, on the right (`press tab twice for more`
    /// with the UI details hidden): the composer empty and idle.
    Shortcuts,
    /// Nothing: a draft is held, or the composer is empty and idle where no
    /// hint is drawn (a narrow pane, `ui.showShortcutsHint = false`).
    Blank,
    /// Words this grammar has not measured (a tip), or no status row.
    Other,
}

const MODES: [&str; 3] = [
    anchor_text("gemini.mode.yolo"),
    anchor_text("gemini.mode.auto_edit"),
    anchor_text("gemini.mode.plan"),
];

fn status_words(row: &str) -> &str {
    let t = crate::text::trim(row);
    t.strip_prefix("● ")
        .and_then(|rest| {
            MODES.iter().find_map(|m| {
                rest.strip_prefix(m)
                    .filter(|after| after.is_empty() || after.starts_with("  "))
            })
        })
        .map_or(t, crate::text::trim)
}

fn status_kind(row: &str) -> Status {
    if is_spinner_row(row) {
        return Status::Spinner;
    }
    let words = status_words(row);
    if words.is_empty() {
        Status::Blank
    } else if words == const { anchor_text("gemini.status.shortcuts") }
        || words == const { anchor_text("gemini.status.details") }
    {
        Status::Shortcuts
    } else {
        Status::Other
    }
}

fn is_status_row(row: &str) -> bool {
    status_kind(row) != Status::Other
}

fn status_of(rows: &[String], f: Frame) -> Status {
    f.status.map_or(Status::Other, |s| status_kind(&rows[s]))
}

fn last_user(rows: &[String], end: usize) -> Option<usize> {
    (1..end.min(rows.len())).rev().find(|&i| {
        run_of(&rows[i], BAND_BOTTOM).is_some() && {
            let mut c = i - 1;
            while c > 0 && !is_sent_row(&rows[c]) && is_band_text(&rows[c]) {
                c -= 1;
            }
            is_sent_row(&rows[c]) && c > 0 && run_of(&rows[c - 1], BAND_TOP).is_some()
        }
    })
}

fn ended_on(rows: &[String]) -> Option<(usize, usize)> {
    let f = frame(rows)?;
    let end = f.status?;
    if status_of(rows, f) == Status::Spinner {
        return None;
    }
    let from = last_user(rows, end).map_or(0, |u| u + 1);
    Some((from, trust_notices_above(rows, from, end)))
}

const TRUST_NOTICES: [&str; 3] = [
    anchor_text("gemini.notice.still_trusted"),
    anchor_text("gemini.notice.untrusted"),
    anchor_text("gemini.notice.agents_skipped"),
];

fn trust_notices_above(rows: &[String], from: usize, end: usize) -> usize {
    let mut end = end;
    loop {
        let Some(last) = (from..end).rev().find(|&i| !blank(&rows[i])) else {
            return end;
        };
        let mut head = last;
        while head > from && !blank(&rows[head - 1]) {
            head -= 1;
        }
        let Some(said) = rows[head]
            .strip_prefix("⚠ ")
            .or_else(|| rows[head].strip_prefix("ℹ "))
        else {
            return end;
        };
        if rows[head + 1..=last].iter().any(|r| leading_spaces(r) < 2) {
            return end;
        }
        let words = std::iter::once(said)
            .chain(rows[head + 1..=last].iter().map(String::as_str))
            .map(crate::text::trim)
            .collect::<Vec<_>>()
            .join(" ");
        if !TRUST_NOTICES.iter().any(|n| words.starts_with(n)) {
            return end;
        }
        end = head;
    }
}

fn is_banner_row(row: &str) -> bool {
    let name = const { anchor_text("gemini.banner") };
    crate::text::find(row, name).is_some_and(|at| {
        row[at + name.len()..]
            .strip_prefix(" v")
            .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// The phase on `rows`, and whether it is evidence.
#[must_use]
pub fn phase(rows: &[String]) -> (Phase, bool) {
    let Some(f) = frame(rows) else {
        return if prompt(rows).is_some() {
            (Phase::Prompt, true)
        } else {
            (Phase::Idle, false)
        };
    };
    if kept_box(rows, f).is_some() {
        return (Phase::Prompt, true);
    }
    let empty = draft(rows, f).iter().all(String::is_empty);
    let idle = match status_of(rows, f) {
        Status::Spinner => return (Phase::Busy, true),
        Status::Shortcuts => empty,
        Status::Blank => true,
        Status::Other => false,
    };
    if !idle || f.menu || f.glyph == CARET_SHELL {
        return (Phase::Idle, false);
    }
    let asks = said_tail(rows).is_some_and(|t| crate::turn::asks(&t));
    (if asks { Phase::Question } else { Phase::Idle }, true)
}

/// No turn yet: the logo naming the build above the composer and no message under it.
#[must_use]
pub fn fresh(rows: &[String]) -> bool {
    frame(rows).is_some_and(|f| {
        last_user(rows, f.live).is_none() && rows[..f.live].iter().any(|r| is_banner_row(r))
    })
}

/// A MESSAGE STANDS ON `rows` ([`crate::ScreenReader::message_drawn`]): the band of a message a person sent, above the live zone (above every row where no composer is drawn), answered or not.
#[must_use]
pub fn message_drawn(rows: &[String]) -> bool {
    let end = frame(rows).map_or(rows.len(), |f| f.live);
    last_user(rows, end).is_some()
}

fn is_tool_row(row: &str) -> bool {
    leading_spaces(row) == 2
        && ["✓  ", "✕  ", "⊷  ", "?  "]
            .iter()
            .any(|g| crate::text::trim_start(row).starts_with(g))
}

/// The agent's last words: the reply under the last message — its `✦ ` row and the rows under it, each indented, one line per row, the `✦` and the indent stripped; the visible rows from the top when its head scrolled away.
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    said_reply(rows).map(|(_, said)| said)
}

/// WHETHER THE REPLY [`said_tail`] READS IS WHOLE ON `rows`: its `✦ ` head is one of them.
#[must_use]
pub fn said_whole(rows: &[String]) -> bool {
    said_reply(rows).is_some_and(|(head, _)| head.is_some())
}

/// THE AGENT'S OTHER WORDS IN THE TURN ITS LAST WORDS END ([`said_tail`]): every `✦ ` reply of the same turn above its last — the words it said before a tool call, say — oldest first, each as [`said_tail`] reads one (its `✦` and indent stripped, one line per row, from its head down to the last of its rows before Gemini CLI's own: a tool's box or row, a notice), joined by a blank line.
#[must_use]
pub fn turn_said_before(rows: &[String]) -> Option<String> {
    let (_, said) = turn_before(rows)?;
    (!said.is_empty()).then(|| said.join("\n\n"))
}

/// WHETHER THE TURN [`said_tail`]'s REPLY ENDS IS WHOLE ON `rows`: the reply is ([`said_whole`]), and so is the band of the person's message that opened the turn.
#[must_use]
pub fn turn_whole(rows: &[String]) -> bool {
    turn_before(rows).is_some_and(|(opened, _)| opened)
}

/// A row a read of the history above the screen stops at, for a turn [`turn_whole`] finds cut: the top of a band (`▄▄▄…`), the person's message's that opened the turn, read walking up — its `▀` bottom and its text are under it.
#[must_use]
pub fn turn_history_stop(row: &str) -> bool {
    run_of(row, BAND_TOP).is_some()
}

/// THE MESSAGE THAT OPENED THE TURN [`said_tail`]'s REPLY ENDS: the text of the band of the person's last message (` > text`, the rows it wrapped onto three columns in), joined by a space.
#[must_use]
pub fn turn_opened_by(rows: &[String]) -> Option<String> {
    said_reply(rows)?;
    let (_, end) = ended_on(rows)?;
    let bottom = last_user(rows, end)?;
    let sent = (1..bottom).rev().find(|&c| is_sent_row(&rows[c]))?;
    let text: Vec<&str> = std::iter::once(rows[sent].get(3..).unwrap_or(""))
        .chain(rows[sent + 1..bottom].iter().map(String::as_str))
        .map(crate::text::trim)
        .filter(|t| !t.is_empty())
        .collect();
    Some(text.join(" "))
}

fn turn_before(rows: &[String]) -> Option<(bool, Vec<String>)> {
    let (from, end) = ended_on(rows)?;
    let head = said_reply(rows)?.0?;
    let own = |r: &String| blank(r) || (leading_spaces(r) >= 2 && !is_tool_row(r));
    let mut said = Vec::new();
    for h in (from..head).filter(|&h| rows[h].starts_with("✦ ")) {
        let last = (h + 1..head)
            .take_while(|&k| own(&rows[k]))
            .filter(|&k| !blank(&rows[k]))
            .last()
            .unwrap_or(h);
        let words: Vec<&str> = rows[h..=last]
            .iter()
            .map(|r| crate::text::trim(r.strip_prefix("✦ ").unwrap_or(r)))
            .filter(|t| !t.is_empty())
            .collect();
        if !words.is_empty() {
            said.push(words.join("\n"));
        }
    }
    Some((last_user(rows, end).is_some(), said))
}

fn said_reply(rows: &[String]) -> Option<(Option<usize>, String)> {
    let (from, end) = ended_on(rows)?;
    if from == 0 && rows[..end].iter().any(|r| is_banner_row(r)) {
        return None;
    }
    let last = (from..end).rev().find(|&i| !blank(&rows[i]))?;
    let head = (from..=last).rev().find(|&i| rows[i].starts_with("✦ "));
    let first = match head {
        Some(h) => h,
        None if from == 0 => (0..=last).find(|&i| !blank(&rows[i]))?,
        None => return None,
    };
    let body = &rows[first..=last];
    let own = |(k, r): (usize, &String)| {
        blank(r) || (k == 0 && head.is_some()) || (leading_spaces(r) >= 2 && !is_tool_row(r))
    };
    if !body.iter().enumerate().all(own) {
        return None;
    }
    let out: Vec<&str> = body
        .iter()
        .map(|r| crate::text::trim(r.strip_prefix("✦ ").unwrap_or(r)))
        .filter(|t| !t.is_empty())
        .collect();
    Some((head, out.join("\n")))
}

/// Whether a PERSON stopped the last turn: under the last message, Gemini CLI's `ℹ Request cancelled.` (Esc in the turn).
#[must_use]
pub fn interrupted(rows: &[String]) -> bool {
    ended_on(rows).is_some_and(|(from, end)| {
        rows[from..end].iter().any(|r| {
            r.strip_prefix("ℹ ").is_some_and(|t| {
                crate::text::trim(t) == const { anchor_text("gemini.turn.cancelled") }
            })
        })
    })
}

const KEY_INVALID: &str = "API_KEY_INVALID";

fn error_kind(message: &str) -> WallKind {
    let squeezed: String = message.chars().filter(|c| !c.is_whitespace()).collect();
    let code = squeezed.find("\"code\":").and_then(|at| {
        let digits: String = squeezed[at + 7..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse::<u16>().ok()
    });
    if code == Some(401) || squeezed.contains(KEY_INVALID) {
        return WallKind::Auth;
    }
    WallKind::ApiError {
        code,
        retryable: code.is_some_and(|c| c == 429 || c >= 500),
        cause: ApiCause::Server,
    }
}

/// The wall the last turn ended on.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    let (from, end) = ended_on(rows)?;
    let at = (from..end).rev().find(|&i| rows[i].starts_with("✕ "))?;
    if !rows[at]["✕ ".len()..].starts_with(const { anchor_text("gemini.error.api") }) {
        return None;
    }
    let mut last = at;
    while last + 1 < end && !blank(&rows[last + 1]) && leading_spaces(&rows[last + 1]) >= 2 {
        last += 1;
    }
    let mut notice = false;
    for r in &rows[last + 1..end] {
        notice = if blank(r) {
            false
        } else if r.starts_with("ℹ ") || (notice && leading_spaces(r) >= 2) {
            true
        } else {
            return None;
        };
    }
    let message = rows[at..=last]
        .iter()
        .map(|r| crate::text::trim(r.strip_prefix("✕ ").unwrap_or(r)))
        .collect::<Vec<_>>()
        .join(" ");
    Some(Wall {
        kind: error_kind(&message),
        message,
        reset: None,
        row: at,
        placement: Placement::ErrorRow,
        under_call: false,
    })
}

/// Whether `w` is the error a turn ends on once a dialog's `2.`
#[must_use]
pub fn stopped_error(w: &Wall) -> bool {
    matches!(
        w.kind,
        WallKind::ApiError {
            code: None,
            retryable: false,
            ..
        }
    ) && w.placement == Placement::ErrorRow
        && w.message
            .contains(const { anchor_text("gemini.error.unknown") })
}

/// Whether `rows` are Gemini CLI's by their layout alone, for a session whose program name says nothing: its composer under the live zone's rule (or with the UI details hidden), or one of its boxes, named ([`prompt`]: a kind this grammar reads, its one-shot `Allow once`, or the first row of its model-fallback dialog).
pub(crate) fn is_gemini_screen(rows: &[String]) -> bool {
    frame(rows).is_some()
        || prompt(rows).is_some_and(|p| {
            p.kind != PromptKind::Other
                || p.options
                    .iter()
                    .any(|o| o.label == const { anchor_text("gemini.box.once") })
                || is_fallback_head(&p.title)
        })
}

/// A box's rows: its border's column, its top border's row (`None` with its head off the rows read), the first row inside it and its bottom border.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GeminiBox {
    col: usize,
    top: Option<usize>,
    first: usize,
    bottom: usize,
}

fn border(row: &str, col: usize, open: char, close: char) -> Option<usize> {
    if leading_spaces(row) != col {
        return None;
    }
    let t = crate::text::trim(row);
    let inner = t.strip_prefix(open)?.strip_suffix(close)?;
    (!inner.is_empty() && inner.chars().all(|c| c == '─')).then(|| inner.chars().count() + 2)
}

fn inside(row: &str, col: usize, width: usize) -> Option<&str> {
    if leading_spaces(row) != col {
        return None;
    }
    let t = crate::text::trim(row);
    let inner = t.strip_prefix('│')?.strip_suffix('│')?;
    (t.chars().count() == width)
        .then(|| crate::text::trim_end(inner.strip_prefix(' ').unwrap_or(inner)))
}

/// The box on `rows`: its bottom border the last row drawn, every row above it inside the box up to
/// its top border (or the top of the rows read), no composer on the screen.
pub(crate) fn gemini_box(rows: &[String]) -> Option<GeminiBox> {
    let bottom = rows.iter().rposition(|r| !blank(r))?;
    let col = leading_spaces(&rows[bottom]);
    let width = border(&rows[bottom], col, '╰', '╯')?;
    let mut first = bottom;
    while first > 0 && inside(&rows[first - 1], col, width).is_some() {
        first -= 1;
    }
    if first == bottom {
        return None;
    }
    let top = first
        .checked_sub(1)
        .filter(|&t| border(&rows[t], col, '╭', '╮') == Some(width));
    if top.is_none() && first > 0 {
        return None;
    }
    if frame(rows).is_some() {
        return None;
    }
    Some(GeminiBox {
        col,
        top,
        first,
        bottom,
    })
}

fn numbered(text: &str) -> Option<(u8, &str, bool)> {
    crate::prompt::numbered_option(text, "● ", ". ")
}

fn approval_role(label: &str) -> Role {
    if label == const { anchor_text("gemini.box.once") } {
        Role::Once
    } else if label == const { anchor_text("gemini.box.session") }
        || label.ends_with(const { anchor_text("gemini.box.session.tail") })
        || crate::text::contains(label, const { anchor_text("gemini.box.saved") })
    {
        Role::Persist
    } else if label == const { anchor_text("gemini.box.deny") } {
        Role::Deny
    } else {
        Role::Other
    }
}

fn role_of(kind: PromptKind, label: &str) -> Role {
    match kind {
        PromptKind::Trust if label.starts_with(const { anchor_text("gemini.trust.folder") }) => {
            Role::Trust
        }
        PromptKind::Trust if label.starts_with(const { anchor_text("gemini.trust.parent") }) => {
            Role::Persist
        }
        PromptKind::Trust if label == const { anchor_text("gemini.trust.no") } => Role::Deny,
        PromptKind::Question if label == const { anchor_text("gemini.question.custom") } => {
            Role::Other
        }
        PromptKind::Question => Role::Answer,
        PromptKind::Bash | PromptKind::Write | PromptKind::Edit | PromptKind::Other => {
            approval_role(label)
        }
        _ => Role::Other,
    }
}

fn is_tab_row(text: &str) -> bool {
    text.starts_with("← ") && text.ends_with(" →") && text.contains(" │ ")
}

fn is_fallback_head(text: &str) -> bool {
    text.starts_with(const { anchor_text("gemini.fallback.no_access") })
        || (text.starts_with("Model \"")
            && text.ends_with(const { anchor_text("gemini.fallback.not_found") }))
        || text == const { anchor_text("gemini.fallback.demand") }
}

fn is_expansion_ask(text: &str) -> bool {
    text.strip_prefix(const { anchor_text("gemini.box.expansion.head") })
        .is_some_and(|rest| rest.starts_with(" ["))
        && text.ends_with(const { anchor_text("gemini.box.expansion.ask") })
}

fn is_expansion_row(text: &str) -> bool {
    text.starts_with("• ")
}

fn kind_of(texts: &[(usize, &str)], first_option: Option<usize>) -> (PromptKind, Option<usize>) {
    let over = |r: usize| first_option.is_none_or(|f| r < f);
    let title = texts.iter().find(|(_, t)| !t.is_empty());
    if let Some(&(row, t)) = title {
        if t == const { anchor_text("gemini.question.title") } {
            let from = texts
                .iter()
                .find(|&&(r, t)| r > row && is_tab_row(t))
                .map_or(row, |&(r, _)| r);
            let asks = texts
                .iter()
                .filter(|&&(r, t)| r > from && over(r) && !t.is_empty())
                .map(|&(r, _)| r)
                .next();
            return (PromptKind::Question, asks);
        }
        if t.starts_with(const { anchor_text("gemini.usage.title") }) {
            return (PromptKind::UsageLimit, None);
        }
        if is_fallback_head(t) {
            return (PromptKind::Other, Some(row));
        }
    }
    let Some(first_option) = first_option else {
        return (PromptKind::Other, None);
    };
    if let Some(&(row, _)) = texts
        .iter()
        .find(|(_, t)| *t == const { anchor_text("gemini.trust.title") })
    {
        return (PromptKind::Trust, Some(row));
    }
    let mut over = texts
        .iter()
        .rev()
        .filter(|&&(r, t)| r < first_option && !t.is_empty())
        .skip_while(|&&(_, t)| is_expansion_row(t));
    let Some(&(row, asks)) = over.next().and_then(|last| {
        if last
            .1
            .starts_with(const { anchor_text("gemini.box.redirection") })
        {
            over.next()
        } else {
            Some(last)
        }
    }) else {
        return (PromptKind::Other, None);
    };
    let tool = title
        .and_then(|(_, t)| t.strip_prefix("? "))
        .and_then(|t| t.split_whitespace().next());
    let kind = if asks.starts_with(const { anchor_text("gemini.box.exec.ask") }) {
        // `[Shell]` is the shell tool's; an MCP tool's box asks `Allow
        // execution of MCP tool "<tool>" from server "<server>"?`.
        if asks.ends_with("[Shell]?") {
            PromptKind::Bash
        } else {
            PromptKind::Other
        }
    } else if asks == const { anchor_text("gemini.box.change.ask") } {
        match tool {
            Some("WriteFile") => PromptKind::Write,
            Some("Edit") => PromptKind::Edit,
            _ => PromptKind::Other,
        }
    } else if is_expansion_ask(asks) || asks == const { anchor_text("gemini.box.proceed") } {
        PromptKind::Other
    } else {
        return (PromptKind::Other, None);
    };
    (kind, Some(row))
}

fn cancel_of(options: &[Opt], hint: Option<&str>) -> Option<Cancel> {
    if let Some(o) = options.iter().find(|o| o.label.ends_with(" (esc)")) {
        return Some(Cancel {
            key: "esc".to_string(),
            verb: o.label.trim_end_matches(" (esc)").to_string(),
            effect: CancelEffect::Reject,
        });
    }
    let hint = hint?;
    let lower = hint.to_lowercase();
    let at = lower.find("esc to ")?;
    let verb = lower[at + "esc to ".len()..]
        .split('·')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    Some(Cancel {
        key: "Esc".to_string(),
        verb,
        effect: CancelEffect::Back,
    })
}

/// WHAT ESC DOES on `p`, a box [`prompt`] read: the Esc it names ([`PromptV2::cancel`]), else, on the trust dialog, which names none, the program's exit.
#[must_use]
pub fn esc(p: &PromptV2) -> Option<CancelEffect> {
    match &p.cancel {
        Some(c) => Some(c.effect),
        None => (p.kind == PromptKind::Trust).then_some(CancelEffect::Exit),
    }
}

/// THE RESET THE USAGE-LIMIT DIALOG NAMES ([`limit_reset`]): its clock and the zone the CLI drew it in, `5:06 AM` and `PDT` of `Access resets at 5:06 AM PDT.` ([`fixtures::LIMIT_DIALOG_ACCOUNT`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitReset {
    /// The clock: its words up to its `AM`/`PM` (en-US's twelve-hour clock), else its first word, a no-break space read as a space.
    pub clock: String,
    /// The words after the clock as drawn (`PDT`, `GMT+5:30`, `UTC`): the zone the clock is in.
    pub zone: Option<String>,
}

impl LimitReset {
    /// The zone's offset east of UTC in seconds ([`zone_offset_s`]): `None` with no zone, or one no en-US short name spells.
    #[must_use]
    pub fn offset_s(&self) -> Option<i32> {
        self.zone.as_deref().and_then(zone_offset_s)
    }
}

impl std::fmt::Display for LimitReset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.zone {
            Some(zone) => write!(f, "{} {zone}", self.clock),
            None => f.write_str(&self.clock),
        }
    }
}

/// THE OFFSET OF THE ZONE THE USAGE-LIMIT DIALOG NAMES ([`LimitReset::zone`]), seconds east of UTC.
#[must_use]
pub fn zone_offset_s(zone: &str) -> Option<i32> {
    const NAMED: [(&str, i32); 17] = [
        ("UTC", 0),
        ("GMT", 0),
        ("PST", -8),
        ("PDT", -7),
        ("MST", -7),
        ("MDT", -6),
        ("CST", -6),
        ("CDT", -5),
        ("EST", -5),
        ("EDT", -4),
        ("AKST", -9),
        ("AKDT", -8),
        ("HST", -10),
        ("HAST", -10),
        ("HADT", -9),
        ("AST", -4),
        ("ADT", -3),
    ];
    if let Some((_, hours)) = NAMED.iter().find(|(name, _)| *name == zone) {
        return Some(hours * 3600);
    }
    let rest = zone.strip_prefix("GMT")?;
    let (sign, rest) = match rest.strip_prefix('+') {
        Some(r) => (1, r),
        None => (-1, rest.strip_prefix('-')?),
    };
    let (hours, minutes) = rest.split_once(':').unwrap_or((rest, "00"));
    let digits = |s: &str, len: std::ops::RangeInclusive<usize>| {
        (len.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<i32>().ok())
            .flatten()
    };
    let (h, m) = (digits(hours, 1..=2)?, digits(minutes, 2..=2)?);
    (h <= 14 && m < 60).then_some(sign * (h * 3600 + m * 60))
}

/// The model named under the newest footer's `/model` column, or `None`.
#[must_use]
pub fn footer_model(rows: &[String]) -> Option<String> {
    let head = rows.iter().rposition(|r| {
        crate::text::contains(r, const { anchor_text("gemini.footer.workspace") })
    })?;
    if !rows[head]
        .trim_end()
        .ends_with(const { anchor_text("gemini.footer.model") })
    {
        return None;
    }
    let value = rows.get(head + 1)?.trim_end();
    let cell = crate::text::trim(value.rsplit("  ").next()?);
    (!cell.is_empty()).then(|| cell.to_string())
}

/// The login identity named beside the startup banner logo, or `None`.
#[must_use]
pub fn banner_login(rows: &[String]) -> Option<String> {
    rows.iter().find_map(|r| {
        let t = crate::text::trim(
            r.trim_end()
                .strip_suffix(const { anchor_text("gemini.banner.auth.tail") })?,
        );
        let at = t
            .find(const { anchor_text("gemini.banner.auth") })
            .or_else(|| t.find(const { anchor_text("gemini.banner.google") }))?;
        Some(t[at..].to_string())
    })
}

/// The reset time named by a usage-limit dialog on `rows`.
#[must_use]
pub fn limit_reset(rows: &[String]) -> Option<LimitReset> {
    let p = prompt(rows)?;
    if p.kind != PromptKind::UsageLimit || p.head_off_screen {
        return None;
    }
    let first_option = p.options.first().map_or(p.span.1, |o| o.row);
    let head = const { anchor_text("gemini.usage.resets") }.trim_end();
    let inner = |r: &String| -> Option<String> {
        let inner = r.trim().strip_prefix('│')?.strip_suffix('│')?;
        Some(crate::text::trim(inner).to_string())
    };
    let body = rows.get(p.span.0 + 1..first_option.min(rows.len()))?;
    let at = body.iter().position(|r| {
        inner(r).is_some_and(|t| {
            t.strip_prefix(head)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
    })?;
    let mut text = inner(&body[at])?
        .strip_prefix(head)?
        .trim_start()
        .to_string();
    for r in &body[at + 1..] {
        if text.ends_with('.') {
            break;
        }
        let Some(more) = inner(r).filter(|t| !t.is_empty() && !t.starts_with('/')) else {
            break;
        };
        text.push(' ');
        text.push_str(&more);
    }
    let text = text.trim_end_matches('.');
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == '\u{202f}' || c == '\u{a0}')
        .filter(|w| !w.is_empty())
        .collect();
    let clock_words = words
        .iter()
        .position(|w| w.eq_ignore_ascii_case("AM") || w.eq_ignore_ascii_case("PM"))
        .map_or(1, |at| at + 1);
    let (clock, zone) = words.split_at_checked(clock_words)?;
    Some(LimitReset {
        clock: clock.join(" "),
        zone: (!zone.is_empty()).then(|| zone.join(" ")),
    })
}

/// Read the active prompt or dialog box on `rows` ([`PromptV2`]).
#[must_use]
pub fn prompt(rows: &[String]) -> Option<PromptV2> {
    read_box(rows).or_else(|| frame(rows).and_then(|f| kept_box(rows, f)))
}

fn over_the_queue(rows: &[String], live: usize) -> &[String] {
    let over = &rows[..live.min(rows.len())];
    let queued = over.iter().rposition(|r| {
        leading_spaces(r) == 2 && crate::text::trim(r) == const { anchor_text("gemini.queued") }
    });
    match queued {
        Some(q)
            if over[q + 1..]
                .iter()
                .all(|r| blank(r) || leading_spaces(r) >= 4) =>
        {
            &over[..q]
        }
        _ => over,
    }
}

fn kept_box(rows: &[String], f: Frame) -> Option<PromptV2> {
    let mut p = read_box(over_the_queue(rows, f.live))?;
    if !p.options.iter().any(|o| o.focused) {
        return None;
    }
    for o in &mut p.options {
        o.role = Role::Other;
    }
    Some(p)
}

fn read_box(rows: &[String]) -> Option<PromptV2> {
    let b = gemini_box(rows)?;
    let width = border(&rows[b.bottom], b.col, '╰', '╯')?;
    let texts: Vec<(usize, &str)> = (b.first..b.bottom)
        .filter_map(|i| inside(&rows[i], b.col, width).map(|t| (i, t)))
        .collect();
    let mut options: Vec<Opt> = Vec::new();
    for &(i, t) in &texts {
        if let Some((n, label, focused)) = numbered(t) {
            options.push(Opt {
                n: Some(n),
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: i,
                kept: false,
            });
        }
    }
    let first_option = options.first().map(|o| o.row);
    let head_off_screen = b.top.is_none();
    let (kind, question) = if head_off_screen {
        (PromptKind::Other, None)
    } else {
        kind_of(&texts, first_option)
    };
    let text_at = |row: usize| {
        texts
            .iter()
            .find(|&&(r, _)| r == row)
            .map_or("", |&(_, t)| t)
    };
    let review = kind == PromptKind::Question
        && question.is_some_and(|q| text_at(q) == const { anchor_text("gemini.question.review") });
    if first_option.is_none() && !review {
        return None;
    }
    let tabs = (kind == PromptKind::Question)
        .then(|| texts.iter().find(|&&(_, t)| is_tab_row(t)).map(|&(_, t)| t))
        .flatten();
    let title_row = texts
        .iter()
        .find(|(_, t)| !t.is_empty())
        .map_or(b.first, |&(r, _)| r);
    let title_text = if text_at(title_row) == const { anchor_text("gemini.box.action") } {
        texts
            .iter()
            .find(|&&(r, t)| r > title_row && !t.is_empty())
            .map_or("", |&(_, t)| t)
    } else {
        text_at(title_row)
    };
    let title = title_text
        .strip_prefix('?')
        .map_or(title_text, crate::text::trim_start);
    let ordered = options
        .iter()
        .enumerate()
        .all(|(k, o)| o.n.is_some_and(|n| usize::from(n) == k + 1));
    let focus = options.iter().filter(|o| o.focused).count();
    let named = kind != PromptKind::Other
        || is_fallback_head(title)
        || options
            .iter()
            .any(|o| o.label == const { anchor_text("gemini.box.once") });
    if ordered && focus == 1 && named && tabs.is_none() {
        for o in &mut options {
            o.role = role_of(kind, &o.label);
        }
    }
    let named_after = title
        .split_once("  ")
        .map_or("", |(_, rest)| crate::text::trim(rest));
    let head_end = first_option.unwrap_or(b.bottom);
    let asks = question.map_or("", text_at);
    let expansion = kind == PromptKind::Other && is_expansion_ask(asks);
    let mut command_rows = Vec::new();
    let mut notes: Vec<String> = tabs.map(str::to_string).into_iter().collect();
    let (mut description, mut path) = (String::new(), None);
    if kind == PromptKind::Bash || expansion {
        command_rows = texts
            .iter()
            .filter(|&&(r, _)| r < head_end)
            .filter_map(|(_, t)| {
                let inner = t.strip_prefix('│')?.strip_suffix('│')?;
                Some(crate::text::trim(inner).to_string())
            })
            .collect();
    }
    match kind {
        PromptKind::Write => {
            path = named_after
                .strip_prefix(const { anchor_text("gemini.box.writing") })
                .map(|p| crate::text::trim(p).to_string());
        }
        PromptKind::Edit => {
            path = named_after
                .split_once(": ")
                .map(|(p, _)| crate::text::trim(p).to_string());
        }
        PromptKind::Question | PromptKind::Trust => {
            description = asks.to_string();
            if review {
                notes.extend(
                    texts
                        .iter()
                        .filter(|&&(r, t)| {
                            question.is_some_and(|q| r > q) && t.contains(" → ") && !is_tab_row(t)
                        })
                        .map(|&(_, t)| t.to_string()),
                );
            }
        }
        PromptKind::UsageLimit => description = title.to_string(),
        PromptKind::Other if is_fallback_head(title) => {
            description = texts
                .iter()
                .filter(|&&(r, t)| r < head_end && !t.is_empty())
                .map(|&(_, t)| t)
                .collect::<Vec<_>>()
                .join(" ");
        }
        PromptKind::Other => {
            description = asks.to_string();
            if expansion {
                notes.extend(
                    texts
                        .iter()
                        .filter(|&&(r, t)| r < head_end && is_expansion_row(t))
                        .map(|&(_, t)| t.to_string()),
                );
            }
        }
        _ => {}
    }
    let command = match kind {
        PromptKind::Bash => command_rows.join(" "),
        PromptKind::Other if expansion => command_rows.join(" "),
        _ => path.clone().unwrap_or_default(),
    };
    let keys_from = first_option.or(question).unwrap_or(b.first);
    let hint = texts
        .iter()
        .rev()
        .find(|&&(r, t)| {
            r > keys_from
                && (t.starts_with(const { anchor_text("gemini.question.keys") })
                    || t.starts_with(const { anchor_text("gemini.question.submit") }))
        })
        .map(|&(_, t)| t);
    Some(PromptV2 {
        kind,
        title: title.to_string(),
        command_rows,
        description_rows: 0,
        gutter: false,
        command,
        description,
        notes,
        auto_deny: None,
        path,
        cancel: cancel_of(&options, hint),
        options,
        select: Select::Digits,
        question,
        span: (title_row, b.bottom),
        footer: hint.map(str::to_string),
        head_off_screen,
        question_dialog: None,
        reason_rows: Vec::new(),
    })
}

/// Gemini CLI's screens, measured, for this crate's tests and a dependent crate's `#[cfg(test)]` code; not part of the documented API.
#[doc(hidden)]
pub mod fixtures {
    /// The folder-trust dialog of a first run (folder trust on, no folder trusted), before any key.
    pub const TRUST: &str = include_str!("fixtures/gemini-0.46.0-first-run-trust.txt");
    /// A fresh session at its composer: the placeholder, `? for shortcuts`.
    pub const IDLE: &str = include_str!("fixtures/gemini-0.46.0-idle.txt");
    /// The first frame a launch draws, before its banner: the composer, its footer and `? for shortcuts`, nothing above them yet — idle, no message and no reply, and NOT `fresh` (no banner row).
    pub const PRE_BANNER: &str = include_str!("fixtures/gemini-0.46.0-pre-banner.txt");
    /// A typed, unsent draft: the status row blank.
    pub const DRAFT: &str = include_str!("fixtures/gemini-0.46.0-draft.txt");
    /// A 538-character paste folded into the composer's chip, unsent ([`super::folded_paste`]): ` > [Pasted Text: 538 chars]`, `Press Ctrl+O to expand pasted text` on the status row (the forever lab's relaunched worker, 2026-10-21).
    pub const FOLDED_PASTE: &str = include_str!("fixtures/gemini-0.46.0-folded-paste.txt");
    /// A raw `send` of 600 characters of one line (`[lab raw send] this is a long raw write …` repeated), unsent: drawn as words wrapped over five rows of the composer, no chip ([`super::folded_paste`]).
    pub const LAB_RAW_SEND_600: &str = include_str!("fixtures/gemini-0.46.0-lab-raw-send-600.txt");
    /// A raw `send` of 1600 characters of the same line, unsent: drawn as words over the composer's ten rows, its first rows scrolled inside the composer, no chip ([`super::folded_paste`]; the lab's idle manager tab, 2026-10-22).
    pub const LAB_RAW_SEND_1600: &str =
        include_str!("fixtures/gemini-0.46.0-lab-raw-send-1600.txt");
    /// A fresh composer with a `GEMINI.md` loaded: `1 GEMINI.md file` on the approval mode's row.
    pub const IDLE_CONTEXT: &str = include_str!("fixtures/gemini-0.46.0-idle-context.txt");
    /// The pane at 72 columns with a `GEMINI.md` loaded, at rest: no `? for shortcuts`, the context summary on a row of its own under a blank.
    pub const NARROW_IDLE: &str = include_str!("fixtures/gemini-0.46.0-narrow-idle.txt");
    /// The same pane holding a typed, unsent draft.
    pub const NARROW_DRAFT: &str = include_str!("fixtures/gemini-0.46.0-narrow-draft.txt");
    /// The same pane, that draft sent: the spinner on the status row.
    pub const NARROW_BUSY: &str = include_str!("fixtures/gemini-0.46.0-narrow-busy.txt");
    /// The same pane at that turn's end: its reply wrapped, the status row blank.
    pub const NARROW_END_OF_TURN: &str =
        include_str!("fixtures/gemini-0.46.0-narrow-end-of-turn.txt");
    /// The pane at 50 columns: a reply of two paragraphs, the second wrapped over two rows at column two, its last `every caller now.`
    pub const REPLY_WRAPPED_50: &str =
        include_str!("fixtures/gemini-0.46.0-lab-reply-wrapped-50col.txt");
    /// A worker aterm relaunched on its session after its own restart (the harness tape's whole read): its defense `✦ Nothing left: …`, then Gemini CLI's trust notices — `⚠  Note: This folder is still trusted because a parent folder is trusted.` and `ℹ This folder is untrusted, …` with its `/permissions` row — above the status row.
    pub const LAB_RESUMED_TRUST_NOTICES: &str =
        include_str!("fixtures/gemini-0.46.0-lab-resumed-trust-notices.txt");
    /// A turn thinking: the spinner on the status row, the thought's subject.
    pub const BUSY_THINKING: &str = include_str!("fixtures/gemini-0.46.0-busy-thinking.txt");
    /// The same turn streaming its reply under `Thinking...`.
    pub const BUSY_STREAMING: &str = include_str!("fixtures/gemini-0.46.0-busy-streaming.txt");
    /// The end of that turn: the reply, `? for shortcuts` back.
    pub const END_OF_TURN: &str = include_str!("fixtures/gemini-0.46.0-end-of-turn.txt");
    /// A turn stopped with Esc: `ℹ Request cancelled.`.
    pub const INTERRUPTED: &str = include_str!("fixtures/gemini-0.46.0-interrupted.txt");
    /// `/` typed: the command menu under the composer.
    pub const SLASH_MENU: &str = include_str!("fixtures/gemini-0.46.0-slash-menu.txt");
    /// The shell tool's approval box.
    pub const BOX_SHELL: &str = include_str!("fixtures/gemini-0.46.0-box-shell.txt");
    /// The same box for a command with a redirection: its warning row under the question (captured under a supervised lab, which escalated it).
    pub const BOX_SHELL_REDIRECTION: &str =
        include_str!("fixtures/gemini-0.46.0-box-shell-redirection.txt");
    /// A shell box answered with Esc (`No, suggest changes (esc)`): the tool's row marked `-`, `ℹ Request cancelled.`, the composer back (captured under a supervised lab at `approve = "none"`; an earlier denial and the harness's `keep going` after it stand above).
    pub const BOX_DENIED: &str = include_str!("fixtures/gemini-0.46.0-box-shell-denied.txt");
    /// The write tool's approval box for a new file.
    pub const BOX_WRITE: &str = include_str!("fixtures/gemini-0.46.0-box-write.txt");
    /// The edit tool's approval box.
    pub const BOX_EDIT: &str = include_str!("fixtures/gemini-0.46.0-box-edit.txt");
    /// The ask-the-user tool's dialog.
    pub const QUESTION: &str = include_str!("fixtures/gemini-0.46.0-question.txt");
    /// The same answered with Enter: the turn's end under it.
    pub const QUESTION_ANSWERED: &str =
        include_str!("fixtures/gemini-0.46.0-question-answered.txt");
    /// `/quit` sent: the farewell box, the resume line, the shell's prompt.
    pub const EXIT: &str = include_str!("fixtures/gemini-0.46.0-exit.txt");
    /// A full-screen redraw (`ESC2J ESC3J ESCH`, no DEC 2026 bracket) cut by one 1024-byte PTY read, three tabs at once (`docs/measured/half-drawn-frame-2026-10-22.md`): the banner and its tips, every row under them blank.
    pub const ERASE_HALF: &str = include_str!("fixtures/gemini-0.46.0-lab-erase-half.txt");
    /// [`ERASE_HALF`]'s redraw whole, 0.4 ms on: the spinner over the composer.
    pub const ERASE_WHOLE: &str = include_str!("fixtures/gemini-0.46.0-lab-erase-whole.txt");
    /// A per-day quota's 429: the usage-limit dialog, before any key.
    pub const LIMIT_DIALOG: &str = include_str!("fixtures/gemini-0.46.0-wall-429-per-day.txt");
    /// A Google account's spent quota (`QUOTA_EXHAUSTED`, its reset in `RetryInfo`): the same dialog with `Access resets at <time>.` under its title, before any key.
    pub const LIMIT_DIALOG_ACCOUNT: &str =
        include_str!("fixtures/gemini-0.46.0-wall-429-account.txt");
    /// Hand-built per-model bucket quota dialog under an API key.
    pub const LIMIT_DIALOG_BUCKET: &str =
        include_str!("fixtures/gemini-0.46.0-wall-429-bucket-hand-built.txt");
    /// Hand-built per-model bucket quota dialog under a Google login.
    pub const LIMIT_DIALOG_BUCKET_UPGRADE: &str =
        include_str!("fixtures/gemini-0.46.0-wall-429-bucket-upgrade-hand-built.txt");
    /// The same dialog answered `2. Stop`.
    pub const LIMIT_STOPPED: &str = include_str!("fixtures/gemini-0.46.0-wall-429-stopped.txt");
    /// The same in a pane of 72 columns, a `GEMINI.md` loaded: the `ℹ` notice under the error wrapped onto a second row.
    pub const NARROW_LIMIT_STOPPED: &str =
        include_str!("fixtures/gemini-0.46.0-narrow-wall-429-stopped.txt");
    /// A 401 on the first turn: `✕ [API Error: {…"code":401…}]`.
    pub const ERROR_401: &str = include_str!("fixtures/gemini-0.46.0-error-401.txt");
    /// The 400 Google answers a key that is no key (`API_KEY_INVALID`).
    pub const ERROR_400_KEY: &str = include_str!("fixtures/gemini-0.46.0-error-400-key.txt");
    /// A 503 on every attempt, 1 s after the second: the spinner, no retry row.
    pub const RETRY_503: &str = include_str!("fixtures/gemini-0.46.0-retry-503.txt");
    /// Two 503s, then the answer.
    pub const RETRY_503_RECOVERED: &str =
        include_str!("fixtures/gemini-0.46.0-retry-503-recovered.txt");
    /// A 500 on every attempt, 1 s after the second: the spinner, no retry row.
    pub const RETRY_500: &str = include_str!("fixtures/gemini-0.46.0-retry-500.txt");
    /// Two 500s, then the answer.
    pub const RETRY_500_RECOVERED: &str =
        include_str!("fixtures/gemini-0.46.0-retry-500-recovered.txt");
    /// A 401 in a pane of 72 columns: the error and its notice wrapped.
    pub const NARROW_ERROR_401: &str = include_str!("fixtures/gemini-0.46.0-narrow-error-401.txt");
    /// A 403 `PERMISSION_DENIED` (`SERVICE_DISABLED`) at 72 columns, a `GEMINI.md` loaded: not retried.
    pub const NARROW_ERROR_403: &str = include_str!("fixtures/gemini-0.46.0-narrow-error-403.txt");
    /// A 404 for the turn's model at 72 columns: the no-access dialog.
    pub const NARROW_NO_ACCESS: &str =
        include_str!("fixtures/gemini-0.46.0-narrow-404-access-dialog.txt");
    /// The same dialog answered `2.`
    pub const NARROW_NO_ACCESS_STOPPED: &str =
        include_str!("fixtures/gemini-0.46.0-narrow-404-stopped.txt");

    /// Shell mode: `!` typed into the empty composer.
    pub const SHELL_MODE: &str = include_str!("fixtures/gemini-0.46.0-shell-mode-idle.txt");
    /// Shell mode holding a typed command.
    pub const SHELL_MODE_DRAFT: &str = include_str!("fixtures/gemini-0.46.0-shell-mode-draft.txt");
    /// That command run: its `$` band, its output's box, the shell-mode composer back.
    pub const SHELL_MODE_RAN: &str = include_str!("fixtures/gemini-0.46.0-shell-mode-ran.txt");
    /// Shell mode left with Esc, the command's band above.
    pub const SHELL_MODE_LEFT: &str = include_str!("fixtures/gemini-0.46.0-shell-mode-left.txt");
    /// YOLO on (ctrl+y): the `*` caret.
    pub const YOLO_IDLE: &str = include_str!("fixtures/gemini-0.46.0-yolo-idle.txt");
    /// YOLO on, a typed draft.
    pub const YOLO_DRAFT: &str = include_str!("fixtures/gemini-0.46.0-yolo-draft.txt");
    /// YOLO on, the draft sent: the spinner.
    pub const YOLO_BUSY: &str = include_str!("fixtures/gemini-0.46.0-yolo-busy.txt");
    /// YOLO on, that turn's end.
    pub const YOLO_END_OF_TURN: &str = include_str!("fixtures/gemini-0.46.0-yolo-end-of-turn.txt");
    /// YOLO on, a shell call run with no box.
    pub const YOLO_TOOL_RAN: &str = include_str!("fixtures/gemini-0.46.0-yolo-tool-ran.txt");
    /// The UI details hidden (Tab twice), at rest.
    pub const DETAILS_HIDDEN_IDLE: &str =
        include_str!("fixtures/gemini-0.46.0-details-hidden-idle.txt");
    /// The UI details hidden, a typed draft.
    pub const DETAILS_HIDDEN_DRAFT: &str =
        include_str!("fixtures/gemini-0.46.0-details-hidden-draft.txt");
    /// The UI details hidden, a turn thinking: the spinner in column 0.
    pub const DETAILS_HIDDEN_BUSY: &str =
        include_str!("fixtures/gemini-0.46.0-details-hidden-busy.txt");
    /// The UI details hidden, that turn's end.
    pub const DETAILS_HIDDEN_END_OF_TURN: &str =
        include_str!("fixtures/gemini-0.46.0-details-hidden-end-of-turn.txt");
    /// The UI details hidden, the shell box.
    pub const DETAILS_HIDDEN_BOX_SHELL: &str =
        include_str!("fixtures/gemini-0.46.0-details-hidden-box-shell.txt");
    /// The UI details hidden with YOLO on, at rest: `● YOLO` and the rule.
    pub const DETAILS_HIDDEN_YOLO_IDLE: &str =
        include_str!("fixtures/gemini-0.46.0-details-hidden-yolo-idle.txt");
    /// The UI details hidden with YOLO on, a draft: the mode's row alone.
    pub const DETAILS_HIDDEN_YOLO_DRAFT: &str =
        include_str!("fixtures/gemini-0.46.0-details-hidden-yolo-draft.txt");
    /// `ui.showShortcutsHint = false`, at rest: the status row blank.
    pub const HINT_OFF_IDLE: &str = include_str!("fixtures/gemini-0.46.0-hint-off-idle.txt");
    /// The same, a turn thinking.
    pub const HINT_OFF_BUSY: &str = include_str!("fixtures/gemini-0.46.0-hint-off-busy.txt");
    /// The same, that turn's end.
    pub const HINT_OFF_END_OF_TURN: &str =
        include_str!("fixtures/gemini-0.46.0-hint-off-end-of-turn.txt");
    /// The same with the UI details hidden, at rest.
    pub const HINT_OFF_DETAILS_HIDDEN_IDLE: &str =
        include_str!("fixtures/gemini-0.46.0-hint-off-details-hidden-idle.txt");
    /// The question tool asking two questions: the first tab.
    pub const QUESTION_MULTI: &str = include_str!("fixtures/gemini-0.46.0-question-multi.txt");
    /// The same after `1`: the second tab.
    pub const QUESTION_MULTI_SECOND: &str =
        include_str!("fixtures/gemini-0.46.0-question-multi-second.txt");
    /// The same after `1` again: the review tab.
    pub const QUESTION_MULTI_REVIEW: &str =
        include_str!("fixtures/gemini-0.46.0-question-multi-review.txt");
    /// The review submitted with Enter: the turn's end.
    pub const QUESTION_MULTI_ANSWERED: &str =
        include_str!("fixtures/gemini-0.46.0-question-multi-answered.txt");
    /// An MCP tool's box (a local stdio server, `lab`).
    pub const BOX_MCP: &str = include_str!("fixtures/gemini-0.46.0-box-mcp.txt");
    /// The web-fetch tool's box.
    pub const BOX_WEB_FETCH: &str = include_str!("fixtures/gemini-0.46.0-box-web-fetch.txt");
    /// The sandbox-expansion box (no sandbox on: a shell call naming `additional_permissions`).
    pub const BOX_SANDBOX_EXPANSION: &str =
        include_str!("fixtures/gemini-0.46.0-box-sandbox-expansion.txt");
    /// `ui.collapseDrawerDuringApproval = false`: the shell box over the composer it keeps open, the status row blank.
    pub const DRAWER_KEPT_BOX_SHELL: &str =
        include_str!("fixtures/gemini-0.46.0-drawer-kept-box-shell.txt");
    /// The same, `hello` typed: the composer takes it.
    pub const DRAWER_KEPT_BOX_SHELL_DRAFT: &str =
        include_str!("fixtures/gemini-0.46.0-drawer-kept-box-shell-draft.txt");
    /// The same, `hello` typed and Enter pressed: queued under the box.
    pub const DRAWER_KEPT_BOX_SHELL_QUEUED: &str =
        include_str!("fixtures/gemini-0.46.0-drawer-kept-box-shell-queued.txt");
    /// The same box with the UI details hidden too.
    pub const DRAWER_KEPT_DETAILS_HIDDEN_BOX_SHELL: &str =
        include_str!("fixtures/gemini-0.46.0-drawer-kept-details-hidden-box-shell.txt");
    /// Under its own seatbelt (`GEMINI_SANDBOX=sandbox-exec`, outside the lab's jail): a fresh session, the footer's sandbox `current process`.
    pub const SANDBOX_IDLE: &str = include_str!("fixtures/gemini-0.46.0-sandbox-idle.txt");
    /// The same, a turn running: the spinner and the thought's subject.
    pub const SANDBOX_BUSY: &str = include_str!("fixtures/gemini-0.46.0-sandbox-busy.txt");
    /// The same turn ended on its reply.
    pub const SANDBOX_END_OF_TURN: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-end-of-turn.txt");
    /// A write outside the project allowed once and denied by the seatbelt: the tool's row `✓`, `Operation not permitted` in its box, the reply.
    pub const SANDBOX_DENIED: &str = include_str!("fixtures/gemini-0.46.0-sandbox-denied.txt");
    /// A `curl` to a non-loopback address allowed once and refused by the seatbelt: `✓`, `Couldn't connect to server`, the reply.
    pub const SANDBOX_DENIED_NETWORK: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-denied-network.txt");
    /// After that denied write, the model's call naming `additional_permissions`: the expansion box under the denied row.
    pub const SANDBOX_RETRY_EXPANSION: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-retry-expansion.txt");
    /// That box allowed once: the seatbelt denies the call again.
    pub const SANDBOX_RETRY_ALLOWED: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-retry-allowed.txt");
    /// That box refused with Esc: `-`, `ℹ Request cancelled.`.
    pub const SANDBOX_RETRY_REFUSED: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-retry-refused.txt");
    /// Its seatbelt and `security.toolSandboxing` both on: the shell command measured (`touch`) fails `sandbox-exec: sandbox_apply: Operation not permitted`.
    pub const SANDBOX_NESTED_DENIED: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-nested-denied.txt");
    /// Its per-tool seatbelt alone (`security.toolSandboxing`): a fresh session, the footer's sandbox `all tools`.
    pub const SANDBOX_TOOLS_IDLE: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-idle.txt");
    /// The expansion box the CLI offers after its per-tool seatbelt denied a write outside the project (`• Write:`, `• Read: /Users//Shared`).
    pub const SANDBOX_TOOLS_EXPANSION: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-expansion.txt");
    /// That box refused with Esc: no row for the call, the model's reply.
    pub const SANDBOX_TOOLS_EXPANSION_REFUSED: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-expansion-refused.txt");
    /// A `curl` call: the expansion box before it runs, `• Network: All Urls`.
    pub const SANDBOX_TOOLS_NETWORK: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-network.txt");
    /// That box refused with Esc: `-`, `ℹ Request cancelled.`.
    pub const SANDBOX_TOOLS_NETWORK_REFUSED: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-network-refused.txt");
    /// [`SANDBOX_TOOLS_EXPANSION`] at 100x30, in a fresh session.
    pub const SANDBOX_TOOLS_EXPANSION_100: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-expansion-100x30.txt");
    /// [`SANDBOX_TOOLS_EXPANSION_REFUSED`] at 100x30.
    pub const SANDBOX_TOOLS_EXPANSION_REFUSED_100: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-expansion-refused-100x30.txt");
    /// [`SANDBOX_IDLE`] at 72 columns.
    pub const SANDBOX_NARROW_IDLE: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-narrow-idle.txt");
    /// [`SANDBOX_TOOLS_IDLE`] at 72 columns.
    pub const SANDBOX_TOOLS_NARROW_IDLE: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-narrow-idle.txt");
    /// [`SANDBOX_TOOLS_EXPANSION`] at 72 columns.
    pub const SANDBOX_TOOLS_NARROW_EXPANSION: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-narrow-expansion.txt");
    /// [`SANDBOX_TOOLS_EXPANSION_REFUSED`] at 72 columns.
    pub const SANDBOX_TOOLS_NARROW_EXPANSION_REFUSED: &str =
        include_str!("fixtures/gemini-0.46.0-sandbox-tools-narrow-expansion-refused.txt");
}

#[cfg(test)]
#[path = "gemini_tests.rs"]
mod tests;
