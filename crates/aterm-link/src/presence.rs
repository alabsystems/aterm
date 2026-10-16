// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence with meaning: the fields the bridge writes on every hosted
//! session's presence row, so a manager reads what a session is doing off the
//! bus instead of off its screen:
//!
//! ```text
//! role=<meta role> detail=<the running program, as `ls` prints it>
//! phase=<busy|idle|prompt|question|survey|unknown|wall:<kind>> title=<the user title>
//! ```
//!
//! beside the `attention=` the row already carried. `phase=` is the SERVER'S
//! agent verdict — `status agent=`, and the `EVENT <local> agent <word> …`
//! push each time it moves — relayed as the one word it is. The fabric runs
//! no reader of its own: aterm's server reads the screen (with `aterm-phase`,
//! for an identified agent only), and this crate carries what it published.
//! `role=`, `title=` and `attention=` are the session's own `meta`; `detail=`
//! is the sanitized running command aterm's `status`/`sessions` report
//! (`claude`, `targo test`, `-` at a prompt).
//!
//! The fabric reads no screen itself: vendor screen knowledge stays in the
//! server (the harness audit's law 3). The server publishes no context figure,
//! so a row carries no `context=` (`aterm link ls` prints `-` for it).
//!
//! `title=` IS `meta user_title=` — the name the owner gave the session with
//! `meta set title` — AND NOTHING ELSE. The terminal title (`meta title=`,
//! the OSC 0/2 string the PROGRAM last wrote) is never published, and that
//! is a privacy rule: Claude Code writes a summary of the conversation there,
//! and a `preexec`-titling shell writes the whole command line — the text
//! `detail=` is sanitized to keep off the wire. No user title: `title=-`.
//!
//! ## The three rules, and where each is enforced
//!
//! * **NEVER TRANSCRIPT TEXT.** Every token here is a WORD FROM A CLOSED SET
//!   (`phase=` is one of [`PHASES`] or `-`, whatever the server sent —
//!   [`Fields::set_agent`]) or a value the session's owner stamped through
//!   `meta` / the program name aterm derived. No screen is read at all.
//! * **ON CHANGE ONLY, AND AT MOST ONCE PER [`REPUBLISH_MIN`].** Presence is a
//!   last-value face on an append-forever log (DESIGN §7 forbids a heartbeat
//!   storm): [`Slot::due`] is owed only when the row would differ from the
//!   one on the bus AND that row is at least [`REPUBLISH_MIN`] old.
//! * **PUSHED, NOT SAMPLED.** `phase=` arrives with the `EVENT … agent` push
//!   and `role=`/`title=`/`attention=` are re-read on the `EVENT … meta`
//!   push, which marks that session's `meta` read stale first; the roster
//!   round reads `meta` again only for a read that failed or that a push, an
//!   adoption or a `GAP` invalidated. The two-second roster still reads
//!   `status` for each session (its `agent=` and `detail=` ride the read the
//!   hold reconciliation already pays for). An idle session costs no `meta`
//!   read and no screen read. `presence = "minimal"` ([`Mode::Minimal`])
//!   writes `attention=` alone.
//!
//! Every value is clamped to a bound of its own ([`TITLE_CAP`] is the design's
//! 128 bytes) on an escape boundary, as the roster's readers expect
//! (`cli.rs`'s `column`, `glance.rs`'s `FIELD_CAP`).

use std::time::{Duration, Instant};

/// The least time between two presence records for one session that differ
/// only in these fields. A change inside the window is carried by the next
/// roster round (2 s), so a steady bridge publishes at most one row per
/// session per round.
pub const REPUBLISH_MIN: Duration = Duration::from_secs(2);

/// The byte cap on `role=`.
pub const ROLE_CAP: usize = 64;
/// The byte cap on `detail=`.
pub const DETAIL_CAP: usize = 128;
/// The byte cap on `title=` — the design's number.
pub const TITLE_CAP: usize = 128;
/// The byte cap on `attention=`: aterm's own (§4.1) and glance's, one number.
pub const ATTENTION_CAP: usize = crate::glance::ATTENTION_CAP;

/// The words `phase=` can carry beside a wall — the server's `agent=`
/// vocabulary since the per-program reader (`status
/// agent=<busy|prompt|question|wall:<kind>|idle|survey|unknown|->`): `unknown`
/// is an identified agent whose screen its reader cannot vouch for (never
/// "idle"). A WALL is carried as `wall:<kind>` ([`is_wall`]).
pub const PHASES: [&str; 6] = ["busy", "idle", "prompt", "question", "survey", "unknown"];

/// The word an older server sent for an agent at a limit, before walls were
/// named; still carried so a bridge newer than its instance loses nothing.
pub const LEGACY_PHASES: [&str; 1] = ["limited"];

/// `wall:<kind>` with `<kind>` of 1..=32 bytes of `[a-z0-9_-]` — the shape
/// the server's `await agent` admits (aterm-gui `control_session`), so a
/// kind a newer reader names is carried, and text riding on it is not.
#[must_use]
pub fn is_wall(word: &str) -> bool {
    word.strip_prefix("wall:").is_some_and(|k| {
        (1..=32).contains(&k.len())
            && k.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    })
}

/// The value an absent field renders as — the same dash every reader prints.
pub const ABSENT: &str = "-";

/// `[fabric] presence` / `--presence`: what a session's row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// The default: `role= detail= phase= title=` beside `attention=`.
    #[default]
    Meta,
    /// `attention=` alone.
    Minimal,
}

impl Mode {
    /// The spelling in the config and on the command line.
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
    pub fn name(self) -> &'static str {
        match self {
            Mode::Meta => "meta",
            Mode::Minimal => "minimal",
        }
    }
}

/// The meaning fields of one session, each a bounded pct-encoded token or
/// [`ABSENT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fields {
    /// `meta attention=`.
    pub attention: String,
    /// `meta role=`.
    pub role: String,
    /// The running program, as `ls` prints it.
    pub detail: String,
    /// `meta user_title=` when the owner set one, else [`ABSENT`] — NEVER
    /// the terminal's title (module doc).
    pub title: String,
    /// One of [`PHASES`], or [`ABSENT`] when the server names no agent
    /// verdict for the session.
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
    /// Take `attention=`, `role=` and the title off a `meta` reply's header
    /// (`OK title= user_title= description= icon= role= attention= cwd=
    /// state=`, every value pct-encoded, `-` unset).
    pub fn read_meta(&mut self, header: &str) {
        let kv = |key: &str| crate::fabric::kv(header, key);
        self.attention = token(kv("attention").unwrap_or(ABSENT), ATTENTION_CAP);
        self.role = token(kv("role").unwrap_or(ABSENT), ROLE_CAP);
        // The user's title only; never falls back to the terminal title
        // (module doc).
        let user = kv("user_title").filter(|v| *v != ABSENT && !v.is_empty());
        self.title = token(user.unwrap_or(ABSENT), TITLE_CAP);
    }

    /// Take `detail=` as `status`/`sessions` report it (already pct-encoded).
    pub fn set_detail(&mut self, raw: Option<&str>) {
        self.detail = token(raw.unwrap_or(ABSENT), DETAIL_CAP);
    }

    /// Take `phase=` from the server's agent verdict — `status agent=`, or the
    /// word of an `EVENT <local> agent <word> …` push. Only one of [`PHASES`],
    /// a `wall:<kind>` ([`is_wall`]) or an older server's [`LEGACY_PHASES`]
    /// is carried; `-`, an absent field and ANY other token read [`ABSENT`],
    /// so a word this build does not know is never relayed as if it were one.
    pub fn set_agent(&mut self, word: Option<&str>) {
        self.phase = word
            .filter(|w| PHASES.contains(w) || LEGACY_PHASES.contains(w) || is_wall(w))
            .unwrap_or(ABSENT)
            .to_string();
    }

    /// The tokens for the presence body, each led by a space, in the order
    /// the readers print them. `minimal` writes `attention=` alone.
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

/// One presence token: printable ASCII only, at most `cap` bytes, cut on a
/// `%XX` boundary, [`ABSENT`] when nothing is left.
///
/// The input is aterm's own reply (pct-encoded, so a space is `%20` and a
/// `-` that is a value is `%2D`) or a stranger's bytes; either way what
/// leaves is one whitespace-free token of graphic ASCII that decodes to a
/// prefix of the original. A cut that would split an escape trims past it,
/// the rule every clamp in this crate keeps (`clamp_pct`).
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

/// What the bridge remembers per hosted session: the fields as last read or
/// pushed, and the row on the bus.
#[derive(Debug, Clone, Default)]
pub struct Slot {
    /// The fields as last sampled.
    pub fields: Fields,
    /// Whether `meta` has been read into `fields` at all — a slot the round's
    /// `status` created holds only `detail=` and `phase=` until then, and a
    /// row is never published off such a slot. Cleared on a `GAP` (a `meta`
    /// push may have been coalesced away), which makes the next round re-read.
    pub sampled: bool,
    /// The tokens on the bus — [`Fields::tokens`] for the mode the row was
    /// published in — and when. Tokens, not fields: a row is owed only when
    /// the ROW would differ, and a `minimal` row carries `attention=` alone.
    published: Option<(String, Instant)>,
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
    /// The fields moved, but the row on the bus is younger than
    /// [`REPUBLISH_MIN`]; the next round carries the change.
    Capped,
}

impl Due {
    /// Whether a publish is owed now.
    #[must_use]
    pub fn now(self) -> bool {
        matches!(self, Due::First | Due::Changed)
    }
}

impl Slot {
    /// Whether the fields owe a record now, as a row in `mode` — see
    /// [`Due`]. A field `mode` does not write cannot make a row due.
    #[must_use]
    pub fn due(&self, mode: Mode, now: Instant) -> Due {
        match &self.published {
            None => Due::First,
            Some((on_bus, _)) if *on_bus == self.fields.tokens(mode) => Due::Unchanged,
            Some((_, at)) if now.saturating_duration_since(*at) < REPUBLISH_MIN => Due::Capped,
            Some(_) => Due::Changed,
        }
    }

    /// Record that the fields as they are now went onto the bus at `now`, as
    /// a row in `mode`.
    pub fn note_published(&mut self, mode: Mode, now: Instant) {
        self.published = Some((self.fields.tokens(mode), now));
    }
}

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
        // NEGATIVE CONTROLS: the absence spellings, a future word, a word with
        // text riding on it, and the screen's own text all read `-`.
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
        // A program title carrying a secret, no user title: not one byte of
        // it is published, in either mode.
        f.read_meta("OK title=Rotating%20the%20sk-live-OSCSECRET42%20key%20for%20acme user_title=- description=- icon=- role=- attention=- cwd=- state=alive");
        assert_eq!(f.title, ABSENT);
        for mode in [Mode::Meta, Mode::Minimal] {
            let t = f.tokens(mode);
            assert!(!t.contains("OSCSECRET") && !t.contains("Rotating"), "{t}");
        }
        f.read_meta("OK title=x user_title=worker%3Aclaude-satcomp description=- icon=- role=worker attention=- cwd=- state=alive");
        assert_eq!(f.title, "worker%3Aclaude-satcomp");
        assert_eq!(f.role, "worker");
        // A user title that is cleared again clears the field: nothing falls
        // back to the program's.
        f.read_meta("OK title=Rotating%20the%20key user_title=- description=- icon=- role=worker attention=- cwd=- state=alive");
        assert_eq!(f.title, ABSENT);

        // CAPS. A 400-escape title is cut at TITLE_CAP and never inside `%XX`;
        // a long role at ROLE_CAP; a long detail at DETAIL_CAP.
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
        // NO WHITESPACE OR CONTROL BYTE SURVIVES, so a row stays one token per
        // column; an empty or absent value is the dash.
        f.set_detail(Some("targo\u{1b}[2J test\n"));
        assert_eq!(f.detail, "targo[2Jtest");
        f.set_detail(Some(""));
        assert_eq!(f.detail, ABSENT);
        f.set_detail(None);
        assert_eq!(f.detail, ABSENT);
        assert_eq!(token("%", 8), ABSENT);
        assert_eq!(token("ab%2", 8), "ab");
    }

    /// Change only, and at most once per [`REPUBLISH_MIN`].
    #[test]
    fn the_writer_publishes_on_change_only_and_at_most_once_per_window() {
        let t0 = Instant::now();
        let m = Mode::Meta;
        let mut slot = Slot::default();
        assert_eq!(slot.due(m, t0), Due::First);
        slot.fields.set_agent(Some("busy"));
        slot.note_published(m, t0);
        // The same fields again: nothing.
        assert_eq!(slot.due(m, t0 + Duration::from_secs(5)), Due::Unchanged);
        // A change inside the window is CAPPED, not dropped: the next round
        // sees it as changed.
        slot.fields.set_agent(Some("idle"));
        assert_eq!(slot.due(m, t0 + Duration::from_millis(500)), Due::Capped);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Changed);
        assert!(slot.due(m, t0 + REPUBLISH_MIN).now());
        assert!(!Due::Capped.now() && !Due::Unchanged.now());
        slot.note_published(m, t0 + REPUBLISH_MIN);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Unchanged);
        // A verdict the server retracts is a change too.
        slot.fields.set_agent(None);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN * 2), Due::Changed);
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
        slot.note_published(m, t0);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Unchanged);
        // Every unpublished field moves; the row does not.
        slot.fields.read_meta("OK title=two user_title=two description=- icon=- role=worker attention=- cwd=- state=alive");
        slot.fields.set_detail(Some("claude"));
        slot.fields.set_agent(Some("busy"));
        assert_eq!(
            slot.due(m, t0 + REPUBLISH_MIN * 2),
            Due::Unchanged,
            "minimal writes attention= alone, so nothing else can make a row due"
        );
        // And in `meta` mode the same move IS a change.
        assert_eq!(slot.due(Mode::Meta, t0 + REPUBLISH_MIN * 2), Due::Changed);
        // The one field it does write moves: due.
        slot.fields.read_meta("OK title=two user_title=two description=- icon=- role=worker attention=stuck cwd=- state=alive");
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN * 2), Due::Changed);
    }

    /// The config spelling, and no other.
    #[test]
    fn the_mode_has_two_spellings() {
        assert_eq!(Mode::parse("meta"), Some(Mode::Meta));
        assert_eq!(Mode::parse(" minimal "), Some(Mode::Minimal));
        assert_eq!(Mode::parse("full"), None);
        assert_eq!(Mode::parse(""), None);
        assert_eq!(Mode::default(), Mode::Meta);
        assert_eq!(Mode::Meta.name(), "meta");
        assert_eq!(Mode::Minimal.name(), "minimal");
    }
}
