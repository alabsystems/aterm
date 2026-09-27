// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The WALL a worker's turn ended on, by kind: a usage window, a model
//! bucket, spend, a full context, an expired login, an API error, overload
//! — and the one wall that is no turn's end, Claude Code's critical-memory
//! banner ([`memory_wall`], placed by position rather than by [`PHRASES`]).
//!
//! [`crate::phase::worker_phase`] has one word for a wall, `limited`, and
//! only for the usage kinds; a turn that ended on `API Error: 529
//! Overloaded` or `Please run /login` reads `idle` there, and a supervisor
//! that trusts `idle` waits for a worker that will never move. [`wall`]
//! reads the SAME rows [`crate::phase::limit_notice`] reads — the footer
//! item, the banner Claude Code parks under the last thing said, the vendor
//! row under the `⎿` gutter of the last message — and names the kind from
//! ONE table, [`PHRASES`].
//!
//! **Placement decides what is the vendor's; the table decides what kind.**
//! A `⎿` block that opens the last thing said is read as a vendor row
//! unless one of two recognised forms says it is output: the `⏺` row it
//! hangs from is a tool call written `⏺ Name(…)` (`⏺ Bash(…)`, `⏺
//! Update(notes.txt)`), or the block opens with the command's own echo
//! (`⎿  $ touch x`, how 2.1.280 draws a running Bash call under `⏺
//! <description>`). Under those the gutter carries the tool's OUTPUT, which
//! quotes anything — a log that says `API Error: 529`, a peer's screen. Any
//! other owner is read as the vendor's: the worker's message, a completion
//! notice (`⏺ Dynamic workflow … completed`, `⏺ Background command …
//! completed`, both measured with a limit notice under them) — and a
//! `⏺ Monitor event: …` row, whose block is its event's payload. A limit
//! notice under a Monitor event is the same shape as under a completion
//! notice (a monitor event opens a turn with no user row), so it is read
//! as a wall; a payload that OPENS with a wall phrase — a peer's screen
//! whose first row is its limit notice — reads as this worker's wall too.
//! A finished Bash call's `⎿` block once its echo is gone is not measured.
//!
//! **Not every `… limit reached` is a wall.** The binary's full set (2.1.280,
//! read with `rg -a 'limit reached'`) includes a subagent quota (`Concurrent
//! subagent limit reached. You can run 5 subagents at once. Do not retry.`),
//! a nesting depth, a subagent budget (`Budget limit reached ($5.01 spent of
//! the $5.00 maximum)`), and fast mode's own fallback (`Fast limit reached and
//! temporarily disabled · resets in 4m`): the worker goes on under each. The
//! old `<= 3 words + "limit reached"` rule read all of them as a usage wall;
//! [`PHRASES`] lists them first, as not walls.

/// Which wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallKind {
    /// The five-hour session window (`You've hit your session limit · resets
    /// 3pm`), or a usage limit that does not say which window (`Usage limit
    /// reached · continuing automatically at 1:50pm`, `Goal paused · usage
    /// limit reached`).
    UsageSession,
    /// The seven-day window (`You've hit your weekly limit · resets Sep 19 at
    /// 11am`).
    UsageWeekly,
    /// A per-model bucket (`You've reached your Fable limit. Run
    /// /usage-credits to continue or switch models with /model.`). `consent`:
    /// the vendor is asking whether to go on on usage credits (`Fable limit
    /// reached · continuing on Sonnet uses usage credits, and the prompt to
    /// confirm …`, from the binary) rather than stopping.
    ModelBucket { consent: bool },
    /// Money: a monthly spend limit, a shared budget, no usage credits left.
    Spend,
    /// The context is full (`Context limit reached · /compact or /clear to
    /// continue`, `Prompt is too long`): `/compact` moves it, waiting does not.
    Context,
    /// The login is gone (`Not logged in · Please run /login`, `API Error:
    /// 401 Invalid API key · Please run /login`): a human's browser moves it.
    Auth,
    /// Any other `API Error: …` the turn ended on. `code` is the HTTP status
    /// when the notice prints one; `retryable` is the vendor's own rule — 408,
    /// 409, 429 and every 5xx retry, and a notice with no status retries when
    /// it names a server or connection failure (`Server error mid-response`,
    /// `Request timed out`). `API Error: Rate limit reached for requests` is
    /// `code: Some(429)`: that is the status the vendor maps to `rate_limit`.
    ApiError { code: Option<u16>, retryable: bool },
    /// The service is overloaded (`API Error: 529 Overloaded. This is a
    /// server-side issue, usually temporary — try again in a moment.`,
    /// `Repeated 529 Overloaded errors`, `Opus is experiencing high load`).
    Overloaded,
    /// Claude Code's critical-memory banner: the vendor's own word that its
    /// process is past saving and must be restarted (resumed with
    /// [`crate::reader::resume_hint`]). Not a turn's end at all — measured
    /// 2026-09-24 on a worker whose spinner still ran 36 minutes into a turn
    /// while it read no input for 2h41m — so it is read by position
    /// ([`memory_wall`]), and the reader keeps it under a hard busy, where it
    /// drops every other wall. Never waited out, retried or typed at.
    Memory,
}

impl WallKind {
    /// The lowercase word the CLI prints (`wall:overloaded`).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            WallKind::UsageSession => "usage-session",
            WallKind::UsageWeekly => "usage-weekly",
            WallKind::ModelBucket { .. } => "model-bucket",
            WallKind::Spend => "spend",
            WallKind::Context => "context",
            WallKind::Auth => "auth",
            WallKind::ApiError { .. } => "api-error",
            WallKind::Overloaded => "overloaded",
            WallKind::Memory => "memory",
        }
    }

    /// Whether [`crate::phase::worker_phase`] reads this wall as
    /// [`crate::phase::Phase::Limited`]: the usage windows, a model bucket,
    /// spend, and an API rate limit (429) — the kinds it has always read so.
    /// The others end a turn that reads `idle` there; [`wall`] names them.
    #[must_use]
    pub fn reads_limited(&self) -> bool {
        match self {
            WallKind::UsageSession
            | WallKind::UsageWeekly
            | WallKind::ModelBucket { .. }
            | WallKind::Spend => true,
            WallKind::ApiError { code, .. } => *code == Some(429),
            WallKind::Context | WallKind::Auth | WallKind::Overloaded | WallKind::Memory => false,
        }
    }
}

/// Where on the screen a wall was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// An item of the footer, under the composer's bottom rule.
    Footer,
    /// A banner row Claude Code parks under the last thing said (`⚠ Usage
    /// limit reached · …`).
    Banner,
    /// The vendor row: a `⎿` block under the worker's last message.
    Gutter,
}

/// One wall, read from a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wall {
    pub kind: WallKind,
    /// The notice as Claude Code says it, its continuation rows joined.
    pub message: String,
    /// When it resets or goes on by itself, if the notice says
    /// (`7:30pm (America/Los_Angeles)`, `in 3h`, `shortly`).
    pub reset: Option<String>,
    /// The row the notice opens on — for a guard bound to it.
    pub row: usize,
    pub placement: Placement,
}

/// What one table row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// The phrase reads like a wall and is not one: the worker goes on.
    NotAWall,
    Session,
    Weekly,
    Model,
    Spend,
    Context,
    Auth,
    Overloaded,
}

/// THE phrase table: the first row whose phrase occurs in a notice's head
/// (its text up to the first sentence end, `·`, `∙` or `|`, lowercased, `’`
/// as `'`, number tokens dropped so `Fable 5 limit` and `Opus 4.1 limit`
/// read as `fable limit` and `opus limit`) names
/// it. Order matters: the not-walls first, then the specific kinds before
/// the generic `usage limit`. Taken from `aterm-agent`'s
/// `harness::limits::banner_class` needles and the 2.1.280 binary's strings.
pub const PHRASES: &[(&str, Tag)] = &[
    ("approaching", Tag::NotAWall),
    ("concurrent subagent limit", Tag::NotAWall),
    ("subagent nesting limit", Tag::NotAWall),
    ("budget limit reached", Tag::NotAWall),
    ("fast limit", Tag::NotAWall),
    ("fast mode", Tag::NotAWall),
    ("request was aborted", Tag::NotAWall),
    ("update installed", Tag::NotAWall),
    ("context limit reached", Tag::Context),
    ("prompt is too long", Tag::Context),
    ("temporarily limiting requests", Tag::Overloaded),
    ("repeated overloaded errors", Tag::Overloaded),
    ("overloaded", Tag::Overloaded),
    ("high load", Tag::Overloaded),
    ("please run /login", Tag::Auth),
    ("run /login", Tag::Auth),
    ("not logged in", Tag::Auth),
    ("login expired", Tag::Auth),
    ("oauth token revoked", Tag::Auth),
    ("invalid api key", Tag::Auth),
    ("invalid auth token", Tag::Auth),
    ("authentication failed", Tag::Auth),
    ("authentication error", Tag::Auth),
    ("token expired", Tag::Auth),
    ("weekly limit", Tag::Weekly),
    ("weekly usage limit", Tag::Weekly),
    ("weekly rate limit", Tag::Weekly),
    ("7-day limit", Tag::Weekly),
    ("seven-day limit", Tag::Weekly),
    ("fable limit", Tag::Model),
    ("opus limit", Tag::Model),
    ("sonnet limit", Tag::Model),
    ("haiku limit", Tag::Model),
    ("requires usage credits", Tag::Model),
    ("now uses usage credits", Tag::Model),
    ("monthly spend limit", Tag::Spend),
    ("spend limit", Tag::Spend),
    ("shared budget", Tag::Spend),
    ("out of usage credits", Tag::Spend),
    ("out of extra usage", Tag::Spend),
    ("usage credit limit", Tag::Spend),
    ("credit balance", Tag::Spend),
    ("out of usage", Tag::Spend),
    ("session limit", Tag::Session),
    ("5-hour limit", Tag::Session),
    ("usage limit", Tag::Session),
    ("hit your limit", Tag::Session),
    ("reached your limit", Tag::Session),
    ("limit will reset", Tag::Session),
];

/// The wall `text` names, if it is one — `text` being a notice as it OPENS
/// a vendor row (a leading status glyph such as `⚠` is skipped). `None` for
/// anything the table does not place, for the not-walls it lists, and for a
/// warning (`Approaching usage limit`). A `Goal paused · <reason> · …`
/// notice is read by its reason.
#[must_use]
pub fn classify_wall(text: &str) -> Option<WallKind> {
    let text = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    let lower = text.replace('’', "'").to_lowercase();
    let lower = match lower.strip_prefix("goal paused") {
        Some(rest) => rest
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '·' | '∙' | '.'))
            .to_string(),
        None => lower,
    };
    let head = notice_head(&lower);
    // `API Error: 529 Overloaded` keeps its number for the code; the table
    // reads the words.
    let words: String = head
        .split_whitespace()
        .filter(|w| {
            !w.chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == ':')
        })
        .collect::<Vec<_>>()
        .join(" ");
    let tag = PHRASES
        .iter()
        .find(|(phrase, _)| opens_with(&words, phrase))
        .map(|&(_, tag)| tag);
    let api = head.starts_with("api error");
    // The usage kinds need the notice's own verb (`… limit reached`, `hit
    // your …`, `… will reset`, `out of …`, `requires usage credits`): a
    // sentence that merely names a weekly or usage limit is not one.
    let usage_verb = || USAGE_VERBS.iter().any(|v| head.contains(v));
    match tag {
        Some(Tag::NotAWall) => None,
        Some(Tag::Session | Tag::Weekly | Tag::Model | Tag::Spend) if !usage_verb() => None,
        Some(Tag::Session) => Some(WallKind::UsageSession),
        Some(Tag::Weekly) => Some(WallKind::UsageWeekly),
        Some(Tag::Model) => Some(WallKind::ModelBucket {
            consent: lower.contains("prompt to confirm") || lower.contains("continuing on "),
        }),
        Some(Tag::Spend) => Some(WallKind::Spend),
        Some(Tag::Context) => Some(WallKind::Context),
        Some(Tag::Auth) => Some(WallKind::Auth),
        Some(Tag::Overloaded) => Some(WallKind::Overloaded),
        None if api => Some(api_error(head)),
        None => None,
    }
}

/// A notice's head: its text up to the first sentence end (a `.` before a
/// space or at the end, so a model's `4.1` or `4.5` stays whole), `·`, `∙`
/// or `|`.
fn notice_head(text: &str) -> &str {
    let end = text
        .char_indices()
        .find(|&(i, c)| {
            matches!(c, '·' | '∙' | '|')
                || (c == '.' && text[i + 1..].chars().next().is_none_or(char::is_whitespace))
        })
        .map_or(text.len(), |(i, _)| i);
    text[..end].trim()
}

/// The verbs a usage, model-bucket or spend notice opens with.
const USAGE_VERBS: &[&str] = &[
    "reached", "hit your", "reset", "out of", "requires", "now uses", "too low",
];

/// How many words may come before a phrase that still OPENS the notice:
/// `you've hit your team's shared budget` is the longest measured lead.
const OPENING_WORDS: usize = 4;

/// Whether `phrase` starts on a word boundary within the first
/// [`OPENING_WORDS`] words of `words`.
fn opens_with(words: &str, phrase: &str) -> bool {
    words.match_indices(phrase).any(|(at, _)| {
        (at == 0 || words[..at].ends_with(' '))
            && words[..at].split_whitespace().count() <= OPENING_WORDS
    })
}

/// `api error: <status>? <words>` → the code it prints (a 529 is
/// [`WallKind::Overloaded`] before this is asked) and whether the vendor
/// retries it.
fn api_error(head: &str) -> WallKind {
    let rest = head["api error".len()..].trim_start_matches([':', ' ']);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let code = if digits.len() == 3 {
        digits.parse::<u16>().ok()
    } else if rest.starts_with("rate limit") {
        Some(429)
    } else {
        None
    };
    if code == Some(529) {
        return WallKind::Overloaded;
    }
    let retryable = match code {
        Some(c) => matches!(c, 408 | 409 | 429) || c >= 500,
        None => [
            "server error",
            "timed out",
            "stopped arriving",
            "mid-response",
            "connection",
        ]
        .iter()
        .any(|k| rest.contains(k)),
    };
    WallKind::ApiError { code, retryable }
}

/// The wall the worker's last turn ended on, if any: Claude Code's
/// critical-memory banner first ([`memory_wall`]), then [`classify_wall`]
/// over the rows [`crate::phase::limit_notice`] reads, every kind. It does
/// not ask whether the worker is busy — a notice stays on the screen while
/// the vendor retries under it — so read it beside
/// [`crate::phase::busy_signal`], as [`crate::reader::read`] does.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    memory_wall(rows).or_else(|| crate::phase::notice(rows, &classify_wall))
}

/// How far above the composer's top rule [`memory_wall`] looks: the live
/// zone's parked rows (a spinner, its `⎿  Tip:` or todo rows, the context
/// indicator, a hint), never the transcript above them.
const MEMORY_ROWS: usize = 6;

/// Claude Code's critical-memory banner, as a [`WallKind::Memory`] wall.
///
/// The 2026-09-24 incident: a worker's spinner read `· Gesticulating… (36m
/// 1s)` while the process sat at 38.8 GiB resident and had read no input for
/// 2h41m, and the ONLY thing on the screen that said so was the vendor's
/// banner — `<anchor> (140.4GB) — restart and resume with claude
/// --continue`, where `<anchor>` is [`crate::anchors`]' `wall.memory` —
/// right-aligned and alone on the row between the spinner and the
/// composer's top rule (it starts at column 67 and ends two columns short of
/// the 144-column rule). Five rows under it the composer held a human's
/// draft that QUOTED the banner word for word. So the banner is read by
/// PLACE, as [`crate::phase::context_left`] reads the context indicator:
///
/// * the screen has the composer frame, `width` being its rule's width;
/// * the row lies between the status row (or, with none, the row under the
///   last transcript row) and the top rule, and among the [`MEMORY_ROWS`]
///   rows right above that rule — never inside the composer or under it,
///   where the draft is, and never up in the transcript;
/// * the row ends against the right edge (within three columns of `width`),
///   and its RIGHT SEGMENT is the text after its last run of two or more
///   spaces: the whole row when the banner is alone on it, its tail when it
///   shares the status row with the spinner, as the first report of the
///   incident placed it;
/// * a row UNDER the status row is the live zone — no transcript row is
///   drawn there — so any such row will do, whatever column it starts at,
///   except a row of a message a person queued there
///   ([`in_queued_message`]). Every other row — the status row itself, and
///   with no status row every row, where the last transcript block runs
///   down to the rule — must start its segment at the hint column or later,
///   or be a hint against the edge
///   ([`crate::phase::is_against_right_edge`]);
/// * that segment opens with the anchor followed by ` (` (the size).
///
/// The column rule is not asked under a status row because the banner is 75
/// columns wide: at aterm's default 80 columns it starts at column 3, left
/// of both the hint column and a hint's column-6 floor, and the first cut
/// of this reader, which asked it everywhere, read nothing at 82 columns or
/// fewer, under the very spinner the incident had (review of 2026-09-24).
///
/// A transcript row quoting the banner starts at the transcript's columns,
/// not the hint column, a draft quoting it is under the top rule, and a
/// queued message quoting it is a `❯` block; all read `None`. The copies
/// this cannot tell from the banner: with no status row, a row of the last
/// transcript block that ends against this very edge with the banner as its
/// right segment from column 6 on; under a status row, a paragraph of a
/// queued message after a blank row inside it (whether Claude Code draws
/// one is not measured). And the banner it misses: with no status row (an
/// idle screen without a done row) in a window of 82 columns or fewer, and
/// at those widths right under a queued message. The layout was measured
/// busy at 144 columns; narrower and idle it is SYNTHETIC and unconfirmed,
/// and under a box it is not read at all
/// ([`crate::reader::ScreenReader::read`]).
#[must_use]
pub fn memory_wall(rows: &[String]) -> Option<Wall> {
    use crate::phase::{HINT_COLUMN, composer_frame, is_against_right_edge, status_block};
    let anchor = crate::anchors::anchor_text("wall.memory");
    let frame = composer_frame(rows)?;
    let width = rows[frame.bottom].trim_end().chars().count();
    let block = status_block(rows, frame.top);
    let from = block
        .status
        .unwrap_or(block.from)
        .max(frame.top.saturating_sub(MEMORY_ROWS));
    (from..frame.top).rev().find_map(|i| {
        let row = rows[i].trim_end();
        if row.chars().count() + 3 < width {
            return None;
        }
        let (column, segment) = right_segment(row);
        let live = block
            .status
            .is_some_and(|s| s < i && !in_queued_message(rows, s, i));
        if !live && column < HINT_COLUMN && !is_against_right_edge(row, width) {
            return None;
        }
        segment
            .strip_prefix(anchor)
            .is_some_and(|rest| rest.starts_with(" ("))
            .then(|| Wall {
                kind: WallKind::Memory,
                message: segment.to_string(),
                reset: None,
                row: i,
                placement: Placement::Banner,
            })
    })
}

/// Whether row `i`, under the status row `status`, belongs to a message a
/// person queued in the live zone — `❯ …`, then its wrapped rows indented
/// under the caret's text: the nearest row at or above `i` and under the
/// status row that is blank or starts in column 0 is a `❯` row. The
/// person's words are the one text the live zone holds that can quote the
/// banner (the 2026-09-24 incident's draft quoted it word for word), so
/// they are never read as it, however narrow the window.
fn in_queued_message(rows: &[String], status: usize, i: usize) -> bool {
    (status + 1..=i)
        .rev()
        .map(|j| rows[j].as_str())
        .find(|row| row.trim().is_empty() || !row.starts_with(char::is_whitespace))
        .is_some_and(|row| row.starts_with('❯'))
}

/// A row's right segment and the column it starts at: the text after its
/// last run of two or more spaces, or the whole row (its indent skipped)
/// when it has none. `row` is trimmed at the end.
fn right_segment(row: &str) -> (usize, &str) {
    match row.rfind("  ") {
        Some(gap) => (row[..gap + 2].chars().count(), &row[gap + 2..]),
        None => {
            let t = row.trim_start();
            (row.chars().count() - t.chars().count(), t)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every phrase the binary and the captures carry lands on its kind.
    #[test]
    fn the_table_names_each_measured_phrase() {
        let cases: &[(&str, Option<WallKind>)] = &[
            (
                "API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment. If it persists, check https://status.claude.com.",
                Some(WallKind::Overloaded),
            ),
            ("Repeated 529 Overloaded errors", Some(WallKind::Overloaded)),
            (
                "Opus is experiencing high load, please use /model to switch to Sonnet",
                Some(WallKind::Overloaded),
            ),
            (
                "Server is temporarily limiting requests (not your usage limit)",
                Some(WallKind::Overloaded),
            ),
            (
                "API Error: 401 Invalid API key · Please run /login",
                Some(WallKind::Auth),
            ),
            ("Not logged in · Please run /login", Some(WallKind::Auth)),
            ("Login expired · Please run /login", Some(WallKind::Auth)),
            (
                "Context limit reached · /compact or /clear to continue",
                Some(WallKind::Context),
            ),
            (
                "You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
                Some(WallKind::UsageSession),
            ),
            (
                "You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)",
                Some(WallKind::UsageWeekly),
            ),
            (
                "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            (
                "You've reached your Fable 5 limit.",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            // A model name with a decimal keeps its verb: the head ends at a
            // sentence end, not at the `.` of `4.1` (the binary builds
            // `${name} requires usage credits.` from the display name).
            (
                "Opus 4.1 requires usage credits.",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            (
                "Sonnet 4.5 limit reached ∙ resets 3am",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            (
                "Opus 4.1 limit reached, now using Sonnet 4.5",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            (
                "Opus 4.1 now uses usage credits",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            // The control: a decimal does not make a sentence a wall.
            ("Opus 4.1 is the default model. Limit reached nowhere", None),
            (
                "Fable limit reached · continuing on Sonnet uses usage credits, and the prompt to confirm",
                Some(WallKind::ModelBucket { consent: true }),
            ),
            (
                "You've hit your monthly spend limit.",
                Some(WallKind::Spend),
            ),
            (
                "You're out of usage credits. Switch to another model",
                Some(WallKind::Spend),
            ),
            (
                "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel",
                Some(WallKind::UsageSession),
            ),
            (
                "Goal paused · usage limit reached · send a message after it resets to continue",
                Some(WallKind::UsageSession),
            ),
            (
                "Claude usage limit reached. Your limit will reset at 3pm (America/New_York).",
                Some(WallKind::UsageSession),
            ),
            (
                "5-hour limit reached ∙ resets 3am",
                Some(WallKind::UsageSession),
            ),
            (
                "You've hit your limit · resets 3am (Europe/London)",
                Some(WallKind::UsageSession),
            ),
            (
                "API Error: Rate limit reached for requests",
                Some(WallKind::ApiError {
                    code: Some(429),
                    retryable: true,
                }),
            ),
            (
                "API Error: 500 Internal server error",
                Some(WallKind::ApiError {
                    code: Some(500),
                    retryable: true,
                }),
            ),
            (
                "API Error: 400 due to tool use concurrency issues.",
                Some(WallKind::ApiError {
                    code: Some(400),
                    retryable: false,
                }),
            ),
            (
                "API Error: Server error mid-response. The response above may be incomplete.",
                Some(WallKind::ApiError {
                    code: None,
                    retryable: true,
                }),
            ),
        ];
        for (text, want) in cases {
            assert_eq!(classify_wall(text), *want, "{text}");
        }
    }

    /// The negative controls: notices the worker goes on under, a warning,
    /// the update banner, an interrupt, and words that merely mention a
    /// limit.
    #[test]
    fn the_not_walls_and_ordinary_words_are_not_walls() {
        for text in [
            "Concurrent subagent limit reached. You can run 5 subagents at once. Do not retry.",
            "Subagent nesting limit reached (depth 3 of 3). Complete this task directly",
            "Budget limit reached ($5.01 spent of the $5.00 maximum). New agents cannot be started.",
            "Fast limit reached and temporarily disabled · resets in 4m",
            "Fast mode overloaded and is temporarily unavailable · resets in 8m",
            "Fast mode disabled · usage credit limit reached",
            "Approaching usage limit · resets at 7pm",
            "✔ Update installed · Restart to update",
            "API Error: Request was aborted.",
            "you've hit your memory limit (512 MiB)",
            "Error: API rate limit exceeded for user ID 1234.",
            "queued 8 runs",
            "Monthly limit reached",
            "Updated the usage limit docs",
            "Checked the weekly limit handling in the parser",
            "The parser now reads the Fable limit notice too, and the session limit one.",
        ] {
            assert_eq!(classify_wall(text), None, "{text}");
        }
    }

    use crate::phase::{Phase, limit_notice, worker_phase};
    use crate::prompt::fixtures::{
        END_529, END_OFFER, END_SESSION_LIMIT, IDLE_AFTER_LIMIT_AND_MODEL_SWITCH, composer, rows,
        screen,
    };

    fn framed(body: &[&str], footer: &str) -> Vec<String> {
        let mut r = rows(body);
        r.extend(composer(footer));
        r
    }

    const FOOTER: &str = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
    const API_529: &str = "API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment. If it persists, check https://status.claude.com.";

    /// The 529 end of turn: `worker_phase` still says idle (its consumers
    /// match five words), `limit_notice` says nothing, and the wall says
    /// overloaded, read from the vendor row under the worker's message.
    #[test]
    fn a_529_end_of_turn_is_an_overloaded_wall_not_just_idle() {
        let r = screen(END_529);
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(limit_notice(&r), None);
        let w = wall(&r).expect("the wall");
        assert_eq!(w.kind, WallKind::Overloaded);
        assert_eq!(w.placement, Placement::Gutter);
        assert_eq!(w.message, API_529);
        assert_eq!(w.reset, None);
        assert!(r[w.row].contains("API Error: 529"));
        // The control: the same screen ending on an offer is no wall.
        assert_eq!(wall(&screen(END_OFFER)), None);
    }

    /// NEGATIVE CONTROLS for the vendor row: the same `API Error: 529` text
    /// as a tool's OUTPUT — under a `⏺ Bash(…)` call, under a 2.1.280
    /// running-command echo (`⎿  $ …`), in the worker's own prose — is not
    /// a wall.
    #[test]
    fn a_529_in_a_tools_output_or_the_workers_words_is_not_a_wall() {
        let under_call = framed(
            &[
                "⏺ Bash(tail -1 build.log)",
                &format!("  ⎿  {API_529}"),
                "",
                "✻ Cooked for 3m 2s · done 4:24 PM",
                "",
            ],
            FOOTER,
        );
        let echo = framed(
            &[
                "⏺ Reading the build log",
                "  ⎿  $ tail -1 build.log",
                &format!("     {API_529}"),
                "",
            ],
            FOOTER,
        );
        let prose = framed(&[&format!("⏺ The last run died on {API_529}"), ""], FOOTER);
        for (name, r) in [("tool call", under_call), ("echo", echo), ("prose", prose)] {
            assert_eq!(wall(&r), None, "{name}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{name}");
        }
        assert!(crate::phase::is_tool_call("⏺ Bash(tail -1 build.log)"));
        assert!(crate::phase::is_tool_call("⏺ Update(notes.txt)"));
        assert!(!crate::phase::is_tool_call(
            "⏺ Dynamic workflow \"Second review pass\" completed · 5s"
        ));
        assert!(!crate::phase::is_tool_call(
            "⏺ Suites are running (all three)."
        ));
    }

    /// What placement does NOT decide (module header): a `⎿` block under a
    /// `⏺ Monitor event: …` row is read as the vendor's, because a limit
    /// notice lands there exactly as under a completion notice — so a
    /// payload that opens with a wall phrase reads as a wall. Pinned so the
    /// day this is told apart shows up here. The control: the same payload
    /// under a `⏺ Name(…)` tool call is output.
    #[test]
    fn a_monitor_events_payload_is_read_as_the_vendors() {
        let payload = "  ⎿  You've hit your session limit · resets 3pm (America/Los_Angeles)";
        let monitor = framed(
            &["⏺ Monitor event: worker s-1 screen changed", payload, ""],
            FOOTER,
        );
        assert_eq!(wall(&monitor).map(|w| w.kind), Some(WallKind::UsageSession));
        let peek = framed(&["⏺ mcp__aterm__peek(s-1)", payload, ""], FOOTER);
        assert_eq!(wall(&peek), None, "the control");
        assert_eq!(worker_phase(&peek), Phase::Idle);
    }

    /// The session limit and the Fable limit read the same through both
    /// doors: `worker_phase` says limited, and the wall names the window.
    #[test]
    fn a_usage_wall_is_limited_and_named() {
        let r = screen(END_SESSION_LIMIT);
        let w = wall(&r).expect("the wall");
        assert_eq!(w.kind, WallKind::UsageSession);
        assert_eq!(w.reset.as_deref(), Some("3pm (America/Los_Angeles)"));
        assert_eq!(
            worker_phase(&r),
            Phase::Limited {
                message: w.message.clone(),
                reset: w.reset.clone(),
            }
        );
        // The real Fable screen, cut before its `/model` (as phase.rs's own
        // test cuts it): a model bucket, no consent asked.
        let full: Vec<String> = IDLE_AFTER_LIMIT_AND_MODEL_SWITCH
            .lines()
            .map(str::to_string)
            .collect();
        let mut cut = full[..56].to_vec();
        cut.extend_from_slice(&full[58..]);
        let w = wall(&cut).expect("the Fable wall");
        assert_eq!(w.kind, WallKind::ModelBucket { consent: false });
        assert!(matches!(worker_phase(&cut), Phase::Limited { .. }));
        // After the `/model` switch it is history.
        assert_eq!(wall(&full), None);
    }

    /// `Context limit reached` is a wall of its own, and not a usage limit
    /// (it read `limited` before); a subagent quota, a subagent budget and
    /// fast mode's fallback are no wall at all, in the gutter or the footer.
    #[test]
    fn context_is_its_own_wall_and_the_quota_notices_are_none() {
        const CONTEXT: &str = "Context limit reached · /compact or /clear to continue";
        let gutter = framed(
            &["⏺ Summarising the run.", &format!("  ⎿  {CONTEXT}"), ""],
            FOOTER,
        );
        let footer = framed(&["⏺ Summarising the run.", ""], &format!("  {CONTEXT}"));
        for r in [&gutter, &footer] {
            let w = wall(r).expect("the context wall");
            assert_eq!(w.kind, WallKind::Context);
            assert_eq!(limit_notice(r), None);
            assert_eq!(worker_phase(r), Phase::Idle);
        }
        for quota in [
            "Concurrent subagent limit reached. You can run 5 subagents at once. Do not retry.",
            "Budget limit reached ($5.01 spent of the $5.00 maximum). New agents cannot be started.",
            "Fast limit reached and temporarily disabled · resets in 4m",
        ] {
            let r = framed(&["⏺ Fanning out.", &format!("  ⎿  {quota}"), ""], FOOTER);
            assert_eq!(wall(&r), None, "{quota}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{quota}");
        }
    }

    /// `Goal paused · usage limit reached` (the 2.1.280 binary's text; where
    /// it is drawn is not measured, so the banner and the gutter both read
    /// it) is a usage wall; the update banner beside it is not.
    #[test]
    fn goal_paused_parses_and_the_update_banner_is_not_a_wall() {
        const PAUSED: &str =
            "⚠ Goal paused · usage limit reached · send a message after it resets to continue";
        let banner = framed(&["⏺ Step 3 is done.", "", PAUSED, ""], FOOTER);
        let w = wall(&banner).expect("the wall");
        assert_eq!(w.kind, WallKind::UsageSession);
        assert_eq!(w.placement, Placement::Banner);
        let update = framed(
            &[
                "⏺ Step 3 is done.",
                "",
                "                                                               ✔ Update installed · Restart to update",
            ],
            FOOTER,
        );
        assert_eq!(wall(&update), None);
        assert_eq!(worker_phase(&update), Phase::Idle);
    }

    /// The banner as the incident drew it, and as it would be drawn beside
    /// the spinner: a memory wall read from the banner row, its message the
    /// banner's whole text. The draft quoting it five rows lower is inside
    /// the composer and never read.
    #[test]
    fn the_memory_banner_is_read_by_its_place() {
        use crate::anchors::anchor_text;
        use crate::prompt::fixtures::{MEMORY_BANNER_BUSY, MEMORY_BANNER_IDLE};
        let r = screen(MEMORY_BANNER_BUSY);
        let w = memory_wall(&r).expect("the banner");
        assert_eq!(w.kind, WallKind::Memory);
        assert_eq!(w.placement, Placement::Banner);
        assert_eq!(w.reset, None);
        assert!(w.message.starts_with(anchor_text("wall.memory")));
        assert!(w.message.ends_with("claude --continue"), "{}", w.message);
        assert_eq!(r[w.row].trim_start(), w.message);
        assert_eq!(crate::phase::leading_spaces(&r[w.row]), 67);
        assert!(
            r[w.row + 1].starts_with('─'),
            "the row right above the rule"
        );
        // The same banner sharing the spinner's row reads the same.
        let mut shared = r.clone();
        let spinner = w.row - 1;
        let pad = 142 - shared[spinner].chars().count() - w.message.chars().count();
        shared[spinner] = format!("{}{}{}", shared[spinner], " ".repeat(pad), w.message);
        shared[w.row] = String::new();
        let s = memory_wall(&shared).expect("beside the spinner");
        assert_eq!((s.row, s.message.as_str()), (spinner, w.message.as_str()));
        // And the idle layout (synthetic, unconfirmed).
        let idle = memory_wall(&screen(MEMORY_BANNER_IDLE)).expect("idle");
        assert_eq!(idle.message, w.message);
        // `wall` asks it first.
        assert_eq!(wall(&r).map(|w| w.kind), Some(WallKind::Memory));
    }

    /// The busy fixture at `width` columns: its rules that wide, every other
    /// row cut to it, and row `at` the banner right-aligned two columns
    /// short of the edge, as it was measured at 144.
    fn narrowed(rows: &[String], at: usize, width: usize, banner: &str) -> Vec<String> {
        rows.iter()
            .enumerate()
            .map(|(i, row)| {
                if i == at {
                    format!("{banner:>w$}", w = width - 2)
                } else if row.starts_with('─') {
                    "─".repeat(width)
                } else {
                    row.chars().take(width).collect()
                }
            })
            .collect()
    }

    /// `text`, then words, to two columns short of `width`: a quote that
    /// runs on to the banner's own edge.
    fn to_edge(text: &str, width: usize) -> String {
        let mut row = format!("{text}, it said, so the run stops here");
        while row.chars().count() < width - 2 {
            row.push_str(" and");
        }
        row.chars().take(width - 2).collect()
    }

    /// The banner is 75 columns wide, so a narrow window starts it left of
    /// the hint column: at aterm's default 80 columns it starts at column 3.
    /// Under a status row every row down to the top rule is the live zone —
    /// no transcript row sits there — so there it is read at every width it
    /// fits, 80 included, where the first cut of this reader read nothing
    /// (the review of 2026-09-24: `None` at 82 columns or fewer, however
    /// busy the spinner over it).
    #[test]
    fn the_memory_banner_reads_at_every_width_under_a_spinner() {
        use crate::prompt::fixtures::MEMORY_BANNER_BUSY;
        let wide = screen(MEMORY_BANNER_BUSY);
        let at = memory_wall(&wide).expect("the control").row;
        let banner = wide[at].trim_start().to_string();
        assert_eq!(banner.chars().count(), 75);
        for width in 77..=144 {
            let r = narrowed(&wide, at, width, &banner);
            let w = memory_wall(&r).unwrap_or_else(|| panic!("{width} columns"));
            assert_eq!((w.row, w.message.as_str()), (at, banner.as_str()));
            let reading = crate::reader::read(Some("claude"), &r, None);
            assert_eq!(reading.phase, Phase::Busy, "{width} columns");
            assert_eq!(
                reading.wall.map(|w| w.kind),
                Some(WallKind::Memory),
                "{width} columns"
            );
        }
        let at_80 = narrowed(&wide, at, 80, &banner);
        assert_eq!(crate::phase::leading_spaces(&at_80[at]), 3);
    }

    /// NEGATIVE CONTROL under the spinner: the one person's text the live
    /// zone holds is a message queued there — `❯ …`, its wrapped rows
    /// indented under the caret's text. One that quotes the banner (the
    /// incident's draft, submitted), wrapped so a row opens with it and
    /// runs to the edge, is no wall at 80 columns or at 144, two rows under
    /// its `❯` or one. CONTROL: the banner itself right above that message
    /// is still read.
    #[test]
    fn a_queued_message_quoting_the_memory_banner_is_not_a_wall() {
        use crate::prompt::fixtures::MEMORY_BANNER_BUSY;
        let wide = screen(MEMORY_BANNER_BUSY);
        let at = memory_wall(&wide).expect("the control").row;
        let banner = wide[at].trim_start().to_string();
        for width in [80, 144] {
            let r = narrowed(&wide, at, width, &banner);
            let quote = to_edge(&format!("  {banner}"), width);
            for message in [
                vec!["❯ the tab above says".to_string(), quote.clone()],
                vec![
                    "❯ the tab above has said for an hour now that it will not".to_string(),
                    "  read a key, and the banner under its spinner says".to_string(),
                    quote.clone(),
                ],
            ] {
                let mut queued = r.clone();
                queued.splice(at..=at, message.iter().cloned());
                assert_eq!(memory_wall(&queued), None, "{width}: {message:?}");
                let reading = crate::reader::read(Some("claude"), &queued, None);
                assert_eq!(reading.phase, Phase::Busy, "{width}");
                assert_eq!(reading.wall, None, "{width}: {message:?}");
                let mut both = r.clone();
                both.splice(at + 1..at + 1, message.iter().cloned());
                assert_eq!(
                    memory_wall(&both).map(|w| w.row),
                    Some(at),
                    "{width}: the banner over it"
                );
            }
        }
    }

    /// NEGATIVE CONTROLS: every copy of the banner that is not the vendor's
    /// banner row reads `None` — the composer draft alone (the incident's
    /// own quote), a copy ending short of the edge, one up in the transcript
    /// beyond the live zone, the anchor with no size after it, and a screen
    /// with no composer frame; and, where the transcript meets the live zone
    /// — at idle, with no status row, the last transcript block runs down
    /// to the rows the banner is parked in — the words in the worker's
    /// message, in a paragraph of it, in a tool's output and in that
    /// output's continuation row, each running to the edge. (The first cut
    /// of these placed the transcript's copies between the spinner and the
    /// rule, where no transcript row is ever drawn.)
    #[test]
    fn a_quoted_memory_banner_is_not_a_wall() {
        use crate::anchors::anchor_text;
        use crate::prompt::fixtures::{MEMORY_BANNER_BUSY, MEMORY_BANNER_IDLE};
        let r = screen(MEMORY_BANNER_BUSY);
        let at = memory_wall(&r).expect("the control").row;
        let banner = r[at].trim_start().to_string();
        let with = |row: String| {
            let mut v = r.clone();
            v[at] = row;
            v
        };
        // Right-aligned as the banner is, ending two columns short of the
        // 144-column rule.
        let right_aligned = |text: &str| format!("{text:>142}");
        let no_size = format!("{} is back to normal", anchor_text("wall.memory"));
        assert_eq!(
            memory_wall(&with(right_aligned(&banner))).map(|w| w.row),
            Some(at)
        );
        let blank = with(String::new());
        assert!(
            blank.iter().any(|row| row.contains(&banner[..40])),
            "the draft still quotes it"
        );
        let idle = screen(MEMORY_BANNER_IDLE);
        let at_idle = memory_wall(&idle).expect("the idle control").row;
        // The banner row replaced by the last rows of a transcript block.
        let ending = |tail: &[&str]| {
            let mut v = idle.clone();
            v.splice(at_idle..=at_idle, tail.iter().map(|row| row.to_string()));
            v
        };
        let edge = |text: String| to_edge(&text, 144);
        let (message, paragraph, output, continued) = (
            edge(format!("⏺ {banner}")),
            edge(format!("  {banner}")),
            edge(format!("  ⎿  {banner}")),
            edge(format!("     {banner}")),
        );
        let cases = [
            ("blanked: only the draft's quote", blank),
            (
                "indented, ending short of the edge",
                with(format!("{}{banner}", " ".repeat(40))),
            ),
            ("the words with no size", with(right_aligned(&no_size))),
            ("idle: the worker's message", ending(&[&message])),
            (
                "idle: a paragraph of the worker's message",
                ending(&["⏺ The banner said:", "", &paragraph]),
            ),
            (
                "idle: a tool's output",
                ending(&["⏺ Bash(tail -1 run.log)", &output]),
            ),
            (
                "idle: a tool output's continuation row",
                ending(&[
                    "⏺ Bash(tail -2 run.log)",
                    "  ⎿  the run stopped:",
                    &continued,
                ]),
            ),
        ];
        for (name, v) in &cases {
            assert_eq!(memory_wall(v), None, "{name}");
            assert_eq!(wall(v), None, "{name}");
        }
        // Up in the transcript, over the spinner: not the live zone.
        let mut above = r.clone();
        above[at] = String::new();
        above.insert(1, r[at].clone());
        assert_eq!(memory_wall(&above), None, "above the status row");
        // No composer frame (the rules gone): nothing is read by place.
        let unframed: Vec<String> = r
            .iter()
            .filter(|row| !row.starts_with('─'))
            .cloned()
            .collect();
        assert_eq!(memory_wall(&unframed), None, "no frame");
    }

    #[test]
    fn only_the_usage_kinds_and_a_rate_limit_read_limited() {
        assert!(WallKind::UsageSession.reads_limited());
        assert!(WallKind::ModelBucket { consent: true }.reads_limited());
        assert!(
            WallKind::ApiError {
                code: Some(429),
                retryable: true
            }
            .reads_limited()
        );
        assert!(!WallKind::Overloaded.reads_limited());
        assert!(!WallKind::Context.reads_limited());
        assert!(!WallKind::Auth.reads_limited());
        assert!(!WallKind::Memory.reads_limited());
        assert_eq!(WallKind::Memory.name(), "memory");
        assert!(
            !WallKind::ApiError {
                code: Some(500),
                retryable: true
            }
            .reads_limited()
        );
    }
}
