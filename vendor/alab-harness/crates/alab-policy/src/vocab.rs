// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DECIDER'S VOCABULARY: what a screen reader tells a harness about an agent's session, as
//! plain data. The phase the agent stands in, the wall its last turn ended on and why an API
//! error happened, the kinds of usage-limit reset a plan offers, and the clock time a limit says
//! it resets at. A harness's own readers build these from a program's screen; the policy decides
//! over them and names no program.

/// The agent's phase, as one screen reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// A turn (or a background shell, workflow, agent or monitor) is running.
    Busy,
    /// An approval box is showing.
    Prompt,
    /// Not busy, no box, and the agent's last turn ended on a usage or rate limit notice.
    Limited {
        /// The notice as the program says it (`You've reached your weekly limit. …`).
        message: String,
        /// When it resets, if the notice says (`7:30pm (America/Los_Angeles)`).
        reset: Option<String>,
    },
    /// Not busy, no box, and the last thing the agent said does not end in `?`.
    Idle,
    /// Not busy, no box, and the last thing the agent said ends with `?`.
    Question,
}

impl Phase {
    /// The lowercase word the CLI prints.
    #[inline]
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Phase::Busy => "busy",
            Phase::Prompt => "prompt",
            Phase::Limited { .. } => "limited",
            Phase::Idle => "idle",
            Phase::Question => "question",
        }
    }
}

/// Which wall a turn ended on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallKind {
    /// The five-hour session window (`You've hit your session limit · resets 3pm`), or a usage
    /// limit that does not say which window (`Usage limit reached · continuing automatically at
    /// 1:50pm`).
    UsageSession,
    /// The seven-day window (`You've hit your weekly limit · resets Sep 19 at 11am`).
    UsageWeekly,
    /// A per-model bucket (`You've reached your <model> limit`).
    ModelBucket {
        /// The notice offers to go on past the bucket once it is confirmed (`prompt to
        /// confirm`, `continuing on …`): a continue that may draw on usage credits.
        consent: bool,
    },
    /// Money: a monthly spend limit, a shared budget, no usage credits left.
    Spend,
    /// The context is full (`Context limit reached · /compact or /clear to continue`, `Prompt is
    /// too long`): a compaction moves it, waiting does not.
    Context,
    /// The login is gone (`Not logged in · Please run /login`, `Login expired · Please run
    /// /login`): a human's browser moves it, and until it does every turn ends on it in
    /// milliseconds, whatever was typed. Never Cloudflare's block or challenge page in the API's
    /// place, which a program may word as a lost login too: that is an [`ApiCause::Server`] API
    /// error.
    Auth,
    /// Any other `API Error: …` the turn ended on, and the program's own refusal of the request
    /// ([`ApiCause::Request`]).
    ApiError {
        /// The HTTP status the error names, if it names one.
        code: Option<u16>,
        /// Whether the error is one a retry may clear.
        retryable: bool,
        /// What went wrong.
        cause: ApiCause,
    },
    /// The service is overloaded (`API Error: 529 Overloaded`, `Repeated 529 Overloaded
    /// errors`): a server-side issue, usually temporary.
    Overloaded,
    /// The program's critical-memory banner: the process must be restarted and resumed on its
    /// conversation.
    Memory,
}

/// Failure category of an [`WallKind::ApiError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiCause {
    /// Server-side failure: a retryable HTTP status (5xx, 408, 409, 429; or 401/451),
    /// Cloudflare's block or challenge page in the API's place, retried whatever its status, or
    /// an unclassified `API Error` without a status (`API Error: terminated`, `API Error: fetch
    /// failed`, `API Error: Please wait a moment and try again.`).
    Server,
    /// Network or DNS failure before reaching the API (e.g. `ENOTFOUND`, `ECONNREFUSED`,
    /// timeout, or sleep).
    Unreachable,
    /// Response cut off mid-stream or stalled before a response was produced.
    CutOff,
    /// TLS, certificate, or proxy-tunnel refusal (not retried by the program).
    Config,
    /// Request refused by the API (non-retryable 4xx statuses other than 401, 408, 409, 429,
    /// 451 and Cloudflare's block page's, or a refusal notice such as a safeguard, an
    /// unsupported model or effort, or an account hold).
    Request,
    /// Response exceeded the program's output token maximum.
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

    /// The whole word `status agent=` publishes for this wall: `wall:` and [`Self::name`]
    /// (`wall:overloaded`).
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

    /// Whether a screen reader reads this wall as [`Phase::Limited`]: the usage windows, a model
    /// bucket, spend, and an API rate limit (429) — the kinds it has always read so.
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

/// A kind of usage-limit reset a plan offers, in the program's own order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetKind {
    /// `session`: the five-hour limit's.
    Session,
    /// `weekly`: a weekly limit's.
    Weekly,
    /// `fable`: gives back what one model, Fable, used of the weekly limit.
    Fable,
    /// `full`: both limits'.
    Full,
}

impl ResetKind {
    /// Every kind, in the program's own order.
    pub const ALL: [ResetKind; 4] = [
        ResetKind::Session,
        ResetKind::Weekly,
        ResetKind::Fable,
        ResetKind::Full,
    ];

    /// The program's id of the kind.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ResetKind::Session => "session",
            ResetKind::Weekly => "weekly",
            ResetKind::Fable => "fable",
            ResetKind::Full => "full",
        }
    }

    /// [`Self::word`] read back.
    #[must_use]
    pub fn of_word(word: &str) -> Option<ResetKind> {
        ResetKind::ALL.into_iter().find(|kind| kind.word() == word)
    }

    /// Whether a harness may take a reset of this kind: every kind but [`ResetKind::Fable`].
    #[inline]
    #[must_use]
    pub const fn takeable(self) -> bool {
        !matches!(self, ResetKind::Fable)
    }
}

/// Wall-clock `(date, hour, minute)` identity of a limit's reset, as its notice names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetClock {
    date: Option<(u32, u32)>,
    hour: u32,
    minute: u32,
}

impl ResetClock {
    /// The clock at `hour:minute` (24-hour time) on `date`, a `(month, day)`, if the notice
    /// names one.
    #[inline]
    #[must_use]
    pub const fn new(date: Option<(u32, u32)>, hour: u32, minute: u32) -> Self {
        Self { date, hour, minute }
    }

    /// Render the reset clock in human-readable form without its timezone suffix (`at 9am`,
    /// `on Oct 23 at 4:20pm`).
    #[must_use]
    pub fn words(&self) -> String {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let meridian = if self.hour < 12 { "am" } else { "pm" };
        let hour = match self.hour % 12 {
            0 => 12,
            h => h,
        };
        let clock = if self.minute == 0 {
            format!("{hour}{meridian}")
        } else {
            format!("{hour}:{:02}{meridian}", self.minute)
        };
        let month = self
            .date
            .and_then(|(m, _)| usize::try_from(m).ok()?.checked_sub(1))
            .and_then(|m| MONTHS.get(m));
        match (self.date, month) {
            (Some((_, day)), Some(month)) => format!("on {month} {day} at {clock}"),
            _ => format!("at {clock}"),
        }
    }
}
