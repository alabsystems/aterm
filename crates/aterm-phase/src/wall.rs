// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Classifies the wall a worker's turn ended on: usage window, model bucket, spend, full context, expired login, API error, overload, or Claude Code's critical-memory banner ([`memory_wall`], detected by screen position rather than [`PHRASES`]).

use crate::phase::Transcript;

/// Which wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallKind {
    /// The five-hour session window (`You've hit your session limit · resets 3pm`), or a usage limit that does not say which window (`Usage limit reached · continuing automatically at 1:50pm`, `Goal paused · usage limit reached`).
    UsageSession,
    /// The seven-day window (`You've hit your weekly limit · resets Sep 19 at 11am`).
    UsageWeekly,
    /// A per-model bucket (`You've reached your Fable limit. Run /usage-credits to continue or
    /// switch models with /model.`).
    ModelBucket { consent: bool },
    /// Money: a monthly spend limit, a shared budget, no usage credits left.
    Spend,
    /// The context is full (`Context limit reached · /compact or /clear to continue`, `Prompt is too long`): `/compact` moves it, waiting does not.
    Context,
    /// The login is gone (`Not logged in · Please run /login`, `API Error: 401 Invalid API key · Please run /login`, `Login expired · Please run /login` — the row Claude Code 2.1.281 writes as its `<synthetic>` `authentication_failed` answer and draws as `⏺ Login expired · …`, [`Placement::ErrorRow`]): a human's browser moves it, and until it does every turn ends on it in milliseconds, whatever was typed.
    Auth,
    /// Any other `API Error: …` the turn ended on, and Claude Code's own refusal of the request ([`ApiCause::Request`]).
    ApiError {
        code: Option<u16>,
        retryable: bool,
        cause: ApiCause,
    },
    /// The service is overloaded (`API Error: 529 Overloaded. This is a server-side issue,
    /// usually temporary — try again in a moment.`, `Repeated 529 Overloaded errors`,
    /// `Opus is experiencing high load`).
    Overloaded,
    /// Claude Code's critical-memory banner: indicates the process must be restarted and resumed on its conversation (`claude --resume <id>`, never `claude --continue`: [`memory_banner_head`]).
    Memory,
}

/// Failure category of an [`WallKind::ApiError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiCause {
    /// Server-side failure: a retryable HTTP status (5xx, 408, 409, 429; or 401/451) or an unclassified `API Error` without a status (`API Error: terminated`, `API Error: fetch failed`, `API Error: Please wait a moment and try again.`).
    Server,
    /// Network or DNS failure before reaching the API (e.g. `ENOTFOUND`,
    /// `ECONNREFUSED`, timeout, or sleep).
    Unreachable,
    /// Response cut off mid-stream or stalled before a response was produced.
    CutOff,
    /// TLS, certificate, or proxy-tunnel refusal (not retried by the vendor).
    Config,
    /// Request refused by the API (non-retryable 4xx statuses other than 401, 408, 409, 429, 451, or [`Tag::Refused`] notices such as safeguards, unsupported model/effort, or account holds).
    Request,
    /// Response exceeded the output token maximum (`CLAUDE_CODE_MAX_OUTPUT_TOKENS`).
    Overlong,
}

impl WallKind {
    /// The lowercase word the CLI prints after `wall:` (`overloaded`).
    #[inline]
    #[must_use]
    pub const fn name(&self) -> &'static str {
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

    /// The whole word `status agent=` publishes for this wall: `wall:` and [`Self::name`] (`wall:overloaded`).
    #[inline]
    #[must_use]
    pub const fn wire(&self) -> &'static str {
        match self {
            WallKind::UsageSession => "wall:usage-session",
            WallKind::UsageWeekly => "wall:usage-weekly",
            WallKind::ModelBucket { .. } => "wall:model-bucket",
            WallKind::Spend => "wall:spend",
            WallKind::Context => "wall:context",
            WallKind::Auth => "wall:auth",
            WallKind::ApiError { .. } => "wall:api-error",
            WallKind::Overloaded => "wall:overloaded",
            WallKind::Memory => "wall:memory",
        }
    }

    /// Whether [`crate::phase::worker_phase`] reads this wall as [`crate::phase::Phase::Limited`]: the usage windows, a model bucket, spend, and an API rate limit (429) — the kinds it has always read so.
    #[inline]
    #[must_use]
    pub const fn reads_limited(&self) -> bool {
        match self {
            WallKind::UsageSession
            | WallKind::UsageWeekly
            | WallKind::ModelBucket { .. }
            | WallKind::Spend => true,
            WallKind::ApiError { code, .. } => matches!(*code, Some(429)),
            WallKind::Context | WallKind::Auth | WallKind::Overloaded | WallKind::Memory => false,
        }
    }
}

/// Where on the screen a wall was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// An item of the footer, under the composer's bottom rule.
    Footer,
    /// A banner row Claude Code parks under the last thing said (`⚠ Usage limit reached · …`).
    Banner,
    /// The vendor row: a `⎿` block under the worker's last message.
    Gutter,
    /// The vendor's error row: an `isApiErrorMessage` row Claude Code 2.1.281 draws with its `⏺` bullet in column 0, as the last thing said (`⏺ Login expired · Please run /login`; `crate::phase`'s `error_row_notice`).
    ErrorRow,
    /// A sign-in screen or menu the program draws instead of its composer ([`Wall::sign_in`]): the one wall whose remedy stands on the screen itself, a sign-in the program already runs.
    SignIn,
}

/// One wall, read from a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wall {
    pub kind: WallKind,
    /// The notice as Claude Code says it, its continuation rows joined.
    pub message: String,
    /// When it resets or goes on by itself, if the notice says (`7:30pm (America/Los_Angeles)`, `in 3h`, `shortly`).
    pub reset: Option<String>,
    /// The row the notice opens on — for a guard bound to it.
    pub row: usize,
    pub placement: Placement,
    /// Whether a [`Placement::Gutter`] wall was read on a later column-2 row under a tool call (`phase::block_is_vendors`).
    pub under_call: bool,
}

impl Wall {
    /// A sign-in menu or screen drawn INSTEAD of the composer, `rows[row]` its message ([`WallKind::Auth`], [`Placement::SignIn`]): agy's and Muse Code's, whose options open a browser and are never read, and Grok Build's device sign-in, whose approval waits in a browser.
    pub(crate) fn sign_in(rows: &[String], row: usize) -> Self {
        Self {
            kind: WallKind::Auth,
            message: crate::text::trim(&rows[row]).to_string(),
            reset: None,
            row,
            placement: Placement::SignIn,
            under_call: false,
        }
    }
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
    /// An account the harness cannot lift from here (seat type, admin allocation, group cap, org switched off): read as [`WallKind::Spend`] — waited out on the limit ladder, with a person told once ([`aterm-agent`'s exception]).
    Blocked,
    /// `Goal paused · the request was rate limited`.
    RateLimit,
    /// The API refused the request itself, by the vendor's own words (a safeguards refusal, a model not found or not on the plan, an account on hold, an effort it does not take): read as [`WallKind::ApiError`] with [`ApiCause::Request`], the status it prints kept.
    Refused,
}

/// THE phrase table: the first row whose phrase occurs in a notice's head (its text up to the first sentence end, `·`, `∙` or `|`, lowercased, `’` as `'`, number tokens dropped so `Fable 5 limit` and `Opus 4.1 limit` read as `fable limit` and `opus limit`) names it.
pub const PHRASES: &[(&str, Tag)] = &[
    ("approaching", Tag::NotAWall),
    ("concurrent subagent limit", Tag::NotAWall),
    ("subagent nesting limit", Tag::NotAWall),
    ("budget limit reached", Tag::NotAWall),
    ("fast limit", Tag::NotAWall),
    ("fast mode", Tag::NotAWall),
    ("request was aborted", Tag::NotAWall),
    ("update installed", Tag::NotAWall),
    ("due to high demand", Tag::NotAWall),
    ("you're now using usage credits", Tag::NotAWall),
    ("now using usage credits", Tag::NotAWall),
    ("you're now using your usage allocation", Tag::NotAWall),
    ("now using your usage allocation", Tag::NotAWall),
    ("you're now using extra usage", Tag::NotAWall),
    ("now using extra usage", Tag::NotAWall),
    ("is drawing from usage credits", Tag::NotAWall),
    ("is now using usage credits instead", Tag::NotAWall),
    ("usage limit has reset", Tag::NotAWall),
    ("usage limit reset", Tag::NotAWall),
    ("usage limit available again", Tag::NotAWall),
    ("continuing automatically", Tag::NotAWall),
    ("session limit reset", Tag::NotAWall),
    ("reset your session limit", Tag::NotAWall),
    ("resetting your session limit", Tag::NotAWall),
    ("session limit is already being reset", Tag::NotAWall),
    ("5-hour limit reset", Tag::NotAWall),
    ("reset your 5-hour limit", Tag::NotAWall),
    ("resetting your 5-hour limit", Tag::NotAWall),
    ("5-hour limit is already being reset", Tag::NotAWall),
    ("weekly limits reset", Tag::NotAWall),
    ("context limit reached", Tag::Context),
    ("prompt is too long", Tag::Context),
    ("context exceeds", Tag::Context),
    ("compaction failed", Tag::Context),
    ("error during compaction", Tag::Context),
    ("the request was rate limited", Tag::RateLimit),
    ("temporarily limiting requests", Tag::Overloaded),
    ("repeated overloaded errors", Tag::Overloaded),
    ("overloaded", Tag::Overloaded),
    ("high load", Tag::Overloaded),
    ("high demand", Tag::Overloaded),
    ("please run /login", Tag::Auth),
    ("run /login", Tag::Auth),
    ("not logged in", Tag::Auth),
    ("login expired", Tag::Auth),
    ("oauth token revoked", Tag::Auth),
    ("invalid api key", Tag::Auth),
    ("invalid auth token", Tag::Auth),
    ("authentication failed", Tag::Auth),
    ("authentication error", Tag::Auth),
    ("authentication required", Tag::Auth),
    (
        "your organization has disabled api key authentication",
        Tag::Auth,
    ),
    ("token expired", Tag::Auth),
    ("not signed in to the cloud gateway", Tag::Auth),
    ("gateway session expired", Tag::Auth),
    ("can't help with this", Tag::Refused),
    ("safeguards flagged", Tag::Refused),
    ("run /rewind", Tag::Refused),
    ("isn't available with thinking", Tag::Refused),
    ("advisor model is not compatible", Tag::Refused),
    ("verify your identity", Tag::Refused),
    ("data loss prevention", Tag::Refused),
    ("does not support this model", Tag::Refused),
    ("issue with the selected model", Tag::Refused),
    ("to pick a different model", Tag::Refused),
    ("not available on your", Tag::Refused),
    ("account is on hold", Tag::Refused),
    ("not available with the claude pro plan", Tag::Refused),
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
    ("your seat type doesn't include", Tag::Blocked),
    ("your usage allocation has been disabled", Tag::Blocked),
    ("your group's usage limit is set to", Tag::Blocked),
    ("this service is disabled for your org", Tag::Blocked),
    (
        "your organization has disabled claude subscription access",
        Tag::Blocked,
    ),
    ("extra usage is required for long context", Tag::Blocked),
    ("usage credits required", Tag::Blocked),
    ("session limit", Tag::Session),
    ("5-hour limit", Tag::Session),
    ("usage limit", Tag::Session),
    ("hit your limit", Tag::Session),
    ("reached your limit", Tag::Session),
    ("limit will reset", Tag::Session),
];

/// The wall `text` names, if it is one — `text` being a notice as it OPENS a vendor row (a leading status glyph such as `⚠` is skipped).
#[must_use]
pub fn classify_wall(text: &str) -> Option<WallKind> {
    let text = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    let lower = text.replace('’', "'").to_lowercase();
    if goal_echo_of_an_unavailable_api(&lower) {
        return Some(WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Server,
        });
    }
    let lower = match lower.strip_prefix("goal paused") {
        Some(rest) => rest
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '·' | '∙' | '.'))
            .to_string(),
        None => lower,
    };
    if opens_with_label(&lower) {
        return None;
    }
    if lower.contains("temporary network issue")
        || lower.contains("another claude code process is refreshing it")
    {
        return Some(WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Unreachable,
        });
    }
    if let Some(rest) = lower.strip_prefix("goal cleared after an unrecoverable error (") {
        return match rest.split(')').next().unwrap_or_default() {
            "authentication failed" | "organization verification required" => Some(WallKind::Auth),
            "credit balance too low" => Some(WallKind::Spend),
            "context limit reached" => Some(WallKind::Context),
            _ => None,
        };
    }
    let bare = lower
        .strip_prefix("error:")
        .map_or(lower.as_str(), str::trim_start);
    if let Some(detail) = bare.strip_prefix("error during compaction:") {
        return match classify_wall(detail) {
            kind @ Some(
                WallKind::Auth
                | WallKind::UsageSession
                | WallKind::UsageWeekly
                | WallKind::Spend
                | WallKind::ModelBucket { .. },
            ) => kind,
            _ => Some(WallKind::Context),
        };
    }
    let head = notice_head(&lower);
    let opening = Opening::of(head);
    let tag = PHRASES
        .iter()
        .find(|(phrase, _)| opening.opens_with(phrase))
        .map(|&(_, tag)| tag);
    let api = head.starts_with("api error");
    let usage_verb = || USAGE_VERBS.iter().any(|v| head.contains(v));
    match tag {
        Some(Tag::NotAWall) => None,
        Some(Tag::Session | Tag::Weekly | Tag::Model | Tag::Spend) if !usage_verb() => None,
        Some(Tag::Session) => Some(WallKind::UsageSession),
        Some(Tag::Weekly) => Some(WallKind::UsageWeekly),
        Some(Tag::Model) => Some(WallKind::ModelBucket {
            consent: lower.contains("prompt to confirm") || lower.contains("continuing on "),
        }),
        Some(Tag::Spend | Tag::Blocked) => Some(WallKind::Spend),
        Some(Tag::RateLimit) => Some(WallKind::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        }),
        Some(Tag::Context) => Some(WallKind::Context),
        Some(Tag::Auth) => Some(WallKind::Auth),
        Some(Tag::Overloaded) => Some(WallKind::Overloaded),
        Some(Tag::Refused) => Some(WallKind::ApiError {
            code: head.strip_prefix("api error").and_then(api_status),
            retryable: false,
            cause: ApiCause::Request,
        }),
        None if api || crate::text::trim(&lower) == "request timed out" => api_error(head, &lower),
        None => None,
    }
}

/// Whether the table reads `text` — a notice as it OPENS, its words held from elsewhere (aterm-agent's transcript of the session) — as one that is NOT a wall ([`Tag::NotAWall`]: `API Error: Request was aborted.`, a subagent's quota, the auto-continue's lifecycle): a reader that names a wall the table does not ([`classify_wall`] `None`) names none for it.
#[must_use]
pub fn not_a_wall(text: &str) -> bool {
    let text = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    let lower = text.replace('’', "'").to_lowercase();
    let opening = Opening::of(notice_head(&lower));
    PHRASES
        .iter()
        .find(|(phrase, _)| opening.opens_with(phrase))
        .is_some_and(|&(_, tag)| tag == Tag::NotAWall)
}

/// Whether [`classify_wall`] reads `text` BY THE STATUS ITS `API Error` HEAD PRINTS ALONE ([`api_error`] with a status: `API Error: 403 Forbidden`, `API Error: 401 Unauthorized`, the API's own JSON body behind its status): no phrase of the table opens the notice ([`Opening`]) and no refusal's row is anywhere in it ([`Tag::Refused`]'s: `API Error: 403 Request blocked by your organization's data loss prevention policy.` is named by its words).
#[must_use]
pub fn named_by_status_alone(text: &str) -> bool {
    let trimmed = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    let lower = trimmed.replace('’', "'").to_lowercase();
    let opening = Opening::of(notice_head(&lower));
    matches!(
        classify_wall(text),
        Some(WallKind::ApiError { code: Some(_), .. })
    ) && !PHRASES.iter().any(|&(phrase, tag)| {
        opening.opens_with(phrase) || (tag == Tag::Refused && lower.contains(phrase))
    })
}

/// Whether `lower` (a notice as it opens, lowercased) is Claude Code's `/goal` echo of a request
/// the API did not answer: it OPENS with `Goal still active` or `Goal paused` (`Goal paused after
/// 3 automatic retries` among them) and names `the API was unavailable or the connection
/// dropped`, the reason Claude Code 2.1.310 gives a transient error, an overload or a server
/// error. Read alone, with no error row above it to read it through to, that echo is a generic
/// API error ([`ApiCause::Server`]), never an unreachable route: its reason covers an overload
/// and a server error too, so it is waited out blind and a measured Up of the route alone never
/// clears it (aterm-agent holds a code-less server error as the connection's outage only where it
/// measured the route down at the wall); only the error row it reads through to (`Can't reach
/// the API server … (ENOTFOUND)`, the owner's outage) names an unreachable route. The guard is on
/// the opening, never a bare `contains`: a message read through to its error row (`API Error: 529
/// Overloaded … — Goal still active · the API was unavailable …`, as aterm-agent keeps and
/// re-reads it) opens with that row and is read by it.
fn goal_echo_of_an_unavailable_api(lower: &str) -> bool {
    let opens = [
        crate::anchor!("wall.goal_still"),
        crate::anchor!("wall.goal_paused"),
    ]
    .iter()
    .any(|echo| strip_ci(lower, echo).is_some());
    opens && lower.contains(&crate::anchor!("wall.goal_api_unavailable").to_ascii_lowercase())
}

fn notice_head(text: &str) -> &str {
    let end = text
        .char_indices()
        .find(|&(i, c)| {
            matches!(c, '·' | '∙' | '|')
                || (c == '.' && text[i + 1..].chars().next().is_none_or(char::is_whitespace))
        })
        .map_or(text.len(), |(i, _)| i);
    crate::text::trim(&text[..end])
}

const USAGE_VERBS: &[&str] = &[
    "reached", "hit your", "reset", "out of", "requires", "now uses", "too low",
];

const OPENING_WORDS: usize = 4;

const NOTICE_HEADS: &[&str] = &["api error:", "error during compaction:", "error:"];

fn opens_with_label(lower: &str) -> bool {
    let mut lead: Vec<&str> = Vec::new();
    for word in notice_head(lower).split_whitespace().take(OPENING_WORDS) {
        lead.push(word);
        let number = word
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ':');
        if word.ends_with(':') && !number {
            let head = if word.starts_with('(') {
                format!("{}:", lead[..lead.len() - 1].join(" "))
            } else {
                lead.join(" ")
            };
            return !NOTICE_HEADS.contains(&head.as_str());
        }
    }
    false
}

/// Whether `text`, a row's text as drawn (a leading glyph such as `⎿` or `⏺` passed over), OPENS WITH A LABEL ([`opens_with_label`]): a program's words follow it, never a notice of Claude Code's own.
pub(crate) fn labelled(text: &str) -> bool {
    let text = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    opens_with_label(&text.replace('\u{2019}', "'").to_lowercase())
}

struct Opening {
    words: String,
    starts: Vec<usize>,
}

impl Opening {
    fn of(head: &str) -> Self {
        let words = head
            .split_whitespace()
            .filter(|w| {
                !w.chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == ':')
            })
            .collect::<Vec<_>>()
            .join(" ");
        let starts = std::iter::once(0)
            .chain(words.match_indices(' ').map(|(at, _)| at + 1))
            .take(OPENING_WORDS + 1)
            .collect();
        Self { words, starts }
    }

    fn opens_with(&self, phrase: &str) -> bool {
        self.starts
            .iter()
            .any(|&at| self.words[at..].starts_with(phrase))
    }
}

fn api_error(head: &str, full: &str) -> Option<WallKind> {
    let code = api_status(head.strip_prefix("api error").unwrap_or(head));
    if code == Some(529) {
        return Some(WallKind::Overloaded);
    }
    if full.contains("context window limit") {
        return Some(WallKind::Context);
    }
    let names = |words: &[&str]| words.iter().any(|w| full.contains(w));
    let refused_words = || {
        PHRASES
            .iter()
            .any(|&(phrase, tag)| tag == Tag::Refused && full.contains(phrase))
    };
    let unnamed = if refused_words() {
        ApiCause::Request
    } else if names(CONFIG_WORDS) {
        ApiCause::Config
    } else if names(OVERLONG_WORDS) {
        ApiCause::Overlong
    } else if names(CUT_OFF_WORDS) {
        ApiCause::CutOff
    } else if names(UNREACHABLE_WORDS) {
        ApiCause::Unreachable
    } else {
        ApiCause::Server
    };
    Some(api_wall(code, unnamed))
}

/// The API wall of an error that printed status `code`, ITS CAUSE THE STATUS'S ([`ApiCause`]): a refused status ([`refused_status`]) is the request's, any other the server's, and only an error with no status takes `unnamed`, the cause its words name. Whether it is retryable follows from the same cause and status. Claude Code's notice (on the screen and in aterm-agent's transcript) and Muse Code's failed model call are named through it.
#[must_use]
pub fn api_wall(code: Option<u16>, unnamed: ApiCause) -> WallKind {
    let cause = match code {
        Some(c) if refused_status(c) => ApiCause::Request,
        Some(_) => ApiCause::Server,
        None => unnamed,
    };
    let retryable = match (cause, code) {
        (ApiCause::Config | ApiCause::Request, _) => false,
        (ApiCause::Unreachable | ApiCause::CutOff | ApiCause::Overlong, _) => true,
        (ApiCause::Server, Some(c)) => matches!(c, 408 | 409 | 429) || c >= 500,
        (ApiCause::Server, None) => true,
    };
    WallKind::ApiError {
        code,
        retryable,
        cause,
    }
}

fn api_status(rest: &str) -> Option<u16> {
    let rest = rest.trim_start_matches([':', ' ']);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() == 3 {
        digits.parse::<u16>().ok()
    } else if rest.starts_with("rate limit") {
        Some(429)
    } else {
        status_in_parens(rest)
    }
}

fn refused_status(code: u16) -> bool {
    (400..500).contains(&code) && !matches!(code, 401 | 408 | 409 | 429 | 451)
}

fn status_in_parens(rest: &str) -> Option<u16> {
    rest.match_indices('(').find_map(|(at, _)| {
        let inner = rest.get(at + 1..at + 5)?;
        let digits = inner.strip_suffix(')')?;
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let code = digits.parse::<u16>().ok()?;
        (400..600).contains(&code).then_some(code)
    })
}

const CONFIG_WORDS: &[&str] = &[
    "certificate",
    "self-signed",
    "self_signed",
    "err_proxy_tunnel",
    "through your proxy",
    "unable_to_verify",
    "unable_to_get_issuer",
    "cert_",
];

const CUT_OFF_WORDS: &[&str] = &[
    "mid-response",
    "could not be processed and was removed",
    "may be incomplete",
    "stopped arriving",
    "never arrived",
    "malformed",
    "stalled",
    "before a response was produced",
];

const OVERLONG_WORDS: &[&str] = &["output token maximum"];

const UNREACHABLE_WORDS: &[&str] = &[
    "can't reach",
    "no internet route",
    "unable to connect",
    "connection refused",
    "connection dropped",
    "connection lost",
    "connection closed",
    "connection reset",
    "timed out",
    "no response from api",
    "asleep",
    "network issue",
    "no such host",
    "network is unreachable",
    "no route to host",
    "failed to lookup address",
    "dns error",
    "error sending request",
    "enotfound",
    "eai_again",
    "econnrefused",
    "econnreset",
    "etimedout",
    "enetunreach",
    "enetdown",
    "ehostunreach",
    "ehostdown",
    "epipe",
    "econnaborted",
    "failedtoopensocket",
    "und_err_socket",
    "err_socket_closed",
    "connectionclosed",
];

/// Whether `text` names a network reachability failure ([`UNREACHABLE_WORDS`]).
#[must_use]
pub(crate) fn is_unreachable(text: &str) -> bool {
    let lower = text.to_lowercase();
    UNREACHABLE_WORDS.iter().any(|w| lower.contains(w))
}

/// The wall the worker's last turn ended on, if any: Claude Code's critical-memory banner first ([`memory_wall`]), then [`classify_wall`] over the rows [`crate::phase::limit_notice`] reads, every kind.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    memory_wall(rows).or_else(|| turn_wall(rows))
}

/// [`wall`] past Claude Code's critical-memory banner: the notice the worker's last turn ended on
/// ([`crate::phase::notice`]), every kind, each limit notice ([`crate::phase::limit_notice`])
/// among them. The banner is no end of a turn: Claude Code draws it over a live turn too, where a
/// reading keeps it as its one wall ([`crate::ScreenReader::health_wall`]).
#[must_use]
pub(crate) fn turn_wall(rows: &[String]) -> Option<Wall> {
    crate::phase::notice(rows)
}

/// Whether the screen says the login is back: the last thing said is the vendor's own word that a `/login` finished — `⎿  Login successful`, or `⎿  Login successful · account or organization changed` — hanging from the `❯ /login` a person typed, each row read up to the side panel's rule ([`crate::phase::Transcript`]).
#[must_use]
pub fn login_restored(rows: &[String]) -> bool {
    use crate::phase::{gutter_open, last_said_in, line_at};
    let transcript = Transcript::of(rows);
    let Some(last) = last_said_in(rows, &transcript) else {
        return false;
    };
    let width = transcript.room(crate::question::screen_width(rows));
    let rows: &[String] = &transcript.rows;
    let Some(open) = gutter_open(rows, last) else {
        return false;
    };
    let (end, line) = line_at(rows, open, rows.len(), width);
    let restored = end >= last
        && crate::text::trim_start(&line)
            .strip_prefix('⎿')
            .is_some_and(|out| crate::text::trim_start(out).starts_with("Login successful"));
    restored
        && rows[..open]
            .iter()
            .rev()
            .find(|r| !crate::text::trim(r).is_empty() && crate::phase::leading_spaces(r) == 0)
            .is_some_and(|r| crate::text::trim_end(r) == "❯ /login")
}

const NOTICE_ROWS: usize = 3;

/// The days Claude Code says the stored login has left — its own `Your login expires in N days · run /login to renew` (anchors `login.expires_in`, `login.renew_hint`, N from 1 to 3) — or `None`. A row over the composer is read up to the side panel's rule and against its room ([`crate::phase::Transcript`]), as the right edge such a hint stands against there; a footer row whole, against the composer's width.
#[must_use]
pub fn login_expires_in(rows: &[String]) -> Option<u32> {
    use crate::phase::{HINT_COLUMN, composer_frame, is_against_right_edge};
    let head = const { crate::anchor!("login.expires_in") };
    let hint = const { crate::anchor!("login.renew_hint") };
    let frame = composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let transcript = Transcript::of(rows);
    let above = (frame.top.saturating_sub(NOTICE_ROWS)..frame.top)
        .map(|i| (transcript.rows[i].as_str(), transcript.room(width)));
    let footer = rows[frame.bottom + 1..].iter().map(|r| (r.as_str(), width));
    above.chain(footer).find_map(|(row, width)| {
        let row = crate::text::trim_end(row);
        let (column, segment) = right_segment(row);
        if column < HINT_COLUMN && !is_against_right_edge(row, width) {
            return None;
        }
        let at = segment.find(head)?;
        let glyph = &segment[..at];
        if glyph.chars().count() > 2 || glyph.chars().any(char::is_alphanumeric) {
            return None;
        }
        let rest = &segment[at + head.len()..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let days: u32 = rest[..digits].parse().ok()?;
        let rest = rest[digits..].strip_prefix(" day")?;
        let rest = rest.strip_prefix('s').unwrap_or(rest);
        let rest = rest.trim_start_matches([' ', '\u{b7}']);
        (rest.is_empty() || rest.starts_with(hint)).then_some(days)
    })
}

/// How Claude Code's own auto-continue of a usage limit ended, by the vendor's word on the screen ([`auto_continue_end`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoContinue {
    /// It went on: the vendor sent the continuation itself — `Usage limit reset · continuing automatically` at the reset, `Usage limit available again · continuing now` before it.
    Fired,
    /// It will not go on by itself: `Automatic continue cancelled · …` (Esc on its countdown; the session moved, relaunched or exited), `Automatic continue stopped · …`, `Automatic continue was turned off · …`, or a reset that passed with nothing sent (`Usage limit has reset · press enter to continue`).
    Unkept,
    /// It has not ended: its countdown is still drawn (`Continuing automatically at 3:15pm · esc to cancel`, `Continuing shortly · esc to cancel` — the footer's row under the banner from 2.1.295 on, a notification under the wall's row in 2.1.284).
    Armed,
}

/// What the screen last says of Claude Code's own auto-continue, once the limit it was armed for has left the screen: Esc on `Continuing automatically at 3:50pm · esc to cancel` draws `⏺ Automatic continue cancelled · /rate-limit-options to re-arm` in the transcript and as the footer's notification; at the reset the vendor draws `⏺ Usage limit reset · continuing automatically` and sends the continuation itself, with no `❯` row, its reply under it. The transcript's rows are read up to the side panel's rule and in its room ([`Transcript`]), the footer's whole.
#[must_use]
pub fn auto_continue_end(rows: &[String]) -> Option<AutoContinue> {
    auto_continue_end_on(rows, &Transcript::of(rows))
}

/// [`auto_continue_end`] on `rows`, whose transcript the caller holds.
fn auto_continue_end_on(rows: &[String], transcript: &Transcript<'_>) -> Option<AutoContinue> {
    if let Some(end) = auto_continue_toast(rows) {
        return Some(end);
    }
    auto_continue_said_row(rows, transcript).and_then(|(end, _)| end)
}

/// The transcript's row, up from the composer and short of a person's prompt, that says how the
/// auto-continue ended or names a limit, read on the transcript's rows ([`Transcript`]).
fn auto_continue_said_row(
    rows: &[String],
    transcript: &Transcript<'_>,
) -> Option<(Option<AutoContinue>, usize)> {
    use crate::phase::line_at;
    let frame = crate::phase::composer_frame(rows)?;
    let width = transcript.room(crate::text::trim_end(&rows[frame.bottom]).chars().count());
    let said: &[String] = &transcript.rows;
    for (i, row) in said[..frame.top].iter().enumerate().rev() {
        if crate::phase::leading_spaces(row) == 0 && crate::text::trim_start(row).starts_with('❯')
        {
            return None;
        }
        let (_, line) = line_at(said, i, frame.top, width);
        for item in gapped_items(&line) {
            let end = auto_continue_said(item);
            let limit = end.is_none() && classify_wall(item).is_some_and(|k| k.reads_limited());
            if (end.is_some() || limit) && crate::phase::programs_words(said, i) {
                break;
            }
            if end.is_some() || limit {
                return Some((end, i));
            }
        }
    }
    None
}

/// The row a limit reader read as Claude Code's for want of the rows above ([`limit_words_above`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordsAbove {
    /// The row its line opens on.
    pub row: usize,
    /// The line's last row: the rows Ink wrapped it onto end here.
    pub end: usize,
}

/// The row a limit reader decided on without its owner: the newest limit notice [`since_limit`] reads, its `/goal` echo, or the transcript's row [`auto_continue_end`] reads — where the walk that tells a program's words from Claude Code's own reached the first of `rows` undecided ([`crate::phase::WordsOf::Above`]), the lowest such row, each read on the transcript's rows ([`Transcript`]).
#[must_use]
pub fn limit_words_above(rows: &[String]) -> Option<WordsAbove> {
    let frame = crate::phase::composer_frame(rows)?;
    let transcript = Transcript::of(rows);
    let width = transcript.room(crate::text::trim_end(&rows[frame.bottom]).chars().count());
    let newest = newest_limit(rows, &transcript);
    let said = auto_continue_toast(rows)
        .is_none()
        .then(|| auto_continue_said_row(rows, &transcript))
        .flatten()
        .map(|(_, i)| i);
    let rows: &[String] = &transcript.rows;
    newest
        .iter()
        .flat_map(|n| [Some(n.row), n.echo])
        .flatten()
        .chain(said)
        .filter(|&i| crate::phase::words_of(rows, i) == crate::phase::WordsOf::Above)
        .max()
        .map(|row| WordsAbove {
            row,
            end: crate::phase::line_at(rows, row, frame.top, width).0,
        })
}

/// What the footer's notification says of Claude Code's own auto-continue ([`auto_continue_end`]'s first read): the live process's own toast, drawn under the composer, never a row of a conversation's history.
#[must_use]
pub fn auto_continue_toast(rows: &[String]) -> Option<AutoContinue> {
    use crate::phase::{footer_end, line_at};
    let frame = crate::phase::composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let until = footer_end(rows, frame.bottom + 1);
    (frame.bottom + 1..until).find_map(|i| {
        let (_, line) = line_at(rows, i, until, width);
        gapped_items(&line).find_map(auto_continue_said)
    })
}

/// The screen's newest usage limit notice and what was served, or chosen, since it ([`since_limit`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinceLimit {
    /// The notice's kind ([`classify_wall`]); a generic countdown row's (`Usage limit reached · continuing automatically at …`), the turn's own notice's above it naming the same reset ([`named_by_the_turn`]).
    pub kind: WallKind,
    /// The reset it names, as printed ([`crate::notice_reset`]: `10:38am`, `Oct 20 at 9am (…)`): what tells one limit's notice from another's.
    pub reset: Option<String>,
    /// A turn was served since it: a person's prompt below it (a `❯` row in column zero with text, no local command — `❯ /usage`, `❯ /model`, a `/resume`: no turn) with the model's reply drawn under that prompt (a `⏺` row in column zero, `●` off macOS, that is no notice of the vendor's: the model's words or a tool's call).
    pub served: bool,
    /// The vendor went on by itself under it: its `⏺ Usage limit reset · continuing automatically` (or `Usage limit available again · continuing now`, [`AutoContinue::Fired`]) drawn in column zero below this notice.
    pub fired: bool,
    /// A model was chosen since it: a person's `/model` below it (`❯ /model`, `❯ /model opus`) with its result drawn under that prompt (`⎿  Set model to …`, the anchor `model.set`).
    pub model_set: bool,
    /// The newest completed `/model` choice's words after `Set model to `.
    pub chosen_model: Option<String>,
    /// The notice names no limit: Claude Code's generic row (`Usage limit reached · continuing automatically at …`) with no notice of its turn drawn above it ([`turns_notice`]: scrolled off, or cut to its wrapped rows).
    pub generic: bool,
}

/// The newest usage limit notice the screen draws and whether a turn was served since it ([`SinceLimit`]): up from the composer, the first line (or wide-gapped item) Claude Code's limit notice opens (`You've hit your session limit · …`, its `Usage limit reached · continuing automatically …` row; a line that names no reset — the `/goal` echo of the notice above it — takes its reset from that notice), and the rows below it. Each row over the composer is read up to the side panel's rule and wrapped in its room ([`Transcript`]), so the panel's text beside it, which the wide gaps of the row's padding set apart as an item, is none of its words.
#[must_use]
pub fn since_limit(rows: &[String]) -> Option<SinceLimit> {
    newest_limit(rows, &Transcript::of(rows)).map(|newest| newest.since)
}

fn dotted_said<'a>(row: &str, line: &'a str) -> Option<&'a str> {
    if crate::phase::is_survey(row) {
        return None;
    }
    crate::text::trim(line)
        .strip_prefix(['⏺', '●'])
        .filter(|said| classify_wall(said).is_none())
}

struct Newest {
    since: SinceLimit,
    row: usize,
    message: String,
    echo: Option<usize>,
}

/// [`since_limit`]'s walk, on the transcript's rows, each up to the side panel's rule and wrapped
/// in its room ([`Transcript`]), which the caller holds for `rows`.
fn newest_limit(rows: &[String], transcript: &Transcript<'_>) -> Option<Newest> {
    use crate::phase::line_at;
    let frame = crate::phase::composer_frame(rows)?;
    let width = transcript.room(crate::text::trim_end(&rows[frame.bottom]).chars().count());
    let screen = rows;
    let rows: &[String] = &transcript.rows;
    let words = |item: &str| {
        item.trim_start_matches(|c: char| !c.is_alphanumeric())
            .to_string()
    };
    let (mut replied, mut served) = (false, false);
    let mut fired = false;
    let mut set: Option<String> = None;
    let (mut model_set, mut chosen_model) = (false, None);
    let mut echo: Option<(WallKind, usize, String)> = None;
    let echoed = |(kind, row, message): (WallKind, usize, String),
                  served,
                  fired,
                  model_set,
                  chosen_model| Newest {
        since: SinceLimit {
            kind,
            reset: None,
            served,
            fired,
            model_set,
            chosen_model,
            generic: false,
        },
        row,
        message,
        echo: None,
    };
    for (i, row) in rows[..frame.top].iter().enumerate().rev() {
        let column0 = crate::phase::leading_spaces(row) == 0;
        let text = crate::text::trim(row);
        if column0 && let Some(prompt) = text.strip_prefix('❯') {
            if let Some(echo) = echo {
                return Some(echoed(echo, served, fired, model_set, chosen_model));
            }
            let prompt = crate::text::trim(prompt);
            served |= replied && !prompt.is_empty() && !prompt.starts_with('/');
            let chose = set.is_some()
                && prompt
                    .strip_prefix("/model")
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '));
            if chose {
                model_set = true;
                if chosen_model.is_none() {
                    chosen_model = set.take();
                }
            }
            (replied, set) = (false, None);
            continue;
        }
        let (_, line) = line_at(rows, i, frame.top, width);
        let limit = gapped_items(&line).find_map(|item| {
            classify_wall(item)
                .filter(|k| k.reads_limited())
                .map(|k| (k, item))
        });
        if let Some((kind, item)) = limit.filter(|_| !crate::phase::programs_words(rows, i)) {
            match crate::phase::notice_reset(item) {
                Some(reset) => {
                    let found = named_by_the_turn(
                        screen,
                        transcript,
                        Wall {
                            kind,
                            message: words(item),
                            reset: Some(reset.clone()),
                            row: i,
                            placement: Placement::ErrorRow,
                            under_call: false,
                        },
                    );
                    let generic = generic_limit(item) && turns_notice(rows, i, width).is_none();
                    return Some(Newest {
                        since: SinceLimit {
                            kind: found.kind,
                            reset: Some(reset),
                            served,
                            fired,
                            model_set,
                            chosen_model,
                            generic,
                        },
                        row: i,
                        message: found.message,
                        echo: echo.map(|(_, row, _)| row),
                    });
                }
                None => {
                    echo.get_or_insert_with(|| (kind, i, words(item)));
                    continue;
                }
            }
        }
        if echo.is_none()
            && let Some(chosen) = crate::text::trim(&line).strip_prefix('⎿').and_then(|out| {
                crate::text::trim_start(out).strip_prefix(crate::anchor!("model.set"))
            })
        {
            set.get_or_insert_with(|| crate::text::trim(chosen).to_string());
        }
        if echo.is_none()
            && column0
            && let Some(said) = dotted_said(row, &line)
        {
            match auto_continue_said(said) {
                None => replied = true,
                Some(AutoContinue::Fired) => fired = true,
                Some(AutoContinue::Unkept | AutoContinue::Armed) => {}
            }
        }
    }
    echo.map(|echo| echoed(echo, served, fired, model_set, chosen_model))
}

const GENERIC_LIMIT: &str = "usage limit reached";

fn generic_limit(item: &str) -> bool {
    let words = item.trim_start_matches(|c: char| !c.is_alphanumeric());
    strip_ci(words, GENERIC_LIMIT).is_some()
}

fn reset_clock_words(reset: &str) -> String {
    let cut = reset.find('(').unwrap_or(reset.len());
    crate::text::trim(&reset[..cut]).to_ascii_lowercase()
}

/// The wall a generic banner stands for: `found` is the notice the reader placed (`crate::phase::notice`'s placement: the footer, a banner) and, where it is the generic `Usage limit reached` ([`generic_limit`]), the kind is the turn's own notice's — the newest limit notice above the composer that names its limit, read as [`since_limit`] reads one (each row as the line it opens, across the rows Ink wrapped it onto, on the transcript's rows, each up to the side panel's rule and wrapped in its room: [`Transcript`], which the caller holds for `rows`), up through local commands (`❯ /rate-limit-options`, which Claude Code types itself to open its dialog) but no person's prompt, and naming the same reset as `found` (the zone aside): the notice of this limit, never an older one a `/resume`d history redraws.
pub(crate) fn named_by_the_turn(rows: &[String], transcript: &Transcript<'_>, found: Wall) -> Wall {
    if !generic_limit(&found.message) {
        return found;
    }
    let Some(said) = found.reset.as_deref().map(reset_clock_words) else {
        return found;
    };
    let Some(frame) = crate::phase::composer_frame(rows) else {
        return found;
    };
    let width = transcript.room(crate::text::trim_end(&rows[frame.bottom]).chars().count());
    let top = frame.top.min(found.row);
    let named = turns_notice(&transcript.rows, top, width)
        .and_then(|(kind, notice)| names_the_reset(&notice, kind, &said, found.kind));
    match named {
        Some((kind, words)) => Wall {
            kind,
            message: format!("{words} \u{2014} {}", found.message),
            ..found
        },
        None => found,
    }
}

fn turns_notice(rows: &[String], top: usize, width: usize) -> Option<(WallKind, String)> {
    use crate::phase::line_at;
    for (i, row) in rows[..top].iter().enumerate().rev() {
        let column0 = crate::phase::leading_spaces(row) == 0;
        if column0 && let Some(prompt) = crate::text::trim(row).strip_prefix('❯') {
            let prompt = crate::text::trim(prompt);
            if prompt.is_empty() || prompt.starts_with('/') {
                continue;
            }
            return None;
        }
        let (_, line) = line_at(rows, i, top, width);
        let Some((kind, item)) = gapped_items(&line)
            .find_map(|item| {
                classify_wall(item)
                    .filter(|k| {
                        k.reads_limited()
                            && !generic_limit(item)
                            && crate::phase::notice_reset(item).is_some()
                    })
                    .map(|k| (k, item))
            })
            .filter(|_| !crate::phase::programs_words(rows, i))
        else {
            continue;
        };
        if crate::phase::under_a_call(rows, i) {
            return None;
        }
        return Some((kind, item.to_string()));
    }
    None
}

fn names_the_reset(
    item: &str,
    kind: WallKind,
    said: &str,
    generic: WallKind,
) -> Option<(WallKind, String)> {
    let same = crate::phase::notice_reset(item).is_some_and(|r| reset_clock_words(&r) == said);
    let words = item.trim_start_matches(|c: char| !c.is_alphanumeric());
    (same && kind != generic).then(|| (kind, words.to_string()))
}

/// The limit a cancel left on the screen: the vendor's own word that its auto-continue will not go on ([`auto_continue_end`] `Unkept`: Esc on its countdown, `⏺ Automatic continue cancelled · /rate-limit-options to re-arm`) drawn as the last thing said, its `⏺` row in column 0, right under the screen's newest usage limit notice ([`since_limit`]: the first thing said above the cancel is that notice's block, or its `/goal` echo) — a notice that names its reset and that no turn has been served since — read as the wall that notice was before the cancel took it off the screen's end: its kind, its words, its reset, the row it opens on ([`Placement::ErrorRow`] for the vendor's `⏺` row, its countdown's `Usage limit reached · continuing automatically at …`; [`Placement::Gutter`] for a `⎿` notice), never under a call. Each row over the composer is read up to the side panel's rule and wrapped in its room ([`Transcript`]).
#[must_use]
pub fn left_wall(rows: &[String]) -> Option<Wall> {
    use crate::phase::{is_said, last_said_in, leading_spaces, line_at};
    let transcript = Transcript::of(rows);
    if auto_continue_end_on(rows, &transcript)? != AutoContinue::Unkept {
        return None;
    }
    let frame = crate::phase::composer_frame(rows)?;
    let width = transcript.room(crate::text::trim_end(&rows[frame.bottom]).chars().count());
    let last = last_said_in(rows, &transcript)?;
    let newest = newest_limit(rows, &transcript)?;
    let rows: &[String] = &transcript.rows;
    let cancel = (0..=last)
        .rev()
        .find(|&i| rows[i].starts_with(['⏺', '●']))?;
    let (end, line) = line_at(rows, cancel, frame.top, width);
    let said = crate::text::trim(&line).trim_start_matches(['⏺', '●']);
    if end < last || auto_continue_said(said) != Some(AutoContinue::Unkept) {
        return None;
    }
    if newest.since.served || newest.row >= cancel {
        return None;
    }
    let reset = newest.since.reset?;
    let block_end = match newest.echo {
        Some(echo) => line_at(rows, echo, frame.top, width).0,
        None => {
            (newest.row + 1..cancel)
                .find(|&i| crate::text::trim(&rows[i]).is_empty() || leading_spaces(&rows[i]) == 0)
                .unwrap_or(cancel)
                - 1
        }
    };
    let opener = |i: usize| {
        (0..=i)
            .rev()
            .find(|&j| leading_spaces(&rows[j]) == 0 || crate::text::trim(&rows[j]).is_empty())
    };
    let said_above = |from: usize| {
        let mut i = from;
        loop {
            i = i.checked_sub(1)?;
            if !is_said(&rows[i], Some(width)) {
                continue;
            }
            match opener(i) {
                Some(j)
                    if j < i
                        && !crate::text::trim(&rows[j]).is_empty()
                        && !is_said(&rows[j], Some(width)) =>
                {
                    i = j;
                }
                _ => break Some(i),
            }
        }
    };
    let mut above = said_above(cancel)?;
    while let Some(dialog) = limit_dialog_at(rows, above) {
        above = said_above(dialog)?;
    }
    if !(newest.row..=block_end).contains(&above) {
        return None;
    }
    let placement = match crate::text::trim_start(&rows[newest.row]).chars().next() {
        Some('⎿') => Placement::Gutter,
        _ => Placement::ErrorRow,
    };
    Some(Wall {
        kind: newest.since.kind,
        message: newest.message,
        reset: Some(reset),
        row: newest.row,
        placement,
        under_call: false,
    })
}

fn limit_dialog_at(rows: &[String], i: usize) -> Option<usize> {
    let j = (0..=i).rev().find(|&j| {
        crate::text::trim(&rows[j]).is_empty() || crate::phase::leading_spaces(&rows[j]) == 0
    })?;
    let command = crate::text::trim(&rows[j])
        .strip_prefix('❯')
        .map(crate::text::trim);
    (command == Some(crate::anchor!("wall.limit_dialog"))).then_some(j)
}

fn gapped_items(row: &str) -> impl Iterator<Item = &str> {
    crate::text::trim(row)
        .split("  ")
        .map(crate::text::trim)
        .filter(|item| !item.is_empty())
}

fn auto_continue_said(item: &str) -> Option<AutoContinue> {
    let text = item
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .replace('\u{2219}', "\u{b7}");
    let opens = |words: &str| {
        [text.as_str(), strip_ci(&text, "your ").unwrap_or("")]
            .iter()
            .any(|t| strip_ci(t, words).is_some())
    };
    if opens(crate::anchor!("wall.auto_continue.fired"))
        || opens(crate::anchor!("wall.auto_continue.now"))
    {
        Some(AutoContinue::Fired)
    } else if [
        crate::anchor!("wall.auto_continue.cancelled"),
        crate::anchor!("wall.auto_continue.stopped"),
        crate::anchor!("wall.auto_continue.off"),
        crate::anchor!("wall.auto_continue.stale"),
    ]
    .iter()
    .any(|words| opens(words))
    {
        Some(AutoContinue::Unkept)
    } else if opens(crate::anchor!("wall.auto_continue"))
        || strip_ci(&text, "continuing shortly").is_some()
    {
        // The countdown's own row opens with its words; the transcript's
        // `Usage limit reached · continuing automatically at …` is the
        // limit's notice, and the walk stops there.
        Some(AutoContinue::Armed)
    } else {
        None
    }
}

fn strip_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

const MEMORY_ROWS: usize = 6;

/// The fact half of a [`memory_wall`]'s message — `Critical memory usage (140.4GB)` — without the vendor's remedy tail after its ` — ` (`restart and resume with claude --continue`); the whole message when it has no such tail.
#[must_use]
pub fn memory_banner_head(message: &str) -> &str {
    message
        .split_once(" \u{2014} ")
        .map_or(message, |(head, _)| head)
        .trim_end()
}

/// Claude Code's critical-memory banner, as a [`WallKind::Memory`] wall, its rows over the
/// composer read up to the side panel's rule and against its room ([`crate::phase::Transcript`]),
/// the right edge the banner stands against beside the panel.
#[must_use]
pub fn memory_wall(rows: &[String]) -> Option<Wall> {
    use crate::phase::{HINT_COLUMN, composer_frame, is_against_right_edge, status_block};
    let anchor = const { crate::anchor!("wall.memory") };
    let frame = composer_frame(rows)?;
    let transcript = Transcript::of(rows);
    let width = transcript.room(crate::text::trim_end(&rows[frame.bottom]).chars().count());
    let block = status_block(rows, frame.top);
    let from = block
        .status
        .unwrap_or(block.from)
        .max(frame.top.saturating_sub(MEMORY_ROWS));
    let said: &[String] = &transcript.rows;
    (from..frame.top).rev().find_map(|i| {
        let row = crate::text::trim_end(&said[i]);
        if row.chars().count() + 3 < width {
            return None;
        }
        let (column, segment) = right_segment(row);
        let live = block
            .status
            .is_some_and(|s| s < i && !in_queued_message(said, s, i));
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
                under_call: false,
            })
    })
}

/// Whether row `i`, under the status row `status`, belongs to a message a person queued in the live zone — `❯ …`, then its wrapped rows indented under the caret's text: the nearest row at or above `i` and under the status row that is blank or starts in column 0 is a `❯` row.
pub(crate) fn in_queued_message(rows: &[String], status: usize, i: usize) -> bool {
    (status + 1..=i)
        .rev()
        .map(|j| rows[j].as_str())
        .find(|row| crate::text::trim(row).is_empty() || !row.starts_with(char::is_whitespace))
        .is_some_and(|row| row.starts_with('❯'))
}

/// A row's right segment and the column it starts at: the text after its last run of two or more spaces, or the whole row (its indent skipped) when it has none.
pub(crate) fn right_segment(row: &str) -> (usize, &str) {
    match row.rfind("  ") {
        Some(gap) => (row[..gap + 2].chars().count(), &row[gap + 2..]),
        None => {
            let t = crate::text::trim_start(row);
            (row.chars().count() - t.chars().count(), t)
        }
    }
}

#[cfg(test)]
#[path = "wall_tests.rs"]
mod tests;
