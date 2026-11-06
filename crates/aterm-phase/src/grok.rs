// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Grok Build (`grok`, xAI) CLI harness version and screen grammar.
//!
//! WHAT IS MEASURED. Grok Build 1.0.70's signed-out screen (`fixtures/grok-1.0.70-signed-out.txt`,
//! a private headless aterm, a scratch `GROK_HOME`): the logo, `Approve in your browser to
//! finish signing in.`, the device code, `Waiting for approval...` and the `ctrl+q  quit` key
//! row, drawn instead of any composer. The limit and login-ended texts are the binary's own
//! strings, the same bytes in 1.0.67 and 1.0.70 ([`crate::anchors::GROK_ANCHORS`]; the
//! not-logged-in one is read whole, ``Not logged in. Run `grok login`.``, as 1.0.70 carries it,
//! since its first words open other CLIs' lines too). 1.0.70 also carries a team's API rate limit
//! (`You’ve hit your team’s API rate limit.`), which its strings put beside the plan's sampler
//! text and both just after `API error (status ): `; so a row may open with that prefix and a
//! status code before either, and the reader steps past it (read from where the strings sit,
//! not measured). And 1.0.70 carries its upsells' titles, each naming the wall's window: the
//! week (`You hit your weekly limit.`), the billing period's credits (`You’ve hit the credit
//! limit for your plan.`, `You’ve hit your spending cap.`, each with a typographic apostrophe),
//! or none (`You hit your free usage limit.`); and the two upsells it names `limit-hit-upsell`
//! and `credit-limit-upsell` each carry a `Try Again` row, `Resubmit the last prompt once you
//! have usage again`. And 1.0.70 carries the other texts of a login of its own that ended
//! (`grok.auth.*`, the ones a sweep of its strings found: a token expired, invalid or refused by
//! the server, a session that could not be renewed, a team sign-in refused, the not-signed-in texts
//! and a compaction's 401), each read from its first words, and a short or common first sentence
//! with the one after it (``Token expired. Run `grok login` to re-authenticate.``, ``Your session
//! has expired. Run `grok login` to sign in again.``), as the not-logged-in one is; a row may carry
//! a text's end on the row under it, as a wrap does. The not-renewed text is read by its opening
//! (`Your session expired and`, before the sign-in helper's name) only with its ending (`could not
//! renew it in the background.`), and the status line `You are not authenticated.` only as a whole
//! row, since their first words open a reply's prose too. Two lowercase texts, a compaction
//! failure's reason (`authentication problem — …`) and voice's `not signed in — …`, sit in the
//! strings among reasons another text leads (compaction's failure reasons, voice's errors), so the
//! screen reads neither as a row's opening (aterm-agent's login-ended words read both). An MCP
//! server's, Grove's or a deployment key's texts are none of them. No signed-in account has drawn
//! any of them on a screen yet, so where they sit on one is not measured. The reader therefore
//! claims nothing it has not seen: every phase is `idle` NOT authoritatively (its busy row is
//! unmeasured), and a wall is read only off a row that OPENS with one of those texts
//! in the screen's live zone, never off prose that quotes one. Its composer's box is measured
//! (1.0.70's idle, busy and draft frames in `fixtures/`): the rounded box whose first inner row
//! opens with `│ ❯`, the model's name in its bottom edge, a draft past one row growing it upward
//! ([`composer_rows`]). Only its rows are read, never its words: the empty composer can offer a
//! suggestion after its caret (`❯ Lab stub session`, `Tab/→:accept suggestion` on the key row),
//! which plain rows cannot tell from a typed draft. Its header's token counts (`12K / 256K`,
//! on the same measured frames) are read as the context left, for the window's displays only.

use crate::wall::{Placement, Wall, WallKind};

/// The Grok (`grok`) harness version, `MAJOR.MINOR` with no third part.
///
/// Its minor goes up once per public release window in which its sources changed
/// ([`crate::CliHarness::sources`]); `targo --unverified ship harness-bump` writes the bump.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(0, 13, "0.13");

/// How many of the screen's last worded rows a wall row may stand in ([`live_from`]): the
/// notice a turn ended on, with room for a composer and key rows under it.
const LIVE_ROWS: usize = 12;

/// Where Grok Build's live zone starts for a wall reading: the last [`LIVE_ROWS`] rows that
/// carry a word. A plain row count from the foot never reaches a notice on a short
/// transcript (measured on 1.0.70's 401 frame, where the failure stands under the prompt at
/// the screen's top): the screen is mostly blank under it, and its full-height scrollbar
/// gutter draws a block on rows of its own all the way down to the composer. Neither a blank
/// row nor a bar, rule or block with no letter or digit in it spends the budget.
fn live_from(rows: &[String]) -> usize {
    let mut budget = LIVE_ROWS;
    for (i, row) in rows.iter().enumerate().rev() {
        if row.chars().any(|c| c.is_alphanumeric()) {
            budget -= 1;
            if budget == 0 {
                return i;
            }
        }
    }
    0
}

/// The texts a usage limit opens its row with, as the build carries each, and the window each
/// names: the plan limit's TUI error card (ASCII apostrophe) and sampler (typographic one), the
/// team's API rate limit (typographic), the free-usage limit and its upsell's title name none
/// ([`WallKind::UsageSession`]); the weekly limit's upsell names the week
/// ([`WallKind::UsageWeekly`]); the credit limit's and the spending cap's name money, the
/// billing period's credits ([`WallKind::Spend`]).
const LIMIT_OPENERS: [(&str, WallKind); 8] = [
    (
        crate::anchor!("grok.wall.plan_rate_limit"),
        WallKind::UsageSession,
    ),
    (
        crate::anchor!("grok.wall.plan_rate_limit.sampler"),
        WallKind::UsageSession,
    ),
    (
        crate::anchor!("grok.wall.team_rate_limit"),
        WallKind::UsageSession,
    ),
    (
        crate::anchor!("grok.wall.free_usage"),
        WallKind::UsageSession,
    ),
    (
        crate::anchor!("grok.wall.free_usage.upsell"),
        WallKind::UsageSession,
    ),
    (crate::anchor!("grok.wall.weekly"), WallKind::UsageWeekly),
    (crate::anchor!("grok.wall.credit_limit"), WallKind::Spend),
    (crate::anchor!("grok.wall.spending_cap"), WallKind::Spend),
];

/// The texts an ended login opens its row with, each the opening of its message: the
/// login-ended texts a sweep of 1.0.70's strings found that open a row and whose opening alone
/// names an ended login. Where on a screen each sits is unmeasured: no signed-in account has
/// drawn one.
const AUTH_OPENERS: [&str; 21] = [
    crate::anchor!("grok.auth.expired"),
    crate::anchor!("grok.auth.not_logged_in"),
    crate::anchor!("grok.auth.token_expired"),
    crate::anchor!("grok.auth.token_invalid"),
    crate::anchor!("grok.auth.rejected"),
    crate::anchor!("grok.auth.recovery_exhausted"),
    crate::anchor!("grok.auth.reauth_required"),
    crate::anchor!("grok.auth.session_expired"),
    crate::anchor!("grok.auth.session_has_expired"),
    crate::anchor!("grok.auth.required"),
    crate::anchor!("grok.auth.refresh_failed"),
    crate::anchor!("grok.auth.team_rejected"),
    crate::anchor!("grok.auth.not_signed_in"),
    crate::anchor!("grok.auth.not_signed_in.no_browser"),
    crate::anchor!("grok.auth.no_cached_token"),
    crate::anchor!("grok.auth.failed"),
    crate::anchor!("grok.auth.no_cached_credentials"),
    crate::anchor!("grok.auth.headless"),
    crate::anchor!("grok.auth.no_session"),
    crate::anchor!("grok.auth.signed_out"),
    crate::anchor!("grok.auth.compaction"),
];

/// Whether the row at `at` in `rows` says the login ended: its opening ([`opening`]) is the
/// status line `You are not authenticated.` whole, or, read on into the row under it where a wrap
/// carries a text's end, it opens with one of `AUTH_OPENERS`, or with the not-renewed text's
/// opening (the words before the sign-in helper's name, ``Your session expired and <helper> could
/// not renew it in the background. …``) and carries its ending, or with the authentication
/// failure a 401 draws (`Authentication required: … Run /login to re-authenticate`, measured on
/// 1.0.70), whose first words alone could open prose too.
fn login_ended(rows: &[String], at: usize) -> bool {
    let text = opening(&rows[at]);
    if text == crate::anchor!("grok.auth.not_authenticated") {
        return true;
    }
    let run = rows.get(at + 1).map_or_else(
        || text.to_string(),
        |next| format!("{text} {}", opening(next)),
    );
    AUTH_OPENERS.iter().any(|w| run.starts_with(w))
        || (run.starts_with(crate::anchor!("grok.auth.not_renewed.named"))
            && run.contains(crate::anchor!("grok.auth.not_renewed.tail")))
        || (run.starts_with(crate::anchor!("grok.auth.required.open"))
            && run.contains(crate::anchor!("grok.auth.required.tail")))
}

/// The row Grok Build's sign-in screen asks the person to approve on, where the screen is that
/// screen (its approval line with `Waiting for approval...` under it), or `None`.
#[must_use]
pub fn sign_in_row(rows: &[String]) -> Option<usize> {
    let approve = rows
        .iter()
        .position(|r| crate::text::trim(r) == crate::anchor!("grok.signin.approve"))?;
    rows[approve + 1..]
        .iter()
        .any(|r| crate::text::trim(r) == crate::anchor!("grok.signin.waiting"))
        .then_some(approve)
}

/// Whether `rows` are Grok Build's own screen by what it alone draws: its sign-in screen over its
/// `ctrl+q  quit` key row ([`sign_in_row`]), its composer's box ([`composer_rows`]), or a
/// question card standing in the composer's place ([`panel_standing`]). All three are measured
/// on its 1.0.70 frames. No other program's measured frame holds one
/// (`its_composer_box_claims_no_other_programs_screen` walks the fixture corpus).
#[must_use]
pub fn is_grok_screen(rows: &[String]) -> bool {
    composer_rows(rows).is_some()
        || panel_standing(rows)
        || sign_in_row(rows).is_some_and(|at| {
            rows[at..]
                .iter()
                .any(|r| crate::text::trim(r) == crate::anchor!("grok.keys.quit"))
        })
}

/// Whether a question card stands on `rows`: the heavy-bar panel an upsell, a question or an
/// approval draws in the composer's place (measured on 1.0.70's upsell, question and approval
/// frames), told by a radio option row on its bar (`┃  1 (○) …`, the taken one `(●)`). Agy's
/// notice cards stand on the same bar with no option on them.
fn panel_standing(rows: &[String]) -> bool {
    rows.iter().any(|row| {
        let Some(inner) = crate::text::trim_start(row).strip_prefix('┃') else {
            return false;
        };
        let inner = inner.trim_start_matches(' ');
        let mut chars = inner.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
            && matches!(chars.as_str(), s if s.starts_with(" (○)") || s.starts_with(" (●)"))
    })
}

/// The text of `row` past its indent, at most one leading mark (a glyph or a frame's edge and
/// the space after it) and an `API error (status N): ` prefix, where the row's words start.
fn opening(row: &str) -> &str {
    let text = crate::text::trim(row);
    let mut chars = text.char_indices();
    let words = match chars.next() {
        Some((_, c)) if !c.is_alphanumeric() => match chars.next() {
            Some((at, ' ')) => crate::text::trim(&text[at..]),
            _ => text,
        },
        _ => text,
    };
    past_api_error(words)
}

/// `text` past a leading `API error (status N): `, N a status code, the prefix the build's
/// strings put before its sampler's limit texts; `text` itself where it has none.
fn past_api_error(text: &str) -> &str {
    text.strip_prefix(crate::anchor!("grok.wall.api_error"))
        .and_then(|rest| rest.split_once("): "))
        .filter(|(status, _)| !status.is_empty() && status.bytes().all(|b| b.is_ascii_digit()))
        .map_or(text, |(_, words)| words)
}

/// Whether `text`, a row's opening ([`opening`]), is a limit upsell's `Try Again` row: its
/// `Resubmit …` line, alone or after the `Try Again` label on the same row.
fn is_try_again(text: &str) -> bool {
    let resubmit = crate::anchor!("grok.wall.try_again.resubmit");
    text.starts_with(resubmit)
        || text
            .strip_prefix(crate::anchor!("grok.wall.try_again"))
            .is_some_and(|rest| crate::text::trim(rest).starts_with(resubmit))
}

/// How many of the screen's first rows the header can stand in. It is drawn at the top, under
/// one blank row on every measured frame.
const HEADER_ROWS: usize = 3;

/// The context the header says is left, as a share of the window it names: its token counts
/// (`12K / 256K` at the header row's right edge, before its `[Dashboard]` segment), read for
/// the window's displays only ([`crate::ScreenReader::context_shown`]). The header says
/// nothing about when the program compacts, so the context watch does not take it. The counts
/// are the program's compact spellings, so the share is as coarse as they are. `None` where no
/// header row stands: the sign-in screen and the welcome card draw none.
#[must_use]
pub fn context_left(rows: &[String]) -> Option<u8> {
    let (used, window) = rows
        .iter()
        .take(HEADER_ROWS)
        .find_map(|row| header_counts(row))?;
    if window == 0 {
        return None;
    }
    let spent = used.saturating_mul(100).div_ceil(window);
    u8::try_from(100u64.saturating_sub(spent)).ok()
}

/// The `(used, window)` token counts a header row ends with, before its last `│` segment.
fn header_counts(row: &str) -> Option<(u64, u64)> {
    let (head, tail) = crate::text::trim_end(row).rsplit_once('│')?;
    if !crate::text::trim(tail).starts_with('[') {
        return None;
    }
    let mut tokens = crate::text::trim_end(head).rsplit(' ');
    let window = token_count(tokens.next()?)?;
    if tokens.next()? != "/" {
        return None;
    }
    let used = token_count(tokens.next()?)?;
    Some((used, window))
}

/// A token count as the header spells it: digits with an optional fraction (`1.3K`), scaled by
/// an optional `K` or `M` suffix.
fn token_count(text: &str) -> Option<u64> {
    let (digits, scale) = if let Some(d) = text.strip_suffix('K') {
        (d, 1_000u64)
    } else if let Some(d) = text.strip_suffix('M') {
        (d, 1_000_000u64)
    } else {
        (text, 1)
    };
    let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut milli = whole.parse::<u64>().ok()?.checked_mul(1000)?;
    if !frac.is_empty() {
        milli += frac.parse::<u64>().ok()? * 10u64.pow(3 - u32::try_from(frac.len()).ok()?);
    }
    milli.checked_mul(scale).map(|v| v / 1000)
}

/// The composer's rows `(top, caret, bottom)`: the last rounded box whose first inner row is its
/// caret row ([`caret_row`]), so a point can be cut above it and no draft typed, pasted or
/// cleared in it moves the point. The model's name stands in the bottom edge
/// (`╰─…─ lab-stub ─╯`); a draft past one row grows the box upward. Its welcome and approval
/// boxes draw no caret row, and a sent message's `❯` stands with no `│` before it.
#[must_use]
pub fn composer_rows(rows: &[String]) -> Option<(usize, usize, usize)> {
    let caret = rows.iter().rposition(|r| caret_row(r))?;
    let top = caret.checked_sub(1)?;
    if !crate::text::trim_start(&rows[top]).starts_with('╭') {
        return None;
    }
    let bottom = caret
        + 1
        + rows[caret + 1..]
            .iter()
            .position(|r| crate::text::trim_start(r).starts_with('╰'))?;
    Some((top, caret, bottom))
}

/// Whether `row` is the composer's caret row: `│ ❯`, then nothing or a space (before the draft,
/// or the suggestion the empty composer offers; the two are not told apart here).
fn caret_row(row: &str) -> bool {
    crate::text::trim_start(row)
        .strip_prefix('│')
        .and_then(|inner| inner.strip_prefix(" ❯"))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// The wall `rows` show: the sign-in screen ([`sign_in_row`], [`WallKind::Auth`]), or a row in the
/// live zone that opens with a limit text (its window, `LIMIT_OPENERS`) or says an ended login's
/// (`login_ended`, [`WallKind::Auth`]). Where no such row stands in the live zone, a limit upsell's
/// `Try Again` row (`is_try_again`) is a usage limit that names no window
/// ([`WallKind::UsageSession`]): the title above it, which names it, is out of the zone. A
/// passing throttle (`The service is busy. …`) is none, and so is any row whose words only quote
/// one.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    if let Some(at) = sign_in_row(rows) {
        return Some(Wall::sign_in(rows, at));
    }
    let from = live_from(rows);
    let at_row = |at: usize, kind: WallKind| Wall {
        kind,
        message: opening(&rows[at]).to_string(),
        reset: None,
        row: at,
        placement: Placement::ErrorRow,
        under_call: false,
    };
    (from..rows.len())
        .rev()
        .find_map(|at| {
            let text = opening(&rows[at]);
            let kind = LIMIT_OPENERS
                .iter()
                .find(|(w, _)| text.starts_with(w))
                .map(|&(_, kind)| kind)
                .or_else(|| login_ended(rows, at).then_some(WallKind::Auth))?;
            Some(at_row(at, kind))
        })
        .or_else(|| {
            (from..rows.len())
                .rev()
                .find(|&at| is_try_again(opening(&rows[at])))
                .map(|at| at_row(at, WallKind::UsageSession))
        })
}

#[cfg(test)]
#[path = "grok_tests.rs"]
mod tests;

/// Grok Build's screens, measured or hand-built (each file's head says which), for tests here
/// and in dependent crates; not part of the documented API.
#[doc(hidden)]
pub mod fixtures {
    /// Grok Build 1.0.70 signed out, its device sign-in screen (MEASURED).
    pub const SIGNED_OUT: &str = include_str!("fixtures/grok-1.0.70-signed-out.txt");
    /// The welcome card over an empty composer, `Logged in with API key` under it (MEASURED).
    pub const IDLE_EMPTY: &str = include_str!("fixtures/grok-1.0.70-idle-empty.txt");
    /// A one-row draft in the idle composer (MEASURED).
    pub const IDLE_DRAFT: &str = include_str!("fixtures/grok-1.0.70-idle-draft.txt");
    /// A two-line typed draft, the composer box grown a row upward (MEASURED).
    pub const DRAFT_MULTILINE_TYPED: &str =
        include_str!("fixtures/grok-1.0.70-draft-multiline-typed.txt");
    /// A reply streaming over a one-row draft (MEASURED).
    pub const BUSY_DRAFT: &str = include_str!("fixtures/grok-1.0.70-busy-draft.txt");
    /// A turn ended over an empty composer (MEASURED).
    pub const TURN_ENDED_EMPTY: &str = include_str!("fixtures/grok-1.0.70-turn-ended-empty.txt");
    /// A shell approval box standing, no composer drawn (MEASURED).
    pub const BOX_SHELL: &str = include_str!("fixtures/grok-1.0.70-box-shell.txt");
    /// A plan approval over the composer (MEASURED).
    pub const PLAN_APPROVAL: &str = include_str!("fixtures/grok-1.0.70-plan-approval.txt");
    /// A 401's drawn authentication failure under the prompt (MEASURED).
    pub const WALL_401_1_0_70: &str = include_str!("fixtures/grok-1.0.70-wall-401.txt");
    /// A 429 after the program's one retry, its server text under the prompt (MEASURED).
    pub const RETRY_429_1_0_70: &str = include_str!("fixtures/grok-1.0.70-wall-429.txt");
    /// The free-usage upsell drawn in the composer's place (MEASURED).
    pub const UPSELL_FREE_1_0_70: &str = include_str!("fixtures/grok-1.0.70-wall-free-usage.txt");
    /// The weekly limit's upsell drawn in the composer's place (MEASURED).
    pub const UPSELL_WEEKLY_1_0_70: &str =
        include_str!("fixtures/grok-1.0.70-wall-weekly-limit.txt");
    /// The plan rate limit's error card under a reply (HAND-BUILT: the text is the binary's).
    pub const WALL_PLAN_RATE_LIMIT: &str =
        include_str!("fixtures/grok-1.0.70-hand-built-wall-plan-rate-limit.txt");
    /// A reply whose prose quotes `rate limit` (HAND-BUILT), which is no wall.
    pub const REPLY_QUOTES_RATE_LIMIT: &str =
        include_str!("fixtures/grok-1.0.70-hand-built-reply-quotes-rate-limit.txt");
    /// The weekly limit's upsell under a reply (HAND-BUILT: the words are the binary's).
    pub const WALL_WEEKLY_LIMIT: &str =
        include_str!("fixtures/grok-1.0.70-hand-built-wall-weekly-limit.txt");
    /// The credit limit's upsell under a reply (HAND-BUILT: the words are the binary's).
    pub const WALL_CREDIT_LIMIT: &str =
        include_str!("fixtures/grok-1.0.70-hand-built-wall-credit-limit.txt");
    /// A reply whose prose quotes the upsells' titles and `Try Again` row (HAND-BUILT), which is
    /// no wall.
    pub const REPLY_QUOTES_UPSELL: &str =
        include_str!("fixtures/grok-1.0.70-hand-built-reply-quotes-upsell.txt");
    /// An invalid or expired auth token's error row under a reply (HAND-BUILT: the text is the
    /// binary's).
    pub const WALL_AUTH_TOKEN_INVALID: &str =
        include_str!("fixtures/grok-1.0.70-hand-built-wall-auth-token-invalid.txt");
    /// A reply whose prose quotes the login-ended texts and texts that end no login of the row's
    /// (HAND-BUILT), which is no wall.
    pub const REPLY_QUOTES_LOGIN_ENDED: &str =
        include_str!("fixtures/grok-1.0.70-hand-built-reply-quotes-login-ended.txt");
}
