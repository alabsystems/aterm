// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The WALL a worker's turn ended on, by kind: a usage window, a model
//! bucket, spend, a full context, an expired login, an API error, overload.
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
            WallKind::Context | WallKind::Auth | WallKind::Overloaded => false,
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

/// The wall the worker's last turn ended on, if any: [`classify_wall`] over
/// the rows [`crate::phase::limit_notice`] reads, every kind. It does not ask
/// whether the worker is busy — a notice stays on the screen while the vendor
/// retries under it — so read it beside [`crate::phase::busy_signal`], as
/// [`crate::reader::read`] does.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    crate::phase::notice(rows, &classify_wall)
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
        assert!(
            !WallKind::ApiError {
                code: Some(500),
                retryable: true
            }
            .reads_limited()
        );
    }
}
