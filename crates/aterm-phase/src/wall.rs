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
//! **Claude Code 2.1.283 draws an API error as a message of its own** — the
//! `⏺` in column 0, the text from column 2 (its `lu()`), never under `⎿` —
//! so a turn that ended on `API Error: Can't reach the API server — check
//! your internet or DNS (ENOTFOUND)` or on a 529 left no gutter row, no wall
//! was read, and the supervisor continued it at once, sixteen times in the
//! outage of 2026-09-27 (and `⏺ Login expired · Please run /login`, drawn
//! the same way, ended every turn for nine hours the same day). That `⏺` row
//! is a place of its own ([`Placement::ErrorRow`], `phase::error_row_notice`),
//! read only when it is the last thing said, one paragraph, and in the
//! vendor's own shape — `API Error` at its head, or a notice and its remedy
//! joined by ` · ` — never any last `⏺` row, whose prose names a wall in its
//! first four words as often as not. What an API error says went wrong on
//! the way is its [`ApiCause`].
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
    /// 401 Invalid API key · Please run /login`, `Login expired · Please run
    /// /login` — the row Claude Code 2.1.281 writes as its `<synthetic>`
    /// `authentication_failed` answer and draws as `⏺ Login expired · …`,
    /// [`Placement::ErrorRow`]): a human's browser moves it, and until it
    /// does every turn ends on it in milliseconds, whatever was typed. What
    /// lifts it on the screen is [`login_restored`].
    Auth,
    /// Any other `API Error: …` the turn ended on. `code` is the HTTP status
    /// when the notice prints one; `retryable` is the vendor's own rule — 408,
    /// 409, 429 and every 5xx retry, and a notice with no status retries
    /// unless it names a TLS or proxy refusal. `cause` says what went wrong
    /// on the way ([`ApiCause`]), which decides what answers it. `API Error:
    /// Rate limit reached for requests` is `code: Some(429)`: that is the
    /// status the vendor maps to `rate_limit`.
    ApiError {
        code: Option<u16>,
        retryable: bool,
        cause: ApiCause,
    },
    /// The service is overloaded (`API Error: 529 Overloaded. This is a
    /// server-side issue, usually temporary — try again in a moment.`,
    /// `Repeated 529 Overloaded errors`, `Opus is experiencing high load`).
    Overloaded,
    /// Claude Code's critical-memory banner: the vendor's own word that its
    /// process is past saving and must be restarted, then resumed on its own
    /// conversation (`claude --resume <id>`, never the banner's own `claude
    /// --continue`: [`memory_banner_head`]). Not a turn's end at all — measured
    /// 2026-09-24 on a worker whose spinner still ran 36 minutes into a turn
    /// while it read no input for 2h41m — so it is read by position
    /// ([`memory_wall`]), and the reader keeps it under a hard busy, where it
    /// drops every other wall. Never waited out, retried or typed at.
    Memory,
}

/// What an API error says went wrong on the way, named by what answers it
/// — Claude Code 2.1.283's connection catalog (its `Kne`, and the stream's
/// own endings), read from the binary on 2026-09-27, the day an outage of an
/// hour turned every hosted session's turn into `API Error: Can't reach the
/// API server — check your internet or DNS (ENOTFOUND)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiCause {
    /// The API answered with a failure: a status (5xx, 4xx), or words the
    /// catalog does not name. Time answers it.
    Server,
    /// The API was never reached: the name did not resolve, no route, the
    /// connection refused, dropped or timed out, no response, the computer
    /// asleep. The network coming back answers it.
    Unreachable,
    /// The reply was cut off mid-stream, or before it began (`… The response
    /// above may be incomplete.`, `… Try again.`). Trying again answers it.
    CutOff,
    /// The connection's TLS, certificate or proxy tunnel was refused. The
    /// vendor does not retry it (`retryable` is false: its own rule), and
    /// what fixes it for good is usually a person's (a CA bundle, a proxy's
    /// credentials) — but a captive portal or an intercepting proxy can
    /// clear by itself, so a supervisor tries it again on its network ladder
    /// and does not ask (aterm-agent's turn-end policy). A handshake that
    /// another client verified is no evidence it has cleared: the agent
    /// verifies against its own trust store.
    Config,
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
    /// The vendor's error row: an `isApiErrorMessage` row Claude Code 2.1.281
    /// draws with its `⏺` bullet in column 0, as the last thing said (`⏺
    /// Login expired · Please run /login`; `crate::phase`'s
    /// `error_row_notice`).
    ErrorRow,
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
        // The exact `Request timed out` is drawn under `⎿` with no prefix:
        // the whole row, never a tool's line that begins so.
        None if api || lower.trim() == "request timed out" => api_error(head, &lower),
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

/// `api error: <status>? <words>` → the wall it is: the code it prints (a
/// 529 is [`WallKind::Overloaded`] before this is asked; 2.1.283's `Request
/// rejected (429)` prints it in parentheses), a full context (`The model has
/// reached its context window limit.`), or what it says went wrong
/// ([`ApiCause`]) and whether the vendor retries it. The status is read from
/// the notice's `head`; everything else from the WHOLE notice, `full`
/// (lowercased): `Unable to connect to API. Check your internet connection`
/// says what it is after its first sentence, and a wrapped message after
/// its first row. The tables are asked in order — a certificate or proxy
/// refusal, then a reply cut off, then a network never reached — so `Server
/// error mid-response` is cut off and `Connection refused — a firewall or
/// proxy may be blocking it` is unreachable (a bare `proxy` names no
/// refusal). `None` for an API error with no status that none of them names
/// (a safeguards refusal, a model that is not found, an effort it does not
/// take): no wall, so the ordinary policy answers it, as it did before
/// 2.1.283 drew one where it could be read.
fn api_error(head: &str, full: &str) -> Option<WallKind> {
    let rest = head
        .strip_prefix("api error")
        .unwrap_or(head)
        .trim_start_matches([':', ' ']);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let code = if digits.len() == 3 {
        digits.parse::<u16>().ok()
    } else if rest.starts_with("rate limit") {
        Some(429)
    } else {
        status_in_parens(rest)
    };
    if code == Some(529) {
        return Some(WallKind::Overloaded);
    }
    if full.contains("context window limit") {
        return Some(WallKind::Context);
    }
    let names = |words: &[&str]| words.iter().any(|w| full.contains(w));
    let cause = if code.is_some() {
        ApiCause::Server
    } else if names(CONFIG_WORDS) {
        ApiCause::Config
    } else if names(CUT_OFF_WORDS) {
        ApiCause::CutOff
    } else if names(UNREACHABLE_WORDS) {
        ApiCause::Unreachable
    } else if full.contains("server error") {
        ApiCause::Server
    } else {
        return None;
    };
    let retryable = match (cause, code) {
        (ApiCause::Config, _) => false,
        (ApiCause::Unreachable | ApiCause::CutOff, _) => true,
        (ApiCause::Server, Some(c)) => matches!(c, 408 | 409 | 429) || c >= 500,
        (ApiCause::Server, None) => true,
    };
    Some(WallKind::ApiError {
        code,
        retryable,
        cause,
    })
}

/// A 4xx or 5xx status printed in parentheses (`request rejected (429)`).
fn status_in_parens(rest: &str) -> Option<u16> {
    rest.match_indices('(').find_map(|(at, _)| {
        let inner = rest.get(at + 1..at + 5)?;
        let code = inner.strip_suffix(')')?.parse::<u16>().ok()?;
        (400..600).contains(&code).then_some(code)
    })
}

/// A certificate or proxy-tunnel refusal: `Unable to connect to API: SSL
/// certificate verification failed` (… has expired, … has been revoked, …
/// hostname mismatch, … is not yet valid), `Self-signed certificate
/// detected`, `(<code>). The certificate comes from an authority Claude Code
/// doesn't trust …`, `Couldn't connect through your proxy
/// (ERR_PROXY_TUNNEL)`, and Node's certificate codes. Not a bare `SSL error
/// (<code>)`: 2.1.283 names a handshake that timed out or a record that
/// broke so (`ERR_TLS_HANDSHAKE_TIMEOUT`, `ERR_SSL_WRONG_VERSION_NUMBER`),
/// and those pass with the network — it reads unreachable, by its `unable
/// to connect`.
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

/// A reply cut off mid-stream or before it began: `The response stopped
/// arriving.`, `Server error mid-response.`, `The response stream was
/// malformed.`, `Part of the response never arrived.`, `Your computer went
/// to sleep mid-response.`, `Connection lost mid-response.` (each `… The
/// response above may be incomplete.`), `The response stalled before a
/// response was produced. Try again.` and its kin.
///
/// So is a reply past its output token maximum (`Claude's response exceeded
/// the N output token maximum.`) and one whose image was dropped (`… in the
/// conversation could not be processed and was removed.`).
const CUT_OFF_WORDS: &[&str] = &[
    "mid-response",
    "output token maximum",
    "could not be processed and was removed",
    "may be incomplete",
    "stopped arriving",
    "never arrived",
    "malformed",
    "stalled",
    "before a response was produced",
];

/// A network never reached: `Can't reach the API server — check your
/// internet or DNS (ENOTFOUND)`, `No internet route — check your connection
/// or VPN (ENETUNREACH)`, `Unable to connect to API. Check your internet
/// connection`, `Unable to connect to API (<code>)`, `Connection refused — a
/// firewall or proxy may be blocking it`, `Connection dropped (ECONNRESET)`,
/// `Request timed out. Check your internet connection and proxy settings`,
/// `No response from API`, `Connection lost while your computer was
/// asleep`, `Connection closed before the response finished`.
const UNREACHABLE_WORDS: &[&str] = &[
    "can't reach",
    "no internet route",
    "unable to connect",
    "connection refused",
    "connection dropped",
    "connection lost",
    "connection closed",
    "timed out",
    "no response from api",
    "asleep",
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

/// Whether the screen says THE LOGIN IS BACK: the last thing said is the
/// vendor's own word that a `/login` finished — `⎿  Login successful`, or
/// `⎿  Login successful · account or organization changed` — hanging from
/// the `❯ /login` a person typed (what Claude Code writes to the transcript
/// as `<local-command-stdout>Login successful</local-command-stdout>`,
/// measured 2026-09-27 at 14:33:36 UTC in the incident's session). It is what
/// lifts a [`WallKind::Auth`] wall on the screen: the wall's row is no longer
/// the last thing said, and this says why. A `/login` that did NOT finish —
/// its dialog dismissed — leaves no such row, and a login finished in ANOTHER
/// tab says nothing here: the wall's row stays the last thing said until
/// something is typed into this one.
#[must_use]
pub fn login_restored(rows: &[String]) -> bool {
    let Some(last) = crate::phase::last_said_index(rows) else {
        return false;
    };
    let restored = rows[last]
        .trim_start()
        .strip_prefix('⎿')
        .is_some_and(|out| out.trim_start().starts_with("Login successful"));
    restored
        && rows[..last]
            .iter()
            .rev()
            .find(|r| !r.trim().is_empty() && crate::phase::leading_spaces(r) == 0)
            .is_some_and(|r| r.trim_end() == "❯ /login")
}

/// How far above the composer's top rule [`memory_wall`] looks: the live
/// zone's parked rows (a spinner, its `⎿  Tip:` or todo rows, the context
/// indicator, a hint), never the transcript above them.
const MEMORY_ROWS: usize = 6;

/// The FACT half of a [`memory_wall`]'s message — `Critical memory usage
/// (140.4GB)` — without the vendor's remedy tail after its ` — `
/// (`restart and resume with claude --continue`); the whole message when it
/// has no such tail.
///
/// WHY (robustness backlog item 2, 2026-09-26): `claude --continue` resumes
/// the NEWEST conversation filed under the working directory, and four live
/// Claude Code processes shared one directory on the owner's Mac that day —
/// a person following the banner's words in one tab resumes a sibling's
/// conversation. aterm names the tab's own (`aterm_agent::harness::resume`),
/// so whatever of the banner it quotes stops at the fact.
#[must_use]
pub fn memory_banner_head(message: &str) -> &str {
    message
        .split_once(" \u{2014} ")
        .map_or(message, |(head, _)| head)
        .trim_end()
}

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
                    cause: ApiCause::Server,
                }),
            ),
            (
                "API Error: 500 Internal server error",
                Some(WallKind::ApiError {
                    code: Some(500),
                    retryable: true,
                    cause: ApiCause::Server,
                }),
            ),
            (
                "API Error: 400 due to tool use concurrency issues.",
                Some(WallKind::ApiError {
                    code: Some(400),
                    retryable: false,
                    cause: ApiCause::Server,
                }),
            ),
            (
                "API Error: Server error mid-response. The response above may be incomplete.",
                Some(WallKind::ApiError {
                    code: None,
                    retryable: true,
                    cause: ApiCause::CutOff,
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
        API_ERROR_529, API_ERROR_ENOTFOUND, API_ERROR_ENOTFOUND_80,
        API_ERROR_ENOTFOUND_80_MEASURED, API_ERROR_ENOTFOUND_MEASURED,
        API_ERROR_QUEUED_SENT_MEASURED, API_ERROR_SLEEP, API_ERROR_TWICE_MEASURED, END_529,
        END_OFFER, END_SESSION_LIMIT, IDLE_AFTER_LIMIT_AND_MODEL_SWITCH, composer, rows, screen,
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

    /// THE LOGIN WALL OF 2026-09-27 (tab `s-b5cf2faabac5ce5127bd`): Claude
    /// Code 2.1.281 answered every turn for nine hours with its synthetic
    /// `authentication_failed` row, drawn `⏺ Login expired · Please run
    /// /login` in column 0 — and main's reader read that screen idle with NO
    /// wall (the same words under the `⎿` gutter it read as `auth`), so the
    /// supervisor continued it and the upgrade announced into it. Read now
    /// as the auth wall, from the vendor's error row: alone, over a done
    /// row, wrapped by a narrow window, and in its other measured wording;
    /// an `API Error` in the same form is its own kind. The screen stays
    /// idle for every other reader (`worker_phase`, `limit_notice`).
    #[test]
    fn the_login_expired_error_row_is_the_auth_wall() {
        use crate::prompt::fixtures::LOGIN_EXPIRED;
        let r = screen(LOGIN_EXPIRED);
        let w = wall(&r).expect("the login wall");
        assert_eq!(w.kind, WallKind::Auth);
        assert_eq!(w.placement, Placement::ErrorRow);
        assert_eq!(w.message, "Login expired · Please run /login");
        assert_eq!(w.reset, None);
        assert_eq!(r[w.row], "⏺ Login expired · Please run /login");
        assert_eq!(worker_phase(&r), Phase::Idle);
        assert_eq!(limit_notice(&r), None);
        let reading = crate::reader::read(Some("claude"), &r, None);
        assert_eq!(reading.phase, Phase::Idle);
        assert!(reading.phase_authoritative);
        assert_eq!(reading.wall.map(|w| w.kind), Some(WallKind::Auth));
        assert!(!login_restored(&r));

        let at = w.row;
        let with = |tail: &[&str]| {
            let mut v = r.clone();
            v.splice(at..=at, tail.iter().map(|s| (*s).to_string()));
            v
        };
        for (name, v, kind) in [
            (
                "over a done row",
                with(&[
                    "⏺ Login expired · Please run /login",
                    "",
                    "✻ Worked for 0s · done 5:00 AM",
                ]),
                WallKind::Auth,
            ),
            (
                "wrapped",
                with(&["⏺ Login expired · Please run", "  /login"]),
                WallKind::Auth,
            ),
            (
                "the profile wording",
                with(&[
                    "⏺ Login expired · Run /login to sign in again, or re-authenticate your \
                     Anthropic profile",
                ]),
                WallKind::Auth,
            ),
            (
                "an API error in the same form",
                with(&[&format!("⏺ {API_529}")]),
                WallKind::Overloaded,
            ),
        ] {
            let w = wall(&v).unwrap_or_else(|| panic!("{name}"));
            assert_eq!((w.kind, w.placement), (kind, Placement::ErrorRow), "{name}");
        }
    }

    /// NEGATIVE CONTROLS for the error row: the worker's own `⏺` words
    /// (a sentence that names a login with no remedy joined on, one that
    /// quotes the notice deep in it, a block longer than a notice), a tool
    /// call that echoes it, a tool's output that prints it, and the row made
    /// history — the person's `/login` finished under it (which
    /// [`login_restored`] reads), or the worker answering after it — are no
    /// wall. `login_restored` answers only the vendor's own `Login successful`
    /// under the `❯ /login` a person typed: not an interrupted login, not the
    /// words in a tool's output.
    #[test]
    fn the_workers_words_and_a_finished_login_are_no_login_wall() {
        use crate::prompt::fixtures::LOGIN_EXPIRED;
        let r = screen(LOGIN_EXPIRED);
        let at = wall(&r).expect("the control").row;
        let with = |tail: &[&str]| {
            let mut v = r.clone();
            v.splice(at..=at, tail.iter().map(|s| (*s).to_string()));
            v
        };
        let login_back = with(&[
            "⏺ Login expired · Please run /login",
            "",
            "❯ /login",
            "  ⎿  Login successful",
        ]);
        assert_eq!(wall(&login_back), None, "the login is back");
        assert!(login_restored(&login_back));
        assert!(login_restored(&with(&[
            "⏺ Login expired · Please run /login",
            "",
            "❯ /login",
            "  ⎿  Login successful · account or organization changed",
        ])));
        for (name, v) in [
            (
                "a sentence with no remedy joined on",
                with(&["⏺ Not logged in to gh, so nothing was pushed."]),
            ),
            (
                "the notice quoted deep in a sentence",
                with(&[
                    "⏺ Pushed the branch; the other tab said Login expired · Please run /login",
                ]),
            ),
            (
                "a block longer than a notice",
                with(&[
                    "⏺ Login expired · Please run /login is what the other tab said,",
                    "  and it said it again after the retry,",
                    "  and a third time after the second,",
                    "  so I stopped there.",
                ]),
            ),
            (
                "a tool call that echoes it",
                with(&["⏺ Bash(echo 'Login expired · Please run /login')"]),
            ),
            (
                "a tool's output",
                with(&[
                    "⏺ Bash(tail -1 peer.log)",
                    "  ⎿  Login expired · Please run /login",
                ]),
            ),
            (
                "answered after it",
                with(&[
                    "⏺ Login expired · Please run /login",
                    "",
                    "❯ continue",
                    "",
                    "⏺ Back at it: the suites are running.",
                ]),
            ),
        ] {
            assert_eq!(wall(&v), None, "{name}");
            assert!(!login_restored(&v), "{name}");
        }
        for (name, v) in [
            (
                "an interrupted login",
                with(&[
                    "⏺ Login expired · Please run /login",
                    "",
                    "❯ /login",
                    "  ⎿  Login interrupted",
                ]),
            ),
            (
                "the words in a tool's output",
                with(&["⏺ Bash(grep -c Login auth.log)", "  ⎿  Login successful"]),
            ),
        ] {
            assert!(!login_restored(&v), "{name}");
        }
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
                retryable: true,
                cause: ApiCause::Server,
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
                retryable: true,
                cause: ApiCause::Server,
            }
            .reads_limited()
        );
    }

    const ENOTFOUND: &str =
        "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";
    const UNREACHABLE: WallKind = WallKind::ApiError {
        code: None,
        retryable: true,
        cause: ApiCause::Unreachable,
    };

    /// THE OUTAGE OF 2026-09-27: Claude Code 2.1.283 draws `API Error: …` as
    /// a `⏺` message of its own, and the supervisor, which read walls only in
    /// the footer, a banner and the `⎿` gutter, typed `keep going` into it
    /// sixteen times. It is a wall — at 144 columns, at 80 where
    /// `(ENOTFOUND)` wraps onto the second row, without a done row, with the
    /// `●` off macOS, and under the vendor's expand hint — and the message is
    /// the whole of it, joined. The SYNTHETIC rows were MEASURED live on
    /// 2026-09-27 word for word, at both widths and under an earlier error.
    #[test]
    fn a_283_api_error_message_row_is_a_wall_at_every_width() {
        for (name, r) in [
            ("144", screen(API_ERROR_ENOTFOUND)),
            ("80", screen(API_ERROR_ENOTFOUND_80)),
            ("144 measured", screen(API_ERROR_ENOTFOUND_MEASURED)),
            ("80 measured", screen(API_ERROR_ENOTFOUND_80_MEASURED)),
            ("two errors measured", screen(API_ERROR_TWICE_MEASURED)),
        ] {
            assert_eq!(worker_phase(&r), Phase::Idle, "{name}");
            let w = wall(&r).unwrap_or_else(|| panic!("{name}: the wall"));
            assert_eq!(w.kind, UNREACHABLE, "{name}");
            assert_eq!(w.placement, Placement::ErrorRow, "{name}");
            assert_eq!(w.message, ENOTFOUND, "{name}");
            assert!(r[w.row].starts_with("⏺ API Error"), "{name}");
        }
        // Two errors on screen (a queued message was sent under the first):
        // the wall is the LAST one, the turn's end.
        let twice = screen(API_ERROR_TWICE_MEASURED);
        let last = twice
            .iter()
            .rposition(|r| r.starts_with("⏺ API Error"))
            .expect("the second error");
        assert_eq!(wall(&twice).map(|w| w.row), Some(last));
        assert!(twice[..last].iter().any(|r| r.starts_with("⏺ API Error")));
        // NEGATIVE CONTROL, measured: the retries spent with a message
        // queued, which Claude sends under the error at once — the error is
        // history, the next turn retrying: busy, no wall.
        let sent = screen(API_ERROR_QUEUED_SENT_MEASURED);
        assert!(sent.iter().any(|r| r.starts_with("⏺ API Error")));
        assert_eq!(worker_phase(&sent), Phase::Busy);
        assert_eq!(wall(&sent), None);
        let variants = [
            framed(&["⏺ Working.", "", &format!("⏺ {ENOTFOUND}"), ""], FOOTER),
            framed(&["● Working.", "", &format!("● {ENOTFOUND}"), ""], FOOTER),
            framed(
                &[
                    "⏺ Working.",
                    "",
                    &format!("⏺ {ENOTFOUND}"),
                    "  (ctrl+o to expand)",
                    "",
                ],
                FOOTER,
            ),
        ];
        for r in &variants {
            let w = wall(r).expect("the wall");
            assert_eq!((w.kind, w.message.as_str()), (UNREACHABLE, ENOTFOUND));
        }
    }

    /// The same shape carries every other API error: a 529 (its URL wrapped
    /// onto the second row) is an overload, and a reply the Mac's sleep cut
    /// off is cut off.
    #[test]
    fn the_283_529_and_sleep_rows_read_their_kinds() {
        let w = wall(&screen(API_ERROR_529)).expect("the 529");
        assert_eq!(
            (w.kind, w.placement),
            (WallKind::Overloaded, Placement::ErrorRow)
        );
        assert_eq!(w.message, API_529);
        let w = wall(&screen(API_ERROR_SLEEP)).expect("the sleep");
        assert_eq!(
            w.kind,
            WallKind::ApiError {
                code: None,
                retryable: true,
                cause: ApiCause::CutOff,
            }
        );
    }

    /// NEGATIVE CONTROLS for the message row. The table names a wall within
    /// a notice's first four words, so a worker's prose that opens so would
    /// read as one if any last `⏺` row were offered: the literal vendor
    /// prefix is required. And the error is history once anything is said
    /// under it — a person's `keep going`, the worker's next words, a tool
    /// call — or when it is not the whole paragraph, not in column 0, or a
    /// person's own row. A `⎿` notice under it is still read, by the
    /// gutter; `Request was aborted.` is still no wall.
    #[test]
    fn the_workers_prose_and_history_are_not_a_message_wall() {
        let error = format!("⏺ {ENOTFOUND}");
        let none: Vec<(&str, Vec<String>)> = vec![
            (
                "overloaded prose",
                framed(
                    &["⏺ The server was overloaded, so I retried the push.", ""],
                    FOOTER,
                ),
            ),
            (
                "high load prose",
                framed(
                    &["⏺ Fixed the high load path in the scheduler.", ""],
                    FOOTER,
                ),
            ),
            (
                "login prose",
                framed(&["⏺ Please run /login when you are back.", ""], FOOTER),
            ),
            (
                "context prose",
                framed(
                    &["⏺ Context limit reached in the fixture was the bug.", ""],
                    FOOTER,
                ),
            ),
            (
                "lowercase",
                framed(&["⏺ api error: can't reach the api server", ""], FOOTER),
            ),
            ("user row", framed(&[&format!("❯ {ENOTFOUND}"), ""], FOOTER)),
            (
                "indented",
                framed(
                    &["⏺ Bash(cat log)", &format!("  ⏺ {ENOTFOUND}"), ""],
                    FOOTER,
                ),
            ),
            (
                "keep going after it",
                framed(&[&error, "", "❯ keep going", ""], FOOTER),
            ),
            (
                "later words",
                framed(&[&error, "", "⏺ Back online; carrying on.", ""], FOOTER),
            ),
            (
                "a tool call after it",
                framed(
                    &[&error, "", "⏺ Bash(git status)", "  ⎿  clean", ""],
                    FOOTER,
                ),
            ),
            (
                "two paragraphs",
                framed(
                    &[
                        "⏺ API Error: 529 Overloaded is what CI hit last night.",
                        "",
                        "  I re-ran it and it passed.",
                        "",
                    ],
                    FOOTER,
                ),
            ),
            (
                "aborted",
                framed(&["⏺ API Error: Request was aborted.", ""], FOOTER),
            ),
        ];
        for (name, r) in none {
            assert_eq!(wall(&r), None, "{name}");
        }
        let under = framed(
            &[
                &error,
                "  ⎿  Context limit reached · /compact or /clear to continue",
                "",
            ],
            FOOTER,
        );
        let w = wall(&under).expect("the gutter's notice");
        assert_eq!(
            (w.kind, w.placement),
            (WallKind::Context, Placement::Gutter)
        );
    }

    /// Claude Code 2.1.283's whole connection catalog, each text as it
    /// follows `API Error: `, lands on its cause: a network never reached, a
    /// reply cut off, a TLS or proxy refusal (never retried) — with the
    /// traps: a refused connection that NAMES a proxy, and a timeout that
    /// names proxy settings, are unreachable; `Unable to connect to API.
    /// Check your internet connection` says so only after its first
    /// sentence. A status keeps its server cause.
    #[test]
    fn every_api_error_family_of_the_283_catalog_classifies() {
        let unreachable = [
            "Can't reach the API server — check your internet or DNS (ENOTFOUND)",
            "Can't reach the API server — check your internet or DNS (EAI_AGAIN)",
            "Can't reach the API server — check your internet or DNS (FailedToOpenSocket)",
            "No internet route — check your connection or VPN (ENETUNREACH)",
            "No internet route — check your connection or VPN (EHOSTDOWN)",
            "Unable to connect to API. Check your internet connection",
            "Unable to connect to API (ECONNABORTED)",
            "Connection refused — a firewall or proxy may be blocking it (ECONNREFUSED)",
            "Connection dropped (ECONNRESET)",
            "Connection dropped (UND_ERR_SOCKET)",
            "Request timed out. Check your internet connection and proxy settings",
            "No response from API",
            "Connection lost while your computer was asleep",
            "Connection closed before the response finished",
            "Unable to connect to API: SSL error (ERR_TLS_HANDSHAKE_TIMEOUT)",
            "Unable to connect to API: SSL error (ERR_SSL_WRONG_VERSION_NUMBER)",
        ];
        let cut_off = [
            "The response stopped arriving. The response above may be incomplete.",
            "Server error mid-response. The response above may be incomplete.",
            "The response stream was malformed. The response above may be incomplete.",
            "Part of the response never arrived. The response above may be incomplete.",
            "Your computer went to sleep mid-response. The response above may be incomplete.",
            "Connection lost mid-response. The response above may be incomplete.",
            "The response stalled before a response was produced. Try again.",
            "The response stream was malformed and no response was produced. Try again.",
            "Part of the response never arrived and no response was produced. Try again.",
            "Your computer went to sleep before a response was produced. Try again.",
            "Connection lost before a response was produced. Try again.",
            "Claude's response exceeded the 32000 output token maximum. To configure this behavior, set the CLAUDE_CODE_MAX_OUTPUT_TOKENS environment variable.",
            "An image in the conversation could not be processed and was removed. Re-read the file with a different approach if you still need it.",
        ];
        let config = [
            "Unable to connect to API: SSL certificate verification failed",
            "Unable to connect to API: SSL certificate has expired",
            "Unable to connect to API: SSL certificate has been revoked",
            "Unable to connect to API: SSL certificate hostname mismatch",
            "Unable to connect to API: SSL certificate is not yet valid",
            "Unable to connect to API: Self-signed certificate detected",
            "Unable to connect to API (SELF_SIGNED_CERT_IN_CHAIN). The certificate comes from an authority Claude Code doesn't trust",
            "Couldn't connect through your proxy (ERR_PROXY_TUNNEL) — the proxy refused the tunnel: check its credentials and that it allows this host",
        ];
        for (texts, cause, retryable) in [
            (&unreachable[..], ApiCause::Unreachable, true),
            (&cut_off[..], ApiCause::CutOff, true),
            (&config[..], ApiCause::Config, false),
        ] {
            for text in texts {
                let text = format!("API Error: {text}");
                assert_eq!(
                    classify_wall(&text),
                    Some(WallKind::ApiError {
                        code: None,
                        retryable,
                        cause,
                    }),
                    "{text}"
                );
            }
        }
        assert_eq!(
            classify_wall("API Error: 503 Service unavailable. Connection refused upstream"),
            Some(WallKind::ApiError {
                code: Some(503),
                retryable: true,
                cause: ApiCause::Server,
            }),
            "a status is the server's, whatever words follow"
        );
        assert_eq!(
            classify_wall(
                "API Error: Request rejected (429) · this may be a temporary capacity issue."
            ),
            Some(WallKind::ApiError {
                code: Some(429),
                retryable: true,
                cause: ApiCause::Server,
            }),
            "2.1.283's own 429"
        );
        assert_eq!(
            classify_wall("API Error: The model has reached its context window limit."),
            Some(WallKind::Context)
        );
        // What the catalog does not name is no wall: the ordinary policy's.
        for text in [
            "API Error: Claude Opus 5.5 can't help with this. Start a new session to continue.",
            "API Error (claude-x-1): The model claude-x-1 is not available. Run /model to pick another.",
            "API Error: Effort 'max' isn't available for this model.",
        ] {
            assert_eq!(classify_wall(text), None, "{text}");
        }
    }

    /// The one connection failure 2.1.283 draws under `⎿` with no prefix,
    /// `Request timed out`, is unreachable there — and a tool's output that
    /// says it is no wall.
    #[test]
    fn request_timed_out_under_the_gutter_is_unreachable() {
        let r = framed(
            &["⏺ Checking the suite.", "  ⎿  Request timed out", ""],
            FOOTER,
        );
        let w = wall(&r).expect("the wall");
        assert_eq!((w.kind, w.placement), (UNREACHABLE, Placement::Gutter));
        let tool = framed(
            &["⏺ Bash(curl -m1 x)", "  ⎿  Request timed out", ""],
            FOOTER,
        );
        assert_eq!(wall(&tool), None);
        let longer = framed(
            &[
                "⏺ Checking the suite.",
                "  ⎿  Request timed out. Retrying the fetch.",
                "",
            ],
            FOOTER,
        );
        assert_eq!(wall(&longer), None, "only the whole row");
    }
}
