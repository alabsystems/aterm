// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use aterm_types::control_verbs::ScreenGen;

/// Minimum time between two presence records for one session.
pub const REPUBLISH_MIN: Duration = Duration::from_secs(2);

/// [`REPUBLISH_MIN`] at test pace (`bridge::fast_timers`), matched to `FAST_ROSTER_REFRESH`.
const FAST_REPUBLISH_MIN: Duration = Duration::from_millis(200);

fn republish_min() -> Duration {
    if crate::bridge::fast_timers() {
        FAST_REPUBLISH_MIN
    } else {
        REPUBLISH_MIN
    }
}

/// The byte cap on `role=`.
pub const ROLE_CAP: usize = 64;
/// The byte cap on `detail=`.
pub const DETAIL_CAP: usize = 128;
/// The byte cap on `title=`.
pub const TITLE_CAP: usize = 128;
/// The byte cap on `attention=`.
pub const ATTENTION_CAP: usize = crate::glance::ATTENTION_CAP;

/// The words `phase=` can carry beside a wall.
pub const PHASES: [&str; 6] = ["busy", "idle", "prompt", "question", "survey", "unknown"];

/// Legacy `phase=` words from older servers.
pub const LEGACY_PHASES: [&str; 1] = ["limited"];

/// Returns true if `word` is `wall:<kind>` with `<kind>` of 1..=32 bytes of `[a-z0-9_-]`.
#[must_use]
pub fn is_wall(word: &str) -> bool {
    word.strip_prefix("wall:").is_some_and(|k| {
        (1..=32).contains(&k.len())
            && k.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    })
}

/// The value an absent field renders as.
pub const ABSENT: &str = "-";

/// What a session's presence row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// `role= detail= phase= title=` beside `attention=`.
    #[default]
    Meta,
    /// `attention=` alone.
    Minimal,
}

impl Mode {
    /// Parse a presence mode from config or CLI.
    #[must_use]
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim() {
            "meta" => Some(Mode::Meta),
            "minimal" => Some(Mode::Minimal),
            _ => None,
        }
    }

    /// The word for the config and the report.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Mode::Meta => "meta",
            Mode::Minimal => "minimal",
        }
    }
}

/// The meaning fields of one session, each a bounded pct-encoded token or [`ABSENT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fields {
    /// `meta attention=`.
    pub attention: String,
    /// `meta role=`.
    pub role: String,
    /// The running program, as `ls` prints it.
    pub detail: String,
    /// `meta user_title=` when set, else [`ABSENT`].
    pub title: String,
    /// The session's agent phase word, or [`ABSENT`].
    pub phase: String,
}

impl Default for Fields {
    fn default() -> Self {
        Self {
            attention: ABSENT.to_string(),
            role: ABSENT.to_string(),
            detail: ABSENT.to_string(),
            title: ABSENT.to_string(),
            phase: ABSENT.to_string(),
        }
    }
}

impl Fields {
    /// Populate `attention`, `role`, and `title` from a `meta` reply header.
    pub fn read_meta(&mut self, header: &str) {
        let kv = |key: &str| crate::fabric::kv(header, key);
        self.attention = token(kv("attention").unwrap_or(ABSENT), ATTENTION_CAP);
        self.role = token(kv("role").unwrap_or(ABSENT), ROLE_CAP);
        let user = kv("user_title").filter(|v| *v != ABSENT && !v.is_empty());
        self.title = token(user.unwrap_or(ABSENT), TITLE_CAP);
    }

    /// Whether these fields already say what [`Fields::read_meta`] would take from `header`.
    #[must_use]
    pub fn says_meta(&self, header: &str) -> bool {
        let mut read = self.clone();
        read.read_meta(header);
        read == *self
    }

    /// Take `detail=` as `status`/`sessions` report it.
    pub fn set_detail(&mut self, raw: Option<&str>) {
        self.detail = token(raw.unwrap_or(ABSENT), DETAIL_CAP);
    }

    /// Take `phase=` from the server's agent verdict.
    pub fn set_agent(&mut self, word: Option<&str>) {
        self.phase = word
            .filter(|w| PHASES.contains(w) || LEGACY_PHASES.contains(w) || is_wall(w))
            .unwrap_or(ABSENT)
            .to_string();
    }

    /// Format the tokens for the presence body in `mode`.
    #[must_use]
    pub fn tokens(&self, mode: Mode) -> String {
        let mut out = format!(" attention={}", self.attention);
        if mode == Mode::Minimal {
            return out;
        }
        out.push_str(&format!(
            " role={} detail={} phase={} title={}",
            self.role, self.detail, self.phase, self.title
        ));
        out
    }
}

/// Clamp `raw` to printable ASCII on a `%XX` boundary, or [`ABSENT`] if empty.
#[must_use]
pub fn token(raw: &str, cap: usize) -> String {
    let out = clamp_pct(raw, cap);
    if out.is_empty() {
        return ABSENT.to_string();
    }
    out
}

/// Clamps `raw` to at most `cap` ASCII graphic bytes, trimming any trailing split `%` or `%X`.
#[must_use]
pub(crate) fn clamp_pct(raw: &str, cap: usize) -> String {
    let mut out = String::with_capacity(cap.min(raw.len()));
    for byte in raw.bytes() {
        if out.len() == cap {
            break;
        }
        if byte.is_ascii_graphic() {
            out.push(byte as char);
        }
    }
    while out.ends_with('%') || (out.len() >= 2 && out.as_bytes()[out.len() - 2] == b'%') {
        out.pop();
    }
    out
}

/// A presence row's `gen=` token for the screen generation `screen`: `<content_seq>:<fp16>`, its
/// content seq and the 16 lowercase hex digits of FNV-1a/64 over its wire spelling
/// `<epoch>.<seq>`, so two generations of one seq in different epochs read apart.
#[must_use]
pub(crate) fn gen_token(screen: ScreenGen) -> String {
    format!(
        "{}:{:016x}",
        screen.seq,
        aterm_hash::fnv1a_64(screen.to_string().as_bytes())
    )
}

/// Per-session presence state remembered by the bridge.
#[derive(Debug, Clone, Default)]
pub struct Slot {
    /// The fields as last sampled.
    pub fields: Fields,
    /// Whether `meta` has been read into `fields`.
    pub sampled: bool,
    /// The newest screen generation aterm named for the session: a status read's (its
    /// `sessions status` row or `@<sid> status`) or an agent verdict's. A read that named none
    /// (`gen=-`: the terminal was busy) leaves the one before ([`Slot::saw_screen`]).
    screen: Option<ScreenGen>,
    /// Whether aterm gave the session's agent verdict ([`Slot::saw_agent`]): an `@<sid> status`
    /// read (whose `agent=` an older server leaves out, which clears the phase), a batch row that
    /// said `agent=`, or an agent verdict event. A batch row that deferred it
    /// (`agent_deferred=1`: another holder had the session's timeline) gives none.
    verdict: bool,
    /// Newest row sent onto the bus (including in-flight rows), as [`Slot::on_bus`] spells it, and
    /// send timestamp.
    published: Option<(String, Instant)>,
    /// Newest row the broker acknowledged as landed, used as fallback on refusal.
    landed: Option<(String, Instant)>,
}

/// [`Slot::due`]'s answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Nothing is on the bus for this session yet.
    First,
    /// The fields moved and the window has passed.
    Changed,
    /// The fields are what the bus already says.
    Unchanged,
    /// The fields moved inside the [`REPUBLISH_MIN`] window.
    Capped,
}

impl Due {
    /// Whether a publish is owed now.
    #[must_use]
    pub const fn now(self) -> bool {
        matches!(self, Due::First | Due::Changed)
    }
}

impl Slot {
    /// Keeps `screen` as the session's newest generation; `None` (a read that named none) keeps
    /// the one before.
    pub(crate) fn saw_screen(&mut self, screen: Option<ScreenGen>) {
        if screen.is_some() {
            self.screen = screen;
        }
    }

    /// Takes `word` as the session's agent verdict: the phase ([`Fields::set_agent`]), and that a
    /// verdict was given.
    pub(crate) fn saw_agent(&mut self, word: Option<&str>) {
        self.fields.set_agent(word);
        self.verdict = true;
    }

    /// Whether status reads have given the session both its screen generation and its agent
    /// verdict. A slot that lacks either (a read that met the terminal busy, `gen=-`, or a
    /// batch row that deferred the verdict) is one its live row reads `@<sid> status` for.
    #[must_use]
    pub(crate) const fn knows_status(&self) -> bool {
        self.screen.is_some() && self.verdict
    }
    /// The row's `gen=` token: [`gen_token`] of the newest generation aterm named, [`ABSENT`]
    /// before any.
    #[must_use]
    pub(crate) fn row_gen(&self) -> String {
        self.screen.map_or_else(|| ABSENT.to_string(), gen_token)
    }

    /// What a row of this slot says that [`Slot::due`] compares: the fields' tokens in `mode`,
    /// then ` gen=-` while no generation was named. A row sent with no generation is owed again
    /// once one is named, and a later generation alone owes no row.
    fn on_bus(&self, mode: Mode) -> String {
        let mut said = self.fields.tokens(mode);
        if self.screen.is_none() {
            said.push_str(" gen=-");
        }
        said
    }

    /// Whether the fields owe a record now in `mode`.
    #[must_use]
    pub fn due(&self, mode: Mode, now: Instant) -> Due {
        match &self.published {
            None => Due::First,
            Some((on_bus, _)) if *on_bus == self.on_bus(mode) => Due::Unchanged,
            Some((_, at)) if now.saturating_duration_since(*at) < republish_min() => Due::Capped,
            Some(_) => Due::Changed,
        }
    }

    /// Records that the row [`Slot::on_bus`] spells in `mode` was pipelined onto the bus at `now`.
    pub(crate) fn mark_published(&mut self, mode: Mode, now: Instant) -> Marked {
        let sent = (self.on_bus(mode), now);
        self.published = Some(sent.clone());
        Marked(sent)
    }

    /// Marks `mark` as acknowledged by the broker.
    pub(crate) fn landed(&mut self, mark: Marked) {
        self.landed = Some(mark.0);
    }

    /// Rolls `published` back to `landed` if `mark` is still the newest sent row.
    pub(crate) fn unmark(&mut self, mark: Marked) {
        if self.published.as_ref() == Some(&mark.0) {
            self.published.clone_from(&self.landed);
        }
    }
}

/// One presence row as [`Slot::mark_published`] sent it: what [`Slot::on_bus`] spelled, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marked((String, Instant));

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
