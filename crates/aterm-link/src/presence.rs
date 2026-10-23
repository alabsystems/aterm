// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

/// Minimum time between two presence records for one session.
pub const REPUBLISH_MIN: Duration = Duration::from_secs(2);

/// [`REPUBLISH_MIN`] at test pace: a quarter, as the round it is matched to
/// runs a quarter (`bridge.rs`'s `FAST_ROSTER_REFRESH`), so a change inside the
/// window is still carried by the next round and a row still costs at most one
/// record per session per round.
const FAST_REPUBLISH_MIN: Duration = Duration::from_millis(500);

/// The republish window in this process: [`REPUBLISH_MIN`], or
/// [`FAST_REPUBLISH_MIN`] when the bridge runs at test pace
/// (`bridge::fast_timers`, a development seam a shipped build never reads).
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

/// The one escape-boundary clamp: the ASCII graphic bytes of `raw`, at most
/// `cap` of them, with a trailing `%` or `%X` that the cap split off its escape
/// trimmed away. Empty when nothing is left; each caller names its own default
/// ([`token`]'s [`ABSENT`], the bridge's halt reason, `cli.rs`'s column).
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

/// Per-session presence state remembered by the bridge.
#[derive(Debug, Clone, Default)]
pub struct Slot {
    /// The fields as last sampled.
    pub fields: Fields,
    /// Whether `meta` has been read into `fields`.
    pub sampled: bool,
    /// The tokens on the bus ([`Fields::tokens`] in the mode the row was published in) and
    /// when. Tokens, not fields: a row is owed only when the row would differ.
    /// A row SENT and not yet answered counts ([`Slot::mark_published`]).
    published: Option<(String, Instant)>,
    /// The newest row the broker ANSWERED as landed: what `published` falls
    /// back to when a row sent after it is refused ([`Slot::unmark`]).
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
    /// Whether the fields owe a record now in `mode`.
    #[must_use]
    pub fn due(&self, mode: Mode, now: Instant) -> Due {
        match &self.published {
            None => Due::First,
            Some((on_bus, _)) if *on_bus == self.fields.tokens(mode) => Due::Unchanged,
            Some((_, at)) if now.saturating_duration_since(*at) < republish_min() => Due::Capped,
            Some(_) => Due::Changed,
        }
    }

    /// Record that the fields as they are now were SENT onto the bus at `now`,
    /// as a row in `mode`, and not yet answered — the bridge pipelines presence
    /// rows, so the answer comes later — answering the mark its answer settles
    /// ([`Slot::landed`], [`Slot::unmark`]). The one way a row is recorded:
    /// there is no "it landed" without the send before it, so a refusal always
    /// has a row that did land to fall back to.
    pub(crate) fn mark_published(&mut self, mode: Mode, now: Instant) -> Marked {
        let sent = (self.fields.tokens(mode), now);
        self.published = Some(sent.clone());
        Marked(sent)
    }

    /// The row `mark` stands for LANDED. Answers come in send order, so the
    /// newest answered row is the last one recorded here.
    pub(crate) fn landed(&mut self, mark: Marked) {
        self.landed = Some(mark.0);
    }

    /// The row `mark` stands for did NOT land: fall back to the newest row the
    /// broker answered as landed, so the next round owes the row again — what
    /// an unanswered blocking publish left. Only while `mark` is the newest row
    /// SENT: one sent after it is the bus's word as soon as it lands, and its
    /// own answer settles it.
    pub(crate) fn unmark(&mut self, mark: Marked) {
        if self.published.as_ref() == Some(&mark.0) {
            self.published.clone_from(&self.landed);
        }
    }
}

/// One presence row as [`Slot::mark_published`] sent it: its tokens and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marked((String, Instant));

#[cfg(test)]
mod tests {
    use super::*;

    /// `phase=` carries the six words, a named wall and the legacy `limited`;
    /// any other word — hostile, screen text, text riding on a wall — is `-`.
    #[test]
    fn phase_is_the_servers_word_from_the_closed_set_or_absent() {
        let mut f = Fields::default();
        for word in PHASES.iter().chain(LEGACY_PHASES.iter()).copied().chain([
            "wall:overloaded",
            "wall:usage-session",
            "wall:api-error",
        ]) {
            f.set_agent(Some(word));
            assert_eq!(f.phase, word);
            assert!(f.tokens(Mode::Meta).contains(&format!(" phase={word} ")));
        }
        for other in [
            None,
            Some("-"),
            Some(""),
            Some("walled"),
            Some("wall:"),
            Some("wall:Overloaded"),
            Some("wall:529 SECRET"),
            Some("busy%20SECRET"),
            Some("Busy"),
            Some("esc to interrupt"),
        ] {
            f.set_agent(Some("busy"));
            f.set_agent(other);
            assert_eq!(f.phase, ABSENT, "{other:?}");
        }
    }

    /// No row carries `context=`, in either mode.
    #[test]
    fn a_row_carries_no_context_token() {
        let mut f = Fields::default();
        f.set_agent(Some("idle"));
        for mode in [Mode::Meta, Mode::Minimal] {
            assert!(!f.tokens(mode).contains("context="), "{mode:?}");
        }
        assert_eq!(
            f.tokens(Mode::Meta),
            " attention=- role=- detail=- phase=idle title=-"
        );
    }

    /// `minimal` writes `attention=` alone, whatever was read.
    #[test]
    fn minimal_writes_attention_alone() {
        let mut f = Fields::default();
        f.read_meta("OK title=vim user_title=worker description=- icon=- role=worker attention=stuck cwd=- state=alive");
        f.set_detail(Some("claude"));
        f.set_agent(Some("busy"));
        assert_eq!(f.tokens(Mode::Minimal), " attention=stuck");
        assert_eq!(
            f.tokens(Mode::Meta),
            " attention=stuck role=worker detail=claude phase=busy title=worker"
        );
    }

    /// Only the user's title is published, never the terminal's, and every
    /// field is capped on an escape boundary.
    #[test]
    fn meta_fields_are_read_capped_and_only_the_user_title_is_published() {
        let mut f = Fields::default();
        f.read_meta("OK title=%E2%9C%B3%20Claude%20Code user_title=- description=- icon=- role=- attention=- cwd=- state=alive");
        assert_eq!(
            f.title, ABSENT,
            "no user title: no title, whatever the program wrote"
        );
        assert_eq!(f.role, ABSENT);
        assert_eq!(f.attention, ABSENT);
        f.read_meta("OK title=Rotating%20the%20sk-live-OSCSECRET42%20key%20for%20acme user_title=- description=- icon=- role=- attention=- cwd=- state=alive");
        assert_eq!(f.title, ABSENT);
        for mode in [Mode::Meta, Mode::Minimal] {
            let t = f.tokens(mode);
            assert!(!t.contains("OSCSECRET") && !t.contains("Rotating"), "{t}");
        }
        f.read_meta("OK title=x user_title=worker%3Aclaude-satcomp description=- icon=- role=worker attention=- cwd=- state=alive");
        assert_eq!(f.title, "worker%3Aclaude-satcomp");
        assert_eq!(f.role, "worker");
        f.read_meta("OK title=Rotating%20the%20key user_title=- description=- icon=- role=worker attention=- cwd=- state=alive");
        assert_eq!(f.title, ABSENT);

        let long = "%C3%A9".repeat(400);
        f.read_meta(&format!(
            "OK title=- user_title={long} description=- icon=- role={} attention={} cwd=- state=alive",
            "r".repeat(500),
            "a".repeat(500)
        ));
        assert!(f.title.len() <= TITLE_CAP, "{}", f.title.len());
        assert_eq!(f.title.len() % 3, 0, "cut inside an escape: {}", f.title);
        assert_eq!(f.role.len(), ROLE_CAP);
        assert_eq!(f.attention.len(), ATTENTION_CAP);
        f.set_detail(Some(&"d".repeat(300)));
        assert_eq!(f.detail.len(), DETAIL_CAP);
        f.set_detail(Some("targo\u{1b}[2J test\n"));
        assert_eq!(f.detail, "targo[2Jtest");
        f.set_detail(Some(""));
        assert_eq!(f.detail, ABSENT);
        f.set_detail(None);
        assert_eq!(f.detail, ABSENT);
        assert_eq!(token("%", 8), ABSENT);
        assert_eq!(token("ab%2", 8), "ab");
    }

    /// A row sent and answered as landed: what the bridge records for a
    /// presence row whose ack came back.
    fn published(slot: &mut Slot, mode: Mode, now: Instant) {
        let mark = slot.mark_published(mode, now);
        slot.landed(mark);
    }

    /// Change only, and at most once per [`REPUBLISH_MIN`].
    #[test]
    fn the_writer_publishes_on_change_only_and_at_most_once_per_window() {
        let t0 = Instant::now();
        let m = Mode::Meta;
        let mut slot = Slot::default();
        assert_eq!(slot.due(m, t0), Due::First);
        slot.fields.set_agent(Some("busy"));
        published(&mut slot, m, t0);
        assert_eq!(slot.due(m, t0 + Duration::from_secs(5)), Due::Unchanged);
        slot.fields.set_agent(Some("idle"));
        assert_eq!(slot.due(m, t0 + Duration::from_millis(500)), Due::Capped);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Changed);
        assert!(slot.due(m, t0 + REPUBLISH_MIN).now());
        assert!(!Due::Capped.now() && !Due::Unchanged.now());
        published(&mut slot, m, t0 + REPUBLISH_MIN);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Unchanged);
        slot.fields.set_agent(None);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN * 2), Due::Changed);
    }

    /// A row marked at its SEND caps the window from the send; a refusal falls
    /// back to the newest row that LANDED — not to a row sent before it that
    /// was refused too — and leaves a newer row in flight standing.
    #[test]
    fn a_refused_row_falls_back_to_the_newest_row_that_landed() {
        let t0 = Instant::now();
        let m = Mode::Meta;
        let mut slot = Slot::default();
        slot.fields.set_agent(Some("busy"));
        let first = slot.mark_published(m, t0);
        assert_eq!(
            slot.due(m, t0),
            Due::Unchanged,
            "in flight counts as on the bus"
        );
        slot.fields.set_agent(Some("idle"));
        assert_eq!(
            slot.due(m, t0),
            Due::Capped,
            "the window counts from the send"
        );
        slot.unmark(first);
        assert_eq!(
            slot.due(m, t0),
            Due::First,
            "refused: nothing is on the bus"
        );

        let idle = slot.mark_published(m, t0);
        slot.landed(idle);
        slot.fields.set_agent(Some("busy"));
        let busy = slot.mark_published(m, t0 + REPUBLISH_MIN);
        slot.fields.set_agent(Some("prompt"));
        let prompt = slot.mark_published(m, t0 + REPUBLISH_MIN * 2);
        slot.unmark(busy);
        assert_eq!(
            slot.due(m, t0 + REPUBLISH_MIN * 2),
            Due::Unchanged,
            "a row sent after the refused one stands until its own answer"
        );
        slot.unmark(prompt);
        slot.fields.set_agent(Some("idle"));
        assert_eq!(
            slot.due(m, t0 + REPUBLISH_MIN * 3),
            Due::Unchanged,
            "the bus says `idle`, the row that landed, and not the refused `busy`"
        );
        slot.fields.set_agent(Some("busy"));
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN * 3), Due::Changed);
    }

    /// A `minimal` row is owed only when `attention=` moved.
    #[test]
    fn a_minimal_row_is_not_owed_for_a_field_it_never_writes() {
        let t0 = Instant::now();
        let m = Mode::Minimal;
        let mut slot = Slot::default();
        slot.fields.read_meta(
            "OK title=one user_title=one description=- icon=- role=- attention=- cwd=- state=alive",
        );
        published(&mut slot, m, t0);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Unchanged);
        slot.fields.read_meta("OK title=two user_title=two description=- icon=- role=worker attention=- cwd=- state=alive");
        slot.fields.set_detail(Some("claude"));
        slot.fields.set_agent(Some("busy"));
        assert_eq!(
            slot.due(m, t0 + REPUBLISH_MIN * 2),
            Due::Unchanged,
            "minimal writes attention= alone, so nothing else can make a row due"
        );
        assert_eq!(slot.due(Mode::Meta, t0 + REPUBLISH_MIN * 2), Due::Changed);
        slot.fields.read_meta("OK title=two user_title=two description=- icon=- role=worker attention=stuck cwd=- state=alive");
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN * 2), Due::Changed);
    }
}
