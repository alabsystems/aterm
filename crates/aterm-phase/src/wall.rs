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
//! A `⎿` block that opens the last thing said is read as a vendor row by the
//! OWNER WALK (`phase::block_is_vendors`), measured on Claude Code 2.1.294 in
//! the real-Claude lab:
//! * Claude Code draws its OWN `⎿` rows at column 2 (`  ⎿  …`); a tool's
//!   output continues at column 5, so a `⎿` the output prints — after a blank
//!   row of it or not, behind carriage returns, tabs and cursor moves — is at
//!   column 5 or deeper, and never a vendor row.
//! * The walk goes up from the block over the rows of the blocks above it —
//!   and over a blank row whose next row below stands past column 2 or is a
//!   column-2 `⎿` row, a blank line of a call's output or of its command —
//!   to the first row in column 0. A CALL there (`phase::is_tool_call`: `⏺
//!   Bash(…)`, `⏺ Web Search(…)`, an MCP tool drawn `⏺ <server> - <title>
//!   (MCP)(…)` or `⏺ mcp__srv__tool (MCP)`, an integration's `⏺ Claude in
//!   Chrome[…](…)`, a shell-mode `! cmd`) owns the block, and its FIRST
//!   column-2 row is the call's output — the first line of what it printed,
//!   which quotes anything (a log that says `API Error: 529`, an issue body,
//!   a peer's screen) — and never a wall. A LATER column-2 row under a call
//!   is read: it is where Claude Code draws the wall when the request after
//!   a tool fails (`⏺ Bash(…)` / `⎿  stamped` / `⎿  Context limit reached ·
//!   …`, a PostToolUse hook's row between them or not; measured).
//! * Every other owner's column-2 rows are read as Claude Code's: the
//!   person's `❯` turn (a full context, a failed `/compact`), a hook summary
//!   (`⏺ Ran 1 stop hook (ctrl+o to expand)`), a completion notice (`⏺
//!   Dynamic workflow … completed`, `⏺ Background command … completed`, `⏺
//!   Goal check-in: …`), the worker's message, and a `⏺ Monitor event: …`
//!   row, whose block is its event's payload. So is a block whose owner the
//!   walk does not reach because a blank row (under it a column-2 row that is
//!   no `⎿` row) or the top of the screen comes first: Claude Code's walls
//!   stand so under a prompt's second paragraph, under a collapsed tool group
//!   (`  Read 1 file (ctrl+o to expand)`, its hint wrapped onto a second row
//!   in a narrow pane or not) and under a long output whose call row has
//!   scrolled off (all measured). Not a block that opens with the command's
//!   own echo (`⎿  $ touch x`, how 2.1.280 draws a running Bash call under `⏺
//!   <description>`).
//!
//! **The screen reads for recall; the transcript decides.** The walk reads
//! some of a tool's output as Claude Code's, and no rule of the screen's can
//! tell them apart (measured on 2.1.294): an MCP result of several content
//! blocks is drawn one column-2 row per block, where an MCP result and then a
//! wall look the same; a PreToolUse hook's row is drawn ABOVE the output,
//! which is then the second column-2 row; a long output's call row scrolls
//! off, and so does the first row of an MCP result's first block; a tool
//! whose row this reader does not take for a call (a bare `⏺ <Name>`, a tool
//! drawn with no row at all) has its output read as the vendor's; and the
//! worker's own words can hold a `⎿` row. So the supervisor ACTS on a wall
//! read here — types `/login`, `/compact` or a retry, waits for a reset,
//! moves the session to another account, raises the login-gone alarm — only
//! where the session's Claude Code transcript confirms it: its newest
//! main-chain entry is Claude Code's own notice row of the same kind
//! (aterm-agent, `supervise/wall_confirm.rs`). A wall the transcript does not
//! confirm is journaled and the turn is an ordinary turn end. Where no
//! transcript can be read, a wall read UNDER A CALL ([`Wall::under_call`]: a
//! later column-2 row under any call this walk recognises, an MCP call's, a
//! shell-mode command's and one a hook's row opens included) is not acted on,
//! and any other is acted on as the screen reads it. Main read walls under
//! MCP and `!` calls (it did not know them as calls) and acted on them, so
//! where no transcript can be read this gives up the real walls drawn there —
//! a knowing loss, ruled 2026-10-08 to refuse the MCP and hook spoofs
//! (`supervise/wall_confirm.rs`), the MCP spoofs whose first block holds or
//! ends on a blank line among them (the walk goes on over a blank row inside
//! the output), at the price of the real wall under an output that holds a
//! blank row, read under its call too. An output whose call row scrolled off
//! is read with no owner, and acted on there, as main read it. A heredoc
//! command's output (main read it with no owner, behind the command's blank
//! row) is the first row under its call: no wall.
//!
//! H6-HARNESS-1 (2026-10-08) is why: under Claude Code 2.1.294's MCP calls and
//! `!` commands the old rule ("the vendor's unless the owner is `⏺ Name(…)`")
//! read a tool's output as a wall, and the harness typed `/login` into the
//! session (measured in the real-Claude lab).
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
//! **A notice Ink wrapped is read as one line** (`crate::wrap`). In a pane
//! narrower than a notice Ink word-wraps it onto the rows under it — the
//! footer's `⚠ Usage limit reached · limit` over `resets 10:25am` at 34
//! columns, the turn's `⎿  You've hit your` over `session limit ·` at 26
//! (measured on 2.1.299, the fixtures `claude-2.1.299-*-NNcol.txt`) — and
//! read a row at a time the banner named no reset and the notice was no wall.
//! Every placement reads a notice across the rows its line was wrapped onto
//! (`phase`'s `line_at`); a `⏺` row's text is its message, the rows under it
//! at column two, as it always was.
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
    /// A `/compact` that failed (`Compaction failed · conversation could not
    /// be reduced below the context limit`, `Error during compaction: …`) left
    /// it as full as it was, and its row is the same wall, bar a detail that
    /// names a wall of its own.
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
    /// The lowercase word the CLI prints after `wall:` (`overloaded`).
    #[must_use]
    pub fn name(&self) -> &'static str {
        let wire = self.wire();
        wire.strip_prefix("wall:").unwrap_or(wire)
    }

    /// The whole word `status agent=` publishes for this wall: `wall:` and
    /// [`Self::name`] (`wall:overloaded`). Static because a published
    /// verdict word is.
    #[must_use]
    pub fn wire(&self) -> &'static str {
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
    /// READ UNDER A TOOL'S CALL: a [`Placement::Gutter`] row the owner walk
    /// (`phase::block_is_vendors`) found below the first column-2 row under a
    /// call (`⏺ Bash(…)`, an MCP tool's row, a shell-mode `! cmd`), a hook's
    /// row above the output or not, blank rows of the output between or not —
    /// where Claude Code draws a wall after a tool's output, and where an MCP
    /// result's second block and the output under a PreToolUse hook's row
    /// stand too. Main read no wall under a `⏺ Name(…)` call (but behind a
    /// blank row of its output), but read every row under an MCP call, a `!
    /// cmd` and a `Name[tool](…)` call (its walk did not know them as calls),
    /// so a real wall there was acted on. A supervisor that has no transcript
    /// to confirm a wall by does not act on one read here, those included (a
    /// loss of recall ruled 2026-10-08: aterm-agent,
    /// `supervise/wall_confirm.rs`), and reads the WHOLE screen for it, not
    /// the tail it decides from.
    /// `false` everywhere else: a wall under the person's turn, a notice, the
    /// worker's message, a collapsed group, or no owner the walk reaches, and
    /// every other placement.
    pub under_call: bool,
}

impl Wall {
    /// A sign-in menu drawn INSTEAD of the composer, `rows[row]` its message
    /// ([`WallKind::Auth`]): agy's and Muse Code's, whose options open a
    /// browser and are never read.
    pub(crate) fn sign_in(rows: &[String], row: usize) -> Self {
        Self {
            kind: WallKind::Auth,
            message: crate::text::trim(&rows[row]).to_string(),
            reset: None,
            row,
            placement: Placement::Banner,
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
    /// An account the harness cannot lift from here (seat
    /// type, admin allocation, group cap, org switched off): read as
    /// [`WallKind::Spend`] — waited out on the limit ladder, with a person
    /// told once ([`aterm-agent`'s exception]).
    Blocked,
    /// `Goal paused · the request was rate limited`.
    RateLimit,
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
    // Usage credits carry the session past its limit: the vendor's own list of
    // those notices (the 2.1.300 bundle; the first two and the fourth measured
    // in the lab), and its per-model ones. Work goes on under each, so none is a
    // wall: `crate::credits` reads them, for the record.
    ("you're now using usage credits", Tag::NotAWall),
    ("now using usage credits", Tag::NotAWall),
    ("you're now using your usage allocation", Tag::NotAWall),
    ("now using your usage allocation", Tag::NotAWall),
    ("you're now using extra usage", Tag::NotAWall),
    ("now using extra usage", Tag::NotAWall),
    ("is drawing from usage credits", Tag::NotAWall),
    ("is now using usage credits instead", Tag::NotAWall),
    // The auto-continue lifecycle's notices that are NOT a
    // standing limit (2.1.283/284): the reset came (`Usage limit reset ·
    // continuing automatically`, `Usage limit available again · continuing
    // now`, the stale `(Your) usage limit has reset · press enter to
    // continue`) and the 284 countdown toast (`Continuing automatically at
    // 4pm · esc to cancel`).
    ("usage limit has reset", Tag::NotAWall),
    ("usage limit reset", Tag::NotAWall),
    ("usage limit available again", Tag::NotAWall),
    ("continuing automatically", Tag::NotAWall),
    // The weekly session reset's own texts (READ in the 2.1.287 bundle,
    // never seen on a screen): its answer `Session limit reset · next reset
    // available {date} · …` says the limit is GONE, and its row's label, its
    // footer notice (`/limit-reset to reset your session limit now · …`),
    // its failure (`Couldn't reset your session limit right now · …`) and
    // its pending line (`Resetting your session limit…`) name the limit
    // beside the verb `reset` without being a notice of one. Ahead of
    // `session limit`, which with that verb is a standing session wall.
    ("session limit reset", Tag::NotAWall),
    ("reset your session limit", Tag::NotAWall),
    ("resetting your session limit", Tag::NotAWall),
    ("context limit reached", Tag::Context),
    ("prompt is too long", Tag::Context),
    ("context exceeds", Tag::Context),
    // A `/compact` that FAILS draws a `⎿` row of its own under its `❯ /compact`
    // echo and no done row (2.1.286, MEASURED 2026-10-01): the context is as full
    // as it was, and that row is the full context's wall. Read as no wall, the
    // supervisor took it for a compaction that worked and looped (six `/compact`
    // and six `keep going` in 2 min 42 s, then twelve acts an hour). `Compaction
    // failed · conversation could not be reduced below the context limit` is the
    // vendor's `exhausted`; `Error during compaction: <detail>` wraps any other
    // failure of the request, and `classify_wall` lets a detail that names a
    // wall with its own answer (a lost login, no credit) win before this row is
    // looked up. Not `Not enough messages to compact` (a person's `/compact` on
    // a short chat) and not `Compacted (ctrl+o to see full summary)`: no wall.
    ("compaction failed", Tag::Context),
    ("error during compaction", Tag::Context),
    ("the request was rate limited", Tag::RateLimit),
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
    ("authentication required", Tag::Auth),
    (
        "your organization has disabled api key authentication",
        Tag::Auth,
    ),
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
    ("your seat type doesn't include", Tag::Blocked),
    ("your usage allocation has been disabled", Tag::Blocked),
    ("your group's usage limit is set to", Tag::Blocked),
    ("this service is disabled for your org", Tag::Blocked),
    (
        "your organization has disabled claude subscription access",
        Tag::Blocked,
    ),
    ("extra usage is required for long context", Tag::Blocked),
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
    // Two notices the vendor files under authentication that
    // pass by themselves — `Authentication error · This may be a temporary
    // network issue, please try again` and `Could not refresh your login
    // because another Claude Code process is refreshing it … Try again in a
    // minute` — are an API that was not reached, never a lost login.
    if lower.contains("temporary network issue")
        || lower.contains("another claude code process is refreshing it")
    {
        return Some(WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Unreachable,
        });
    }
    // `Goal cleared after an unrecoverable error
    // (<label>): "<goal>". Run /goal again to continue.` is the goal loop's
    // echo of a cause that ended it; the label says which.
    if let Some(rest) = lower.strip_prefix("goal cleared after an unrecoverable error (") {
        return match rest.split(')').next().unwrap_or_default() {
            "authentication failed" | "organization verification required" => Some(WallKind::Auth),
            "credit balance too low" => Some(WallKind::Spend),
            "context limit reached" => Some(WallKind::Context),
            _ => None,
        };
    }
    // `Error during compaction: <detail>` is a `/compact` whose own request failed.
    // Where the detail names a wall with an answer of its own (a lost login, no
    // credit, a usage limit, a model's bucket) that wall stands, as it did before
    // the wrapper was read: `/compact` is no answer to them. Anything else (an
    // API error, a network error) leaves the context as full as it was.
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
        Some(Tag::Spend | Tag::Blocked) => Some(WallKind::Spend),
        Some(Tag::RateLimit) => Some(WallKind::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        }),
        Some(Tag::Context) => Some(WallKind::Context),
        Some(Tag::Auth) => Some(WallKind::Auth),
        Some(Tag::Overloaded) => Some(WallKind::Overloaded),
        // The exact `Request timed out` is drawn under `⎿` with no prefix:
        // the whole row, never a tool's line that begins so.
        None if api || crate::text::trim(&lower) == "request timed out" => api_error(head, &lower),
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
    crate::text::trim(&text[..end])
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
/// something is typed into this one. In a pane narrower than the row Ink
/// wraps it (`crate::wrap`), and the last thing said is its rest
/// (`organization changed`): the row is read as the line the last thing said
/// belongs to.
#[must_use]
pub fn login_restored(rows: &[String]) -> bool {
    use crate::phase::{gutter_open, last_said_index, line_at};
    let Some(last) = last_said_index(rows) else {
        return false;
    };
    let Some(open) = gutter_open(rows, last) else {
        return false;
    };
    let (end, line) = line_at(rows, open, rows.len(), crate::question::screen_width(rows));
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

/// How many rows above the composer's top rule a notification is looked
/// for in ([`login_expires_in`]): Claude Code parks its notifications on the
/// row right above it, right-aligned (`✔ Update installed · Restart to
/// update`, MEASURED in the `idle-after-limit-and-model-switch` capture), and
/// the slack is a row or two of a banner under it.
const NOTICE_ROWS: usize = 3;

/// THE DAYS CLAUDE CODE SAYS THE STORED LOGIN HAS LEFT — its own `Your login
/// expires in N days · run /login to renew` (anchors `login.expires_in`,
/// `login.renew_hint`; READ in the 2.1.300 bundle, N from 1 to 3) — or
/// `None`. Read where Claude Code draws a notification and nowhere a
/// conversation is: the right segment of a row just above the composer's top
/// rule ([`NOTICE_ROWS`]) or under its bottom rule (the footer, where the
/// bundle draws the same words at a day or less), starting at the hint
/// column or against the right edge as a notification does, after at most a
/// status glyph, and followed by `day`/`days` and the renew hint (or the
/// row's end, where the width cut it). A reply (`⏺`), a tool's output
/// (`⎿`), a tool row or a person's words quoting the sentence are none of
/// those rows' right segments, and read `None`. SYNTHETIC: the notice's
/// placement on a screen was NOT measured (no lab login can be seeded with an
/// end); it is read where the measured notification of its kind is drawn.
#[must_use]
pub fn login_expires_in(rows: &[String]) -> Option<u32> {
    use crate::phase::{HINT_COLUMN, composer_frame, is_against_right_edge};
    let head = const { crate::anchors::anchor_text("login.expires_in") };
    let hint = const { crate::anchors::anchor_text("login.renew_hint") };
    let frame = composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let above = frame.top.saturating_sub(NOTICE_ROWS)..frame.top;
    let footer = frame.bottom + 1..rows.len();
    above.chain(footer).find_map(|i| {
        let row = crate::text::trim_end(&rows[i]);
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

/// How Claude Code's OWN AUTO-CONTINUE of a usage limit ended, by the
/// vendor's word on the screen ([`auto_continue_end`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoContinue {
    /// It went on: the vendor sent the continuation itself — `Usage limit
    /// reset · continuing automatically` at the reset, `Usage limit available
    /// again · continuing now` before it.
    Fired,
    /// It will not go on by itself: `Automatic continue cancelled · …` (Esc
    /// on its countdown; the session moved, relaunched or exited), `Automatic
    /// continue stopped · …`, `Automatic continue was turned off · …`, or a
    /// reset that passed with nothing sent (`Usage limit has reset · press
    /// enter to continue`).
    Unkept,
    /// It has not ended: its COUNTDOWN is still drawn (`Continuing
    /// automatically at 3:15pm · esc to cancel`, `Continuing shortly · esc to
    /// cancel` — the footer's row under the banner from 2.1.295 on, a
    /// notification under the wall's row in 2.1.284). Where no limit's notice
    /// is read, this is what says the vendor still goes on by itself; where
    /// nothing is drawn, it does not: a `/resume` or a `/clear` drops the
    /// countdown without a word (W4 of ATERM-KI-1015-1, the lab on 2.1.300).
    Armed,
}

/// What the screen last says of Claude Code's own auto-continue, once the
/// limit it was armed for has left the screen. Measured on 2.1.296 (the lab,
/// 2026-10-09): Esc on `Continuing automatically at 3:50pm · esc to cancel`
/// draws `⏺ Automatic continue cancelled · /rate-limit-options to re-arm` in
/// the transcript and as the footer's notification; at the reset the vendor
/// draws `⏺ Usage limit reset · continuing automatically` and sends the
/// continuation itself, with no `❯` row, its reply under it. Either way the
/// limit's notice is no longer the last thing said, so [`wall`] finds none,
/// and only these rows say which way the limit's end went.
///
/// Read where the vendor draws them: the footer's notification first (when it
/// shows it is the newest word), then up from the composer through the
/// transcript, the first row or wide-gapped item that OPENS with one of the
/// words ([`crate::anchors`]' `wall.auto_continue.*`, a leading glyph
/// passed over) — each row read as the line it opens, across the rows Ink
/// wrapped it onto (`crate::wrap`; a `⏺` row's, its message: at 26 columns
/// `⏺ Automatic` stands over `continue` over `cancelled · /rat…`, 2.1.299).
/// The walk up ends at the turn's own `❯` row — a person's `/usage` after the
/// cancel is a new turn, and what came before it is that turn's history — and
/// at a limit notice: a limit below the vendor's word stands (its
/// continuation met the limit again). A countdown still drawn there is
/// [`AutoContinue::Armed`]: not ended. `None` when nothing says — and once a
/// countdown was armed, nothing drawn says it was dropped (a `/resume`, a
/// `/clear`: W4 of ATERM-KI-1015-1).
#[must_use]
pub fn auto_continue_end(rows: &[String]) -> Option<AutoContinue> {
    use crate::phase::{footer_end, line_at};
    let frame = crate::phase::composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let until = footer_end(rows, frame.bottom + 1);
    if let Some(end) = (frame.bottom + 1..until).find_map(|i| {
        let (_, line) = line_at(rows, i, until, width);
        gapped_items(&line).find_map(auto_continue_said)
    }) {
        return Some(end);
    }
    for (i, row) in rows[..frame.top].iter().enumerate().rev() {
        if crate::phase::leading_spaces(row) == 0 && crate::text::trim_start(row).starts_with('❯')
        {
            return None;
        }
        let (_, line) = line_at(rows, i, frame.top, width);
        for item in gapped_items(&line) {
            if let Some(end) = auto_continue_said(item) {
                return Some(end);
            }
            if classify_wall(item).is_some_and(|k| k.reads_limited()) {
                return None;
            }
        }
    }
    None
}

/// The screen's NEWEST USAGE LIMIT NOTICE and what was served, or chosen,
/// since it ([`since_limit`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinceLimit {
    /// The notice's kind ([`classify_wall`]).
    pub kind: WallKind,
    /// The reset it names, as printed ([`crate::notice_reset`]: `10:38am`,
    /// `Oct 20 at 9am (…)`): what tells one limit's notice from another's.
    pub reset: Option<String>,
    /// A TURN WAS SERVED since it: a person's prompt below it (a `❯` row in
    /// column zero with text, no local command — `❯ /usage`, `❯ /model`, a
    /// `/resume`: no turn) with the model's reply drawn under that prompt (a
    /// `⏺` row in column zero, `●` off macOS, that is no notice of the
    /// vendor's: the model's words or a tool's call).
    pub served: bool,
    /// A MODEL WAS CHOSEN since it: a person's `/model` below it (`❯ /model`,
    /// `❯ /model opus`) with its result drawn under that prompt (`⎿  Set model
    /// to …`, the anchor `model.set`). This alone says nothing about whether
    /// the choice left the model family a bucket limits.
    pub model_set: bool,
    /// The NEWEST completed `/model` choice's words after `Set model to `.
    /// Kept without interpreting model names: a caller can compare the chosen
    /// family with the bucket's, and an unknown choice remains unknown rather
    /// than falling back to an older choice it recognizes.
    pub chosen_model: Option<String>,
}

/// THE NEWEST USAGE LIMIT NOTICE THE SCREEN DRAWS and whether a turn was
/// SERVED since it ([`SinceLimit`]): up from the composer, the first line (or
/// wide-gapped item) Claude Code's limit notice opens (`You've hit your
/// session limit · …`, its `Usage limit reached · continuing automatically …`
/// row; a line that names no reset — the `/goal` echo of the notice above it —
/// takes its reset from that notice), and the rows below it. The screen's
/// order is the conversation's, so
/// a prompt drawn below the notice was submitted after it; where the limit
/// still stood, its turn met it again and drew a notice below the prompt,
/// which the walk meets first; where the API never answered it (an Esc
/// before its reply: `⎿  Interrupted · …`), no `⏺` row hangs under it. The
/// notice's reset says WHICH limit it is: a conversation `/resume`d redraws
/// its own history, an older limit's notice with turns served under it, and
/// the caller ties the notice to the limit it waits on by that reset (the
/// review of 2026-10-14, the lab on 2.1.300). `None` where the screen above
/// the composer draws no limit's notice (scrolled off, cleared) or no
/// composer: the screen says nothing of the limit. Read where the limit's
/// notice has LEFT the screen's end (Esc on the vendor's countdown), and a
/// person's turn ran there that no read saw busy (measured on 2.1.299: `❯
/// what is left to do?`, the reply and its done row under `⏺ Automatic
/// continue cancelled · …`). A shell-mode `! cmd` is drawn without the `❯`
/// and is not read here.
///
/// Each row is read as the line it opens, across the rows Ink wrapped it
/// onto (`crate::wrap`, as [`auto_continue_end`] reads them): in a pane
/// narrower than the notice (`⏺ Usage limit` over `reached ·` over
/// `continuing` at 26 columns, 2.1.299) a row at a time named no limit, and
/// the cancel's own `⏺ Automatic` row read as a reply of the model's.
#[must_use]
pub fn since_limit(rows: &[String]) -> Option<SinceLimit> {
    newest_limit(rows).map(|newest| newest.since)
}

/// A LINE OF THE MODEL'S: `line`, opened by `row`, a row the caller found in
/// column 0 ([`since_limit`]), says `⏺` — `●` where the
/// platform draws that (Claude Code draws `●` off macOS), never the session
/// survey ([`crate::phase::is_survey`]) — and a reply or a tool's call, never
/// a notice of the vendor's ([`classify_wall`], [`auto_continue_end`]'s
/// words).
fn models_line(row: &str, line: &str) -> bool {
    !crate::phase::is_survey(row)
        && crate::text::trim(line)
            .strip_prefix(['⏺', '●'])
            .is_some_and(|said| classify_wall(said).is_none() && auto_continue_said(said).is_none())
}

/// What [`newest_limit`] read: the notice ([`SinceLimit`]), the row its line
/// opens on, its words as said (the item, its leading glyph dropped), and the
/// row its `/goal` echo opens on below it, if one took its reset from it.
struct Newest {
    since: SinceLimit,
    row: usize,
    message: String,
    echo: Option<usize>,
}

/// [`since_limit`]'s walk, keeping the notice's row and words for
/// [`left_wall`].
fn newest_limit(rows: &[String]) -> Option<Newest> {
    use crate::phase::line_at;
    let frame = crate::phase::composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let words = |item: &str| {
        item.trim_start_matches(|c: char| !c.is_alphanumeric())
            .to_string()
    };
    // A reply of the model's below the rows walked so far, and a prompt
    // that drew one.
    let (mut replied, mut served) = (false, false);
    // A model's choice below the rows walked so far (`⎿  Set model to …`),
    // and a `/model` that drew one.
    let mut set: Option<String> = None;
    let (mut model_set, mut chosen_model) = (false, None);
    // The newest limit line met, where it named no reset: an ECHO of the
    // notice above it (`⏺ Goal paused · usage limit reached · send a message
    // after it resets to continue`), whose reset the notice names. The walk
    // goes on up to it, through no prompt.
    let mut echo: Option<(WallKind, usize, String)> = None;
    let echoed =
        |(kind, row, message): (WallKind, usize, String), served, model_set, chosen_model| Newest {
            since: SinceLimit {
                kind,
                reset: None,
                served,
                model_set,
                chosen_model,
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
                return Some(echoed(echo, served, model_set, chosen_model));
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
        if let Some((kind, item)) = gapped_items(&line).find_map(|item| {
            classify_wall(item)
                .filter(|k| k.reads_limited())
                .map(|k| (k, item))
        }) {
            match crate::phase::notice_reset(item) {
                Some(reset) => {
                    return Some(Newest {
                        since: SinceLimit {
                            kind,
                            reset: Some(reset),
                            served,
                            model_set,
                            chosen_model,
                        },
                        row: i,
                        message: words(item),
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
                crate::text::trim_start(out).strip_prefix(crate::anchors::anchor("model.set"))
            })
        {
            set.get_or_insert_with(|| crate::text::trim(chosen).to_string());
        }
        if echo.is_none() && column0 && models_line(row, &line) {
            replied = true;
        }
    }
    echo.map(|echo| echoed(echo, served, model_set, chosen_model))
}

/// The words a GENERIC usage limit notice opens with: Claude Code 2.1.295 and
/// later draw every subscription limit's footer banner so (`⚠ Usage limit
/// reached · limit resets 3:34pm`), and its auto-continue's transcript row
/// (`⏺ Usage limit reached · continuing automatically at 3:34pm · …`), whatever
/// the limit is — the five-hour window, the weekly one, a model's bucket.
/// Only the turn's own `⎿` notice names which (`You've hit your Sonnet limit ·
/// resets …`).
const GENERIC_LIMIT: &str = "usage limit reached";

/// Whether `item` is a GENERIC usage limit notice ([`GENERIC_LIMIT`]): it
/// names no window and no bucket.
fn generic_limit(item: &str) -> bool {
    let words = item.trim_start_matches(|c: char| !c.is_alphanumeric());
    strip_ci(words, GENERIC_LIMIT).is_some()
}

/// A reset as its notice prints it, its zone left out (the footer's banner
/// names none: `Oct 17 at 3:34pm`; the turn's notice does: `Oct 17 at 3:34pm
/// (America/Los_Angeles)`), so that two notices of one limit compare equal.
fn reset_clock_words(reset: &str) -> String {
    let cut = reset.find('(').unwrap_or(reset.len());
    crate::text::trim(&reset[..cut]).to_ascii_lowercase()
}

/// THE WALL A GENERIC BANNER STANDS FOR (W3 of ATERM-KI-1015-1, the lab on
/// Claude Code 2.1.300, 2026-10-15): `found` is the notice the reader placed
/// (`crate::phase::notice`'s placement: the footer, a banner) and, where it
/// is the generic `Usage limit reached` ([`generic_limit`]), the KIND is the
/// TURN'S OWN NOTICE's — the newest limit notice above the composer that
/// names its limit, read as [`since_limit`] reads one (each row as the line
/// it opens, across the rows Ink wrapped it onto), up through local commands
/// (`❯ /rate-limit-options`, which Claude Code types itself to open its
/// dialog) but no person's prompt, and naming the SAME reset as `found` (the
/// zone aside): the notice of this limit, never an older one a `/resume`d
/// history redraws. Its words then open the message, the banner's follow
/// (`You've hit your Sonnet limit · resets … — ⚠ Usage limit reached · …`),
/// so the bucket's model is read off it and a loop that takes the wall up
/// from its ledger row classifies it the same. Measured before: a Sonnet
/// bucket's banner read `usage-session`, the default `model_fallback =
/// "opus"` never ran, and the session waited days for the bucket's reset; a
/// weekly limit read `usage-session` too. `found` as it is anywhere else: a
/// banner that names its limit, a turn's notice of the same kind, one naming
/// another reset or none, a screen with no composer.
pub(crate) fn named_by_the_turn(
    rows: &[String],
    found: Wall,
    classify: &dyn Fn(&str) -> Option<WallKind>,
) -> Wall {
    use crate::phase::line_at;
    if !generic_limit(&found.message) {
        return found;
    }
    let Some(said) = found.reset.as_deref().map(reset_clock_words) else {
        return found;
    };
    let Some(frame) = crate::phase::composer_frame(rows) else {
        return found;
    };
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let top = frame.top.min(found.row);
    for (i, row) in rows[..top].iter().enumerate().rev() {
        let column0 = crate::phase::leading_spaces(row) == 0;
        if column0 && let Some(prompt) = crate::text::trim(row).strip_prefix('❯') {
            let prompt = crate::text::trim(prompt);
            if prompt.is_empty() || prompt.starts_with('/') {
                continue;
            }
            return found;
        }
        let (_, line) = line_at(rows, i, top, width);
        let Some((kind, item)) = gapped_items(&line).find_map(|item| {
            classify(item)
                .filter(|k| k.reads_limited() && !generic_limit(item))
                .map(|k| (k, item))
        }) else {
            continue;
        };
        let same = crate::phase::notice_reset(item).is_some_and(|r| reset_clock_words(&r) == said);
        if !same || kind == found.kind {
            return found;
        }
        let words = item.trim_start_matches(|c: char| !c.is_alphanumeric());
        return Wall {
            kind,
            message: format!("{words} \u{2014} {}", found.message),
            ..found
        };
    }
    found
}

/// THE LIMIT A CANCEL LEFT ON THE SCREEN: the vendor's own word that its
/// auto-continue will NOT go on ([`auto_continue_end`] `Unkept`: Esc on its
/// countdown, `⏺ Automatic continue cancelled · /rate-limit-options to
/// re-arm`) drawn as the LAST THING SAID, its `⏺` row in column 0, right
/// under the screen's newest usage limit notice ([`since_limit`]: the first
/// thing said above the cancel is that notice's block, or its `/goal` echo)
/// — a notice that names
/// its reset and that no turn has been served since — read as the wall that
/// notice was before the cancel took it off the screen's end: its kind, its
/// words, its reset, the row it opens on ([`Placement::ErrorRow`] for the
/// vendor's `⏺` row, its countdown's `Usage limit reached · continuing
/// automatically at …`; [`Placement::Gutter`] for a `⎿` notice), never under
/// a call. [`wall`] reads none there: the cancel is the last thing said.
///
/// Measured on 2.1.300 (the lab, `tw16after`, 2026-10-14): the supervisor's
/// `keep going` met a five-hour limit at once, and a person's Esc on the
/// countdown came before the supervisor's first read after its act — that
/// read was already the cancel, no wall, and nothing tracked the limit. This
/// says only what the screen draws; whether the notice is the limit a
/// supervisor must wait on — the answer to an act of its own, not an older
/// limit a `/resume`d conversation redraws — is the caller's (aterm-agent,
/// `policy/turn_end.rs`'s `left_answer`). `None` where the vendor's last word
/// is not the cancel, anything is said after it or between it and the
/// notice (a tool's output that quotes both lines ends on the model's reply,
/// and its rows are no `⏺` row in column 0), the newest notice names no
/// reset or has a turn served under it, or the screen draws no composer.
#[must_use]
pub fn left_wall(rows: &[String]) -> Option<Wall> {
    use crate::phase::{is_said, leading_spaces, line_at};
    if auto_continue_end(rows)? != AutoContinue::Unkept {
        return None;
    }
    let frame = crate::phase::composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    // The cancel is the last thing said: its `⏺` row and the rows Ink
    // wrapped it onto.
    let last = crate::phase::last_said_index(rows)?;
    let cancel = (0..=last)
        .rev()
        .find(|&i| rows[i].starts_with(['⏺', '●']))?;
    let (end, line) = line_at(rows, cancel, frame.top, width);
    let said = crate::text::trim(&line).trim_start_matches(['⏺', '●']);
    if end < last || auto_continue_said(said) != Some(AutoContinue::Unkept) {
        return None;
    }
    let newest = newest_limit(rows)?;
    if newest.since.served || newest.row >= cancel {
        return None;
    }
    let reset = newest.since.reset?;
    // Right under the notice: the first thing said above the cancel is a row
    // of the notice's block (its wrapped rows, a gutter block's rows under
    // it, up to the first blank row or row in column 0), or of its `/goal`
    // echo (`⏺ Goal paused · usage limit reached · continues automatically
    // when it resets`, whose reset the notice names).
    let block_end = match newest.echo {
        Some(echo) => line_at(rows, echo, frame.top, width).0,
        None => {
            (newest.row + 1..cancel)
                .find(|&i| crate::text::trim(&rows[i]).is_empty() || leading_spaces(&rows[i]) == 0)
                .unwrap_or(cancel)
                - 1
        }
    };
    // The first thing said above the cancel; a wrapped row of a line that
    // is no thing said (`✻ Brewed for 0s · done` over `10:40 AM` at 26
    // columns) is that line's.
    let opener = |i: usize| {
        (0..=i)
            .rev()
            .find(|&j| leading_spaces(&rows[j]) == 0 || crate::text::trim(&rows[j]).is_empty())
    };
    let mut i = cancel;
    let above = loop {
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
            _ => break i,
        }
    };
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

/// A row's items: its text split at each run of two or more spaces (a
/// footer's notification is drawn right of its mode line).
fn gapped_items(row: &str) -> impl Iterator<Item = &str> {
    crate::text::trim(row)
        .split("  ")
        .map(crate::text::trim)
        .filter(|item| !item.is_empty())
}

/// The auto-continue's end an item opens with ([`auto_continue_end`]).
fn auto_continue_said(item: &str) -> Option<AutoContinue> {
    let text = item
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .replace('\u{2219}', "\u{b7}");
    let opens = |id: &str| {
        let words = crate::anchors::anchor(id);
        // `Your usage limit has reset · …` is the stale notice too.
        [text.as_str(), strip_ci(&text, "your ").unwrap_or("")]
            .iter()
            .any(|t| strip_ci(t, words).is_some())
    };
    if opens("wall.auto_continue.fired") || opens("wall.auto_continue.now") {
        Some(AutoContinue::Fired)
    } else if [
        "wall.auto_continue.cancelled",
        "wall.auto_continue.stopped",
        "wall.auto_continue.off",
        "wall.auto_continue.stale",
    ]
    .iter()
    .any(|id| opens(id))
    {
        Some(AutoContinue::Unkept)
    } else if opens("wall.auto_continue") || strip_ci(&text, "continuing shortly").is_some() {
        // The countdown's own row OPENS with its words; the transcript's
        // `Usage limit reached · continuing automatically at …` is the
        // limit's notice, and the walk stops there.
        Some(AutoContinue::Armed)
    } else {
        None
    }
}

/// `text` past its opening `prefix`, ignoring ASCII case.
fn strip_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
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
/// WHY: `claude --continue` resumes the NEWEST conversation filed under the
/// working directory, and several live Claude Code processes can share one
/// directory — a person following the banner's words in one tab resumes a sibling's
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
/// of both the hint column and a hint's column-6 floor, so a reader that
/// asked it everywhere would read nothing at 82 columns or fewer.
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
    let anchor = const { crate::anchors::anchor_text("wall.memory") };
    let frame = composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let block = status_block(rows, frame.top);
    let from = block
        .status
        .unwrap_or(block.from)
        .max(frame.top.saturating_sub(MEMORY_ROWS));
    (from..frame.top).rev().find_map(|i| {
        let row = crate::text::trim_end(&rows[i]);
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
                under_call: false,
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
pub(crate) fn in_queued_message(rows: &[String], status: usize, i: usize) -> bool {
    (status + 1..=i)
        .rev()
        .map(|j| rows[j].as_str())
        .find(|row| crate::text::trim(row).is_empty() || !row.starts_with(char::is_whitespace))
        .is_some_and(|row| row.starts_with('❯'))
}

/// A row's right segment and the column it starts at: the text after its
/// last run of two or more spaces, or the whole row (its indent skipped)
/// when it has none. `row` is trimmed at the end.
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
mod tests {
    use super::*;

    /// The vendor's own limit catalog (Claude Code 2.1.283 and 2.1.284: the
    /// limit functions, the auto-continue notices, the `/goal` interruption
    /// table, the authentication notices), one string per row and the kind the
    /// reader must give it. Nineteen of these read as something else before the
    /// catalog was classified: an account limit no reset ends was no wall, a
    /// notice that the reset came was a limit still standing, and a network
    /// hiccup filed under authentication was a lost login.
    #[test]
    fn the_vendor_limit_catalog_classifies() {
        let catalog: &[(&str, Option<WallKind>)] = &[
            (
                "You've hit your weekly limit · resets 4pm (America/Los_Angeles)",
                Some(WallKind::UsageWeekly),
            ),
            (
                "You've hit your session limit · resets 3pm (America/Los_Angeles)",
                Some(WallKind::UsageSession),
            ),
            (
                "You've hit your Opus limit · resets 4pm (America/Los_Angeles)",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            (
                "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            (
                "You've reached your Fable limit. /model to switch models.",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            (
                "Fable 5 requires usage credits. Run /usage-credits to continue or switch models with /model.",
                Some(WallKind::ModelBucket { consent: false }),
            ),
            (
                "You're out of usage credits. Run /usage-credits to keep using Fable or /model to switch models.",
                Some(WallKind::Spend),
            ),
            (
                "You're out of extra usage · resets 4pm",
                Some(WallKind::Spend),
            ),
            (
                "You've hit your monthly spend limit.",
                Some(WallKind::Spend),
            ),
            (
                "You've hit your team's shared budget. /model to switch models.",
                Some(WallKind::Spend),
            ),
            (
                "Your org is out of usage · add funds to continue",
                Some(WallKind::Spend),
            ),
            (
                "Your org is out of usage · contact your admin",
                Some(WallKind::Spend),
            ),
            (
                "Your seat type doesn't include usage credits",
                Some(WallKind::Spend),
            ),
            (
                "Your seat type doesn't include extra usage",
                Some(WallKind::Spend),
            ),
            (
                "Your usage allocation has been disabled by your admin",
                Some(WallKind::Spend),
            ),
            (
                "Your group's usage limit is set to $0",
                Some(WallKind::Spend),
            ),
            (
                "This service is disabled for your org",
                Some(WallKind::Spend),
            ),
            (
                "Your organization has disabled Claude subscription access · Sign in again",
                Some(WallKind::Spend),
            ),
            ("Credit balance is too low", Some(WallKind::Spend)),
            (
                "Usage limit reached · continuing automatically at 2:10pm · esc to cancel",
                Some(WallKind::UsageSession),
            ),
            (
                "Usage limit reached · continuing automatically when it resets · esc to cancel",
                Some(WallKind::UsageSession),
            ),
            (
                "Usage limit reached · continuing shortly · esc to cancel",
                Some(WallKind::UsageSession),
            ),
            (
                "Usage limit reached again · continuing automatically at 2:10pm · esc to cancel",
                Some(WallKind::UsageSession),
            ),
            ("Continuing automatically at 2:10pm · esc to cancel", None),
            ("Usage limit reset · continuing automatically", None),
            ("Usage limit available again · continuing now", None),
            ("Usage limit has reset · press enter to continue", None),
            ("Your usage limit has reset · press enter to continue", None),
            (
                "Automatic continue cancelled · /rate-limit-options to re-arm",
                None,
            ),
            (
                "Automatic continue stopped · the usage limit now resets more than 24 hours out, so this task will not resume on its own (/rate-limit-options to wait anyway)",
                None,
            ),
            (
                "Automatic continue was turned off · this task will not resume on its own",
                None,
            ),
            (
                "Goal paused · usage limit reached · send a message after it resets to continue",
                Some(WallKind::UsageSession),
            ),
            (
                "Goal paused · usage limit reached · continues automatically when it resets",
                Some(WallKind::UsageSession),
            ),
            (
                "Goal paused · the request was rate limited · send a message to retry",
                Some(WallKind::ApiError {
                    code: Some(429),
                    retryable: true,
                    cause: ApiCause::Server,
                }),
            ),
            (
                "Goal paused · the API rejected the last request · send a message to continue, or run /goal clear",
                None,
            ),
            (
                "Goal paused · the last turn could not finish · send a message to continue",
                None,
            ),
            (
                "Goal paused · a hook ended the turn · send a message to continue",
                None,
            ),
            (
                "Goal still active · the API was unavailable or the connection dropped · retrying in 5 min (2/3) · send a message to retry now",
                None,
            ),
            (
                "Goal paused after 3 automatic retries · the API was unavailable or the connection dropped · send a message to continue",
                None,
            ),
            (
                "Goal cleared after an unrecoverable error (authentication failed): \"x\". Run /goal again to continue.",
                Some(WallKind::Auth),
            ),
            (
                "Goal cleared after an unrecoverable error (credit balance too low): \"x\". Run /goal again to continue.",
                Some(WallKind::Spend),
            ),
            (
                "Goal cleared after an unrecoverable error (context limit reached): \"x\". Run /goal again to continue.",
                Some(WallKind::Context),
            ),
            (
                "Goal cleared after an unrecoverable error (model unavailable): \"x\". Run /goal again to continue.",
                None,
            ),
            (
                "Authentication error · This may be a temporary network issue, please try again",
                Some(WallKind::ApiError {
                    code: None,
                    retryable: true,
                    cause: ApiCause::Unreachable,
                }),
            ),
            (
                "Authentication error · The gateway could not authenticate with its upstream provider — contact your gateway administrator",
                Some(WallKind::Auth),
            ),
            (
                "Could not refresh your login because another Claude Code process is refreshing it (or exited mid-refresh) · Try again in a minute",
                Some(WallKind::ApiError {
                    code: None,
                    retryable: true,
                    cause: ApiCause::Unreachable,
                }),
            ),
            ("Login expired · Please run /login", Some(WallKind::Auth)),
            (
                "OAuth token revoked · Please run /login",
                Some(WallKind::Auth),
            ),
            ("Not logged in · Please run /login", Some(WallKind::Auth)),
            (
                "Authentication required · Sign in again to continue",
                Some(WallKind::Auth),
            ),
            (
                "Invalid API key · Fix external API key",
                Some(WallKind::Auth),
            ),
            (
                "Your organization has disabled API key authentication · Sign in again with your claude.ai account",
                Some(WallKind::Auth),
            ),
            (
                "There's an issue with the selected model (x). It may not exist or you may not have access to it. Run /model to pick a different model.",
                None,
            ),
            (
                "Context exceeds the 200000-token limit · run /compact",
                Some(WallKind::Context),
            ),
            (
                "Extra usage is required for long context · run /extra-usage",
                Some(WallKind::Spend),
            ),
        ];
        assert_eq!(catalog.len(), 55);
        for (text, want) in catalog {
            assert_eq!(classify_wall(text), *want, "{text}");
        }
    }

    /// The auto-continue's lifecycle notices say a limit is over, cancelled or
    /// armed for the next reset; none of them is a limit standing. NEGATIVE
    /// CONTROL: the same words with `Usage limit reached` in front still are.
    #[test]
    fn the_lifecycle_notices_are_no_wall() {
        for text in [
            "Usage limit reset · continuing automatically",
            "Usage limit available again · continuing now",
            "Usage limit has reset · press enter to continue",
            "Your usage limit has reset · press enter to continue",
            "Continuing automatically at 2:10pm · esc to cancel",
            "Automatic continue cancelled · /rate-limit-options to re-arm",
            "Automatic continue was turned off · this task will not resume on its own",
        ] {
            assert_eq!(classify_wall(text), None, "{text}");
        }
        assert_eq!(
            classify_wall(
                "Usage limit reached · continuing automatically at 2:10pm · esc to cancel"
            ),
            Some(WallKind::UsageSession)
        );
    }

    /// `Authentication error` and a refresh another process is doing are the
    /// network, tried again on its ladder, never the lost login a person must
    /// answer. NEGATIVE CONTROL: a gateway that cannot authenticate, and a
    /// plain `Authentication required`, are the login.
    #[test]
    fn authentication_that_passes_by_itself_is_the_network_not_the_login() {
        let network = Some(WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Unreachable,
        });
        for text in [
            "Authentication error · This may be a temporary network issue, please try again",
            "Could not refresh your login because another Claude Code process is refreshing it (or exited mid-refresh) · Try again in a minute",
        ] {
            assert_eq!(classify_wall(text), network, "{text}");
        }
        for text in [
            "Authentication error · The gateway could not authenticate with its upstream provider — contact your gateway administrator",
            "Authentication required · Sign in again to continue",
        ] {
            assert_eq!(classify_wall(text), Some(WallKind::Auth), "{text}");
        }
    }

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
            (
                "Your seat type doesn't include usage credits",
                Some(WallKind::Spend),
            ),
            (
                "Goal paused · the request was rate limited · send a message to retry",
                Some(WallKind::ApiError {
                    code: Some(429),
                    retryable: true,
                    cause: ApiCause::Server,
                }),
            ),
            (
                "Context exceeds the 200000-token limit · run /compact",
                Some(WallKind::Context),
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
            "Usage limit has reset · press enter to continue",
            "Usage limit available again · continuing now",
            "Continuing automatically at 3pm · esc to cancel",
        ] {
            assert_eq!(classify_wall(text), None, "{text}");
        }
    }

    use crate::phase::{Phase, limit_notice, worker_phase};
    use crate::prompt::fixtures::{
        API_ERROR_529, API_ERROR_ENOTFOUND, API_ERROR_ENOTFOUND_80,
        API_ERROR_ENOTFOUND_80_MEASURED, API_ERROR_ENOTFOUND_MEASURED,
        API_ERROR_QUEUED_SENT_MEASURED, API_ERROR_SLEEP, API_ERROR_TWICE_MEASURED,
        COMPACT_ERROR_500_MEASURED, COMPACT_FAILED_3_MEASURED, COMPACT_FAILED_FIRST_MEASURED,
        COMPACT_NOT_ENOUGH_MEASURED, COMPACT_SUCCEEDED_MEASURED,
        CONTEXT_WALL_COMPACT_MENU_MEASURED, CONTEXT_WALL_MEASURED, END_529, END_OFFER,
        END_SESSION_LIMIT, IDLE_AFTER_LIMIT_AND_MODEL_SWITCH, composer, rows, screen,
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
        let mcp = framed(
            &[
                "⏺ github - get_issue (MCP)(owner: \"acme\", repo: \"app\", issue_number: 7)",
                &format!("  ⎿  {API_529}"),
                "",
                "✻ Cooked for 3m 2s · done 4:24 PM",
                "",
            ],
            FOOTER,
        );
        for (name, r) in [
            ("tool call", under_call),
            ("echo", echo),
            ("prose", prose),
            ("mcp", mcp),
        ] {
            assert_eq!(wall(&r), None, "{name}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{name}");
        }
        assert!(crate::phase::is_tool_call("⏺ Bash(tail -1 build.log)"));
        assert!(crate::phase::is_tool_call("⏺ Update(notes.txt)"));
        assert!(crate::phase::is_tool_call(
            "⏺ github - get_issue (MCP)(owner: \"acme\")"
        ));
        assert!(crate::phase::is_tool_call("⏺ github - ping (MCP)"));
        assert!(crate::phase::is_tool_call("! cat notes.txt"));
        assert!(!crate::phase::is_tool_call(
            "⏺ Dynamic workflow \"Second review pass\" completed · 5s"
        ));
        assert!(!crate::phase::is_tool_call(
            "⏺ Suites are running (all three)."
        ));
        assert!(!crate::phase::is_tool_call(
            "⏺ Wired the github server in (MCP), and it answers."
        ));
        assert!(crate::phase::is_tool_call(
            "⏺ Claude in Chrome[get_page_text](tabId: 1)"
        ));
        assert!(!crate::phase::is_tool_call(
            "⏺ Fixed rows[i] so the parser keeps the last one (it dropped it)."
        ));
    }

    /// THE SCREEN'S HALF OF H6-HARNESS-1 (2026-10-08), on hand-built screens.
    /// NEVER A WALL: a wall's words in a call's FIRST column-2 `⎿` row — the
    /// first line of its output — under every row Claude Code draws for a
    /// call or a command (the MCP forms: no arguments, spaces in the server
    /// or the title, the unknown-tool stub; the shell mode's two; a name of
    /// several words; an integration's `Name[tool](…)`), and a `⎿` at any
    /// column but 2, which is what an output prints (after a blank row of it
    /// or not), and the first row under a call whose own row wraps (the
    /// wrapped row is no `⎿` row) or whose command holds a blank row (a
    /// heredoc's, its rows at column 6). Main read the MCP, shell-mode,
    /// column-5 and heredoc ones as walls, and its harness typed `/login`
    /// (measured on 2.1.294).
    /// READ, for recall: a LATER column-2 row under every call — Claude
    /// Code's wall after a tool's output, which main missed — an MCP call's
    /// and one a hook's row opens included, where the screen cannot tell the
    /// wall from the output's own row and the transcript decides: each says
    /// it was read UNDER A CALL ([`Wall::under_call`]), which a supervisor
    /// with no transcript does not act on — behind a blank row of the output
    /// too, and behind blank rows that end a block of it (the walk goes on
    /// over a blank row whose next row is at column 5 or a column-2 `⎿`
    /// row); and a column-2 row whose owner the walk does not reach (a blank
    /// row over a column-2 row that opens an item, or the top of the screen,
    /// comes first),
    /// where pass 2 of the fix read nothing and so missed the real
    /// walls measured there (a prompt's second paragraph, a wrapped group
    /// row, a long output whose call scrolled off). CONTROLS: every owner
    /// that is no call reads its wall, and none of those is under a call.
    #[test]
    fn the_owner_walk_reads_a_wall_but_never_in_a_calls_first_row() {
        const WALLS: [(&str, WallKind); 4] = [
            (API_529, WallKind::Overloaded),
            ("Not logged in · Please run /login", WallKind::Auth),
            (
                "Context limit reached · /compact or /clear to continue",
                WallKind::Context,
            ),
            (
                "You've hit your weekly limit · resets Oct 30 at 7pm (America/Los_Angeles)",
                WallKind::UsageWeekly,
            ),
        ];
        let screen_of = |body: &[&str]| {
            let mut rows: Vec<&str> = body.to_vec();
            rows.extend(["", "✻ Worked for 4s · done 5:47 AM", ""]);
            framed(&rows, FOOTER)
        };
        let read = |r: &Vec<String>| {
            let reading = crate::reader::read(Some("claude"), r, None);
            super::wall(r)
                .or(reading.wall)
                .map(|w| (w.kind, w.placement, w.under_call))
        };
        let calls = [
            "⏺ Bash(tail -1 build.log)",
            "⏺ Web Search(\"aterm release notes\")",
            "⏺ Update(notes.txt)",
            "! cat notes.txt",
            "❯ !cat notes.txt",
            "⏺ github - get_issue (MCP)(owner: \"acme\", issue_number: 7)",
            "⏺ github - ping (MCP)",
            "⏺ ticket desk - get_issue (MCP)(owner: \"acme\", issue_number: 8)",
            "⏺ github - Fetch Page (MCP)(url: \"http://example.invalid/x\")",
            "⏺ claude.ai Gmail - search_threads (MCP)(query: \"from:billing\")",
            "⏺ mcp__github__get_issue (MCP)(issue_number: 7)",
            "⏺ mcp__github__get_issue(issue_number: 7)",
            "⏺ Claude in Chrome[get_page_text](tabId: 1)",
            "⏺ Claude in Chrome[tabs_context_mcp]",
        ];
        let mut wrong = Vec::new();
        for (wall, kind) in WALLS {
            let gutter = format!("  ⎿  {wall}");
            // Read under a call's later row, or anywhere else.
            let under = Some((kind, Placement::Gutter, true));
            let want = Some((kind, Placement::Gutter, false));
            for owner in calls {
                assert!(crate::phase::is_tool_call(owner), "{owner}");
                // The output's first row: never a wall.
                let first = screen_of(&[owner, &gutter, "     the second row of the output"]);
                if let Some(w) = read(&first) {
                    wrong.push(format!("first row under {owner}: {w:?}"));
                }
                // A later row: read, whatever the call.
                let later = screen_of(&[owner, "  ⎿  stamped", "     a second row", &gutter]);
                if read(&later) != under {
                    wrong.push(format!("later row under {owner}: {:?}", read(&later)));
                }
                // Under a hook's row above the output: read too.
                let hooked = screen_of(&[
                    owner,
                    "  ⎿  PreToolUse:Bash says: remember to run the tests",
                    &gutter,
                ]);
                if read(&hooked) != under {
                    wrong.push(format!("hooked under {owner}: {:?}", read(&hooked)));
                }
                // Behind a blank row of the output (its next row at column
                // 5): the walk goes on over it to the call, and reads the
                // row under the call too.
                let blank = screen_of(&[
                    owner,
                    "  ⎿  Issue 7: the parser drops a row",
                    "",
                    "     Steps to reproduce: run it",
                    &gutter,
                ]);
                if read(&blank) != under {
                    wrong.push(format!(
                        "behind a blank row under {owner}: {:?}",
                        read(&blank)
                    ));
                }
                // Behind blank rows that END a block of the output (the row
                // under them a column-2 `⎿` row): read under the call too.
                let ended = screen_of(&[
                    owner,
                    "  ⎿  Issue 7: the parser drops a row",
                    "",
                    "",
                    &gutter,
                ]);
                if read(&ended) != under {
                    wrong.push(format!(
                        "behind blank rows ending a block under {owner}: {:?}",
                        read(&ended)
                    ));
                }
            }
            // A CALL'S OWN WRAPPED ROW is no `⎿` row, at column 2 or not: the
            // output's first row under it is still its first (hand-built: a
            // call row whose arguments run onto a row of their own).
            for wrapped in [
                screen_of(&[
                    "⏺ Bash(cargo test --workspace --no-fail-fast -- --nocapture",
                    "  2>&1 | tail -40)",
                    &gutter,
                ]),
                screen_of(&[
                    "⏺ github - get_issue (MCP)(owner: \"acme\", repo: \"app\",",
                    "  issue_number: 7)",
                    &gutter,
                ]),
                // A heredoc command, its blank line a blank row of the call's
                // own (its rows at column 6): the walk goes on over it.
                screen_of(&[
                    "⏺ Bash(python3 - <<'EOF'",
                    "      print('first line')",
                    "",
                    "      print('third line')",
                    "      EOF)",
                    &gutter,
                ]),
            ] {
                if let Some(w) = read(&wrapped) {
                    wrong.push(format!("first row under a wrapped call: {w:?}"));
                }
            }
            // A `⎿` the output printed: at column 5 (after a blank row of the
            // output or not), at any column but 2.
            for (name, r) in [
                (
                    "column 5, after a blank row",
                    screen_of(&[
                        "⏺ Bash(cat issue.txt)",
                        "  ⎿  Issue 7: the parser drops a row",
                        "",
                        &format!("     ⎿  {wall}"),
                    ]),
                ),
                (
                    "column 5",
                    screen_of(&[
                        "⏺ Bash(cat issue.txt)",
                        "  ⎿  Issue 7: the parser drops a row",
                        &format!("     ⎿  {wall}"),
                    ]),
                ),
                (
                    "column 3, under no call",
                    screen_of(&["⏺ The runs are queued.", &format!("   ⎿  {wall}")]),
                ),
            ] {
                if let Some(w) = read(&r) {
                    wrong.push(format!("{name}: {w:?}"));
                }
            }
            // No owner the walk reaches: read.
            for (name, r) in [
                (
                    "scrolled off",
                    framed(&[&gutter, "     more output", ""], FOOTER),
                ),
                (
                    "under a long output whose call scrolled off",
                    screen_of(&["     78", "     79", "     80", &gutter]),
                ),
                (
                    "under a prompt's second paragraph",
                    screen_of(&[
                        "❯ Please fix the parser.",
                        "",
                        "  The second paragraph says what done looks like.",
                        &gutter,
                    ]),
                ),
                (
                    "under a wrapped group row",
                    screen_of(&[
                        "❯ toolstart",
                        "",
                        "  Searched for 2 patterns, read 1 file (ctrl+o",
                        "  to expand)",
                        &gutter,
                    ]),
                ),
                // A blank row ends the walk where the row under it opens a new
                // item, so a call's block above it does not own the wall.
                (
                    "under a group row behind a call's block",
                    screen_of(&[
                        "⏺ Bash(ls)",
                        "  ⎿  out",
                        "",
                        "  Read 1 file (ctrl+o to expand)",
                        &gutter,
                    ]),
                ),
            ] {
                if read(&r) != want {
                    wrong.push(format!("{name}: {:?}", read(&r)));
                }
            }
            // CONTROLS: every other owner's column-2 rows are Claude Code's.
            for owner in [
                &["❯ fourth turn: the stub says the prompt is too long"][..],
                &["⏺ Suites are running; I'll report the number when it lands."],
                &["⏺ Here is the plan:"],
                &["⏺ Done:", "  - parser fixed", "  - tests added"],
                &["⏺ Dynamic workflow \"Second review pass\" completed · 5s"],
                &["⏺ Background command \"Run the cache tests\" completed (exit code 0)"],
                &["⏺ Goal check-in: background work still running"],
                &["⏺ The runs are queued.", ""],
                &["⏺ Monitor event: worker s-1 screen changed"],
                &["❯ toolstart", "", "  Read 1 file (ctrl+o to expand)"],
                &[
                    "❯ toolstart",
                    "",
                    "  Called github 2 times (ctrl+o to expand)",
                ],
                &[
                    "⏺ Ran 1 stop hook (ctrl+o to expand)",
                    "  ⎿  Stop hook error: check the tests",
                ],
            ] {
                let mut body = owner.to_vec();
                body.push(&gutter);
                if read(&screen_of(&body)) != want {
                    wrong.push(format!("control {owner:?}: {:?}", read(&screen_of(&body))));
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The owner walk on the screens it was measured on (Claude Code 2.1.294,
    /// the real-Claude lab, 2026-10-08). WALLS, each a real context wall
    /// read from the gutter: under a collapsed tool group, under a Stop
    /// hook's summary, under a Bash call's output (plain, holding a blank
    /// row, a PostToolUse hook's row between), under a prompt's second
    /// paragraph, under an MCP call's one-block output, under a long output
    /// whose call scrolled off, and — hand-built from two measured screens —
    /// under a group row wrapped in a narrow pane. Pass 1 of the fix read
    /// neither of the first two, pass 2 none of the last four, and main none
    /// under a call. OUTPUTS the screen tells from a wall, each the last
    /// thing said, `idle` and no wall: an MCP result's first row, a
    /// shell-mode command's output, a file's own `⎿` at column 5, and a
    /// heredoc command's output (the first row under its call, the walk gone
    /// on over the command's own blank row). OUTPUTS it cannot tell from
    /// one, READ as walls here and refused by the session's transcript
    /// (aterm-agent, `supervise/wall_confirm.rs`): an MCP result of three
    /// blocks (its last a login phrase), two of two whose first block holds
    /// or ends on a blank line, one whose long first block scrolled its call
    /// off, and a
    /// Bash output under a PreToolUse hook's row and under its error rows.
    /// Pass 2 read none of those, at the price of the walls above. Each wall
    /// says whether it was read under a call ([`Wall::under_call`]); of the
    /// outputs, all but the scrolled-off MCP block are (main read the MCP
    /// ones as walls: its walk did not know the MCP call); of the walls, the
    /// ones after a Bash, a PostToolUse hook's and an MCP output, a blank row
    /// in the output or not (since the walk goes on over a blank row inside
    /// an output, ruled 2026-10-08).
    #[test]
    fn the_owner_walk_on_its_measured_screens() {
        use crate::prompt::fixtures::{
            BASH_BLANK_ROW_THEN_CONTEXT_WALL_MEASURED, BASH_OUTPUT_THEN_CONTEXT_WALL_MEASURED,
            COLLAPSED_GROUP_CONTEXT_WALL_MEASURED, GUTTER_PHRASE_IN_OUTPUT_MEASURED,
            LONG_OUTPUT_CONTEXT_WALL_MEASURED,
            MCP_BLANK_ROW_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED,
            MCP_LONG_BLOCK_THEN_WALL_PHRASE_MEASURED, MCP_MULTI_BLOCK_OUTPUT_MEASURED,
            MCP_OUTPUT_ENDS_TURN_MEASURED, MCP_OUTPUT_THEN_CONTEXT_WALL_MEASURED,
            MCP_TRAILING_BLANK_ROWS_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED,
            MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED, MULTILINE_COMMAND_OUTPUT_MEASURED,
            POST_HOOK_THEN_CONTEXT_WALL_MEASURED, PRE_HOOK_ABOVE_OUTPUT_MEASURED,
            PRE_HOOK_ERROR_ABOVE_OUTPUT_MEASURED, SHELL_MODE_OUTPUT_ENDS_TURN_MEASURED,
            STOP_HOOK_CONTEXT_WALL_MEASURED, WRAPPED_GROUP_ROW_CONTEXT_WALL,
        };
        let read = |capture: &str| {
            let r = screen(capture);
            let reading = crate::reader::read(Some("claude"), &r, None);
            assert_eq!(
                wall(&r).map(|w| w.kind),
                reading.wall.as_ref().map(|w| w.kind),
                "the reader and `wall` agree"
            );
            (wall(&r), reading)
        };
        const CONTEXT: &str = "Context limit reached · /compact or /clear to continue";
        // Each with whether it was read UNDER A CALL (a later column-2 row
        // under it): the walls a supervisor with no transcript leaves alone.
        for (name, capture, under_call) in [
            (
                "collapsed group",
                COLLAPSED_GROUP_CONTEXT_WALL_MEASURED,
                false,
            ),
            ("stop hook", STOP_HOOK_CONTEXT_WALL_MEASURED, false),
            ("bash output", BASH_OUTPUT_THEN_CONTEXT_WALL_MEASURED, true),
            // Under the call since the walk goes on over the output's
            // blank row (pass 5 read it under no owner).
            (
                "bash output, a blank row",
                BASH_BLANK_ROW_THEN_CONTEXT_WALL_MEASURED,
                true,
            ),
            ("post hook", POST_HOOK_THEN_CONTEXT_WALL_MEASURED, true),
            (
                "a prompt's second paragraph",
                MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED,
                false,
            ),
            ("mcp output", MCP_OUTPUT_THEN_CONTEXT_WALL_MEASURED, true),
            ("long output", LONG_OUTPUT_CONTEXT_WALL_MEASURED, false),
        ] {
            let (w, reading) = read(capture);
            let w = w.unwrap_or_else(|| panic!("{name}: the wall"));
            assert_eq!(
                (w.kind, w.placement, w.message.as_str(), w.under_call),
                (WallKind::Context, Placement::Gutter, CONTEXT, under_call),
                "{name}"
            );
            assert_eq!(
                reading.wall.map(|w| w.under_call),
                Some(under_call),
                "{name}"
            );
        }
        let (w, _) = read(WRAPPED_GROUP_ROW_CONTEXT_WALL);
        assert_eq!(
            w.map(|w| (w.kind, w.placement, w.under_call)),
            Some((WallKind::Context, Placement::Gutter, false)),
            "a wrapped group row"
        );
        for (name, capture) in [
            ("mcp", MCP_OUTPUT_ENDS_TURN_MEASURED),
            ("shell mode", SHELL_MODE_OUTPUT_ENDS_TURN_MEASURED),
            ("column 5", GUTTER_PHRASE_IN_OUTPUT_MEASURED),
            // The first row under its call, the walk gone on over the
            // command's own blank row (pass 5 read it under no owner).
            ("heredoc", MULTILINE_COMMAND_OUTPUT_MEASURED),
        ] {
            let (w, reading) = read(capture);
            assert_eq!(w, None, "{name}");
            assert_eq!(reading.phase, Phase::Idle, "{name}");
            assert!(reading.phase_authoritative, "{name}");
        }
        // Under a call: the MCP ones, which main read as walls (it did not
        // know the MCP call), the two whose first block holds or ends on a
        // blank line among them (pass 5 read them under no owner: its walk
        // stopped at the blank row), and the two hook ones, which main never
        // read (it knew `⏺ Bash(…)`); not under one: the one main read as a
        // wall too (its walk reached the top of the screen as this one does).
        for (name, capture, under_call) in [
            ("mcp, three blocks", MCP_MULTI_BLOCK_OUTPUT_MEASURED, true),
            (
                "mcp, a blank row in the first block",
                MCP_BLANK_ROW_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED,
                true,
            ),
            (
                "mcp, the first block ending on a blank line",
                MCP_TRAILING_BLANK_ROWS_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED,
                true,
            ),
            (
                "mcp, a long block then another",
                MCP_LONG_BLOCK_THEN_WALL_PHRASE_MEASURED,
                false,
            ),
            ("pre hook", PRE_HOOK_ABOVE_OUTPUT_MEASURED, true),
            ("pre hook error", PRE_HOOK_ERROR_ABOVE_OUTPUT_MEASURED, true),
        ] {
            let (w, _) = read(capture);
            assert_eq!(
                w.map(|w| (w.kind, w.placement, w.under_call)),
                Some((WallKind::Auth, Placement::Gutter, under_call)),
                "{name}: read, for the transcript to refuse"
            );
        }
    }

    /// THE WALK NEEDS EVERY ROW THE SCREEN SHOWS (the review of pass 4,
    /// measured): an MCP result's second block, a weekly limit's words, on a
    /// 50-row pane whose call row is row 9 and whose last 31 rows are blank.
    /// The whole screen reads the limit UNDER THE CALL; the pane's last 40
    /// rows — the tail a supervisor reads — hold the limit and not the call,
    /// and the walk, reaching their top, reads it under no owner. So whoever
    /// acts on [`Wall::under_call`] reads the whole screen for it
    /// (aterm-agent, `supervise/wall_confirm.rs`).
    #[test]
    fn a_wall_is_under_its_call_on_the_whole_screen_and_not_on_a_tail_that_cut_the_call() {
        use crate::prompt::fixtures::MCP_LIMIT_PHRASE_CALL_ROW_ABOVE_TAIL_MEASURED;
        let whole = screen(MCP_LIMIT_PHRASE_CALL_ROW_ABOVE_TAIL_MEASURED);
        assert_eq!(whole.len(), 50);
        assert_eq!(whole[9], "⏺ github - get_limit (MCP)(n: 7)");
        let tail = &whole[whole.len() - 40..];
        assert!(
            tail[0].starts_with("  ⎿") && tail[0].ends_with("Issue 7: the parser drops a row"),
            "the tail opens on the result's first block: {:?}",
            tail[0]
        );
        let read = |rows: &[String]| {
            crate::reader::read(Some("claude"), rows, None)
                .wall
                .map(|w| (w.kind, w.placement, w.under_call))
        };
        assert_eq!(
            read(&whole),
            Some((WallKind::UsageWeekly, Placement::Gutter, true)),
            "the whole screen"
        );
        assert_eq!(
            read(tail),
            Some((WallKind::UsageWeekly, Placement::Gutter, false)),
            "the tail"
        );
    }

    /// THE LOGIN WALL OF 2026-09-27 (tab `s-b5cf2faabac5ce5127bd`): Claude
    /// Code 2.1.281 answered every turn for nine hours with its synthetic
    /// `authentication_failed` row, drawn `⏺ Login expired · Please run
    /// /login` in column 0 — read as idle with NO wall (the same words under
    /// the `⎿` gutter read as `auth`), so the supervisor continued it and
    /// the upgrade announced into it. Read as the auth wall,
    /// from the vendor's error row: alone, over a done row, wrapped by a
    /// narrow window, and in its other measured wording;
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

    /// THE LOGIN BACK, ITS ROW WRAPPED (HAND-BUILT: the measured row `⎿  Login
    /// successful · account or organization changed` as Ink's word wrap breaks
    /// it in a pane narrower than its 56 columns, here 40; the lab cannot
    /// finish a `/login`, having no egress). The last thing said is the row's
    /// rest, `organization changed`: read a row at a time, nothing said the
    /// login is back. NEGATIVE CONTROL: the same rows under a tool's call.
    #[test]
    fn a_login_back_wrapped_in_a_narrow_pane_is_read() {
        let rule = "─".repeat(40);
        let screen_with = |call: &str| {
            let mut v = rows(&[
                "⏺ Login expired · Please run /login",
                "",
                call,
                "  ⎿  Login successful · account or",
                "     organization changed",
                "",
            ]);
            v.extend(rows(&[&rule, "❯", &rule, "  ? for shortcuts"]));
            v
        };
        let back = screen_with("❯ /login");
        assert_eq!(wall(&back), None);
        assert!(login_restored(&back));
        assert!(!login_restored(&screen_with("⏺ Bash(tail -2 login.log)")));
    }

    /// HOW THE VENDOR'S OWN AUTO-CONTINUE ENDED, on the lab's captures of
    /// Claude Code 2.1.296: armed, the wall stands and its countdown is drawn
    /// (`Armed`: nothing ended it); after
    /// Esc on its countdown, unkept — the transcript's row and the footer's
    /// notification each say so alone; past the reset, fired. The lifecycle's
    /// other words in the cancelled row's place read as theirs. NEGATIVE
    /// CONTROLS: a turn's `❯` row under the word makes it history (a person's
    /// `/usage` after the cancel); a limit notice under it stands (the
    /// vendor's continuation met the limit again); the armed countdown is no
    /// end (`Armed`); a screen with no composer says nothing.
    #[test]
    fn the_auto_continues_end_is_read_off_the_vendors_word() {
        use crate::prompt::fixtures::{
            WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
            WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED, WALL_SESSION_RESUMED_BY_ITSELF_MEASURED,
            screen,
        };
        let armed = screen(WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED);
        let cancelled = screen(WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED);
        let resumed = screen(WALL_SESSION_RESUMED_BY_ITSELF_MEASURED);
        assert!(wall(&armed).is_some());
        assert_eq!(auto_continue_end(&armed), Some(AutoContinue::Armed));
        for left in [&cancelled, &resumed] {
            assert_eq!(wall(left), None);
        }
        assert_eq!(auto_continue_end(&cancelled), Some(AutoContinue::Unkept));
        assert_eq!(auto_continue_end(&resumed), Some(AutoContinue::Fired));
        let row = |rows: &[String], opens: &str| {
            rows.iter()
                .position(|r| r.starts_with(opens))
                .unwrap_or_else(|| panic!("no {opens:?} row"))
        };
        let cancel_row = row(&cancelled, "⏺ Automatic continue cancelled");
        let footer = row(&cancelled, "  ⏵⏵ auto mode on");
        // Each place says it alone.
        let mut only_footer = cancelled.clone();
        only_footer[cancel_row] = String::new();
        assert_eq!(auto_continue_end(&only_footer), Some(AutoContinue::Unkept));
        let mut only_transcript = cancelled.clone();
        only_transcript[footer] =
            "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents".to_string();
        assert_eq!(
            auto_continue_end(&only_transcript),
            Some(AutoContinue::Unkept)
        );
        // The lifecycle's other words, in the transcript row's place.
        for (said, want) in [
            (
                "⏺ Usage limit available again · continuing now",
                AutoContinue::Fired,
            ),
            (
                "⏺ Automatic continue stopped · the usage limit now resets more than 24 hours \
                 out, so this task will not resume on its own",
                AutoContinue::Unkept,
            ),
            (
                "⏺ Automatic continue was turned off · this task will not resume on its own",
                AutoContinue::Unkept,
            ),
            (
                "⏺ Usage limit has reset · press enter to continue",
                AutoContinue::Unkept,
            ),
            (
                "⏺ Your usage limit has reset · press enter to continue",
                AutoContinue::Unkept,
            ),
            (
                "⚠ Usage limit reset ∙ continuing automatically",
                AutoContinue::Fired,
            ),
        ] {
            let mut v = only_transcript.clone();
            v[cancel_row] = said.to_string();
            assert_eq!(auto_continue_end(&v), Some(want), "{said}");
        }
        // NEGATIVE CONTROLS. A turn after the word.
        let mut later = only_transcript.clone();
        later.splice(
            cancel_row + 1..cancel_row + 1,
            ["", "❯ /usage", "  ⎿  Status dialog dismissed"].map(str::to_string),
        );
        assert_eq!(auto_continue_end(&later), None);
        // A limit under the vendor's word: its continuation met it again.
        let fired = row(&resumed, "⏺ Usage limit reset");
        let mut again = resumed.clone();
        again.splice(
            fired + 1..fired + 1,
            [
                "",
                "⏺ Usage limit reached again · continuing automatically at 8:50pm · esc to cancel",
            ]
            .map(str::to_string),
        );
        assert_eq!(auto_continue_end(&again), None);
        // The countdown alone is no end: it stands. No composer says nothing.
        let mut countdown = only_transcript.clone();
        countdown[cancel_row] = "  Continuing automatically at 3:50pm · esc to cancel".to_string();
        assert_eq!(auto_continue_end(&countdown), Some(AutoContinue::Armed));
        let top = cancelled
            .iter()
            .rposition(|r| r.starts_with('─'))
            .expect("rule");
        assert_eq!(auto_continue_end(&cancelled[..top]), None);
    }

    /// WHAT A `/RESUME` AT AN ARMED LIMIT LEAVES OF ITS COUNTDOWN (W1 and W4
    /// of ATERM-KI-1015-1, the lab on Claude Code 2.1.300): armed, the
    /// countdown is drawn (`Armed`); after a `/resume`, the history redrawn
    /// says what the vendor did with ITS OWN limit — `Fired` under that
    /// limit's notice (`8:48am`), which names it, not the armed one
    /// (`9:19am`) — or nothing at all, where the history holds no limit: the
    /// countdown dropped without a word. A `/clear` the same.
    #[test]
    fn a_resume_at_an_armed_limit_draws_no_countdown() {
        use crate::prompt::fixtures::{
            CLEARED_AT_ARMED_LIMIT_2_1_300, RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
            RESUMED_OLDER_LIMIT_CONTINUED_AT_ARMED_LIMIT_2_1_300,
            WALL_SESSION_ARMED_CONVERSATION_B_2_1_300,
        };
        let armed = screen(WALL_SESSION_ARMED_CONVERSATION_B_2_1_300);
        assert_eq!(
            wall(&armed).and_then(|w| w.reset).as_deref(),
            Some("9:19am")
        );
        assert_eq!(auto_continue_end(&armed), Some(AutoContinue::Armed));
        let older = screen(RESUMED_OLDER_LIMIT_CONTINUED_AT_ARMED_LIMIT_2_1_300);
        assert_eq!(wall(&older), None);
        assert_eq!(auto_continue_end(&older), Some(AutoContinue::Fired));
        assert_eq!(
            since_limit(&older),
            Some(SinceLimit {
                kind: WallKind::UsageSession,
                reset: Some("8:48am".to_string()),
                served: false,
                model_set: false,
                chosen_model: None,
            })
        );
        for dropped in [
            RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
            CLEARED_AT_ARMED_LIMIT_2_1_300,
        ] {
            let rows = screen(dropped);
            assert_eq!(
                (wall(&rows), auto_continue_end(&rows), since_limit(&rows)),
                (None, None, None),
                "{rows:#?}"
            );
        }
    }

    /// THE NEWEST LIMIT AND A TURN SERVED SINCE IT, read off the screen's
    /// order (Claude Code 2.1.299 in the lab): Esc on the countdown, then a
    /// person's short turn under the cancel — the prompt drawn below the
    /// limit's notice, the reply under the prompt — is a served turn since
    /// that notice, whose reset (`10:38am`) says which limit it is; so is one
    /// after a person's `/usage`. NEGATIVE CONTROLS, none served: the armed
    /// wall and the cancel alone (the only prompt is the one that met the
    /// limit, above its notice); the vendor's own continuation (no `❯` row);
    /// a local command in the prompt's place; a turn that met the limit again
    /// (its notice below the prompt, the newest, with its own reset); a prompt
    /// the API never answered (Esc before its reply, `⎿  Interrupted · …`).
    /// No limit at all: the same turn with the notice off the screen (nothing
    /// orders it), and a screen with no composer.
    #[test]
    fn the_newest_limit_and_a_turn_served_since_are_read_off_the_screens_order() {
        use crate::prompt::fixtures::{
            WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
            WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
            WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED,
            WALL_SESSION_RESUMED_BY_ITSELF_MEASURED, screen,
        };
        let since = |kind: WallKind, reset: &str, served: bool| {
            Some(SinceLimit {
                kind,
                reset: Some(reset.to_string()),
                served,
                model_set: false,
                chosen_model: None,
            })
        };
        let turned = screen(WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED);
        assert_eq!(wall(&turned), None);
        let countdown = classify_wall(
            "⏺ Usage limit reached · continuing automatically at 10:38am · esc to cancel",
        )
        .expect("the countdown reads as the limit");
        assert_eq!(since_limit(&turned), since(countdown, "10:38am", true));
        let row = |opens: &str| {
            turned
                .iter()
                .position(|r| r.starts_with(opens))
                .unwrap_or_else(|| panic!("no {opens:?} row"))
        };
        let prompt = row("❯ what is left to do?");
        let mut after_usage = turned.clone();
        after_usage.splice(
            prompt..prompt,
            ["❯ /usage", "  ⎿  Status dialog dismissed", ""].map(str::to_string),
        );
        assert_eq!(since_limit(&after_usage), since(countdown, "10:38am", true));
        // NEGATIVE CONTROLS: a limit, nothing served since.
        for (name, text, reset) in [
            ("armed", WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED, "3:15pm"),
            (
                "cancelled",
                WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
                "3:50pm",
            ),
            ("resumed", WALL_SESSION_RESUMED_BY_ITSELF_MEASURED, "3:35pm"),
        ] {
            let read = since_limit(&screen(text)).unwrap_or_else(|| panic!("{name}: no limit"));
            assert_eq!(
                (read.served, read.reset.as_deref()),
                (false, Some(reset)),
                "{name}"
            );
        }
        let mut local = turned.clone();
        local[prompt] = "❯ /usage".to_string();
        assert_eq!(since_limit(&local), since(countdown, "10:38am", false));
        let reply = row("⏺ Hi! I am the local lab stub, not a model. (reply 3)");
        let mut met_again = turned.clone();
        met_again[reply] =
            "  ⎿  You've hit your session limit · resets 11:38am (America/Los_Angeles)".to_string();
        assert_eq!(
            since_limit(&met_again),
            since(
                WallKind::UsageSession,
                "11:38am (America/Los_Angeles)",
                false
            )
        );
        let mut unanswered = turned.clone();
        unanswered[reply] = "  ⎿  Interrupted · What should Claude do instead?".to_string();
        assert_eq!(since_limit(&unanswered), since(countdown, "10:38am", false));
        // A `/goal` echo names no reset: the notice it echoes does (2.1.283,
        // the weekly wall measured on the owner's tab).
        let weekly = screen(include_str!(
            "fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
        ));
        assert_eq!(
            since_limit(&weekly),
            since(WallKind::UsageWeekly, "4pm (America/Los_Angeles)", false)
        );
        // No limit on the screen.
        let notice = row("❯ carry on with the task");
        let cancel = row("⏺ Automatic continue cancelled");
        let mut unordered = turned.clone();
        unordered.drain(notice..=cancel);
        assert_eq!(since_limit(&unordered), None);
        let top = turned
            .iter()
            .rposition(|r| r.starts_with('─'))
            .expect("rule");
        assert_eq!(since_limit(&turned[..top]), None);
    }

    /// A MODEL CHOSEN SINCE THE NEWEST LIMIT is read off the screen's order
    /// ([`SinceLimit::model_set`]): a person's `/model` below the notice, its
    /// `⎿  Set model to …` under the prompt. Measured on Claude Code 2.1.28x
    /// (the owner's tab): a Fable bucket's notice that names no reset, then
    /// `❯ /model` and its result. CONSTRUCTED from the lab's 2.1.300 Sonnet
    /// bucket (the countdown off the screen, `/model` and its result under
    /// it): the bucket's notice and its reset. NEGATIVE CONTROLS, none
    /// chosen: the armed bucket; the same result row under another local
    /// command; a `/model` ABOVE the notice (chosen before the limit); the
    /// words in a reply of the model's.
    #[test]
    fn a_model_chosen_since_the_newest_limit_is_read_off_the_screens_order() {
        use crate::prompt::fixtures::{
            IDLE_AFTER_LIMIT_AND_MODEL_SWITCH, WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300,
            screen,
        };
        let measured = screen(IDLE_AFTER_LIMIT_AND_MODEL_SWITCH);
        let read = since_limit(&measured).expect("the Fable bucket's notice");
        assert_eq!(
            (read.kind, read.reset, read.served, read.model_set),
            (WallKind::ModelBucket { consent: false }, None, false, true)
        );
        assert_eq!(
            read.chosen_model.as_deref(),
            Some("Opus 5 (1M context) (default) and saved as your default for new sessions")
        );
        let armed = screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
        let reset = "Oct 17 at 4:20pm (America/Los_Angeles)";
        let bucket = |model_set: bool| {
            Some(SinceLimit {
                kind: WallKind::ModelBucket { consent: false },
                reset: Some(reset.to_string()),
                served: false,
                model_set,
                chosen_model: model_set
                    .then(|| "Opus 5.5 and saved as your default for new sessions".to_string()),
            })
        };
        let set_row = "  ⎿  Set model to Opus 5.5 and saved as your default for new sessions";
        let under = |prompt: &str, result: &str| {
            let mut rows: Vec<String> = armed
                .iter()
                .filter(|r| {
                    !r.contains("Usage limit reached") && !r.contains("Continuing automatically")
                })
                .cloned()
                .collect();
            let top = rows
                .iter()
                .position(|r| r.starts_with('─'))
                .expect("the frame");
            rows.splice(top..top, [prompt, result, ""].map(str::to_string));
            rows
        };
        assert_eq!(since_limit(&under("❯ /model", set_row)), bucket(true));
        assert_eq!(since_limit(&under("❯ /model opus", set_row)), bucket(true));
        let mut twice = under("❯ /model", set_row);
        let top = twice.iter().position(|r| r.starts_with('─')).unwrap();
        twice.splice(
            top..top,
            ["❯ /model", "  ⎿  Set model to new-default", ""].map(str::to_string),
        );
        assert_eq!(
            since_limit(&twice).and_then(|s| s.chosen_model),
            Some("new-default".to_string()),
            "the newest completed choice stands even when its model is unknown"
        );
        // NEGATIVE CONTROLS.
        assert_eq!(since_limit(&armed), bucket(false));
        assert_eq!(since_limit(&under("❯ /usage", set_row)), bucket(false));
        assert_eq!(
            since_limit(&under("❯ /models-list", set_row)),
            bucket(false)
        );
        let notice = armed
            .iter()
            .position(|r| r.contains("You've hit your Sonnet limit"))
            .expect("the notice");
        let mut before = armed.clone();
        before.splice(
            notice - 1..notice - 1,
            ["❯ /model", set_row, ""].map(str::to_string),
        );
        assert_eq!(since_limit(&before), bucket(false));
        let quoted = under(
            "❯ what did /model print?",
            "⏺ It printed: Set model to Opus 5.5 and saved as your default.",
        );
        assert_eq!(
            since_limit(&quoted).map(|s| s.model_set),
            Some(false),
            "{quoted:#?}"
        );
    }

    /// THE LIMIT A CANCEL LEFT is read as the wall its notice was (round 16's
    /// residual (a), measured on 2.1.296 and, at 26 columns, 2.1.299): the
    /// vendor's `Automatic continue cancelled` under the newest notice, nothing
    /// served since — the notice's kind, its words, its reset, its row, the
    /// vendor's `⏺` row's placement. At 26 columns the countdown's line is
    /// read across its rows (`⏺ Usage limit` over `reached ·` over
    /// `continuing`), and so is the cancel's (`⏺ Automatic` over `continue`),
    /// which a row at a time read as a reply of the model's. NEGATIVE
    /// CONTROLS: the armed wall (no cancel: the reader's own wall), the
    /// vendor's own continuation (`Fired`), a person's turn served under the
    /// cancel, a local command after it (a new turn: the cancel is history),
    /// a turn sent into the limit after the cancel (its notice is the newest
    /// and stands: the reader's wall), a notice naming no reset, and no
    /// composer.
    #[test]
    fn the_limit_a_cancel_left_is_read_as_the_wall_it_was() {
        use crate::prompt::fixtures::{
            WALL_SESSION_AFTER_CANCEL_26_2_1_299, WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
            WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299,
            WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
            WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED,
            WALL_SESSION_RESUMED_BY_ITSELF_MEASURED, screen,
        };
        let cancelled = screen(WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED);
        let countdown = cancelled
            .iter()
            .rposition(|r| r.starts_with("⏺ Usage limit reached"))
            .expect("the countdown row");
        assert_eq!(wall(&cancelled), None);
        assert_eq!(
            left_wall(&cancelled),
            Some(Wall {
                kind: WallKind::UsageSession,
                message: "Usage limit reached · continuing automatically at 3:50pm · esc to cancel"
                    .to_string(),
                reset: Some("3:50pm".to_string()),
                row: countdown,
                placement: Placement::ErrorRow,
                under_call: false,
            })
        );
        // At 26 columns, every line wrapped.
        let narrow = screen(WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299);
        assert_eq!(wall(&narrow), None);
        assert_eq!(
            since_limit(&narrow),
            Some(SinceLimit {
                kind: WallKind::UsageSession,
                reset: Some("10:50am".to_string()),
                served: false,
                model_set: false,
                chosen_model: None,
            })
        );
        let left = left_wall(&narrow).expect("the narrow cancel's limit");
        assert_eq!(
            (
                left.kind,
                left.reset.as_deref(),
                left.placement,
                narrow[left.row].as_str()
            ),
            (
                WallKind::UsageSession,
                Some("10:50am"),
                Placement::ErrorRow,
                "⏺ Usage limit"
            )
        );
        assert!(
            left.message
                .starts_with("Usage limit reached · continuing automatically at 10:50am"),
            "{left:?}"
        );
        // Under a `/goal` echo of the notice (2.1.283's weekly wall, the
        // owner's tab), the cancel drawn under the echo: the notice under the
        // `⎿` gutter, its reset the echo's.
        let mut goal = screen(include_str!(
            "fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
        ));
        let done = goal
            .iter()
            .position(|r| r.starts_with("✻ Sautéed"))
            .expect("the done row");
        goal.splice(
            done..done,
            [
                "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
                "",
            ]
            .map(str::to_string),
        );
        let notice = goal
            .iter()
            .position(|r| r.contains("You've hit your weekly limit"))
            .expect("the notice");
        let left = left_wall(&goal).expect("the goal's limit");
        assert_eq!(
            (left.kind, left.reset.as_deref(), left.placement, left.row),
            (
                WallKind::UsageWeekly,
                Some("4pm (America/Los_Angeles)"),
                Placement::Gutter,
                notice
            ),
            "{left:?}"
        );
        // NEGATIVE CONTROLS.
        for (name, rows) in [
            ("armed", screen(WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED)),
            ("fired", screen(WALL_SESSION_RESUMED_BY_ITSELF_MEASURED)),
            (
                "served",
                screen(WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED),
            ),
            ("met again", screen(WALL_SESSION_AFTER_CANCEL_26_2_1_299)),
        ] {
            assert_eq!(left_wall(&rows), None, "{name}");
        }
        let cancel = cancelled
            .iter()
            .position(|r| r.starts_with("⏺ Automatic continue cancelled"))
            .expect("the cancel row");
        let mut local = cancelled.clone();
        local.splice(
            cancel + 1..cancel + 1,
            ["", "❯ /usage", "  ⎿  Status dialog dismissed"].map(str::to_string),
        );
        let footer = local
            .iter()
            .position(|r| r.starts_with("  ⏵⏵ auto mode on"))
            .expect("the footer");
        local[footer] = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents".to_string();
        assert_eq!(left_wall(&local), None, "a local command after the cancel");
        let mut no_reset = cancelled.clone();
        no_reset[countdown] = "⏺ Usage limit reached · esc to cancel".to_string();
        for r in &mut no_reset {
            *r = r.replace("resets 3:50pm (America/Los_Angeles)", "limit reached");
        }
        assert_eq!(left_wall(&no_reset), None, "no reset named");
        // A tool's output that quotes the notice and the cancel: with the
        // model's reply after it, or as the last thing said (its rows no `⏺`
        // row in column 0), it is no cancel of the vendor's.
        let asked = cancelled
            .iter()
            .rposition(|r| r == "❯ carry on with the task")
            .expect("the prompt the limit met");
        let quoting = |reply: bool| {
            let mut q = cancelled.clone();
            let mut rows = vec![
                "❯ read the notes",
                "",
                "⏺ Bash(cat notes.txt)",
                "  ⎿  You've hit your session limit · resets 3:50pm (America/Los_Angeles)",
                "     ⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
            ];
            if reply {
                rows.extend(["", "⏺ The notes quote a limit notice."]);
            }
            q.splice(asked..=cancel, rows.into_iter().map(str::to_string));
            q
        };
        for reply in [true, false] {
            let q = quoting(reply);
            assert_eq!(
                auto_continue_end(&q),
                Some(AutoContinue::Unkept),
                "the footer still says it"
            );
            assert_eq!(left_wall(&q), None, "quoted by a tool (reply {reply})");
        }
        let top = cancelled
            .iter()
            .rposition(|r| r.starts_with('─'))
            .expect("rule");
        assert_eq!(left_wall(&cancelled[..top]), None, "no composer");
    }

    /// A WALL'S LINE INK WRAPPED IN A NARROW PANE IS READ AS ONE (found live by
    /// the 2026-10-09 sweep's reader review; measured on Claude Code 2.1.299
    /// in the lab, 2026-10-14). In a pane narrower than one of its lines Ink
    /// word-wraps the line onto the rows under it: the footer's banner
    /// (`⚠ Usage limit reached · limit` over `resets 10:25am` at 34 columns,
    /// `⚠ Usage limit reached` over `· limit resets 10:51am` at 26), its
    /// countdown (`Continuing automatically at` over `10:25am · esc to
    /// cancel`; `Continuing` alone at 26), the turn's `⎿` notice (`You've hit
    /// your` over `session limit ·` at 26) and the cancel's `⏺` row (`⏺
    /// Automatic` over `continue` over `cancelled · /rat`). Read a row at a
    /// time, the banner named no reset — the harness waited on its 30-minute
    /// backstop (`WAITING until=…17:50:32Z` for a 17:25:30Z reset) and, after
    /// Esc on the countdown, typed `keep going` into the standing limit at
    /// once — the countdown at 26 columns said nothing, the turn's notice was
    /// no wall at all (`idle`: the continue policy typed `keep going` into it
    /// every back-off), and the cancel no word of the vendor's.
    #[test]
    fn a_walls_line_ink_wrapped_is_read_as_one() {
        use crate::prompt::fixtures::{
            WALL_SESSION_AFTER_CANCEL_26_2_1_299, WALL_SESSION_AUTO_CONTINUE_ARMED_26_2_1_299,
            WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299,
            WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299,
            WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299,
        };
        let footer = |text: &str, banner: &str, countdown: &str, reset: &str| {
            let r = screen(text);
            let w = wall(&r).expect("the wall");
            assert_eq!(
                (w.kind, w.placement),
                (WallKind::UsageSession, Placement::Footer)
            );
            assert_eq!(w.message, format!("{banner} \u{2014} {countdown}"));
            assert_eq!(w.reset.as_deref(), Some(reset));
            assert_eq!(limit_notice(&r), Some((w.message, w.reset)));
        };
        footer(
            WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299,
            "⚠ Usage limit reached · limit resets 10:25am",
            "Continuing automatically at 10:25am · esc to cancel",
            "10:25am",
        );
        // The weekly limit's banner is the same generic one: the turn's own
        // notice names the limit, wrapped over three rows too
        // ([`a_generic_banner_is_the_limit_the_turns_own_notice_names`]).
        let r = screen(WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299);
        let w = wall(&r).expect("the weekly wall");
        assert_eq!(
            (w.kind, w.placement, w.reset.as_deref()),
            (
                WallKind::UsageWeekly,
                Placement::Footer,
                Some("Oct 15 at 12:33pm")
            )
        );
        assert_eq!(
            w.message,
            "You've hit your weekly limit · resets Oct 15 at 12:33pm (America/Los_Angeles) \
             \u{2014} ⚠ Usage limit reached · limit resets Oct 15 at 12:33pm \u{2014} Continuing \
             automatically at Oct 15 at 12:33pm · esc to cancel"
        );
        assert_eq!(limit_notice(&r), Some((w.message, w.reset)));
        footer(
            WALL_SESSION_AUTO_CONTINUE_ARMED_26_2_1_299,
            "⚠ Usage limit reached · limit resets 10:51am",
            "Continuing automatically at 10:51am · esc to cancel",
            "10:51am",
        );

        // The turn's notice, the last thing said once the banner has gone.
        let r = screen(WALL_SESSION_AFTER_CANCEL_26_2_1_299);
        let w = wall(&r).expect("the turn's notice");
        assert_eq!(
            (w.kind, w.placement),
            (WallKind::UsageSession, Placement::Gutter)
        );
        assert_eq!(
            w.message,
            "You've hit your session limit · resets 10:51am (America/Los_Angeles) /upgrade to \
             increase your usage limit."
        );
        assert_eq!(w.reset.as_deref(), Some("10:51am (America/Los_Angeles)"));
        assert_eq!(
            worker_phase(&r),
            Phase::Limited {
                message: w.message,
                reset: w.reset,
            }
        );

        // The cancel: no wall, and the vendor's word that it will not go on.
        let r = screen(WALL_SESSION_AUTO_CONTINUE_CANCELLED_26_2_1_299);
        assert_eq!(wall(&r), None);
        assert_eq!(auto_continue_end(&r), Some(AutoContinue::Unkept));

        // The transcript's own `⏺ Usage limit reached · continuing
        // automatically at 10:25am · esc to cancel`, the last thing said above
        // the composer (the footer's banner and countdown left out, as the
        // footer is read first): four rows at 34 columns, two at 80, where it
        // is read.
        let mut r = screen(WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299);
        let at = |r: &[String], opens: &str| r.iter().position(|x| x.starts_with(opens));
        let banner = at(&r, "  ⚠ Usage limit").expect("the banner");
        let mode = at(&r, "  ⏵⏵").expect("the mode line");
        r.drain(banner..mode);
        let w = wall(&r).expect("the transcript's notice");
        assert_eq!(
            (w.kind, w.placement),
            (WallKind::UsageSession, Placement::ErrorRow)
        );
        assert_eq!(
            w.message,
            "Usage limit reached · continuing automatically at 10:25am · esc to cancel"
        );
        assert_eq!(w.reset.as_deref(), Some("10:25am"));
    }

    /// A GENERIC BANNER IS THE LIMIT THE TURN'S OWN NOTICE NAMES (W3 of
    /// ATERM-KI-1015-1, the lab on Claude Code 2.1.300, 2026-10-15). From
    /// 2.1.295 on, every subscription limit's footer banner reads `⚠ Usage
    /// limit reached · limit resets …`, whatever the limit: only the turn's `⎿`
    /// notice names it. A Sonnet bucket (`You've hit your Sonnet limit`) read
    /// off the banner first was `usage-session`: the default `model_fallback =
    /// "opus"` never ran and the session waited two days for the bucket's
    /// reset; a weekly limit read `usage-session` too. Read as the turn's
    /// notice names it, with that notice's words opening the message (the
    /// bucket's model is read off them, and a loop that takes the wall up from
    /// its ledger row classifies the same words). NEGATIVE CONTROLS, the
    /// banner as it reads: a five-hour limit (the turn's notice of the same
    /// kind), a turn's notice naming ANOTHER reset (an older limit, not this
    /// one's), and one above a person's prompt (history).
    #[test]
    fn a_generic_banner_is_the_limit_the_turns_own_notice_names() {
        use crate::prompt::fixtures::{
            WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299,
            WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300,
            WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299,
        };
        let sonnet = screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
        let w = wall(&sonnet).expect("the Sonnet bucket");
        assert_eq!(
            (w.kind, w.placement, w.reset.as_deref()),
            (
                WallKind::ModelBucket { consent: false },
                Placement::Footer,
                Some("Oct 17 at 4:20pm")
            ),
            "{w:#?}"
        );
        assert_eq!(
            w.message,
            "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles) \u{2014} \
             ⚠ Usage limit reached · limit resets Oct 17 at 4:20pm \u{2014} Continuing \
             automatically at Oct 17 at 4:20pm · esc to cancel"
        );
        assert_eq!(classify_wall(&w.message), Some(w.kind));
        assert_eq!(limit_notice(&sonnet), Some((w.message, w.reset)));
        // A weekly limit at 34 columns: its notice wrapped over three rows.
        let weekly = screen(WALL_WEEKLY_AUTO_CONTINUE_ARMED_34_2_1_299);
        let w = wall(&weekly).expect("the weekly limit");
        assert_eq!(
            (w.kind, w.placement, w.reset.as_deref()),
            (
                WallKind::UsageWeekly,
                Placement::Footer,
                Some("Oct 15 at 12:33pm")
            ),
            "{w:#?}"
        );
        assert!(
            w.message.starts_with(
                "You've hit your weekly limit · resets Oct 15 at 12:33pm (America/Los_Angeles) \
                 \u{2014} ⚠ Usage limit reached"
            ),
            "{w:#?}"
        );

        // NEGATIVE CONTROLS. A five-hour limit: its banner as it reads.
        let session = screen(WALL_SESSION_AUTO_CONTINUE_ARMED_34_2_1_299);
        let w = wall(&session).expect("the session limit");
        assert_eq!(
            (w.kind, w.message.as_str()),
            (
                WallKind::UsageSession,
                "⚠ Usage limit reached · limit resets 10:25am \u{2014} Continuing automatically at \
                 10:25am · esc to cancel"
            )
        );
        let banner_only = |rows: &[String]| {
            let w = wall(rows).expect("the banner");
            (w.kind, w.message.starts_with("⚠ Usage limit reached"))
        };
        // The turn's notice names another reset: an older limit's.
        let older: Vec<String> = sonnet
            .iter()
            .map(|r| {
                r.replace(
                    "resets Oct 17 at 4:20pm (America",
                    "resets Oct 12 at 9:05am (America",
                )
            })
            .collect();
        assert_eq!(banner_only(&older), (WallKind::UsageSession, true));
        // The notice above a person's prompt: history.
        let notice = sonnet
            .iter()
            .position(|r| r.contains("You've hit your Sonnet limit"))
            .expect("the notice");
        let mut history = sonnet.clone();
        history.splice(
            notice + 1..notice + 1,
            ["", "❯ and the tests?", "", "⏺ Running them."].map(str::to_string),
        );
        assert_eq!(banner_only(&history), (WallKind::UsageSession, true));
    }

    /// The control: a line Ink wrapped where the reader always joined its
    /// rows — the `⏺ API Error` row (`error_row_notice`), here a 529 over
    /// eight rows of a 34-column pane, the done row's `AM` on a row of its own
    /// under it.
    #[test]
    fn an_api_error_row_wrapped_in_a_narrow_pane_is_read_whole() {
        let r = screen(crate::prompt::fixtures::API_ERROR_529_34_2_1_299);
        let w = wall(&r).expect("the 529");
        assert_eq!(
            (w.kind, w.placement),
            (WallKind::Overloaded, Placement::ErrorRow)
        );
        assert!(
            w.message
                .starts_with("API Error: 529 Overloaded. This is a server-side issue,")
                && w.message.ends_with("inference gateway (127.0.0.1:58931)."),
            "{}",
            w.message
        );
    }

    /// EVERY NOTICE'S LINE RUNS ON OVER THE ROWS INK WRAPPED IT ONTO, in the
    /// three places no capture shows a wrapped one in (the review of
    /// fix/r15-banner, 2026-10-14: each join could be undone with every test
    /// still green). HAND-PLACED, each a pane 26 or 34 columns wide:
    /// * the `⎿` notice's rows Claude Code 2.1.300 drew at 34 columns in UTC
    ///   while its own continue was armed (the lab, 2026-10-14: `⎿  You've
    ///   hit your session limit` over `· resets 8:15pm (UTC)`), here the last
    ///   thing said. Its last row opens with `·`, which reads as a status row,
    ///   so the last thing said ends on the row above it: the notice's line
    ///   runs on past it, or the reset is lost;
    /// * the footer's notification `Automatic continue cancelled ·
    ///   /rate-limit-options to re-arm`, wrapped as the banner and its
    ///   countdown wrap there (the measured one is cut instead, `Automatic
    ///   continue ca…`, and the transcript's own row says it);
    /// * the usage banner above the composer, wrapped as the footer's is at
    ///   26 columns (`⚠ Usage limit reached` over `· limit resets 10:51am`;
    ///   2.1.295 and later draw it in the footer, where it is measured).
    #[test]
    fn every_notices_line_runs_on_over_its_wrapped_rows() {
        let framed_at = |width: usize, body: &[&str], footer: &[&str]| {
            let rule = "─".repeat(width);
            let mut r = rows(body);
            r.extend([rule.clone(), "❯".to_string(), rule]);
            r.extend(rows(footer));
            r
        };
        let r = framed_at(
            34,
            &[
                "❯ carry on with the task",
                "  ⎿  You've hit your session limit",
                "     · resets 8:15pm (UTC)",
                "",
                "✻ Cogitated for 0s · done 8:05 PM",
                "",
            ],
            &["  ⏵⏵ auto mode on (shift+tab to"],
        );
        let w = wall(&r).expect("the turn's notice");
        assert_eq!(
            (w.kind, w.placement, w.reset.as_deref()),
            (
                WallKind::UsageSession,
                Placement::Gutter,
                Some("8:15pm (UTC)")
            )
        );
        assert_eq!(
            limit_notice(&r),
            Some((
                "You've hit your session limit · resets 8:15pm (UTC)".to_string(),
                Some("8:15pm (UTC)".to_string())
            ))
        );

        let r = framed_at(
            26,
            &[
                "❯ carry on with the task",
                "  ⎿  You've hit your",
                "     session limit ·",
                "     resets 10:51am",
                "     (America/Los_Angeles)",
                "",
                "✻ Worked for 0s · done",
                "  10:36 AM",
                "",
            ],
            &[
                "  ⏵⏵ auto mode on      ·",
                "  Automatic continue",
                "  cancelled ·",
                "  /rate-limit-options to",
                "  re-arm",
            ],
        );
        assert_eq!(auto_continue_end(&r), Some(AutoContinue::Unkept));

        let r = framed_at(
            26,
            &[
                "❯ carry on with the task",
                "",
                "⏺ On it.",
                "",
                "  ⚠ Usage limit reached",
                "  · limit resets 10:51am",
                "",
            ],
            &["  ? for shortcuts"],
        );
        let w = wall(&r).expect("the banner");
        assert_eq!(
            (w.kind, w.placement, w.reset.as_deref()),
            (WallKind::UsageSession, Placement::Banner, Some("10:51am"))
        );
        assert_eq!(w.message, "⚠ Usage limit reached · limit resets 10:51am");
    }

    /// `text` as Claude Code draws its error row or notice in a pane `width`
    /// columns wide (2.1.300's `su` and `ig`: a column `columns - 10` cells
    /// wide beside the `⏺`, Ink's greedy word wrap), the last thing said over
    /// a composer `width` cells wide. Panics on a word wider than the column
    /// (Ink would break it).
    fn vendor_box(text: &str, width: usize) -> Vec<String> {
        let room = width - 10;
        let mut lines: Vec<String> = vec![String::new()];
        for word in text.split(' ') {
            let n = word.chars().count();
            assert!(n <= room, "{word:?} is wider than {room} cells");
            let line = lines.last_mut().expect("a line");
            if line.is_empty() {
                line.push_str(word);
            } else if line.chars().count() + 1 + n <= room {
                line.push(' ');
                line.push_str(word);
            } else {
                lines.push(word.to_string());
            }
        }
        let mut r = rows(&["❯ carry on with the task", ""]);
        for (k, line) in lines.iter().enumerate() {
            r.push(format!("{}{line}", if k == 0 { "⏺ " } else { "  " }));
        }
        r.push(String::new());
        let rule = "─".repeat(width);
        r.extend([
            rule.clone(),
            "❯".to_string(),
            rule,
            "  ? for shortcuts".to_string(),
        ]);
        r
    }

    /// THE WORKER'S PROSE IS NO ERROR ROW, IN A PANE OF ANY WIDTH (the review
    /// of fix/r15-banner, 2026-10-14; measured on Claude Code 2.1.300 in the
    /// lab). The vendor's error row and notices are drawn in a column ten
    /// columns short of the pane (2.1.300's `su` and `ig`), the worker's reply
    /// to the pane's edge; and at 80 columns or wider a `<notice> · <remedy>`
    /// row is the vendor's only in at most three rows (`phase`'s
    /// `ERROR_ROWS`).
    /// The narrow-pane bound counted the rows a message would take at 80
    /// columns as its cells over 70, its rows joined wherever a row was too
    /// full for the next word in the vendor's column, and it applied at 80
    /// columns too: the worker's reply `Usage limit reached · pausing.` and
    /// three more hard lines at 34 columns, and its one paragraph over four
    /// rows at 80, read as the usage wall (`limited`, where main read both
    /// idle). NEGATIVE CONTROL: the vendor's own notice, drawn in its column,
    /// is its wall at every width, and a text as long as that paragraph drawn
    /// in the vendor's column is no wall at any width under 80, as it is none
    /// at 80 (four rows there by Ink's greedy wrap, three by cells over 70).
    #[test]
    fn the_workers_prose_is_no_error_row_in_a_pane_of_any_width() {
        use crate::prompt::fixtures::{PROSE_LIMIT_WORDS_34_2_1_300, PROSE_LIMIT_WORDS_80_2_1_300};
        for (label, text) in [
            ("34 columns", PROSE_LIMIT_WORDS_34_2_1_300),
            ("80 columns", PROSE_LIMIT_WORDS_80_2_1_300),
        ] {
            let r = screen(text);
            assert_eq!(wall(&r), None, "{label}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{label}");
        }
        // HAND-BUILT: four hard lines at 80 columns, the first and the third
        // a word short of the 70 cells of the vendor's column there. Counted
        // as that column's lines re-wrapped, they take two rows; at 80
        // columns and wider the rows themselves are counted, as on main.
        let rule = "─".repeat(80);
        let mut r = rows(&[
            "❯ status?",
            "",
            "⏺ Usage limit reached · the parser is done and the lexer is next",
            "  today.",
            "  Then the migration, the docs and the review of every one of the",
            "  notes.",
            "",
        ]);
        r.extend([
            rule.clone(),
            "❯".to_string(),
            rule,
            "  ? for shortcuts".to_string(),
        ]);
        assert_eq!(wall(&r), None);
        let notice = "Usage limit reached · continuing automatically at 10:25am · esc to cancel";
        let long = "Usage limit reached · the next step edits the files named \
                    crates/aterm-agent/src/supervise/limit.rs and then \
                    crates/aterm-agent/src/supervise/run.rs and also \
                    crates/aterm-phase/src/wall.rs.";
        for width in [26, 34, 52, 60, 70, 79, 80] {
            let w = wall(&vendor_box(notice, width)).expect("the vendor's notice");
            assert_eq!(
                (w.kind, w.placement, w.reset.as_deref()),
                (WallKind::UsageSession, Placement::ErrorRow, Some("10:25am")),
                "{width} columns"
            );
            if width >= 52 {
                assert_eq!(wall(&vendor_box(long, width)), None, "{width} columns");
            }
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

    /// THE FABLE BUCKET ON 2.1.301, MEASURED in the lab (crates/aterm-lab,
    /// 2026-10-15): both forms of its wall are the model bucket, the one with
    /// a reset naming it and the one without naming none (the second is
    /// drawn under the first, and the newest is read). NEGATIVE CONTROL: the
    /// Fable warning on the footer's last row is no wall, and the session
    /// reads idle.
    #[test]
    fn the_2_1_301_fable_walls_are_the_model_bucket_with_and_without_a_reset() {
        use crate::prompt::fixtures::{
            FABLE_WARNING_2_1_301, WALL_FABLE_NO_RESET_2_1_301, WALL_FABLE_RESET_2_1_301, screen,
        };
        let bucket = WallKind::ModelBucket { consent: false };
        let w = wall(&screen(WALL_FABLE_RESET_2_1_301)).expect("the Fable wall");
        assert_eq!(
            (w.kind, w.reset.as_deref()),
            (bucket, Some("1:36am (America/Los_Angeles)"))
        );
        let w = wall(&screen(WALL_FABLE_NO_RESET_2_1_301)).expect("the Fable wall, no reset");
        assert_eq!((w.kind, w.reset), (bucket, None));
        let warning = screen(FABLE_WARNING_2_1_301);
        assert_eq!(wall(&warning), None);
        assert_eq!(worker_phase(&warning), Phase::Idle);
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
    ///
    /// So is the row of a `/compact` that FAILED — `Compaction failed · …`,
    /// `Error during compaction: …`, drawn under the `❯ /compact` echo with no
    /// done row after it: the context is as full as it was, and read as no
    /// wall the supervisor took the failure for a
    /// compaction that worked. Claude Code 2.1.286's own screens (MEASURED
    /// 2026-10-01). NEGATIVE CONTROLS: `Compacted (ctrl+o to see full
    /// summary)` and a person's `/compact` on a chat with `Not enough
    /// messages to compact.` are no wall.
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
        // The screens of a full context and of its `/compact`.
        for (name, capture, notice) in [
            (
                "the wall",
                CONTEXT_WALL_MEASURED,
                Some("Context limit reached · /compact or /clear to continue"),
            ),
            (
                "a failed /compact",
                COMPACT_FAILED_FIRST_MEASURED,
                Some(
                    "Compaction failed · conversation could not be reduced below the context limit",
                ),
            ),
            (
                "the third failure in a row",
                COMPACT_FAILED_3_MEASURED,
                Some(
                    "Compaction failed · conversation could not be reduced below the context limit",
                ),
            ),
            (
                "a /compact whose request failed, wrapped",
                COMPACT_ERROR_500_MEASURED,
                Some("Error during compaction: API Error: 500 Internal server error."),
            ),
            ("a compaction that worked", COMPACT_SUCCEEDED_MEASURED, None),
            ("nothing to compact", COMPACT_NOT_ENOUGH_MEASURED, None),
            (
                "the wall with `/compact` typed and the slash menu open (2.1.292)",
                CONTEXT_WALL_COMPACT_MENU_MEASURED,
                Some("Context limit reached · /compact or /clear to continue"),
            ),
        ] {
            let r = screen(capture);
            let found = wall(&r);
            match (notice, found) {
                (Some(head), Some(w)) => {
                    assert_eq!(
                        (w.kind, w.placement),
                        (WallKind::Context, Placement::Gutter),
                        "{name}"
                    );
                    assert!(w.message.starts_with(head), "{name}: {}", w.message);
                }
                (None, None) => {}
                (want, got) => panic!("{name}: wanted {want:?}, read {got:?}"),
            }
            assert_eq!(limit_notice(&r), None, "{name}");
            assert_eq!(worker_phase(&r), Phase::Idle, "{name}");
        }
        // The detail of a failed compaction wins where it names a wall with an
        // answer of its own: a lost login met by `/compact` is still a lost login
        // (measured: the wrapper first read as Context hid both, and the
        // supervisor retried `/compact` for ever instead of `/login`).
        for (text, kind) in [
            (
                "Error during compaction: Invalid API key · Fix external API key",
                WallKind::Auth,
            ),
            (
                "Error during compaction: Credit balance is too low",
                WallKind::Spend,
            ),
            (
                "Error during compaction: API Error: Connection to the API was lost (ECONNRESET).",
                WallKind::Context,
            ),
        ] {
            assert_eq!(classify_wall(text), Some(kind), "{text}");
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
    /// fits, 80 included (a column-6 floor reads `None` at 82 columns or
    /// fewer, however busy the spinner over it).
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

    /// THE DRIFT CANARY FOR THE PHRASE TABLE. The reader names a wall by
    /// [`PHRASES`], and a vendor build that words a notice differently reads
    /// as no wall — behind a shell, as `busy` for as long as the shell runs.
    /// Every row (lowercased) must be in the
    /// store claude's binary, but for the rows older builds carry and neither
    /// 2.1.283 nor 2.1.284 does (measured). It RUNS wherever a binary is found
    /// (`$ATERM_CLAUDE_BIN`, else `aterm pkg which claude`), so the next gate
    /// after a build lands names what moved; without a binary it says so
    /// loudly and returns — a canary that could not look has not passed.
    #[test]
    fn every_wall_phrase_is_in_the_store_claude() {
        /// Rows of [`PHRASES`] that older builds carry and 2.1.283 and 2.1.284
        /// do not (`strings -a` of both, 2026-09-28).
        const OLDER_BUILDS: &[&str] = &[
            "repeated overloaded errors",
            "weekly rate limit",
            "seven-day limit",
            "haiku limit",
            "reached your limit",
            "limit will reset",
        ];
        let path = crate::anchors::canary_binary("claude", "ATERM_CLAUDE_BIN");
        let Some(bytes) = path
            .and_then(|p| std::fs::read(p).ok())
            .filter(|bytes| crate::anchors::looks_like_a_build(bytes))
        else {
            eprintln!(
                "CANARY NOT RUN: no claude binary found (set ATERM_CLAUDE_BIN, or `aterm pkg \
                 install claude`) — the wall phrases are NOT checked against a build"
            );
            return;
        };
        let lower = bytes.to_ascii_lowercase();
        // The vendor draws `\u{b7}` and `\u{2019}` from escapes, and drops
        // number tokens from a notice's head before the table reads it.
        let missing: Vec<&str> = PHRASES
            .iter()
            .map(|(p, _)| *p)
            .filter(|p| !OLDER_BUILDS.contains(p))
            .filter(|p| {
                !crate::anchors::contains_bytes(&lower, p.replace('\'', "\u{2019}").as_bytes())
                    && !crate::anchors::contains_bytes(&lower, p.as_bytes())
            })
            .collect();
        assert!(
            missing.is_empty(),
            "wall phrases the store claude no longer carries: {missing:?}"
        );
    }

    /// Usage credits carrying a session are NO WALL: the vendor's own list of
    /// its credits notices (the 2.1.300 bundle; the first two and the fourth
    /// measured), the notice credits start on, the warning that a window is
    /// filling and the one that credits near their cap read as nothing.
    /// NEGATIVE CONTROL: a model's bucket that the vendor says now uses usage
    /// credits is still that bucket, and running out of credits still spend.
    #[test]
    fn usage_credits_and_the_vendors_warnings_are_no_wall() {
        for notice in [
            "You're now using usage credits",
            "You're now using usage credits · Your session limit resets 9:43am (America/Los_Angeles)",
            "Now using usage credits",
            "Now using usage credits for Opus · Your Opus limit resets 4pm",
            "You're now using your usage allocation",
            "Now using your usage allocation",
            "You're now using extra usage",
            "Now using extra usage",
            "Fable 5 is drawing from usage credits",
            "Opus is now using usage credits instead of your plan limits",
            "You've used 92% of your session limit · resets 4:12am (America/Los_Angeles)",
            "You've used 80% of your weekly limit · resets Oct 18 at 8:50pm (America/Los_Angeles)",
            "You're close to your usage credit limit",
        ] {
            assert_eq!(classify_wall(notice), None, "{notice}");
        }
        assert_eq!(
            classify_wall("Opus 4.1 now uses usage credits"),
            Some(WallKind::ModelBucket { consent: false })
        );
        assert_eq!(
            classify_wall("You're out of usage credits. Switch to another model"),
            Some(WallKind::Spend)
        );
    }

    /// OFF MACOS, CLAUDE CODE DRAWS THE MODEL'S LINES WITH `●` (the 2.1.300
    /// bundle: `⏺` on macOS, `●` elsewhere), and a reply so drawn is the
    /// model's as one drawn with `⏺` is: a person's turn served under a
    /// limit's notice is served since it ([`since_limit`]). NEGATIVE CONTROL:
    /// the same turn's prompt with no reply under it serves nothing.
    #[test]
    fn off_macos_a_line_of_the_models_drawn_with_a_dot_is_its_reply() {
        use crate::prompt::fixtures::{WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED, screen};
        let dotted = |rows: Vec<String>| {
            rows.into_iter()
                .map(|r| r.replace('⏺', "●"))
                .collect::<Vec<_>>()
        };
        let turned = dotted(screen(WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED));
        let read = since_limit(&turned).expect("the limit");
        assert_eq!(
            (read.served, read.reset.as_deref()),
            (true, Some("10:38am"))
        );
        let unanswered: Vec<String> = turned
            .iter()
            .filter(|r| !r.starts_with("● Hi!"))
            .cloned()
            .collect();
        let read = since_limit(&unanswered).expect("the limit");
        assert_eq!(
            (read.served, read.reset.as_deref()),
            (false, Some("10:38am"))
        );
    }

    /// THE EXPIRY NOTICE (`login_expires_in`), composed on the measured
    /// capture whose notification row (`✔ Update installed · Restart to
    /// update`, right-aligned above the composer's top rule) it stands in for:
    /// read there and in the footer, by its day count. NEGATIVE CONTROLS: the
    /// same words in a reply (`⏺`), a tool row, a tool's output, the composer
    /// itself, and a notice that names no day count read nothing.
    #[test]
    fn the_login_expiry_notice_is_read_where_a_notification_is_drawn_and_only_there() {
        const CAPTURE: &str = include_str!("fixtures/idle-after-limit-and-model-switch.txt");
        let rows: Vec<String> = CAPTURE.lines().map(str::to_string).collect();
        let at = rows
            .iter()
            .position(|r| r.contains("Update installed"))
            .expect("the measured notification row");
        let notice = |words: &str| {
            let mut r = rows.clone();
            r[at] = format!("{:>136}", words);
            r
        };
        assert_eq!(login_expires_in(&rows), None, "the capture as measured");
        for (words, days) in [
            (
                "\u{26a0} Your login expires in 2 days \u{b7} run /login to renew",
                2,
            ),
            ("Your login expires in 1 day \u{b7} run /login to renew", 1),
            ("\u{26a0} Your login expires in 3 days", 3),
        ] {
            assert_eq!(login_expires_in(&notice(words)), Some(days), "{words}");
        }
        // The footer's segment, at the right edge under the bottom rule.
        let mut footer = rows.clone();
        let last = footer.len() - 1;
        footer[last] = format!(
            "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle){:>90}",
            "Your login expires in 1 day \u{b7} run /login to renew"
        );
        assert_eq!(login_expires_in(&footer), Some(1));
        // NEGATIVE CONTROLS: a conversation's rows, and words that are not it.
        let quoted = |row: String| {
            let mut r = rows.clone();
            r.insert(at, row);
            r
        };
        for row in [
            "\u{23fa} Your login expires in 2 days \u{b7} run /login to renew".to_string(),
            "  Your login expires in 2 days \u{b7} run /login to renew".to_string(),
            "  \u{23bf}  Your login expires in 2 days \u{b7} run /login to renew".to_string(),
            "\u{23fa} Bash(echo 'Your login expires in 2 days \u{b7} run /login to renew')"
                .to_string(),
        ] {
            assert_eq!(login_expires_in(&quoted(row.clone())), None, "{row}");
        }
        for words in [
            "Note: Your login expires in 2 days \u{b7} run /login to renew",
            "Your login expires in two days \u{b7} run /login to renew",
            "Your login expires in 2 weeks",
            "Your login expires in 2 days, they say",
        ] {
            assert_eq!(login_expires_in(&notice(words)), None, "{words}");
        }
        let mut drafted = rows.clone();
        let caret = drafted
            .iter()
            .rposition(|r| r.starts_with('\u{276f}'))
            .expect("the caret row");
        drafted[caret] =
            "\u{276f} Your login expires in 2 days \u{b7} run /login to renew".to_string();
        assert_eq!(login_expires_in(&drafted), None, "a draft is the person's");
    }
}
