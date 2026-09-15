// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PRESENCE WITH MEANING — the fields the bridge writes on every hosted
//! session's presence row so a manager reads WHAT A SESSION IS DOING off the
//! bus instead of off its screen (round 13 C):
//!
//! ```text
//! role=<meta role> detail=<the running program, as `ls` prints it>
//! phase=<busy|idle|prompt|question|limited|survey> [context=<n>%] title=<the user title>
//! ```
//!
//! beside the `attention=` the row already carried. `phase=` and `context=`
//! are read by [`aterm_phase`] — the SAME reader `aterm drive phase` prints
//! from, so the bus and the drive CLI cannot disagree about a worker — over
//! the last [`TAIL_ROWS`] rows of the screen. `role=`, `title=` and
//! `attention=` are the session's own `meta`; `detail=` is the sanitized
//! running command aterm's `status`/`sessions` report (`claude`, `targo test`,
//! `-` at a prompt).
//!
//! `title=` IS `meta user_title=` — the name the owner gave the session with
//! `meta set title` — AND NOTHING ELSE. The terminal title (`meta title=`,
//! the OSC 0/2 string the PROGRAM last wrote) is never published, and that
//! is a privacy rule, not a preference: Claude Code writes a summary of the
//! conversation there (measured 2026-09-14 on the live worker: `◑ SAT-COMP
//! campaign docs handoff`, with no user title), and a `preexec`-titling shell
//! writes the whole command line with its arguments — the very text
//! `detail=` is sanitized to keep off the wire. A session with no user title
//! publishes `title=-`.
//!
//! ## The three rules, and where each is enforced
//!
//! * **NEVER TRANSCRIPT TEXT.** Every token here is a WORD THE BRIDGE CHOSE
//!   (`Phase::name`, one of six), a NUMBER it read, or a value the session's
//!   owner stamped through `meta` / the program name aterm derived. Nothing a
//!   row of the screen says reaches the bus: [`Phase::Limited`] carries the
//!   notice's message and this module publishes only its name;
//!   [`Fields::read_screen`] takes rows and returns nothing but a word and a
//!   percentage. `tests::a_screen_full_of_text_publishes_no_word_of_it` holds
//!   a sentinel against it.
//! * **ON CHANGE ONLY, AND AT MOST ONCE PER [`REPUBLISH_MIN`].** Presence is a
//!   last-value face on an append-forever log (DESIGN §7 forbids a heartbeat
//!   storm): [`Slot::due`] answers `true` only when the fields differ from the
//!   row on the bus AND that row is at least [`REPUBLISH_MIN`] old. A screen
//!   that flips twice inside the window costs one record, carrying the later
//!   state.
//! * **AN IDLE SESSION COSTS NOTHING.** The screen is re-read ONLY when the
//!   session's `status revision=` moved since the last read
//!   ([`Slot::needs_screen`]); `meta` and `status` are the reads the roster
//!   round already pays for. `presence = "minimal"` ([`Mode::Minimal`]) never
//!   reads a screen at all and writes today's fields only.
//!
//! Every value is clamped to a bound of its own ([`TITLE_CAP`] is the design's
//! 128 bytes) on an escape boundary, as the roster's readers expect
//! (`cli.rs`'s `column`, `glance.rs`'s `FIELD_CAP`).

use std::time::{Duration, Instant};

/// The least time between two presence records for one session that differ
/// only in these fields. The roster round is 2 s too, so a steady bridge
/// publishes at most one row per session per round.
pub const REPUBLISH_MIN: Duration = Duration::from_secs(2);

/// How many rows of the screen the phase reader is shown — the live zone
/// around the composer is at the bottom, and `aterm drive` reads the same
/// forty (`run.rs`'s `TAIL_ROWS`).
pub const TAIL_ROWS: usize = 40;

/// The byte cap on `role=`.
pub const ROLE_CAP: usize = 64;
/// The byte cap on `detail=`.
pub const DETAIL_CAP: usize = 128;
/// The byte cap on `title=` — the design's number.
pub const TITLE_CAP: usize = 128;
/// The byte cap on `attention=`: aterm's own (§4.1) and glance's, one number.
pub const ATTENTION_CAP: usize = crate::glance::ATTENTION_CAP;

/// The six words `phase=` can carry, and no other.
pub const PHASES: [&str; 6] = ["busy", "idle", "prompt", "question", "limited", "survey"];

/// The value an absent field renders as — the same dash every reader prints.
pub const ABSENT: &str = "-";

/// `[fabric] presence` / `--presence`: what a session's row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// The default: `role= detail= phase= [context=] title=` beside
    /// `attention=`.
    #[default]
    Meta,
    /// Today's fields only — `attention=` — and no screen is ever read.
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
    /// One of [`PHASES`], or [`ABSENT`] before the first screen read.
    pub phase: String,
    /// The context indicator's percentage, when Claude Code shows it.
    pub context: Option<u8>,
}

impl Default for Fields {
    fn default() -> Self {
        Self {
            attention: ABSENT.to_string(),
            role: ABSENT.to_string(),
            detail: ABSENT.to_string(),
            title: ABSENT.to_string(),
            phase: ABSENT.to_string(),
            context: None,
        }
    }
}

impl Fields {
    /// Take `attention=`, `role=` and the title off a `meta` reply's header
    /// (`OK title= user_title= description= icon= role= attention= cwd=
    /// state=`, every value pct-encoded, `-` unset).
    pub fn read_meta(&mut self, header: &str) {
        let kv = |key: &str| {
            header
                .split_whitespace()
                .find_map(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        };
        self.attention = token(kv("attention").unwrap_or(ABSENT), ATTENTION_CAP);
        self.role = token(kv("role").unwrap_or(ABSENT), ROLE_CAP);
        // THE USER'S TITLE AND ONLY THAT: `meta set title` is a deliberate
        // name for the session (`worker-satcomp`). The terminal title is
        // whatever the program last wrote, and for Claude Code that is a
        // transcript-derived summary (module doc) — it does not fall back.
        let user = kv("user_title").filter(|v| *v != ABSENT && !v.is_empty());
        self.title = token(user.unwrap_or(ABSENT), TITLE_CAP);
    }

    /// Take `detail=` as `status`/`sessions` report it (already pct-encoded).
    pub fn set_detail(&mut self, raw: Option<&str>) {
        self.detail = token(raw.unwrap_or(ABSENT), DETAIL_CAP);
    }

    /// Read the phase word and the context indicator off the screen's rows.
    /// NOTHING ELSE LEAVES THIS FUNCTION: the rows go in, a word and a number
    /// come out.
    pub fn read_screen(&mut self, rows: &[String]) {
        self.phase = phase_word(rows).to_string();
        self.context = aterm_phase::context_left(rows);
    }

    /// The tokens for the presence body, each led by a space, in the order
    /// the readers print them. `minimal` writes `attention=` alone — the row
    /// exactly as it was before this module.
    #[must_use]
    pub fn tokens(&self, mode: Mode) -> String {
        let mut out = format!(" attention={}", self.attention);
        if mode == Mode::Minimal {
            return out;
        }
        out.push_str(&format!(
            " role={} detail={} phase={}",
            self.role, self.detail, self.phase
        ));
        if let Some(n) = self.context {
            out.push_str(&format!(" context={n}%"));
        }
        out.push_str(&format!(" title={}", self.title));
        out
    }
}

/// The phase word for a screen: [`aterm_phase::worker_phase`]'s name, except
/// that an IDLE worker with the session survey parked above its composer is
/// `survey` — the next digit typed into it is taken as a rating, which a
/// manager about to type must know. A busy worker, a prompt, a question and a
/// limit notice all outrank the survey (each is what the worker is waiting
/// on; the survey is what it is parked beside).
#[must_use]
pub fn phase_word(rows: &[String]) -> &'static str {
    let phase = aterm_phase::worker_phase(rows);
    if phase == aterm_phase::Phase::Idle && aterm_phase::survey_open(rows) {
        return "survey";
    }
    phase.name()
}

/// One presence token: printable ASCII only, at most `cap` bytes, cut on a
/// `%XX` boundary, [`ABSENT`] when nothing is left.
///
/// The input is aterm's own reply (pct-encoded, so a space is `%20` and a
/// `-` that is a value is `%2D`) or a stranger's bytes; either way what
/// leaves is one whitespace-free token of graphic ASCII that decodes to a
/// prefix of the original. A cut that would split an escape trims past it,
/// the rule every other clamp in this crate keeps (`halt_reason_token`).
#[must_use]
pub fn token(raw: &str, cap: usize) -> String {
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
    if out.is_empty() {
        return ABSENT.to_string();
    }
    out
}

/// What the bridge remembers per hosted session: the fields as last sampled,
/// the `status revision=` the screen was last read at, and the row on the
/// bus.
#[derive(Debug, Clone, Default)]
pub struct Slot {
    /// The fields as last sampled.
    pub fields: Fields,
    /// Whether `meta` (and, in `meta` mode, the screen) has been sampled into
    /// `fields` at all — a slot the round's `status` created holds only
    /// `detail=` until then, and a row is never published off such a slot.
    pub sampled: bool,
    /// The `status revision=` at the last screen read; `None` before one.
    pub text_rev: Option<u64>,
    /// The tokens on the bus — [`Fields::tokens`] for the mode the row was
    /// published in — and when they were published. THE TOKENS, NOT THE
    /// FIELDS: what decides whether a row is owed is whether the ROW would
    /// differ, and a `minimal` row carries `attention=` alone. Comparing the
    /// whole struct republished a `minimal` bridge on every field it never
    /// writes (measured 2026-09-14: four terminal-title changes, four
    /// `attention=-` rows that differed in `t=` alone).
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
    /// Whether the screen must be read again: never read, or `revision` is a
    /// reading and it moved since the last read. A session whose `status`
    /// could not be read (`None`) after a first read is NOT re-read — nothing
    /// says it moved.
    #[must_use]
    pub fn needs_screen(&self, revision: Option<u64>) -> bool {
        match (self.text_rev, revision) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(last), Some(now)) => last != now,
        }
    }

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
    use aterm_phase::prompt::fixtures::{bash_one_row, composer, rows};

    const SENTINEL: &str = "SECRET-TRANSCRIPT-TEXT";

    fn screen(body: &[&str], footer: &str) -> Vec<String> {
        let mut r = rows(body);
        r.extend(composer(footer));
        r
    }

    fn busy() -> Vec<String> {
        screen(
            &[
                &format!("⏺ {SENTINEL} the plan"),
                "",
                "✶ Deliberating… (3s · esc to interrupt)",
                "",
            ],
            "  ? for shortcuts",
        )
    }

    fn idle_with_context() -> Vec<String> {
        let indicator = format!("{}12% until auto-compact", " ".repeat(96));
        screen(
            &[
                &format!("⏺ {SENTINEL} the plan"),
                "",
                "✻ Cooked for 3s · done 2:41 PM",
                &indicator,
            ],
            "  ? for shortcuts",
        )
    }

    fn question() -> Vec<String> {
        screen(
            &[&format!("⏺ {SENTINEL} — keep it or rewrite it?"), ""],
            "  ? for shortcuts",
        )
    }

    fn limited() -> Vec<String> {
        screen(
            &[
                &format!("⏺ {SENTINEL} first"),
                &format!("  ⎿  You've reached your Fable limit {SENTINEL}. Resets 7:30pm"),
                "",
            ],
            "  ? for shortcuts",
        )
    }

    fn survey() -> Vec<String> {
        screen(
            &[
                &format!("⏺ {SENTINEL} done."),
                "",
                "● How is Claude doing this session? (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ],
            "  ? for shortcuts",
        )
    }

    /// **THE FIVE PHASES AND THE SURVEY, EACH ONE WORD, AND `context=` ONLY
    /// WHEN SHOWN.**
    #[test]
    fn a_screen_reads_as_one_of_the_six_words_and_context_only_when_shown() {
        let mut f = Fields::default();
        for (rows, word) in [
            (busy(), "busy"),
            (idle_with_context(), "idle"),
            (question(), "question"),
            (limited(), "limited"),
            (bash_one_row(), "prompt"),
            (survey(), "survey"),
        ] {
            f.read_screen(&rows);
            assert_eq!(f.phase, word, "{rows:#?}");
            assert!(PHASES.contains(&f.phase.as_str()));
            let t = f.tokens(Mode::Meta);
            if word == "idle" {
                assert_eq!(f.context, Some(12));
                assert!(t.contains(" context=12% "), "{t}");
            } else {
                assert_eq!(f.context, None, "{word}: {t}");
                assert!(!t.contains("context="), "{t}");
            }
        }
    }

    /// **NEVER A WORD OF THE TRANSCRIPT.** Every fixture above carries a
    /// sentinel in the worker's words, a tool's output, a limit notice and a
    /// question; none of it reaches the tokens, in either mode.
    #[test]
    fn a_screen_full_of_text_publishes_no_word_of_it() {
        for rows in [busy(), idle_with_context(), question(), limited(), survey()] {
            assert!(rows.iter().any(|r| r.contains(SENTINEL)));
            let mut f = Fields::default();
            f.read_screen(&rows);
            for mode in [Mode::Meta, Mode::Minimal] {
                let t = f.tokens(mode);
                assert!(!t.contains(SENTINEL), "{t}");
                assert!(!t.contains("plan") && !t.contains("Fable"), "{t}");
            }
        }
        // And `Phase::Limited`'s message in particular — the one phase that
        // carries text — is reduced to its name.
        let mut f = Fields::default();
        f.read_screen(&limited());
        assert_eq!(
            f.tokens(Mode::Meta),
            " attention=- role=- detail=- phase=limited title=-"
        );
    }

    /// **`minimal` IS TODAY'S ROW**: `attention=` alone, whatever was read.
    #[test]
    fn minimal_writes_attention_alone() {
        let mut f = Fields::default();
        f.read_meta("OK title=vim user_title=worker description=- icon=- role=worker attention=stuck cwd=- state=alive");
        f.set_detail(Some("claude"));
        f.read_screen(&busy());
        assert_eq!(f.tokens(Mode::Minimal), " attention=stuck");
        assert_eq!(
            f.tokens(Mode::Meta),
            " attention=stuck role=worker detail=claude phase=busy title=worker"
        );
    }

    /// **THE USER'S TITLE IS THE TITLE, THE TERMINAL'S NEVER IS, AND EVERY
    /// FIELD IS CAPPED ON AN ESCAPE BOUNDARY.**
    ///
    /// The terminal title is the program's: Claude Code writes a summary of
    /// the conversation there, and a `preexec` shell the command line with
    /// its arguments. Neither reaches the bus, whatever `user_title=` says.
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
        // The measured shape (2026-09-14): a program title that carries a
        // secret, no user title. NOT ONE BYTE of it is published, in either
        // mode.
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

    /// **CHANGE ONLY, AND AT MOST ONCE PER [`REPUBLISH_MIN`].**
    #[test]
    fn the_writer_publishes_on_change_only_and_at_most_once_per_window() {
        let t0 = Instant::now();
        let m = Mode::Meta;
        let mut slot = Slot::default();
        assert_eq!(slot.due(m, t0), Due::First);
        slot.fields.read_screen(&busy());
        slot.note_published(m, t0);
        // The same fields again: nothing.
        assert_eq!(slot.due(m, t0 + Duration::from_secs(5)), Due::Unchanged);
        // A change inside the window is CAPPED, not dropped: the next round
        // sees it as changed.
        slot.fields.read_screen(&idle_with_context());
        assert_eq!(slot.due(m, t0 + Duration::from_millis(500)), Due::Capped);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Changed);
        assert!(slot.due(m, t0 + REPUBLISH_MIN).now());
        assert!(!Due::Capped.now() && !Due::Unchanged.now());
        slot.note_published(m, t0 + REPUBLISH_MIN);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN), Due::Unchanged);
        // Only the meaning fields count: a context move alone is a change.
        slot.fields.context = Some(9);
        assert_eq!(slot.due(m, t0 + REPUBLISH_MIN * 2), Due::Changed);
    }

    /// **A `minimal` ROW IS OWED ONLY WHEN `attention=` MOVED.** The review
    /// of 2026-09-14 measured a `minimal` bridge republishing `attention=-`
    /// on every terminal-title change — a field its row never carries — so
    /// what is compared is the ROW, in the mode it is published in.
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
        slot.fields.read_screen(&busy());
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

    /// **AN IDLE SESSION COSTS NOTHING**: the screen is re-read only when the
    /// revision moved.
    #[test]
    fn the_screen_is_read_once_and_then_only_on_a_revision_move() {
        let mut slot = Slot::default();
        assert!(slot.needs_screen(None), "never read: read it");
        assert!(slot.needs_screen(Some(7)));
        slot.text_rev = Some(7);
        assert!(!slot.needs_screen(Some(7)), "nothing moved");
        assert!(
            !slot.needs_screen(None),
            "an unreadable status says nothing moved"
        );
        assert!(slot.needs_screen(Some(8)), "it moved");
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
