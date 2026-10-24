// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's policy: aterm.toml's `[harness]` table.

use std::path::{Path, PathBuf};

use aterm_phase::{Program, ResetKind};

use super::policy::approval::QuestionPriority;

use super::memory::MemoryCeiling;
use crate::harness::accounts::Mode;
use crate::harness::harness_mark;

/// CRUISE'S CONTINUATION, the default `continue_text` (Autopilot Cruise).
pub const CONTINUE_TEXT: &str = concat!(
    harness_mark!(),
    " Autopilot is Cruise: keep going. Finish everything in hand, verify it and land it, then \
     keep hunting for what would make the work more complete and more correct. Never stop to \
     ask: put what only a person can give on a line starting NEEDS YOU: and carry on with \
     everything else."
);

/// Every `[harness]` key, in the order the help and Settings list them.
pub const KEYS: &[&str] = &[
    "enabled",
    "headless",
    "autopilot",
    "land_at",
    "approve",
    "trust_roots",
    "rm_breaker_ask",
    "answer_questions",
    "question_priority",
    "answer_text",
    "dismiss_surveys",
    "continue",
    "continue_text",
    "continue_per_hour",
    "rules_file",
    "retry_api_errors",
    "probe_api",
    "resume_limits",
    "limit_wait",
    "accounts",
    "limit_reset",
    "limit_reset_kinds",
    "usage_tap",
    "usage_notes",
    "usage_credits",
    "logins",
    "hosts",
    "model_fallback",
    "compact_on_context_wall",
    "autocompact_offer",
    "relaunch",
    "stall_term_after_s",
    "stall_term",
    "memory_ceiling",
    "upgrade",
    "upgrade_ask",
    "human_grace_s",
    "occupancy",
    "facts",
    "stuck_break_note",
];

/// The `[harness]` keys this reader retired.
pub const RETIRED_KEYS: &[&str] = &[
    "approve_all",
    "auto_reads",
    "rm_breaker",
    "read_outside_cwd",
    "trust_dialog",
    "rate_nudge",
];

/// What the supervisor may approve on a permission box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approve {
    All,
    Safe,
    None,
}

impl Approve {
    const fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Safe => 1,
            Self::All => 2,
        }
    }

    /// This level, capped at `cap`: a limit can lower it, never raise it.
    #[must_use]
    pub const fn at_most(self, cap: Self) -> Self {
        if cap.rank() < self.rank() { cap } else { self }
    }

    fn parse(v: &str) -> Option<Self> {
        match v.trim().to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "safe" => Some(Self::Safe),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// The level as `[harness] approve` spells it: `all`, `safe` or `none`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Safe => "safe",
            Self::None => "none",
        }
    }
}

/// How far the harness carries a session's work at its turn ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Autopilot {
    #[default]
    Cruise,
    Land,
    Off,
}

impl Autopilot {
    /// Every mode, in the order the chip cycles them.
    pub const ALL: [Autopilot; 3] = [Autopilot::Cruise, Autopilot::Land, Autopilot::Off];

    /// The mode's word as the table, the session's `meta` word and the chip spell it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Autopilot::Cruise => "cruise",
            Autopilot::Land => "land",
            Autopilot::Off => "off",
        }
    }

    /// A mode from its word ([`Self::word`]) or the owner's own words for it (`forever`, `infinity`: [`Autopilot::Cruise`]; `wrap-up`, `wrapup`, `wrap`: [`Autopilot::Land`]), trimmed, in any case.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "cruise" | "forever" | "infinity" => Some(Autopilot::Cruise),
            "land" | "wrap-up" | "wrapup" | "wrap" => Some(Autopilot::Land),
            "off" => Some(Autopilot::Off),
            _ => None,
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Autopilot::Off => 0,
            Autopilot::Land => 1,
            Autopilot::Cruise => 2,
        }
    }
}

/// WHAT THE HARNESS DOES WITH WHAT IT MEASURES OF A SESSION'S WAIT (`[harness] occupancy`; `docs/DESIGN-harness-occupancy-2026-10-20.md` §1.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Occupancy {
    Off,
    #[default]
    Observe,
    Prompt,
}

impl Occupancy {
    /// Every mode, least power first.
    pub const ALL: [Occupancy; 3] = [Occupancy::Off, Occupancy::Observe, Occupancy::Prompt];

    /// The mode's word as the table spells it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Occupancy::Off => "off",
            Occupancy::Observe => "observe",
            Occupancy::Prompt => "prompt",
        }
    }

    /// A mode from its word ([`Self::word`]), trimmed, in any case.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Occupancy::Off),
            "observe" => Some(Occupancy::Observe),
            "prompt" => Some(Occupancy::Prompt),
            _ => None,
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Occupancy::Off => 0,
            Occupancy::Observe => 1,
            Occupancy::Prompt => 2,
        }
    }

    /// THE ONE PREDICATE a typed fact line passes: a [`PromptGrant`] under `prompt`, and nothing under `observe` or `off`.
    /// Crate-private, so no other crate (the GUI's host, a lab) can mint one: only this crate's
    /// loop asks the owner's configured policy, and the census (`prompt-grant`) fences where.
    #[must_use]
    pub(crate) const fn grant(self) -> Option<PromptGrant> {
        match self {
            Occupancy::Prompt => Some(PromptGrant { _sealed: () }),
            Occupancy::Off | Occupancy::Observe => None,
        }
    }

    /// Whether anything is measured into the journal and the ledger: every mode but `off`.
    #[must_use]
    pub const fn observes(self) -> bool {
        !matches!(self, Occupancy::Off)
    }
}

/// The proof a fact line may be typed: `Occupancy::grant` under `prompt` alone.
#[derive(Debug)]
pub struct PromptGrant {
    _sealed: (),
}

/// The proof the hung-call stuck fact may be typed at a break (`[harness] stuck_break_note`,
/// the hung-call design's R3 and the owner's call O10; the occupancy design's §2.1 rank 4):
/// [`SupervisorConfig::stuck_break_grant`] alone makes one. Not a [`PromptGrant`], which
/// [`Occupancy::grant`] alone makes: the two keys grant two lines, whatever `occupancy` says.
#[derive(Debug)]
pub struct StuckBreakGrant {
    _sealed: (),
}

/// WHICH OWNER'S GRANT a line typed at a break goes under (`Session::type_fact`): the occupancy
/// door's `prompt` or the stuck fact's key.
#[derive(Debug, Clone, Copy)]
pub enum BreakGrant<'g> {
    /// `[harness] occupancy = "prompt"` ([`Occupancy::grant`]).
    Occupancy(&'g PromptGrant),
    /// `[harness] stuck_break_note = true` ([`SupervisorConfig::stuck_break_grant`]).
    StuckBreak(&'g StuckBreakGrant),
}

const fn accounts_rank(mode: Mode) -> u8 {
    match mode {
        Mode::Off => 0,
        Mode::Wall => 1,
        Mode::Early => 2,
    }
}

/// The owner's policy for every supervised agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// The master switch (Settings: search "harness").
    pub enabled: bool,
    /// The agents whose sessions are not supervised.
    pub off: Vec<Program>,
    /// Supervise the sessions of a headless instance too.
    pub headless: bool,
    /// The [`Autopilot`] of a session with no `autopilot` word of its own ([`Self::autopilot_for`]): [`Autopilot::Cruise`] by default.
    pub autopilot: Autopilot,
    /// THE TIME A TIMED CRUISE IS OFFERED TO LAND AT (`[harness] land_at`, local `HH:MM`, `docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.4).
    pub land_at: Option<LandAt>,
    /// What a permission box may be answered with, one level for every agent.
    pub approve: Approve,
    /// Folders whose trust dialog is answered under [`Approve::Safe`] — Claude Code's.
    pub trust_roots: Vec<String>,
    /// Under `approve = "safe"`, hand an unproven rm circuit breaker to a person.
    pub rm_breaker_ask: bool,
    /// Answer a question, a choice between options or a request for a decision with [`Self::answer_text`] instead of holding it for a person.
    pub answer_questions: bool,
    /// How a question is answered while [`Self::answer_questions`] answers it (`[harness] question_priority`).
    pub question_priority: QuestionPriority,
    /// What a question is answered with.
    pub answer_text: String,
    /// Dismiss Claude Code's rating surveys.
    pub dismiss_surveys: bool,
    /// Type a continuation when a worker ends a turn with work left.
    pub continue_policy: bool,
    /// What a continuation types when the worker offers no suggestion of its own.
    pub continue_text: String,
    /// At most this many continuations per session per hour; `0` is no cap.
    pub continue_per_hour: u32,
    /// A file whose text is appended to a continuation (standing rules).
    pub rules_file: Option<PathBuf>,
    /// Answer an API error or an overload, however many, by what it says went wrong (the network, a
    /// reply cut off, the server), never with an ask, but for a Gemini CLI error other than a 429
    /// or 5xx and agy's non-retryable 4xx, which go to a person.
    pub retry_api_errors: bool,
    /// Let the window's host probe API reachability (`api.anthropic.com` TLS handshake) while a session waits on a network API error.
    pub probe_api: bool,
    /// Continue once a session or weekly usage limit has reset.
    pub resume_limits: bool,
    /// At Claude Code's usage-limit dialog (`What do you want to do?`), choose `Wait here, then continue automatically …` by its label (never a spending row, never `Stop`), arming the vendor's own continue for the reset (`limit-wait@v1`); at Gemini CLI's, wait through the reset it names (or, naming none, the back-off ladder's rung while a person is told), then choose `Keep trying` (`limit-retry@v1`), or on its per-model dialog `Stop` (`limit-stop@v1`, never the switch nor the upgrade) and then the resume. Off: the dialog is a person's.
    pub limit_wait: bool,
    /// At Claude Code's autocompact offer (`We recommend compacting earlier`, 2.1.302), choose `Autocompact at ‹ <size> › tokens` at this size, in kilo-tokens (`autocompact-offer@v1`).
    pub autocompact_offer: Option<u32>,
    /// Which sessions rotate across the owner's listed Claude Code accounts (`accounts.toml` beside this file, `aterm harness accounts`; `docs/DESIGN-claude-accounts-2026-10-01.md` §3, §6).
    pub accounts: Mode,
    /// Take a limit reset the plan offers at Claude Code's usage-limit dialog, ahead of [`Self::limit_wait`]'s row (`limit-reset@v1`; `docs/SPEC-claude-accounts-resets-2026-10-02.md`).
    pub limit_reset: bool,
    /// WHICH KINDS OF LIMIT RESET [`Self::limit_reset`] MAY SPEND, AND IN WHAT ORDER (`[harness] limit_reset_kinds = ["full", "weekly", "5-hour"]`; [`reset_kind_word`] names each kind).
    pub limit_reset_kinds: Vec<ResetKind>,
    /// The usage tap (`crate::harness::tap`): the footer's live weekly figure and the account-stats recorder; at a usage limit its readings time the continuation (`super::tap_clock`). On by default; `false` is a limit.
    pub usage_tap: bool,
    /// On a machine with no listed Claude Code accounts, track the single account's usage from session screens and raise the digest's two lines from it.
    pub usage_notes: bool,
    /// CONTINUING ON USAGE CREDITS: off by default, so a session stays on its subscription; `true` lets the harness carry a session onto paid usage credits.
    pub usage_credits: bool,
    /// THE LOGIN LIFECYCLE (`[harness] logins`, `"renew"` or `"off"`; `docs/DESIGN-claude-accounts-v2-2026-10-15.md` §5).
    pub logins: bool,
    /// The machines `aterm harness accounts renew --due` renews on in one sitting and `aterm ai doctor --fleet` runs on, over SSH (`host` or `host:/abs/path/to/aterm`).
    pub hosts: Vec<String>,
    /// Claude Code's model fallback.
    pub model_fallback: Option<String>,
    /// Type `/compact` when the worker stops on a full context.
    pub compact_on_context_wall: bool,
    /// Relaunch a crashed agent (Claude Code, Muse Code, agy, Gemini CLI, or codex-alab) on its validated session or conversation.
    pub relaunch: bool,
    /// How long an agent stands frozen (not reading its input, nobody's hand on it) before the supervisor restarts it (`restart if-stalled`) for its relaunch (`supervise/stall.rs`).
    pub stall_term_after_s: u32,
    /// Restart a frozen agent (`restart pid=<n> if-stalled`) once it has stood [`Self::stall_term_after_s`].
    pub stall_term: bool,
    /// How much memory a supervised agent's process group (physical footprint) may use before aterm warns.
    pub memory_ceiling: MemoryCeiling,
    /// Restart an idle agent onto a newer installed build of itself.
    pub upgrade: bool,
    /// The live upgrade asks first.
    pub upgrade_ask: bool,
    /// Seconds the supervisor keeps its hands off a session after a person's keystroke into it.
    pub human_grace_s: u32,
    /// What the harness does with a session's measured wait ([`Occupancy`]).
    pub occupancy: Occupancy,
    /// AN `aterm:` FACT ON A PROMPT THE HARNESS TYPES ANYWAY (`[harness] facts`, system
    /// awareness §14.4): a continuation, a done check, Land's instruction or its nudge may end
    /// with one measured fact of the session's own (a background call of the agent's that has
    /// made no progress), once per episode, only where it fits whole. `false` is a limit: no fact
    /// rides, and none is typed at a break either.
    pub facts: bool,
    /// THE STUCK FACT AT A BREAK (`[harness] stuck_break_note`, the owner's call O10): at a break
    /// of the agent's own background work, where no turn end comes for a fact to ride, the
    /// session's untold no-progress fact is typed as one line of its own, once per episode. A
    /// key whose `true` is more power: it ships `false`, and only the root `[harness]` table's
    /// `true` turns it on ([`Self::stuck_break_grant`]).
    pub stuck_break_note: bool,
    /// Safe rules a retired key switched off ([`RETIRED_KEYS`]); no key of its own sets it.
    pub withheld: Withheld,
}

/// A LOCAL CLOCK TIME, `HH:MM` in 24-hour time (`[harness] land_at`, [`SupervisorConfig::land_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LandAt {
    minute: u16,
}

impl LandAt {
    /// `HH:MM` with two digits each, `00:00` to `23:59`, surrounding blanks ignored.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (h, m) = text.split_once(':')?;
        if h.len() != 2 || m.len() != 2 || !(h.bytes().chain(m.bytes())).all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let (h, m) = (h.parse::<u16>().ok()?, m.parse::<u16>().ok()?);
        (h < 24 && m < 60).then_some(Self { minute: h * 60 + m })
    }

    /// The time at `minute` of the day, `None` past `23:59`.
    #[must_use]
    pub fn of_minute(minute: u16) -> Option<Self> {
        (minute < 24 * 60).then_some(Self { minute })
    }

    /// The minute of the day it names (`0..1440`).
    #[must_use]
    pub const fn minute(self) -> u16 {
        self.minute
    }

    /// `HH:MM`, as [`Self::parse`] takes it back.
    #[must_use]
    pub fn text(self) -> String {
        format!("{:02}:{:02}", self.minute / 60, self.minute % 60)
    }
}

/// The two safe rules only a retired key can take away ([`RETIRED_KEYS`]), each read under `approve = "safe"`, the level the same key caps `approve` at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Withheld {
    /// `rm_breaker = false`: an rm is never proven under a scratch root.
    pub rm_breaker: bool,
    /// `read_outside_cwd = false`: a Read box is never proven.
    pub read_outside_cwd: bool,
}

impl Withheld {
    const fn rules_mut(&mut self) -> [(&'static str, &mut bool); 2] {
        let Self {
            rm_breaker,
            read_outside_cwd,
        } = self;
        [
            ("rm_breaker", rm_breaker),
            ("read_outside_cwd", read_outside_cwd),
        ]
    }
}

/// [`SupervisorConfig::policy_texts`]: the whole policy as the texts [`SupervisorConfig::set`] takes back, for a tape's header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyTexts {
    /// [`SupervisorConfig::texts`]: every key of [`KEYS`], in its order.
    pub keys: Vec<(String, String)>,
    /// [`SupervisorConfig::agent_texts`]: `<agent>.enabled` per supervisable program.
    pub agents: Vec<(String, String)>,
    /// [`SupervisorConfig::withheld_texts`]: each withheld rule's retired key, `false`.
    pub withheld: Vec<(String, String)>,
}

/// [`SupervisorConfig::rebuild`]'s answer: the policy, and what of it the texts did not say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebuilt {
    pub policy: SupervisorConfig,
    /// Each key of [`KEYS`] and each agent's switch the texts did not name (texts from a build before it), left at its full-power default.
    pub supplied: Vec<String>,
    /// For each key the texts named that this build has retired, the note [`SupervisorConfig::set`] gave as it read it.
    pub retired: Vec<String>,
    /// For each value [`SupervisorConfig::set`] refused, the note it gave as it read the value the way an owner's file is read (the key's default kept, or an off read as its limit), and the value it read in its place (`…; this build read <key>=<text>`).
    pub refused: Vec<String>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            off: Vec::new(),
            headless: true,
            autopilot: Autopilot::Cruise,
            land_at: None,
            approve: Approve::All,
            trust_roots: Vec::new(),
            rm_breaker_ask: false,
            answer_questions: true,
            question_priority: QuestionPriority::Recommended,
            answer_text: "Nobody is here to answer. Decide for yourself with your best \
                          judgment: take the option you would recommend, prefer reversible \
                          steps, and keep going."
                .to_string(),
            dismiss_surveys: true,
            continue_policy: true,
            continue_text: CONTINUE_TEXT.to_string(),
            continue_per_hour: 0,
            rules_file: None,
            retry_api_errors: true,
            probe_api: true,
            resume_limits: true,
            limit_wait: true,
            autocompact_offer: Some(AUTOCOMPACT_DEFAULT_K),
            accounts: Mode::Early,
            limit_reset: false,
            limit_reset_kinds: Vec::new(),
            usage_tap: true,
            usage_notes: false,
            usage_credits: false,
            logins: true,
            hosts: Vec::new(),
            model_fallback: Some("opus".to_string()),
            compact_on_context_wall: true,
            relaunch: true,
            stall_term_after_s: 600,
            stall_term: false,
            memory_ceiling: MemoryCeiling::Auto,
            upgrade: true,
            upgrade_ask: false,
            human_grace_s: 120,
            occupancy: Occupancy::Observe,
            facts: true,
            stuck_break_note: false,
            withheld: Withheld::default(),
        }
    }
}

impl SupervisorConfig {
    /// Set one key from its value's text.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let v = value.trim();
        let counts = matches!(key, "continue_per_hour" | "human_grace_s");
        let bad = |this: &mut Self, what: &str| {
            if spells_off(v) && !counts {
                this.limit(key);
                Err(format!(
                    "harness.{key}: expected {what}, got {v:?} — an off, read as its limit"
                ))
            } else {
                Err(format!(
                    "harness.{key}: expected {what}, got {v:?} — no setting, so {key} keeps its default"
                ))
            }
        };
        let flag = switch_word(v);
        if let Some((agent, leaf)) = agent_key(key) {
            if leaf != "enabled" {
                return Err(per_agent_only(key));
            }
            match flag {
                Some(false) => self.switch_off(agent),
                Some(true) => {}
                None => return bad(self, "true or false"),
            }
            return Ok(());
        }
        let text = (!v.is_empty()).then(|| v.to_string());
        macro_rules! switch {
            ($field:ident) => {
                match flag {
                    Some(b) => self.$field = b,
                    None => return bad(self, "true or false"),
                }
            };
        }
        match key {
            "enabled" => switch!(enabled),
            "headless" => switch!(headless),
            "answer_questions" => switch!(answer_questions),
            "dismiss_surveys" => switch!(dismiss_surveys),
            "continue" => switch!(continue_policy),
            "retry_api_errors" => switch!(retry_api_errors),
            "probe_api" => switch!(probe_api),
            "resume_limits" => switch!(resume_limits),
            "limit_wait" => switch!(limit_wait),
            "limit_reset" => switch!(limit_reset),
            "usage_tap" => switch!(usage_tap),
            "usage_notes" => switch!(usage_notes),
            "usage_credits" => switch!(usage_credits),
            "compact_on_context_wall" => switch!(compact_on_context_wall),
            "relaunch" => switch!(relaunch),
            "upgrade" => switch!(upgrade),
            "upgrade_ask" => switch!(upgrade_ask),
            "stall_term" => switch!(stall_term),
            "rm_breaker_ask" => switch!(rm_breaker_ask),
            "facts" => switch!(facts),
            "stuck_break_note" => switch!(stuck_break_note),
            "approve" => match Approve::parse(value) {
                Some(a) => self.approve = a,
                None => return bad(self, "all, safe or none"),
            },
            "autopilot" => match Autopilot::parse(value) {
                Some(a) => self.autopilot = a,
                None => return bad(self, "cruise, land or off"),
            },
            "occupancy" => match Occupancy::parse(value) {
                Some(o) => self.occupancy = o,
                None => return bad(self, "off, observe or prompt"),
            },
            "land_at" if v.is_empty() || spells_off(v) => self.land_at = None,
            "land_at" => match LandAt::parse(v) {
                Some(at) => self.land_at = Some(at),
                None => {
                    return Err(format!(
                        "harness.land_at: expected a local time HH:MM (00:00 to 23:59), got {v:?} \
                         — no setting, so land_at keeps its default"
                    ));
                }
            },
            "accounts" => match Mode::parse(&v.to_ascii_lowercase()) {
                Some(mode) => self.accounts = mode,
                None => return bad(self, "early, wall or off"),
            },
            "logins" => match v.to_ascii_lowercase().as_str() {
                "renew" => self.logins = true,
                "off" => self.logins = false,
                _ => return bad(self, "renew or off"),
            },
            "limit_reset_kinds" => match reset_kinds(v) {
                Ok(kinds) => self.limit_reset_kinds = kinds,
                Err(why) => {
                    return Err(format!(
                        "harness.limit_reset_kinds: {why} — no setting, so limit_reset_kinds \
                         keeps its default"
                    ));
                }
            },
            "hosts" => {
                self.hosts = value
                    .split(',')
                    .filter_map(|v| {
                        let v = v.trim();
                        (!v.is_empty()).then(|| v.to_string())
                    })
                    .collect();
            }
            "question_priority" => match QuestionPriority::parse(value) {
                Some(p) => self.question_priority = p,
                None => {
                    return Err(format!(
                        "harness.question_priority: expected {}, got {:?} — no setting, so \
                         question_priority keeps its default",
                        QuestionPriority::WORDS.join(", "),
                        value.trim()
                    ));
                }
            },
            "trust_roots" => {
                self.trust_roots = value
                    .split(',')
                    .filter_map(|v| {
                        let v = v.trim();
                        (!v.is_empty()).then(|| v.to_string())
                    })
                    .collect();
            }
            "answer_text" => match text {
                Some(t) => self.answer_text = t,
                None => {
                    return Err(format!(
                        "harness.{key}: expected a non-empty text — {key} keeps its default"
                    ));
                }
            },
            "continue_text" => match text {
                Some(t) => self.continue_text = t,
                None => {
                    return Err(format!(
                        "harness.{key}: expected a non-empty text — {key} keeps its default"
                    ));
                }
            },
            "continue_per_hour" => match value.trim().parse() {
                Ok(n) => self.continue_per_hour = n,
                Err(_) => return bad(self, "a count"),
            },
            "human_grace_s" => match value.trim().parse() {
                Ok(n) => self.human_grace_s = n,
                Err(_) => return bad(self, "a count of seconds"),
            },
            "rules_file" | "model_fallback" if flag == Some(true) => {
                return Err(format!(
                    "harness.{key}: expected a name, got {v:?} — no setting, so {key} keeps its \
                     default"
                ));
            }
            "rules_file" | "model_fallback" if spells_off(v) || v.eq_ignore_ascii_case("none") => {
                if key == "rules_file" {
                    self.rules_file = None;
                } else {
                    self.model_fallback = None;
                }
            }
            "stall_term_after_s" => match value.trim().parse() {
                Ok(n) => self.stall_term_after_s = n,
                Err(_) => return bad(self, "a count of seconds (0: never)"),
            },
            "memory_ceiling" => match MemoryCeiling::parse(value) {
                Some(c) => self.memory_ceiling = c,
                None => return bad(self, "auto, off or a whole number of GiB"),
            },
            "autocompact_offer" => match autocompact_size(v) {
                Some(size) => self.autocompact_offer = size,
                None => return bad(self, "a size of 300k to 900k, or off"),
            },
            "rules_file" => self.rules_file = text.map(PathBuf::from),
            "model_fallback" => self.model_fallback = text,
            retired if RETIRED_KEYS.contains(&retired) => {
                return Err(match retired_reading(v) {
                    Some(on) => self.retired(retired, !on),
                    None => retired_no_setting(retired),
                });
            }
            other => return Err(unknown_key(other)),
        }
        Ok(())
    }

    fn retired(&mut self, key: &str, limits: bool) -> String {
        if key == "rate_nudge" {
            return format!(
                "harness.{key}: {RETIRED_KEY} ({}): the Codex harness is retired, so it changes \
                 nothing — remove it",
                retired_in(key)
            );
        }
        if !limits {
            return format!(
                "harness.{key}: {RETIRED_KEY} ({RETIRED_IN}); `true` is the full-power default, so \
                 it changes nothing — remove it"
            );
        }
        let (cap, what, instead) = match key {
            "approve_all" => (
                Approve::Safe,
                "approve at most \"safe\"",
                "approve = \"safe\"",
            ),
            "auto_reads" => (Approve::None, "approve = \"none\"", "approve = \"none\""),
            "rm_breaker" => {
                self.withheld.rm_breaker = true;
                (
                    Approve::Safe,
                    "approve at most \"safe\", and no rm answered under a scratch root",
                    "approve = \"safe\"",
                )
            }
            "read_outside_cwd" => {
                self.withheld.read_outside_cwd = true;
                (
                    Approve::Safe,
                    "approve at most \"safe\", and no Read box answered",
                    "approve = \"safe\"",
                )
            }
            _ => {
                self.trust_roots.clear();
                (
                    Approve::Safe,
                    "approve at most \"safe\", and no trust root",
                    "approve = \"safe\" and trust_roots = []",
                )
            }
        };
        self.approve = self.approve.at_most(cap);
        format!(
            "harness.{key}: {RETIRED_KEY} ({RETIRED_IN}), read as the limit it named — {what}; \
             write `{instead}` instead"
        )
    }

    /// Set `key` to its limiting value: the least power it can give.
    pub fn limit(&mut self, key: &str) {
        match key {
            "enabled" => self.enabled = false,
            "headless" => self.headless = false,
            "autopilot" => self.autopilot = Autopilot::Off,
            "approve" => self.approve = Approve::None,
            "answer_questions" => self.answer_questions = false,
            "dismiss_surveys" => self.dismiss_surveys = false,
            "continue" => self.continue_policy = false,
            "continue_per_hour" => self.continue_per_hour = 1,
            "retry_api_errors" => self.retry_api_errors = false,
            "probe_api" => self.probe_api = false,
            "resume_limits" => self.resume_limits = false,
            "limit_wait" => self.limit_wait = false,
            "autocompact_offer" => self.autocompact_offer = None,
            "accounts" => self.accounts = Mode::Off,
            "logins" => self.logins = false,
            "hosts" => self.hosts.clear(),
            "limit_reset" => self.limit_reset = false,
            "limit_reset_kinds" => {}
            "usage_tap" => self.usage_tap = false,
            "usage_notes" => self.usage_notes = false,
            "usage_credits" => self.usage_credits = false,
            "model_fallback" => self.model_fallback = None,
            "compact_on_context_wall" => self.compact_on_context_wall = false,
            "relaunch" => self.relaunch = false,
            "stall_term_after_s" => self.stall_term_after_s = 0,
            "memory_ceiling" => self.memory_ceiling = MemoryCeiling::Off,
            "upgrade" => self.upgrade = false,
            "upgrade_ask" => self.upgrade_ask = true,
            "rm_breaker_ask" => self.rm_breaker_ask = true,
            "stall_term" => self.stall_term = false,
            "human_grace_s" => self.human_grace_s = 3600,
            "occupancy" => self.occupancy = Occupancy::Off,
            "facts" => self.facts = false,
            "stuck_break_note" => self.stuck_break_note = false,
            retired if RETIRED_KEYS.contains(&retired) => {
                let _ = self.retired(retired, true);
            }
            _ => {
                if let Some((agent, "enabled")) = agent_key(key) {
                    self.switch_off(agent);
                }
            }
        }
    }

    /// Tests: the full-power default with `key` alone moved off it.
    #[cfg(test)]
    pub(crate) fn moved_off_default(key: &str) -> Self {
        let full = Self::default();
        let mut moved = full.clone();
        moved.limit(key);
        for value in ["true", "long-term", "~/flip/root", "18:00", "full"] {
            if moved != full {
                break;
            }
            let mut c = full.clone();
            if c.set(key, value).is_ok() {
                moved = c;
            }
        }
        assert_ne!(moved, full, "no value moves {key} off its default");
        moved
    }

    /// Whether `agent`'s sessions are supervised: the master switch on and that agent not switched off by its own table ([`Self::off`]).
    #[must_use]
    pub fn on_for(&self, agent: Program) -> bool {
        self.enabled && !self.off.contains(&agent)
    }

    /// Whether the usage tap ([`Self::usage_tap`]) is on for Claude Code, the one reading every surface shares: the relaunch lines a pass arms it on (the window's lane, `aterm harness upgrade`), the footer's reason for a missing week, and `accounts stats`' tap.
    #[must_use]
    pub fn usage_tap_on(&self) -> bool {
        self.on_for(Program::Claude) && self.usage_tap
    }

    fn switch_off(&mut self, agent: Program) {
        if !self.off.contains(&agent) {
            self.off.push(agent);
            self.off.sort_by_key(|p| p.name());
        }
    }

    /// A session's Autopilot.
    #[must_use]
    pub fn autopilot_for(&self, word: Option<Autopilot>) -> Autopilot {
        word.unwrap_or(self.autopilot)
    }

    /// THE ONE PREDICATE the stuck fact's line at a break passes: a [`StuckBreakGrant`] where the
    /// owner's root table turned [`Self::stuck_break_note`] on and [`Self::facts`] is not off,
    /// whatever [`Self::occupancy`] says; nothing otherwise.
    #[must_use]
    pub const fn stuck_break_grant(&self) -> Option<StuckBreakGrant> {
        if self.stuck_break_note && self.facts {
            Some(StuckBreakGrant { _sealed: () })
        } else {
            None
        }
    }

    /// The reader of aterm.toml's `[harness]` table.
    #[must_use]
    pub fn from_aterm_toml(text: &str) -> (Self, Vec<String>) {
        let mut cfg = Self::default();
        let mut notes = Vec::new();
        match aterm_toml::from_str::<aterm_toml::Value>(text) {
            Ok(doc) => {
                if let Some(h) = doc.get("harness").filter(|h| h.as_table().is_none()) {
                    if one_value_text(h).is_some_and(|t| spells_off(&t)) {
                        cfg.enabled = false;
                        notes.push(
                            "harness: written off itself — the master switch is off (write \
                             `enabled = false` under [harness])"
                                .to_string(),
                        );
                    } else {
                        notes.push(format!(
                            "harness: not a table (a TOML {}) — ignored",
                            h.type_str()
                        ));
                    }
                }
                let mut misplaced = Vec::new();
                harness_tables(&doc, String::new(), &mut misplaced);
                let root = doc.get("harness").and_then(aterm_toml::Value::as_table);
                let (retired, current): (Vec<_>, Vec<_>) = root
                    .map(entries)
                    .unwrap_or_default()
                    .into_iter()
                    .partition(|(key, _)| RETIRED_KEYS.contains(&key.as_str()));
                let disk_notes: Vec<String> = crate::harness::cli::disk_faults(text)
                    .into_iter()
                    .filter(|f| f.path == "harness.disk" || f.path.starts_with("harness.disk."))
                    .map(|f| f.note)
                    .collect();
                for (key, value) in current.into_iter().chain(retired) {
                    if key == "disk" && !disk_notes.is_empty() {
                        notes.extend(disk_notes.iter().cloned());
                        continue;
                    }
                    if let Err(e) = cfg.set_value(&key, value) {
                        notes.push(e);
                    }
                }
                for (at, table) in misplaced {
                    for (key, value) in entries(table) {
                        let before = cfg.clone();
                        let refused = cfg.set_value(&key, value).err();
                        let limits = cfg.limits(&before);
                        if !limits {
                            cfg = before;
                        }
                        notes.push(misplaced_note(&at, &key, limits, refused));
                    }
                }
                let mut scalars = Vec::new();
                harness_scalars(&doc, String::new(), &mut scalars);
                for (at, value) in scalars {
                    if one_value_text(value).is_some_and(|t| spells_off(&t)) {
                        cfg.enabled = false;
                        notes.push(misplaced_note(&at, "enabled", true, None));
                    }
                }
            }
            Err(e) => {
                notes.push(format!(
                    "aterm.toml is not valid TOML ({}); its [harness] lines were read one by one",
                    e.to_string().lines().next().unwrap_or("").trim()
                ));
                let mut seen: Vec<String> = Vec::new();
                let (retired, current): (Vec<_>, Vec<_>) =
                    assignments(text).into_iter().partition(|(path, _)| {
                        path.last().is_some_and(|key| RETIRED_KEYS.contains(key))
                    });
                for (path, value) in current.into_iter().chain(retired) {
                    let (head, key, one_value): (&[&str], String, String) = match path.as_slice() {
                        [head @ .., "harness"] if !value.trim_start().starts_with(['[', '{']) => {
                            if !spells_off(&unquote(value)) {
                                continue;
                            }
                            (head, "enabled".to_string(), "false".to_string())
                        }
                        [head @ .., "harness", key] => (head, key.to_string(), unquote(value)),
                        [head @ .., "harness", agent, key] if agent_named(agent).is_some() => {
                            (head, format!("{agent}.{key}"), unquote(value))
                        }
                        _ => continue,
                    };
                    let key = key.as_str();
                    let quoted = value.trim_start().starts_with(['"', '\'']);
                    let before = cfg.clone();
                    let list = matches!(key, "trust_roots" | "hosts" | "limit_reset_kinds");
                    let refused = if value.trim_start().starts_with('[') && !list {
                        Some(no_shape(key, "array"))
                    } else {
                        cfg.set_scalar(key, &one_value, quoted || list).err()
                    };
                    let limits = cfg.limits(&before);
                    let repeat = head.is_empty() && seen.iter().any(|k| k == key);
                    if head.is_empty() {
                        seen.push(key.to_string());
                    }
                    if !(limits || head.is_empty() && !repeat) {
                        cfg = before;
                    }
                    if !head.is_empty() {
                        let at = head.join(".").replace(".[]", "[]");
                        notes.push(misplaced_note(&at, key, limits, refused));
                    } else if repeat && !limits {
                        notes.push(format!(
                            "harness.{key}: said again below; a repeat may only limit, so it \
                             changed nothing"
                        ));
                    } else {
                        notes.extend(refused);
                    }
                }
            }
        }
        (cfg, notes)
    }

    /// [`Self::from_aterm_toml`] over the file at `path`: no path or no file is the defaults.
    #[must_use]
    pub fn from_path(path: Option<&Path>) -> (Self, Vec<String>) {
        let Some(path) = path else {
            return (Self::default(), Vec::new());
        };
        match std::fs::read(path) {
            Ok(bytes) => Self::from_aterm_toml(&String::from_utf8_lossy(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), Vec::new()),
            Err(e) => (
                Self::default(),
                vec![format!(
                    "{}: cannot be read ({e}); the defaults apply",
                    path.display()
                )],
            ),
        }
    }

    fn set_value(&mut self, key: &str, value: &aterm_toml::Value) -> Result<(), String> {
        use aterm_toml::Value;
        if let Some((program, leaf)) = agent_key(key)
            && own_keys(program).contains(&leaf)
        {
            return Ok(());
        }
        match value {
            Value::String(text) => self.set_scalar(key, text, true),
            Value::Boolean(_) | Value::Integer(_) => {
                self.set_scalar(key, &scalar_text(value).unwrap_or_default(), false)
            }
            Value::Array(_) if matches!(key, "trust_roots" | "hosts" | "limit_reset_kinds") => {
                match scalar_text(value) {
                    Some(text) => self.set(key, &text),
                    None => Err(no_shape(key, value.type_str())),
                }
            }
            _ if RETIRED_KEYS.contains(&key) => Err(retired_no_setting(key)),
            _ if KEYS.contains(&key) => Err(no_shape(key, value.type_str())),
            _ => match agent_key(key) {
                Some((_, "enabled")) => Err(no_shape(key, value.type_str())),
                Some(_) => Err(per_agent_only(key)),
                None => Err(unknown_key(key)),
            },
        }
    }

    fn set_scalar(&mut self, key: &str, text: &str, quoted: bool) -> Result<(), String> {
        if !quoted && matches!(key, "answer_text" | "continue_text") {
            return Err(format!(
                "harness.{key}: expected a text, got a bare {text} — no setting, so {key} keeps \
                 its default"
            ));
        }
        self.set(key, text)
    }

    fn limits(&self, before: &Self) -> bool {
        let cap = |n: u32| if n == 0 { u32::MAX } else { n };
        let no_more = |now: bool, then: bool| !now || then;
        self != before
            && no_more(self.enabled, before.enabled)
            && before.off.iter().all(|agent| self.off.contains(agent))
            && no_more(self.headless, before.headless)
            && self.autopilot.rank() <= before.autopilot.rank()
            && self.approve.rank() <= before.approve.rank()
            && no_more(!self.rm_breaker_ask, !before.rm_breaker_ask)
            && self
                .trust_roots
                .iter()
                .all(|r| before.trust_roots.contains(r))
            && no_more(self.answer_questions, before.answer_questions)
            && self.question_priority == before.question_priority
            && self.land_at == before.land_at
            && self.answer_text == before.answer_text
            && no_more(self.dismiss_surveys, before.dismiss_surveys)
            && no_more(self.continue_policy, before.continue_policy)
            && self.continue_text == before.continue_text
            && cap(self.continue_per_hour) <= cap(before.continue_per_hour)
            && self.rules_file == before.rules_file
            && no_more(self.retry_api_errors, before.retry_api_errors)
            && no_more(self.probe_api, before.probe_api)
            && no_more(self.resume_limits, before.resume_limits)
            && no_more(self.limit_wait, before.limit_wait)
            && (self.autocompact_offer.is_none()
                || self.autocompact_offer == before.autocompact_offer)
            && accounts_rank(self.accounts) <= accounts_rank(before.accounts)
            && no_more(self.limit_reset, before.limit_reset)
            && (!self.limit_reset || self.limit_reset_kinds == before.limit_reset_kinds)
            && no_more(self.usage_tap, before.usage_tap)
            && no_more(self.usage_notes, before.usage_notes)
            && no_more(self.usage_credits, before.usage_credits)
            && no_more(self.logins, before.logins)
            && self.hosts.iter().all(|h| before.hosts.contains(h))
            && (self.model_fallback.is_none() || self.model_fallback == before.model_fallback)
            && no_more(self.compact_on_context_wall, before.compact_on_context_wall)
            && no_more(self.relaunch, before.relaunch)
            && cap(self.stall_term_after_s) >= cap(before.stall_term_after_s)
            && no_more(self.stall_term, before.stall_term)
            && self.memory_ceiling.no_more_than(before.memory_ceiling)
            && no_more(self.upgrade, before.upgrade)
            && no_more(!self.upgrade_ask, !before.upgrade_ask)
            && self.human_grace_s >= before.human_grace_s
            && self.occupancy.rank() <= before.occupancy.rank()
            && no_more(self.facts, before.facts)
            && no_more(self.stuck_break_note, before.stuck_break_note)
            && (self.withheld.rm_breaker || !before.withheld.rm_breaker)
            && (self.withheld.read_outside_cwd || !before.withheld.read_outside_cwd)
    }

    /// Every key's value, in [`KEYS`] order, as the text [`Self::set`] takes back.
    #[must_use]
    pub fn texts(&self) -> [(&'static str, String); 40] {
        let Self {
            enabled,
            off: _,
            headless,
            autopilot,
            land_at,
            approve,
            trust_roots,
            rm_breaker_ask,
            answer_questions,
            question_priority,
            answer_text,
            dismiss_surveys,
            continue_policy,
            continue_text,
            continue_per_hour,
            rules_file,
            retry_api_errors,
            probe_api,
            resume_limits,
            limit_wait,
            autocompact_offer,
            accounts,
            limit_reset,
            limit_reset_kinds,
            usage_tap,
            usage_notes,
            usage_credits,
            logins,
            hosts,
            model_fallback,
            compact_on_context_wall,
            relaunch,
            stall_term_after_s,
            stall_term,
            memory_ceiling,
            upgrade,
            upgrade_ask,
            human_grace_s,
            occupancy,
            facts,
            stuck_break_note,
            withheld: _,
        } = self;
        let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
        [
            ("enabled", enabled.to_string()),
            ("headless", headless.to_string()),
            ("autopilot", autopilot.word().to_string()),
            ("land_at", land_at.map(LandAt::text).unwrap_or_default()),
            ("approve", approve.word().to_string()),
            ("trust_roots", trust_roots.join(",")),
            ("rm_breaker_ask", rm_breaker_ask.to_string()),
            ("answer_questions", answer_questions.to_string()),
            ("question_priority", question_priority.word().to_string()),
            ("answer_text", answer_text.clone()),
            ("dismiss_surveys", dismiss_surveys.to_string()),
            ("continue", continue_policy.to_string()),
            ("continue_text", continue_text.clone()),
            ("continue_per_hour", continue_per_hour.to_string()),
            ("rules_file", path(rules_file).unwrap_or_default()),
            ("retry_api_errors", retry_api_errors.to_string()),
            ("probe_api", probe_api.to_string()),
            ("resume_limits", resume_limits.to_string()),
            ("limit_wait", limit_wait.to_string()),
            ("accounts", accounts.as_str().to_string()),
            ("limit_reset", limit_reset.to_string()),
            (
                "limit_reset_kinds",
                reset_kinds_text(limit_reset_kinds, ","),
            ),
            ("usage_tap", usage_tap.to_string()),
            ("usage_notes", usage_notes.to_string()),
            ("usage_credits", usage_credits.to_string()),
            ("logins", if *logins { "renew" } else { "off" }.to_string()),
            ("hosts", hosts.join(",")),
            ("model_fallback", model_fallback.clone().unwrap_or_default()),
            (
                "compact_on_context_wall",
                compact_on_context_wall.to_string(),
            ),
            (
                "autocompact_offer",
                autocompact_offer.map_or_else(|| "off".to_string(), |k| format!("{k}k")),
            ),
            ("relaunch", relaunch.to_string()),
            ("stall_term_after_s", stall_term_after_s.to_string()),
            ("stall_term", stall_term.to_string()),
            ("memory_ceiling", memory_ceiling.text()),
            ("upgrade", upgrade.to_string()),
            ("upgrade_ask", upgrade_ask.to_string()),
            ("human_grace_s", human_grace_s.to_string()),
            ("occupancy", occupancy.word().to_string()),
            ("facts", facts.to_string()),
            ("stuck_break_note", stuck_break_note.to_string()),
        ]
    }

    /// Each agent's own switch as its `[harness.<agent>]` table spells it, `(<agent>.enabled, true|false)`, for every supervisable program in [`Program::ALL`]'s order.
    #[must_use]
    pub fn agent_texts(&self) -> Vec<(String, String)> {
        Program::ALL
            .into_iter()
            .filter(|program| program.harness_table())
            .map(|program| {
                (
                    format!("{}.enabled", program.name()),
                    (!self.off.contains(&program)).to_string(),
                )
            })
            .collect()
    }

    /// Each safe rule a retired key took away ([`Withheld`]), as `(<that key>, false)`; empty where none did.
    #[must_use]
    pub fn withheld_texts(&self) -> Vec<(&'static str, String)> {
        let mut withheld = self.withheld;
        withheld
            .rules_mut()
            .into_iter()
            .filter(|(_, withheld)| **withheld)
            .map(|(key, _)| (key, false.to_string()))
            .collect()
    }

    /// THE POLICY AS TEXT, WHOLE: [`Self::texts`], [`Self::agent_texts`] and [`Self::withheld_texts`].
    #[must_use]
    pub fn policy_texts(&self) -> PolicyTexts {
        PolicyTexts {
            keys: self
                .texts()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            agents: self.agent_texts(),
            withheld: self
                .withheld_texts()
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    /// The policy [`Self::policy_texts`] wrote.
    pub fn rebuild(texts: &PolicyTexts) -> Result<Rebuilt, String> {
        Self::rebuild_reading(texts, false)
    }

    /// [`Self::rebuild`] for texts of a policy NO FILE CAN SPELL.
    pub fn rebuild_nearest(texts: &PolicyTexts) -> Result<Rebuilt, String> {
        Self::rebuild_reading(texts, true)
    }

    fn rebuild_reading(texts: &PolicyTexts, nearest: bool) -> Result<Rebuilt, String> {
        let PolicyTexts {
            keys,
            agents,
            withheld,
        } = texts;
        let mut refused = Vec::new();
        let mut cfg = Self::default();
        let wanted: Vec<String> = cfg.texts().iter().map(|(k, _)| k.to_string()).collect();
        let agent_keys: Vec<String> = cfg.agent_texts().into_iter().map(|(k, _)| k).collect();
        let mut supplied = Vec::new();
        let mut retired_keys = Vec::new();
        for (list, names, what) in [(keys, &wanted, "key"), (agents, &agent_keys, "agent")] {
            for name in names {
                match list.iter().filter(|(k, _)| k == name).count() {
                    0 => supplied.push(name.clone()),
                    1 => {}
                    _ => return Err(format!("harness.{name}: said more than once")),
                }
            }
            for (key, value) in list {
                if names.contains(key) {
                    match cfg.set(key, value) {
                        Ok(()) => {}
                        Err(note) if nearest => {
                            let read = cfg
                                .texts()
                                .into_iter()
                                .map(|(k, v)| (k.to_string(), v))
                                .chain(cfg.agent_texts())
                                .find(|(k, _)| k == key)
                                .map_or_else(String::new, |(_, v)| v);
                            refused.push(format!("{note}; this build read {key}={read}"));
                        }
                        Err(e) => return Err(format!("{e} (as the policy wrote it)")),
                    }
                } else if what == "key" && RETIRED_KEYS.contains(&key.as_str()) {
                    retired_keys.push((key, value));
                } else {
                    return Err(format!(
                        "harness.{key}: no {what} this build has — a policy a newer build wrote"
                    ));
                }
            }
        }
        let mut retired = Vec::new();
        for (key, value) in retired_keys {
            retired.push(match cfg.set(key, value) {
                Ok(()) => format!("harness.{key}: a retired key"),
                Err(note) => note,
            });
        }
        for (key, value) in withheld {
            if value != "false" {
                return Err(format!(
                    "harness.{key}: a withheld rule is written false, not {value:?}"
                ));
            }
            let mut rules = cfg.withheld.rules_mut();
            match rules.iter_mut().find(|(k, _)| *k == key.as_str()) {
                Some((_, on)) => **on = true,
                None => return Err(format!("harness.{key}: no withheld rule this build has")),
            }
        }
        Ok(Rebuilt {
            policy: cfg,
            supplied,
            retired,
            refused,
        })
    }

    /// `key`'s full-power default as Settings and the help show it (`true`, `all`, `[]`, `0 (no cap)`), or `None` for a key with no default value (`rules_file`) or no such key.
    #[must_use]
    pub fn default_shown(key: &str) -> Option<String> {
        if let Some((_, "enabled")) = agent_key(key) {
            return Some(true.to_string());
        }
        let full = Self::default();
        let (_, text) = full.texts().into_iter().find(|(k, _)| *k == key)?;
        match key {
            "trust_roots" => Some(format!("[{}]", full.trust_roots.join(", "))),
            "hosts" => Some(format!("[{}]", full.hosts.join(", "))),
            "limit_reset_kinds" => Some(format!(
                "[{}]",
                reset_kinds_text(&full.limit_reset_kinds, ", ")
            )),
            "land_at" if text.is_empty() => Some("none".to_string()),
            _ if text.is_empty() => None,
            "continue_per_hour" if full.continue_per_hour == 0 => Some(format!("{text} (no cap)")),
            "stall_term_after_s" => Some(format!("{text} (0: never)")),
            _ => Some(text),
        }
    }

    /// What this policy writes against the full-power default, in [`KEYS`] order.
    #[must_use]
    pub fn against_default(&self) -> Written {
        let full = Self::default();
        let mut written = Written::default();
        for ((key, value), (_, default)) in self.texts().into_iter().zip(full.texts()) {
            if value == default || key == "trust_roots" && self.approve != Approve::Safe {
                continue;
            }
            let mut at_default = self.clone();
            if at_default.set(key, &default).is_err() || !self.limits(&at_default) {
                written.own.push(key);
                continue;
            }
            let shown = match value.as_str() {
                "false" | "" => "off".to_string(),
                _ if key == "trust_roots" => self.trust_roots.join(", "),
                _ if key == "hosts" => self.hosts.join(", "),
                _ if key == "limit_reset_kinds" => reset_kinds_text(&self.limit_reset_kinds, ", "),
                _ => value,
            };
            written.limits.push(format!("{key}: {shown}"));
        }
        for (key, _) in self.withheld_texts() {
            written.limits.push(format!("{key}: off"));
        }
        for agent in &self.off {
            written.limits.push(format!("{}: off", agent.name()));
        }
        written
    }
}

/// [`SupervisorConfig::against_default`]: what a policy writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Written {
    /// Each key that takes power away, as `<key>: <value>` (`approve: safe`, `continue: off`, `continue_per_hour: 6`).
    pub limits: Vec<String>,
    /// Each key changed without taking power away: the owner's own text or choice (`answer_text`, `question_priority`, `rules_file`).
    pub own: Vec<&'static str>,
}

fn unknown_key(key: &str) -> String {
    format!(
        "harness.{key}: {UNKNOWN_KEY}, ignored (known: {})",
        KEYS.join(", ")
    )
}

fn per_agent_only(key: &str) -> String {
    format!(
        "harness.{key}: {UNKNOWN_KEY}, ignored — only `enabled` is per agent; write it under \
         `[harness]`"
    )
}

fn agent_named(id: &str) -> Option<Program> {
    aterm_phase::program_of(id).filter(|p| p.name() == id && p.harness_table())
}

/// The keys an agent's `[harness.<agent>]` table holds beyond `enabled`, which their own reader
/// reads: agy's model rotation ([`crate::harness::agy_quota::Rotation::from_aterm_toml`]).
fn own_keys(program: Program) -> &'static [&'static str] {
    match program {
        Program::Agy => crate::harness::agy_quota::ROTATION_KEYS,
        Program::Claude | Program::Codex | Program::Gemini | Program::Muse | Program::Generic => {
            &[]
        }
    }
}

fn agent_key(key: &str) -> Option<(Program, &str)> {
    let (id, leaf) = key.split_once('.')?;
    Some((agent_named(id)?, leaf))
}

const UNKNOWN_KEY: &str = "not a harness key";

const RETIRED_KEY: &str = "a retired harness key";

const RETIRED_IN: &str = "0.93.0";

fn retired_in(key: &str) -> &'static str {
    match key {
        "rate_nudge" => "0.105.0",
        _ => RETIRED_IN,
    }
}

/// Whether the retired `[harness] <key>` written `value` still LIMITS the supervisor ([`SupervisorConfig::retired`]).
#[must_use]
pub fn retired_still_limits(key: &str, value: Option<bool>) -> bool {
    RETIRED_KEYS.contains(&key) && key != "rate_nudge" && value == Some(false)
}

/// Whether `note` (one of [`SupervisorConfig::from_aterm_toml`]'s) is a retired key's ([`RETIRED_KEYS`]), which the window's config language says itself, in the same words ([`retired_key_note`]).
#[must_use]
pub fn is_retired_key_note(note: &str) -> bool {
    note.contains(RETIRED_KEY)
}

/// What the reader says of the retired `[harness] <key>` written `value` (`None`: a value that is no switch, which changes nothing).
#[must_use]
pub fn retired_key_note(key: &str, value: Option<bool>) -> Option<String> {
    RETIRED_KEYS.contains(&key).then(|| match value {
        Some(on) => SupervisorConfig::default().retired(key, !on),
        None => retired_no_setting(key),
    })
}

/// [`retired_key_note`] from the value's one-value text (`None` for a table, array, or float), read by [`retired_reading`].
#[must_use]
pub fn retired_key_note_text(key: &str, text: Option<&str>) -> Option<String> {
    retired_key_note(key, text.and_then(retired_reading))
}

/// A retired key's value as an on or an off.
#[must_use]
pub fn retired_reading(value: &str) -> Option<bool> {
    if spells_off(value) {
        Some(false)
    } else {
        switch_word(value)
    }
}

fn retired_no_setting(key: &str) -> String {
    format!(
        "harness.{key}: {RETIRED_KEY} ({}), and its value is no on or off — no setting, so it \
         changes nothing; remove it",
        retired_in(key)
    )
}

fn no_shape(key: &str, shape: &str) -> String {
    format!(
        "harness.{key}: a TOML {shape} is not a value it takes — no setting, so it changes nothing"
    )
}

fn one_value_text(v: &aterm_toml::Value) -> Option<String> {
    use aterm_toml::Value;
    match v {
        Value::Boolean(_) | Value::Integer(_) | Value::String(_) => scalar_text(v),
        _ => None,
    }
}

fn harness_scalars<'a>(
    v: &'a aterm_toml::Value,
    at: String,
    out: &mut Vec<(String, &'a aterm_toml::Value)>,
) {
    let join = |seg: &str| {
        if at.is_empty() || seg.starts_with('[') {
            format!("{at}{seg}")
        } else {
            format!("{at}.{seg}")
        }
    };
    match v {
        aterm_toml::Value::Table(t) => {
            for (k, v) in t {
                if k == "harness" && !at.is_empty() && v.as_table().is_none() {
                    out.push((at.clone(), v));
                } else if k != "harness" || !at.is_empty() {
                    harness_scalars(v, join(k), out);
                }
            }
        }
        aterm_toml::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                harness_scalars(v, join(&format!("[{i}]")), out);
            }
        }
        _ => {}
    }
}

/// A value's text as an explicit on or off.
#[must_use]
pub fn switch_word(value: &str) -> Option<bool> {
    let v = value.trim().to_ascii_lowercase();
    match v.as_str() {
        "true" | "yes" | "on" => return Some(true),
        "false" | "no" | "off" => return Some(false),
        _ => {}
    }
    let digits = v.replace('_', "");
    let digits = digits.strip_prefix(['+', '-']).unwrap_or(&digits);
    let (radix, body) = [("0x", 16), ("0o", 8), ("0b", 2)]
        .into_iter()
        .find_map(|(p, r)| digits.strip_prefix(p).map(|b| (r, b)))
        .unwrap_or((10, digits));
    if body.is_empty() || !body.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    match u64::from_str_radix(body, radix).ok()? {
        0 => Some(false),
        1 if !v.starts_with('-') => Some(true),
        _ => None,
    }
}

/// `[harness] autocompact_offer`'s default size, in kilo-tokens.
pub const AUTOCOMPACT_DEFAULT_K: u32 = 400;

fn autocompact_size(value: &str) -> Option<Option<u32>> {
    let v = value.trim().to_ascii_lowercase();
    if spells_off(&v) || v == "off" {
        return Some(None);
    }
    if switch_word(&v) == Some(true) {
        return Some(Some(AUTOCOMPACT_DEFAULT_K));
    }
    let n: u32 = v.strip_suffix('k').unwrap_or(&v).parse().ok()?;
    let k = if v.ends_with('k') || n < 1000 {
        n
    } else if n.is_multiple_of(1000) {
        n / 1000
    } else {
        return None;
    };
    aterm_phase::autocompact::LADDER
        .contains(&k)
        .then_some(Some(k))
}

/// Whether a value's text says off for any key: a switch's off ([`switch_word`]), or `none` / `never` / `disabled`.
#[must_use]
pub fn spells_off(value: &str) -> bool {
    switch_word(value) == Some(false)
        || matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "none" | "never" | "disabled"
        )
}

/// `[harness] limit_reset_kinds`' word for a kind of reset.
#[must_use]
pub const fn reset_kind_word(kind: ResetKind) -> &'static str {
    match kind {
        ResetKind::Session => "5-hour",
        ResetKind::Weekly => "weekly",
        ResetKind::Fable => "fable",
        ResetKind::Full => "full",
    }
}

fn reset_kinds_text(kinds: &[ResetKind], sep: &str) -> String {
    kinds
        .iter()
        .map(|kind| reset_kind_word(*kind))
        .collect::<Vec<_>>()
        .join(sep)
}

fn reset_kinds(text: &str) -> Result<Vec<ResetKind>, String> {
    let mut kinds = Vec::new();
    for word in text.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        let kind = ResetKind::ALL
            .into_iter()
            .find(|kind| reset_kind_word(*kind).eq_ignore_ascii_case(word))
            .ok_or_else(|| format!("expected 5-hour, weekly or full, got {word:?}"))?;
        if !kind.takeable() {
            return Err(format!("{word:?}: a Fable reset is never taken"));
        }
        if kinds.contains(&kind) {
            return Err(format!("{word:?} is listed twice"));
        }
        kinds.push(kind);
    }
    Ok(kinds)
}

/// Whether `note` (one of [`SupervisorConfig::from_aterm_toml`]'s) says a key is not a `[harness]` key, which a reader that already names every unknown key of the file (the window's config language) leaves out.
#[must_use]
pub fn is_unknown_key_note(note: &str) -> bool {
    note.contains(UNKNOWN_KEY)
}

fn misplaced_note(at: &str, key: &str, limits: bool, refused: Option<String>) -> String {
    let effect = if limits {
        "it limits all the same; delete that line and set the key under [harness]"
    } else {
        "a key there may only limit, so it changed nothing"
    };
    let why = refused.map_or_else(String::new, |e| format!(" ({e})"));
    format!(
        "harness.{key}: written below another table's header, which TOML files as \
         `{at}.harness.{key}` — {effect}{why}"
    )
}

fn entries(table: &aterm_toml::Table) -> Vec<(String, &aterm_toml::Value)> {
    table
        .iter()
        .flat_map(|(key, value)| match value.as_table() {
            Some(agent) if agent_named(key).is_some() => agent
                .iter()
                .map(|(leaf, v)| (format!("{key}.{leaf}"), v))
                .collect(),
            _ => vec![(key.clone(), value)],
        })
        .collect()
}

fn harness_tables<'a>(
    v: &'a aterm_toml::Value,
    at: String,
    out: &mut Vec<(String, &'a aterm_toml::Table)>,
) {
    let join = |seg: &str| {
        if at.is_empty() || seg.starts_with('[') {
            format!("{at}{seg}")
        } else {
            format!("{at}.{seg}")
        }
    };
    match v {
        aterm_toml::Value::Table(t) => {
            for (k, v) in t {
                if k == "harness" && !at.is_empty() {
                    if let Some(h) = v.as_table() {
                        out.push((at.clone(), h));
                    }
                } else {
                    harness_tables(v, join(k), out);
                }
            }
        }
        aterm_toml::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                harness_tables(v, join(&format!("[{i}]")), out);
            }
        }
        _ => {}
    }
}

fn scalar_text(v: &aterm_toml::Value) -> Option<String> {
    use aterm_toml::Value;
    match v {
        Value::Boolean(b) => Some(b.to_string()),
        Value::Integer(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items
            .iter()
            .map(|i| i.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .map(|v| v.join(",")),
        _ => None,
    }
}

/// Every one-line `key = value` of a file the TOML parser refuses, as its whole key path and its value's text.
pub(crate) fn assignments(text: &str) -> Vec<(Vec<&str>, &str)> {
    fn flatten<'a>(path: Vec<&'a str>, value: &'a str, out: &mut Vec<(Vec<&'a str>, &'a str)>) {
        let value = value.trim();
        let Some(inner) = value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) else {
            out.push((path, value));
            return;
        };
        for pair in top_level(inner, ',') {
            if let [k, v] = top_level(pair, '=')[..] {
                let mut deeper = path.clone();
                deeper.extend(key_path(k));
                flatten(deeper, v, out);
            }
        }
    }
    let mut out = Vec::new();
    let mut header: Vec<&str> = Vec::new();
    let mut in_string: Option<&str> = None;
    let mut array_depth = 0i32;
    for raw in text.strip_prefix('\u{feff}').unwrap_or(text).lines() {
        if let Some(q) = in_string {
            if raw.contains(q) {
                in_string = None;
            }
            continue;
        }
        if array_depth > 0 {
            array_depth += open_brackets(raw);
            continue;
        }
        let line = top_level(raw, '#')[0].trim();
        if let Some(h) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            header = match h.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
                Some(array) => {
                    let mut path = key_path(array);
                    path.push("[]");
                    path
                }
                None => key_path(h),
            };
            continue;
        }
        if let [lhs, rhs] = top_level(line, '=')[..] {
            let value = rhs.trim();
            if let Some(q) = ["\"\"\"", "'''"].into_iter().find(|q| value.starts_with(q)) {
                if !value[3..].contains(q) {
                    in_string = Some(q);
                }
                continue;
            }
            let depth = open_brackets(value);
            if value.starts_with('[') && depth > 0 {
                array_depth = depth;
                continue;
            }
            let mut path = header.clone();
            path.extend(key_path(lhs));
            flatten(path, rhs, &mut out);
        }
    }
    out
}

fn open_brackets(line: &str) -> i32 {
    let (mut depth, mut quote, mut escaped) = (0i32, None, false);
    for c in top_level(line, '#')[0].chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' && q == '"' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
    }
    depth
}

fn unquote(value: &str) -> String {
    let one = |v: &str| {
        let v = v.trim();
        ['"', '\'']
            .into_iter()
            .find_map(|q| v.strip_prefix(q)?.strip_suffix(q))
            .unwrap_or(v)
            .to_string()
    };
    match value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        Some(items) => top_level(items, ',')
            .into_iter()
            .filter(|i| !i.trim().is_empty())
            .map(one)
            .collect::<Vec<_>>()
            .join(","),
        None => one(value),
    }
}

fn key_path(key: &str) -> Vec<&str> {
    top_level(key, '.')
        .into_iter()
        .map(|seg| {
            let seg = seg.trim();
            ['"', '\'']
                .into_iter()
                .find_map(|q| seg.strip_prefix(q)?.strip_suffix(q))
                .unwrap_or(seg)
        })
        .collect()
}

fn top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut depth, mut quote, mut escaped) = (0, 0i32, None, false);
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' && q == '"' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            _ if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> SupervisorConfig {
        SupervisorConfig::from_aterm_toml(text).0
    }

    #[test]
    fn the_autocompact_offer_key_reads_a_size_or_off() {
        let full = SupervisorConfig::default();
        assert_eq!(full.autocompact_offer, Some(400));
        for (text, want) in [
            ("\"300k\"", Some(300)),
            ("\"900K\"", Some(900)),
            ("500", Some(500)),
            ("600000", Some(600)),
            ("true", Some(400)),
            ("false", None),
            ("\"off\"", None),
            ("\"never\"", None),
        ] {
            let c = read(&format!("[harness]\nautocompact_offer = {text}\n"));
            assert_eq!(c.autocompact_offer, want, "{text}");
        }
        for text in ["\"1000k\"", "\"250k\"", "\"lots\"", "450500"] {
            let mut c = full.clone();
            assert!(
                c.set("autocompact_offer", text.trim_matches('"')).is_err(),
                "{text}"
            );
            assert_eq!(c.autocompact_offer, Some(400), "{text}");
        }
        let mut c = full.clone();
        c.limit("autocompact_offer");
        assert_eq!(c.autocompact_offer, None);
        assert!(c.limits(&full), "off is less power");
        assert_eq!(
            c.texts()
                .into_iter()
                .find(|(k, _)| *k == "autocompact_offer")
                .map(|(_, v)| v),
            Some("off".to_string())
        );
        let mut other = full.clone();
        other.set("autocompact_offer", "300k").expect("a size");
        assert!(
            !other.limits(&full),
            "another size is a choice, not a limit"
        );
    }

    #[test]
    fn the_default_is_every_power_and_every_key_limits_it() {
        let full = SupervisorConfig::default();
        assert!(full.enabled && full.headless && full.approve == Approve::All);
        assert!(full.answer_questions && full.continue_policy && full.relaunch && full.upgrade);
        assert!(full.limit_wait, "the usage-limit wait is chosen by default");
        assert!(!read("[harness]\nlimit_wait = false\n").limit_wait);
        assert!(!read("[harness]\nlimit_wait = \"never\"\n").limit_wait);
        assert_eq!(full.continue_per_hour, 0, "no cap");
        assert_eq!(read(""), full, "no file, no table: every power");
        assert!(!full.upgrade_ask && !full.stall_term && !full.rm_breaker_ask);
        let strict =
            read("[harness]\nupgrade_ask = true\nstall_term = true\nrm_breaker_ask = true\n");
        assert!(strict.upgrade_ask && strict.stall_term && strict.rm_breaker_ask);
        assert!(!full.limit_reset, "a limit reset is an opt-in");
        for key in KEYS {
            let mut c = full.clone();
            c.limit(key);
            let text_key = matches!(
                *key,
                "trust_roots"
                    | "answer_text"
                    | "question_priority"
                    | "land_at"
                    | "continue_text"
                    | "rules_file"
                    | "stall_term"
                    | "limit_reset"
                    | "limit_reset_kinds"
                    | "usage_notes"
                    | "usage_credits"
                    | "stuck_break_note"
                    | "hosts"
            );
            assert_eq!(c == full, text_key, "{key}: only a text has no limit");
            assert!(
                c == full || c.limits(&full),
                "{key}: a limit never adds power"
            );
        }
        let mut c = full.clone();
        c.limit("enabled");
        assert!(!full.limits(&c));
    }

    #[test]
    fn a_value_it_cannot_read_keeps_its_default_and_only_an_off_limits() {
        let full = SupervisorConfig::default();
        let (c, notes) = SupervisorConfig::from_aterm_toml(
            "[harness]\ncontinue = \"no\"\napprove = \"everything\"\ncontinu = false\n",
        );
        assert!(!c.continue_policy, "an explicit `no` is an off");
        assert_eq!(
            c.approve,
            Approve::All,
            "an unreadable approve keeps its default"
        );
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("approve:")), "{notes:?}");
        assert_eq!(notes.iter().filter(|n| is_unknown_key_note(n)).count(), 1);
        assert_eq!(
            SupervisorConfig {
                continue_policy: true,
                ..c
            },
            full,
            "nothing else moved"
        );
        for text in [
            "[harness]\napprove = \"maybe\"\n",
            "[harness]\nenabled = \"sometimes\"\n",
            "[harness]\nenabled = 2.5\n",
            "[harness]\nupgrade = [false]\n",
            "[harness]\nrelaunch = { on = false }\n",
            "[harness]\ncontinue_per_hour = 2.5\n",
            "[harness]\ncontinue_per_hour = \"off\"\n",
            "[harness]\nhuman_grace_s = \"off\"\n",
            "[harness]\ncontinue_text = false\n",
            "[harness]\nanswer_text = 1\n",
            "[harness]\nanswer_text = \"\"\n",
            "[harness]\nmodel_fallback = true\n",
            "[harness]\nmodel_fallback = \"yes\"\n",
            "[harness]\nrules_file = 1\n",
            "[harness]\nenabled = [\"no\"]\n",
            "[harness]\napprove = [\"none\"]\n",
            "[harness]\nrm_breakr = false\nsome_future_key = true\n",
            "harness = 5\n",
            "harness = [\"off\"]\n",
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(text);
            assert_eq!(c, full, "{text:?} took power away: {notes:?}");
            assert!(!notes.is_empty(), "{text:?} is said");
        }
        for (text, want) in [
            ("[harness]\napprove = \"ALL\"\n", Approve::All),
            ("[harness]\napprove = \"Safe\"\n", Approve::Safe),
            ("[harness]\napprove = \"none\"\n", Approve::None),
            ("[harness]\napprove = \"no\"\n", Approve::None),
            ("[harness]\napprove = false\n", Approve::None),
            ("[harness]\napprove = \"disabled\"\n", Approve::None),
        ] {
            assert_eq!(read(text).approve, want, "{text:?}");
        }
        for (text, on) in [
            ("[harness]\nenabled = \"yes\"\n", true),
            ("[harness]\nenabled = \"ON\"\n", true),
            ("[harness]\nenabled = 1\n", true),
            ("[harness]\nenabled = \"off\"\n", false),
            ("[harness]\nenabled = 0\n", false),
            ("[harness]\nenabled = 0x0\n", false),
            ("[harness]\nenabled = \"False\"\n", false),
        ] {
            assert_eq!(read(text).enabled, on, "{text:?}");
        }
        assert_eq!(
            read("[harness]\ncontinue_text = \"yes\"\n").continue_text,
            "yes"
        );
        assert_eq!(read("[harness]\nanswer_text = \"1\"\n").answer_text, "1");
        assert_eq!(
            read("[harness]\nmodel_fallback = false\n").model_fallback,
            None
        );
        assert_eq!(
            read("[harness]\nmodel_fallback = \"none\"\n").model_fallback,
            None
        );
        assert_eq!(read("[harness]\nrules_file = false\n").rules_file, None);
        let (c, notes) = SupervisorConfig::from_aterm_toml("harness = false\n");
        assert!(!c.enabled, "{notes:?}");
        let (c, notes) = SupervisorConfig::from_aterm_toml("[x]\nharness = \"off\"\n");
        assert!(!c.enabled, "{notes:?}");
        assert!(notes[0].contains("`x.harness.enabled`"), "{notes:?}");
        let (c, notes) = SupervisorConfig::from_aterm_toml(
            "[harness]\nupgrade = 0\n[harness.sub]\nupgrade = false\n",
        );
        assert!(!c.upgrade, "`0` is an off");
        assert!(
            notes
                .iter()
                .any(|n| n.contains("sub:") && is_unknown_key_note(n))
        );
    }

    #[test]
    fn every_spelling_toml_reads_is_the_key() {
        for text in [
            "harness = { upgrade = false }\n",
            "harness = {upgrade=false}\n",
            "harness . \"upgrade\" = false\n",
            "'harness'.'upgrade' = false\n",
            "[\"harness\"]\nupgrade = false\n",
            "[harness]\n\"upgrade\" = false\n",
            "[harness]\nupgrade = \"false\"\n",
            "harness.upgrade = false\n",
        ] {
            assert!(!read(text).upgrade, "{text:?}");
        }
        assert!(read("[harness]\nupgrade = \"true\"\n").upgrade);
        for text in [
            "[other]\nupgrade = false\n",
            "[[harness]]\nupgrade = false\n",
            "harness = { upgradex = false }\n",
        ] {
            assert!(read(text).upgrade, "{text:?}");
        }
        let (c, notes) = SupervisorConfig::from_aterm_toml("harness = 5\n");
        assert_eq!(c, SupervisorConfig::default());
        assert!(!notes.is_empty());
    }

    #[test]
    fn a_misplaced_harness_key_limits_but_never_grants() {
        let (c, notes) = SupervisorConfig::from_aterm_toml(
            "[harness]\nrelaunch = false\n[theme]\nname = \"x\"\nharness.enabled = false\n\
             harness.relaunch = true\n",
        );
        assert!(!c.enabled, "the misplaced kill switch still limits");
        assert!(
            !c.relaunch,
            "a misplaced key cannot undo the root table's limit"
        );
        for at in ["`theme.harness.enabled`", "`theme.harness.relaunch`"] {
            assert!(notes.iter().any(|n| n.contains(at)), "{at}: {notes:?}");
        }
        assert!(!read("[a]\nharness.relaunch = false\n[harness]\nrelaunch = true\n").relaunch);
        for (text, at) in [
            (
                "[theme.dark]\nharness = { enabled = false }\n",
                "theme.dark.harness.enabled",
            ),
            (
                "[[keybind]]\nkey = \"a\"\nharness.enabled = false\n",
                "keybind[0].harness.enabled",
            ),
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(text);
            assert!(!c.enabled, "{text:?}");
            assert!(notes[0].contains(&format!("`{at}`")), "{notes:?}");
        }
        let c = read(
            "[theme]\nharness.enabled = true\nharness.trust_roots = [\"/\"]\n\
             harness.continue_text = \"x\"\nharness.continue_per_hour = 0\n",
        );
        assert_eq!(c, SupervisorConfig::default());
        let c = read("[harness]\ncontinue_per_hour = 3\n[x]\nharness.continue_per_hour = 9\n");
        assert_eq!(c.continue_per_hour, 3, "a higher cap is not a limit");
    }

    #[test]
    fn a_file_that_is_not_toml_is_read_inside_its_harness_section() {
        let (c, notes) = SupervisorConfig::from_aterm_toml(
            "[theme\nbroken\n[harness]\napprove = \"safe\"\ntrust_roots = [\"~/a*\", \"/b\"]\n",
        );
        assert_eq!(c.approve, Approve::Safe);
        assert_eq!(c.trust_roots, vec!["~/a*".to_string(), "/b".to_string()]);
        assert!(!notes.is_empty());
        const TYPO: &str = "font_px = \n";
        for tail in [
            "[harness]\nenabled = false\n",
            "harness = { enabled = false }\n",
            "harness . \"enabled\" = false # off\n",
            "[harness]\nenabled = true\nenabled = false\n",
            "[harness]\nenabled = false\nenabled = true\n",
            "[profile]\nharness = { enabled = false }\n",
            "[theme]\nname = \"x\"\nharness.enabled = false\n",
            "[[keybind]]\nharness.enabled = false\n",
        ] {
            let text = format!("{TYPO}{tail}");
            assert!(
                aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
                "the fixture must be one the parser refuses: {text:?}"
            );
            assert!(!read(&text).enabled, "{text:?}");
        }
        for tail in [
            "[harness]\nenabled = true\n",
            "harness = { enabled = true }\n",
            "[profile]\nharness = { enabled = true }\n",
            "[[harness]]\nenabled = false\n",
            "[harness]\nnote = \"enabled = false\"\n",
            "\"harness.enabled\" = false\n",
            "# harness.enabled = false\n",
        ] {
            let text = format!("{TYPO}{tail}");
            assert!(read(&text).enabled, "{text:?}");
        }
        assert!(!read(&format!("\u{feff}[harness]\nenabled = false\n{TYPO}")).enabled);
        assert!(read(&format!("{TYPO}[harness]\nupgrade = true\n")).upgrade);
        assert!(!read(&format!("{TYPO}harness = false\n")).enabled);
        assert!(!read(&format!("{TYPO}[x]\nharness = false\n")).enabled);
        let redefined = "harness = false\n[harness]\nenabled = true\n";
        assert!(aterm_toml::from_str::<aterm_toml::Value>(redefined).is_err());
        assert!(!read(redefined).enabled);
        for text in [
            "[other]\nnote = \"\"\"\n[harness]\nenabled = false\n\"\"\"\n",
            "[other]\nnote = '''\nharness.approve = \"none\"\n'''\n",
            "[other]\nlist = [\n  \"[harness]\",\n]\nenabled = false\n",
        ] {
            let text = format!("{TYPO}{text}");
            let c = read(&text);
            assert!(c.enabled && c.approve == Approve::All, "{text:?}");
        }
        let c = read(&format!(
            "{TYPO}[harness]\ntrust_roots = [\n  \"/x\",\n]\nupgrade = false\n"
        ));
        assert!(!c.upgrade && c.trust_roots == SupervisorConfig::default().trust_roots);
        assert_eq!(
            read(&format!(
                "{TYPO}[harness]\napprove = \"maybe\"\nenabled = \"sometimes\"\n"
            )),
            SupervisorConfig::default()
        );
    }

    #[test]
    fn accounts_reads_its_three_words_and_only_limits() {
        use crate::harness::accounts::Mode;
        assert_eq!(SupervisorConfig::default().accounts, Mode::Early);
        assert_eq!(
            SupervisorConfig::default_shown("accounts").as_deref(),
            Some("early")
        );
        for (text, want) in [
            ("\"early\"", Mode::Early),
            ("\"wall\"", Mode::Wall),
            ("\"off\"", Mode::Off),
            ("\"Wall\"", Mode::Wall),
            ("false", Mode::Off),
            ("\"none\"", Mode::Off),
            ("\"sometimes\"", Mode::Early),
            ("true", Mode::Early),
            ("3", Mode::Early),
        ] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\naccounts = {text}\n"));
            assert_eq!(c.accounts, want, "{text}: {notes:?}");
            let own = matches!(text, "\"early\"" | "\"wall\"" | "\"off\"" | "\"Wall\"");
            assert_eq!(notes.is_empty(), own, "{text}: {notes:?}");
        }
        assert_eq!(
            read("[harness]\naccounts = \"wall\"\n")
                .against_default()
                .limits,
            ["accounts: wall"]
        );
        assert_eq!(
            read("[harness]\naccounts = \"off\"\n")
                .against_default()
                .limits,
            ["accounts: off"]
        );
        assert_eq!(
            read("[theme]\nharness.accounts = \"wall\"\n").accounts,
            Mode::Wall
        );
        assert_eq!(
            read("[harness]\naccounts = \"wall\"\n[theme]\nharness.accounts = \"early\"\n")
                .accounts,
            Mode::Wall,
            "a misplaced key cannot undo the root table's limit"
        );
        assert_eq!(
            read("[harness]\naccounts = \"wall\"\n[theme]\nharness.accounts = \"off\"\n").accounts,
            Mode::Off
        );
    }

    #[test]
    fn autopilot_reads_its_three_words_and_only_limits() {
        let full = SupervisorConfig::default();
        assert_eq!(full.autopilot, Autopilot::Cruise);
        assert_eq!(Autopilot::default(), Autopilot::Cruise);
        assert_eq!(
            SupervisorConfig::default_shown("autopilot").as_deref(),
            Some("cruise")
        );
        let at = KEYS.iter().position(|k| *k == "autopilot").expect("a key");
        assert_eq!(
            (KEYS[at - 1], KEYS[at + 1], KEYS[at + 2]),
            ("headless", "land_at", "approve"),
            "after which sessions and beside the time a timed Cruise is offered to land at, \
             before what a box gets"
        );
        for mode in Autopilot::ALL {
            assert_eq!(Autopilot::parse(mode.word()), Some(mode), "{mode:?}");
        }
        for (word, want) in [
            ("cruise", Some(Autopilot::Cruise)),
            ("  CRUISE ", Some(Autopilot::Cruise)),
            ("forever", Some(Autopilot::Cruise)),
            ("Infinity", Some(Autopilot::Cruise)),
            ("land", Some(Autopilot::Land)),
            ("wrap-up", Some(Autopilot::Land)),
            ("WrapUp", Some(Autopilot::Land)),
            ("wrap", Some(Autopilot::Land)),
            ("off", Some(Autopilot::Off)),
            ("Off", Some(Autopilot::Off)),
            ("on", None),
            ("true", None),
            ("false", None),
            ("none", None),
            ("wrap it up", None),
            ("", None),
        ] {
            assert_eq!(Autopilot::parse(word), want, "{word:?}");
        }
        for (text, want, said) in [
            ("\"cruise\"", Autopilot::Cruise, None),
            ("\"land\"", Autopilot::Land, None),
            ("\"Land\"", Autopilot::Land, None),
            ("\"off\"", Autopilot::Off, None),
            ("\"forever\"", Autopilot::Cruise, None),
            ("\"wrap-up\"", Autopilot::Land, None),
            ("false", Autopilot::Off, Some("its limit")),
            ("\"no\"", Autopilot::Off, Some("its limit")),
            ("0", Autopilot::Off, Some("its limit")),
            ("\"none\"", Autopilot::Off, Some("its limit")),
            ("\"disabled\"", Autopilot::Off, Some("its limit")),
            ("true", Autopilot::Cruise, Some("keeps its default")),
            (
                "\"sometimes\"",
                Autopilot::Cruise,
                Some("keeps its default"),
            ),
            ("3", Autopilot::Cruise, Some("keeps its default")),
            ("2.5", Autopilot::Cruise, Some("no setting")),
            ("[\"off\"]", Autopilot::Cruise, Some("no setting")),
        ] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\nautopilot = {text}\n"));
            assert_eq!(c.autopilot, want, "{text}: {notes:?}");
            assert_eq!(
                SupervisorConfig {
                    autopilot: Autopilot::Cruise,
                    ..c
                },
                full,
                "{text}: nothing else moved"
            );
            match said {
                None => assert!(notes.is_empty(), "{text}: {notes:?}"),
                Some(word) => assert!(
                    notes.len() == 1
                        && notes[0].starts_with("harness.autopilot:")
                        && notes[0].contains(word),
                    "{text}: {notes:?}"
                ),
            }
        }
        let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nautopilot = false\n");
        assert!(
            notes[0].contains("expected cruise, land or off"),
            "{notes:?}"
        );
        let land = read("[harness]\nautopilot = \"land\"\n");
        let off = read("[harness]\nautopilot = \"off\"\n");
        assert!(land.limits(&full) && off.limits(&full) && off.limits(&land));
        assert!(!full.limits(&land) && !land.limits(&off), "never up");
        let mut c = full.clone();
        c.limit("autopilot");
        assert_eq!(c, off);
        assert_eq!(land.against_default().limits, ["autopilot: land"]);
        assert_eq!(off.against_default().limits, ["autopilot: off"]);
        assert!(full.texts().contains(&("autopilot", "cruise".to_string())));
        assert_eq!(
            read("[theme]\nharness.autopilot = \"land\"\n").autopilot,
            Autopilot::Land
        );
        assert_eq!(
            read("[harness]\nautopilot = \"land\"\n[theme]\nharness.autopilot = \"cruise\"\n")
                .autopilot,
            Autopilot::Land,
            "a misplaced key cannot undo the root table's limit"
        );
        assert_eq!(
            read("[harness]\nautopilot = \"land\"\n[theme]\nharness.autopilot = \"off\"\n")
                .autopilot,
            Autopilot::Off
        );
        assert_eq!(
            read("font_px = \n[harness]\nautopilot = \"wrap-up\"\n").autopilot,
            Autopilot::Land
        );
        for table in Autopilot::ALL {
            let cfg = SupervisorConfig {
                autopilot: table,
                ..full.clone()
            };
            assert_eq!(cfg.autopilot_for(None), table, "{table:?}");
            for word in Autopilot::ALL {
                assert_eq!(cfg.autopilot_for(Some(word)), word, "{table:?} {word:?}");
            }
        }
    }

    #[test]
    fn limit_reset_ships_off_and_only_the_root_table_turns_it_on() {
        let full = SupervisorConfig::default();
        assert!(!full.limit_reset, "the default is off");
        assert!(!read("").limit_reset && !read("[harness]\n").limit_reset);
        assert_eq!(
            SupervisorConfig::default_shown("limit_reset").as_deref(),
            Some("false")
        );
        for key in KEYS.iter().filter(|k| **k != "limit_reset") {
            let mut c = full.clone();
            let _ = c.set(key, "true");
            assert!(!c.limit_reset, "{key} = true");
            c.limit(key);
            assert!(!c.limit_reset, "{key} at its limit");
        }
        for key in RETIRED_KEYS {
            for value in ["true", "false"] {
                let c = read(&format!("[harness]\n{key} = {value}\n"));
                assert!(!c.limit_reset, "{key} = {value}");
            }
        }
        let on = SupervisorConfig {
            limit_reset: true,
            ..full.clone()
        };
        for text in ["true", "\"true\"", "\"yes\"", "\"On\"", "1"] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\nlimit_reset = {text}\n"));
            assert_eq!(c, on, "{text}");
            assert!(notes.is_empty(), "{text}: {notes:?}");
        }
        for (text, said) in [
            ("false", 0),
            ("\"no\"", 0),
            ("0", 0),
            ("\"never\"", 1),
            ("\"none\"", 1),
            ("\"sometimes\"", 1),
            ("3", 1),
            ("2.5", 1),
            ("[true]", 1),
        ] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\nlimit_reset = {text}\n"));
            assert_eq!(c, full, "{text}");
            assert_eq!(notes.len(), said, "{text}: {notes:?}");
        }
        let c = read("[harness]\nlimit_reset = true\nresume_limits = false\n");
        assert!(c.limit_reset && !c.resume_limits);
        let c = read("[harness]\nlimit_reset = true\nenabled = false\n");
        assert!(c.limit_reset && !c.enabled);
        assert!(!on.limits(&full), "on is no limit of off");
        assert!(full.limits(&on), "off limits on");
        let mut c = on.clone();
        c.limit("limit_reset");
        assert_eq!(c, full, "its limit is off");
        assert_eq!(
            on.against_default(),
            Written {
                limits: Vec::new(),
                own: vec!["limit_reset"],
            }
        );
        assert!(on.texts().contains(&("limit_reset", "true".to_string())));
        let mut c = full.clone();
        c.set("limit_reset", "true").expect("a switch");
        assert_eq!(c, on);
        let (c, notes) = SupervisorConfig::from_aterm_toml("[theme]\nharness.limit_reset = true\n");
        assert_eq!(c, full);
        assert!(
            notes[0].contains("`theme.harness.limit_reset`"),
            "{notes:?}"
        );
        assert!(
            !read("[harness]\nlimit_reset = true\n[theme]\nharness.limit_reset = false\n")
                .limit_reset,
            "a misplaced off limits all the same"
        );
        assert!(
            !read("[a]\nharness.limit_reset = true\n[harness]\nlimit_reset = false\n").limit_reset,
            "a table that sorts before `harness` grants nothing either"
        );
        const TYPO: &str = "font_px = \n";
        for (tail, want) in [
            ("[harness]\nlimit_reset = true\n", true),
            ("harness = { limit_reset = true }\n", true),
            (
                "[harness]\nlimit_reset = false\nlimit_reset = true\n",
                false,
            ),
            (
                "[harness]\nlimit_reset = true\nlimit_reset = false\n",
                false,
            ),
            ("[theme]\nharness.limit_reset = true\n", false),
            ("[[keybind]]\nharness.limit_reset = true\n", false),
            ("[harness]\nnote = \"limit_reset = true\"\n", false),
            ("# harness.limit_reset = true\n", false),
        ] {
            let text = format!("{TYPO}{tail}");
            assert!(
                aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
                "the fixture must be one the parser refuses: {text:?}"
            );
            assert_eq!(read(&text).limit_reset, want, "{text:?}");
        }
    }

    #[test]
    fn limit_reset_kinds_is_the_owners_order_and_moves_only_with_the_step_off() {
        use aterm_phase::ResetKind::{Full, Session, Weekly};
        let full = SupervisorConfig::default();
        assert!(
            full.limit_reset_kinds.is_empty(),
            "the default is the lone offer"
        );
        assert_eq!(
            SupervisorConfig::default_shown("limit_reset_kinds").as_deref(),
            Some("[]")
        );
        for (text, want) in [
            (
                "[\"full\", \"weekly\", \"5-hour\"]",
                vec![Full, Weekly, Session],
            ),
            ("[\"Weekly\"]", vec![Weekly]),
            ("\"5-hour, full\"", vec![Session, Full]),
            ("[]", vec![]),
            ("\"\"", vec![]),
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(&format!(
                "[harness]\nlimit_reset = true\nlimit_reset_kinds = {text}\n"
            ));
            assert_eq!(c.limit_reset_kinds, want, "{text}");
            assert!(notes.is_empty(), "{text}: {notes:?}");
            let mut back = full.clone();
            let (_, said) = c
                .texts()
                .into_iter()
                .find(|(k, _)| *k == "limit_reset_kinds")
                .expect("a text");
            back.set("limit_reset_kinds", &said).expect("its own text");
            assert_eq!(
                back.limit_reset_kinds, want,
                "{text} read back from {said:?}"
            );
        }
        for text in [
            "[\"fable\"]",
            "[\"full\", \"Fable\"]",
            "[\"full\", \"full\"]",
            "[\"session\"]",
            "[\"monthly\"]",
            "false",
            "\"none\"",
            "[1]",
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(&format!(
                "[harness]\nlimit_reset_kinds = {text}\n"
            ));
            assert_eq!(c, full, "{text}: no setting");
            assert_eq!(notes.len(), 1, "{text}: {notes:?}");
            assert!(notes[0].contains("limit_reset_kinds"), "{text}: {notes:?}");
        }
        let listed = |on: bool, kinds: Vec<aterm_phase::ResetKind>| SupervisorConfig {
            limit_reset: on,
            limit_reset_kinds: kinds,
            ..full.clone()
        };
        assert!(!listed(true, vec![Full]).limits(&listed(true, vec![])));
        assert!(!listed(true, vec![]).limits(&listed(true, vec![Full])));
        assert!(!listed(true, vec![Weekly]).limits(&listed(true, vec![Full, Weekly])));
        assert!(
            listed(false, vec![Full]).limits(&listed(true, vec![])),
            "off limits"
        );
        assert!(listed(false, vec![Full]).limits(&listed(false, vec![])));
        let mut c = listed(true, vec![Full]);
        c.limit("limit_reset_kinds");
        assert_eq!(c, listed(true, vec![Full]), "no least of its own");
        assert_eq!(
            listed(true, vec![Full, Weekly]).against_default(),
            Written {
                limits: Vec::new(),
                own: vec!["limit_reset", "limit_reset_kinds"],
            }
        );
        let misplaced = "[theme]\nharness.limit_reset_kinds = [\"full\"]\n";
        let on = read(&format!("[harness]\nlimit_reset = true\n{misplaced}"));
        assert_eq!(on, listed(true, vec![]), "refused with the step on");
        let off = read(misplaced);
        assert_eq!(off, listed(false, vec![Full]), "admitted with the step off");
    }

    #[test]
    fn usage_notes_ships_off_and_only_the_root_table_turns_it_on() {
        let full = SupervisorConfig::default();
        assert!(!full.usage_notes, "the default is off");
        assert_eq!(
            SupervisorConfig::default_shown("usage_notes").as_deref(),
            Some("false")
        );
        let on = SupervisorConfig {
            usage_notes: true,
            ..full.clone()
        };
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\nusage_notes = true\n");
        assert_eq!(c, on);
        assert!(notes.is_empty(), "{notes:?}");
        for off in ["false", "\"no\"", "0"] {
            assert_eq!(
                read(&format!("[harness]\nusage_notes = {off}\n")),
                full,
                "{off}"
            );
        }
        assert!(full.limits(&on) && !on.limits(&full));
        let mut c = on.clone();
        c.limit("usage_notes");
        assert_eq!(c, full, "its limit is off");
        assert_eq!(
            on.against_default(),
            Written {
                limits: Vec::new(),
                own: vec!["usage_notes"],
            }
        );
        let (c, _) = SupervisorConfig::from_aterm_toml("[theme]\nharness.usage_notes = true\n");
        assert_eq!(c, full, "misplaced, it grants nothing");
        assert!(
            !read("[harness]\nusage_notes = true\n[theme]\nharness.usage_notes = false\n")
                .usage_notes,
            "a misplaced off limits all the same"
        );
        for key in KEYS.iter().filter(|k| **k != "usage_notes") {
            let mut c = full.clone();
            let _ = c.set(key, "true");
            assert!(!c.usage_notes, "{key} = true");
        }
    }

    /// THE USAGE TAP SHIPS ON: no file, an empty table and every other key leave it on,
    /// `false` in the root table is a limit like any other off, and a misplaced off limits
    /// all the same.
    #[test]
    fn usage_tap_ships_on_and_the_root_tables_off_limits_it() {
        let full = SupervisorConfig::default();
        assert!(full.usage_tap, "the default is on");
        assert!(read("").usage_tap && read("[harness]\n").usage_tap);
        assert_eq!(
            SupervisorConfig::default_shown("usage_tap").as_deref(),
            Some("true")
        );
        for key in KEYS.iter().filter(|k| **k != "usage_tap") {
            let mut c = full.clone();
            let _ = c.set(key, "true");
            assert!(c.usage_tap, "{key} = true");
            c.limit(key);
            assert!(c.usage_tap, "{key} at its limit");
        }
        let off = SupervisorConfig {
            usage_tap: false,
            ..full.clone()
        };
        for text in ["false", "\"no\"", "0"] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\nusage_tap = {text}\n"));
            assert_eq!(c, off, "{text}");
            assert!(notes.is_empty(), "{text}: {notes:?}");
        }
        for text in ["true", "\"yes\"", "1"] {
            assert_eq!(read(&format!("[harness]\nusage_tap = {text}\n")), full);
        }
        let c = read("[harness]\naccounts = \"off\"\n");
        assert!(c.usage_tap, "the accounts' switch does not silence the tap");
        assert!(full.usage_tap_on() && !off.usage_tap_on());
        for text in [
            "[harness]\nenabled = false\n",
            "[harness.claude]\nenabled = false\n",
        ] {
            let c = read(text);
            assert!(
                c.usage_tap && !c.usage_tap_on(),
                "{text}: no tap where no harness"
            );
        }
        assert!(
            read("[harness.codex]\nenabled = false\n").usage_tap_on(),
            "another agent's off"
        );
        assert!(off.limits(&full), "off limits on");
        assert!(!full.limits(&off), "on is no limit of off");
        let mut c = full.clone();
        c.limit("usage_tap");
        assert_eq!(c, off, "its limit is off");
        assert_eq!(
            off.against_default(),
            Written {
                limits: vec!["usage_tap: off".to_owned()],
                own: Vec::new(),
            }
        );
        let (c, notes) = SupervisorConfig::from_aterm_toml("[theme]\nharness.usage_tap = true\n");
        assert_eq!(c, full, "a misplaced true grants nothing");
        assert!(notes[0].contains("`theme.harness.usage_tap`"), "{notes:?}");
        assert!(
            !read("[theme]\nharness.usage_tap = false\n").usage_tap,
            "a misplaced off limits all the same"
        );
    }

    #[test]
    fn usage_credits_ships_off_and_only_the_root_table_turns_it_on() {
        let full = SupervisorConfig::default();
        assert!(!full.usage_credits, "the default is off");
        assert!(!read("").usage_credits && !read("[harness]\n").usage_credits);
        assert_eq!(
            SupervisorConfig::default_shown("usage_credits").as_deref(),
            Some("false")
        );
        for key in KEYS.iter().filter(|k| **k != "usage_credits") {
            let mut c = full.clone();
            let _ = c.set(key, "true");
            assert!(!c.usage_credits, "{key} = true");
            c.limit(key);
            assert!(!c.usage_credits, "{key} at its limit");
        }
        for key in RETIRED_KEYS {
            for value in ["true", "false"] {
                let c = read(&format!("[harness]\n{key} = {value}\n"));
                assert!(!c.usage_credits, "{key} = {value}");
            }
        }
        let on = SupervisorConfig {
            usage_credits: true,
            ..full.clone()
        };
        for text in ["true", "\"true\"", "\"yes\"", "\"On\"", "1"] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\nusage_credits = {text}\n"));
            assert_eq!(c, on, "{text}");
            assert!(notes.is_empty(), "{text}: {notes:?}");
        }
        for (text, said) in [
            ("false", 0),
            ("\"no\"", 0),
            ("0", 0),
            ("\"never\"", 1),
            ("\"sometimes\"", 1),
            ("3", 1),
            ("[true]", 1),
        ] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\nusage_credits = {text}\n"));
            assert_eq!(c, full, "{text}");
            assert_eq!(notes.len(), said, "{text}: {notes:?}");
        }
        assert!(!on.limits(&full), "on is no limit of off");
        assert!(full.limits(&on), "off limits on");
        let mut c = on.clone();
        c.limit("usage_credits");
        assert_eq!(c, full, "its limit is off");
        assert_eq!(
            on.against_default(),
            Written {
                limits: Vec::new(),
                own: vec!["usage_credits"],
            }
        );
        assert!(on.texts().contains(&("usage_credits", "true".to_string())));
        let mut c = full.clone();
        c.set("usage_credits", "true").expect("a switch");
        assert_eq!(c, on);
        let (c, notes) =
            SupervisorConfig::from_aterm_toml("[theme]\nharness.usage_credits = true\n");
        assert_eq!(c, full);
        assert!(
            notes[0].contains("`theme.harness.usage_credits`"),
            "{notes:?}"
        );
        assert!(
            !read("[harness]\nusage_credits = true\n[theme]\nharness.usage_credits = false\n")
                .usage_credits,
            "a misplaced off limits all the same"
        );
        assert!(
            !read("[a]\nharness.usage_credits = true\n[harness]\nusage_credits = false\n")
                .usage_credits,
            "a table that sorts before `harness` grants nothing either"
        );
        const TYPO: &str = "font_px = \n";
        for (tail, want) in [
            ("[harness]\nusage_credits = true\n", true),
            ("harness = { usage_credits = true }\n", true),
            (
                "[harness]\nusage_credits = false\nusage_credits = true\n",
                false,
            ),
            (
                "[harness]\nusage_credits = true\nusage_credits = false\n",
                false,
            ),
            ("[theme]\nharness.usage_credits = true\n", false),
            ("[[keybind]]\nharness.usage_credits = true\n", false),
            ("[harness]\nnote = \"usage_credits = true\"\n", false),
            ("# harness.usage_credits = true\n", false),
        ] {
            let text = format!("{TYPO}{tail}");
            assert!(
                aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
                "the fixture must be one the parser refuses: {text:?}"
            );
            assert_eq!(read(&text).usage_credits, want, "{text:?}");
        }
    }

    #[test]
    fn occupancy_ships_observe_and_only_the_root_table_turns_prompt_on() {
        let full = SupervisorConfig::default();
        assert_eq!(full.occupancy, Occupancy::Observe, "the default is observe");
        assert!(full.occupancy.grant().is_none(), "observe grants nothing");
        assert_eq!(read("").occupancy, Occupancy::Observe);
        assert_eq!(read("[harness]\n").occupancy, Occupancy::Observe);
        assert_eq!(
            SupervisorConfig::default_shown("occupancy").as_deref(),
            Some("observe")
        );
        for (mode, grants, observes) in [
            (Occupancy::Off, false, false),
            (Occupancy::Observe, false, true),
            (Occupancy::Prompt, true, true),
        ] {
            assert_eq!(mode.grant().is_some(), grants, "{mode:?}");
            assert_eq!(mode.observes(), observes, "{mode:?}");
            assert_eq!(Occupancy::parse(mode.word()), Some(mode), "{mode:?}");
        }
        for key in KEYS.iter().filter(|k| **k != "occupancy") {
            for value in ["true", "prompt"] {
                let mut c = full.clone();
                let _ = c.set(key, value);
                assert_eq!(c.occupancy, Occupancy::Observe, "{key} = {value}");
            }
            let mut c = full.clone();
            c.limit(key);
            assert_eq!(c.occupancy, Occupancy::Observe, "{key} at its limit");
        }
        for key in RETIRED_KEYS {
            for value in ["true", "false"] {
                let c = read(&format!("[harness]\n{key} = {value}\n"));
                assert_eq!(c.occupancy, Occupancy::Observe, "{key} = {value}");
            }
        }
        let prompt = SupervisorConfig {
            occupancy: Occupancy::Prompt,
            ..full.clone()
        };
        let off = SupervisorConfig {
            occupancy: Occupancy::Off,
            ..full.clone()
        };
        for text in ["\"prompt\"", "\"Prompt\"", "\" PROMPT \""] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\noccupancy = {text}\n"));
            assert_eq!(c, prompt, "{text}");
            assert!(c.occupancy.grant().is_some(), "{text}");
            assert!(notes.is_empty(), "{text}: {notes:?}");
        }
        for (text, want) in [("\"observe\"", &full), ("\"off\"", &off)] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\noccupancy = {text}\n"));
            assert_eq!(&c, want, "{text}");
            assert!(notes.is_empty(), "{text}: {notes:?}");
        }
        for (text, want) in [
            ("false", &off),
            ("\"no\"", &off),
            ("0", &off),
            ("\"never\"", &off),
            ("\"none\"", &off),
            ("\"disabled\"", &off),
            ("true", &full),
            ("\"true\"", &full),
            ("\"yes\"", &full),
            ("\"on\"", &full),
            ("\"On\"", &full),
            ("1", &full),
            ("\"sometimes\"", &full),
            ("3", &full),
            ("[\"prompt\"]", &full),
        ] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\noccupancy = {text}\n"));
            assert_eq!(&c, want, "{text}");
            assert!(c.occupancy.grant().is_none(), "{text}");
            assert_eq!(notes.len(), 1, "{text}: {notes:?}");
        }
        assert!(!prompt.limits(&full), "prompt is no limit of observe");
        assert!(full.limits(&prompt), "observe limits prompt");
        assert!(off.limits(&full) && off.limits(&prompt), "off limits both");
        assert!(!full.limits(&off), "observe is no limit of off");
        let mut c = prompt.clone();
        c.limit("occupancy");
        assert_eq!(c, off, "its limit is off");
        assert_eq!(
            prompt.against_default(),
            Written {
                limits: Vec::new(),
                own: vec!["occupancy"],
            }
        );
        assert_eq!(
            off.against_default(),
            Written {
                limits: vec!["occupancy: off".to_string()],
                own: Vec::new(),
            }
        );
        assert!(
            prompt
                .texts()
                .contains(&("occupancy", "prompt".to_string()))
        );
        assert!(full.texts().contains(&("occupancy", "observe".to_string())));
        let mut c = full.clone();
        c.set("occupancy", "prompt").expect("a mode");
        assert_eq!(c, prompt);
        let (c, notes) =
            SupervisorConfig::from_aterm_toml("[theme]\nharness.occupancy = \"prompt\"\n");
        assert_eq!(c, full);
        assert!(notes[0].contains("`theme.harness.occupancy`"), "{notes:?}");
        assert_eq!(
            read("[harness]\noccupancy = \"prompt\"\n[theme]\nharness.occupancy = \"off\"\n")
                .occupancy,
            Occupancy::Off,
            "a misplaced off limits all the same"
        );
        assert_eq!(
            read("[a]\nharness.occupancy = \"prompt\"\n[harness]\noccupancy = \"observe\"\n")
                .occupancy,
            Occupancy::Observe,
            "a table that sorts before `harness` grants nothing either"
        );
        const TYPO: &str = "font_px = \n";
        for (tail, want) in [
            ("[harness]\noccupancy = \"prompt\"\n", Occupancy::Prompt),
            ("harness = { occupancy = \"prompt\" }\n", Occupancy::Prompt),
            (
                "[harness]\noccupancy = \"observe\"\noccupancy = \"prompt\"\n",
                Occupancy::Observe,
            ),
            (
                "[harness]\noccupancy = \"prompt\"\noccupancy = \"observe\"\n",
                Occupancy::Observe,
            ),
            ("[harness]\noccupancy = true\n", Occupancy::Observe),
            ("[harness]\noccupancy = false\n", Occupancy::Off),
            (
                "[theme]\nharness.occupancy = \"prompt\"\n",
                Occupancy::Observe,
            ),
            (
                "[[keybind]]\nharness.occupancy = \"prompt\"\n",
                Occupancy::Observe,
            ),
            (
                "[harness]\nnote = \"occupancy = prompt\"\n",
                Occupancy::Observe,
            ),
            ("# harness.occupancy = \"prompt\"\n", Occupancy::Observe),
        ] {
            let text = format!("{TYPO}{tail}");
            assert!(
                aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
                "the fixture must be one the parser refuses: {text:?}"
            );
            assert_eq!(read(&text).occupancy, want, "{text:?}");
        }
    }

    /// THE STUCK FACT'S KEY SHIPS OFF, AND ONLY THE ROOT TABLE GRANTS IT: no
    /// grant by default; the root table's `true` grants it under every
    /// occupancy mode, and `facts = false` (a limit, in any table) takes it
    /// away; a misplaced table's `true`, a repeat that says it again, or any
    /// other key's `true` or limit grants nothing, and the grant it makes is
    /// no occupancy `prompt`.
    #[test]
    fn the_stuck_break_note_ships_off_and_only_the_root_table_grants_it() {
        let full = SupervisorConfig::default();
        assert!(full.facts && !full.stuck_break_note);
        assert!(full.stuck_break_grant().is_none());
        for mode in Occupancy::ALL {
            let on = read(&format!(
                "[harness]\nstuck_break_note = true\noccupancy = \"{}\"\n",
                mode.word()
            ));
            assert!(on.stuck_break_grant().is_some(), "{mode:?}");
            assert_eq!(on.occupancy.grant().is_some(), mode == Occupancy::Prompt);
        }
        for text in [
            "[harness]\nstuck_break_note = true\nfacts = false\n",
            "[harness]\nstuck_break_note = true\n[theme]\nharness.facts = false\n",
            "[theme]\nharness.stuck_break_note = true\n",
            "[[keybind]]\nharness.stuck_break_note = true\n",
            "[a]\nharness.stuck_break_note = true\n[harness]\nfacts = true\n",
            "[harness]\nstuck_break_note = false\nstuck_break_note = true\n",
            "[harness]\nstuck_break_note = \"sometimes\"\n",
        ] {
            assert!(read(text).stuck_break_grant().is_none(), "{text:?}");
        }
        for key in KEYS.iter().filter(|k| **k != "stuck_break_note") {
            let mut c = full.clone();
            let _ = c.set(key, "true");
            assert!(c.stuck_break_grant().is_none(), "{key} = true");
            c.limit(key);
            assert!(c.stuck_break_grant().is_none(), "{key} at its limit");
        }
        let on = read("[harness]\nstuck_break_note = true\n");
        assert!(!on.limits(&full) && full.limits(&on));
        let mut c = on.clone();
        c.limit("facts");
        assert!(c.limits(&on) && c.stuck_break_grant().is_none());
    }

    #[test]
    fn agent_texts_are_each_agents_switch_and_read_back() {
        let full = SupervisorConfig::default();
        let supervisable: Vec<String> = Program::ALL
            .into_iter()
            .filter(|program| program.harness_table())
            .map(|program| format!("{}.enabled", program.name()))
            .collect();
        let keys: Vec<String> = full.agent_texts().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, supervisable);
        assert!(full.agent_texts().iter().all(|(_, text)| text == "true"));
        for key in &supervisable {
            let mut off = full.clone();
            off.set(key, "false").expect(key);
            let texts = off.agent_texts();
            for (k, text) in &texts {
                assert_eq!(text == "false", k == key, "{key}: {texts:?}");
                assert_eq!(
                    SupervisorConfig::default_shown(k).as_deref(),
                    Some("true"),
                    "{k}"
                );
            }
            let mut back = full.clone();
            for (k, text) in &texts {
                back.set(k, text).expect(k);
            }
            assert_eq!(back, off, "{key}");
        }
        assert!(full.withheld_texts().is_empty());
        let (retired, _) = SupervisorConfig::from_aterm_toml("[harness]\nrm_breaker = false\n");
        assert_eq!(
            retired.withheld_texts(),
            [("rm_breaker", "false".to_string())]
        );
    }

    #[test]
    fn texts_are_the_keys_and_read_back_as_the_policy() {
        let full = SupervisorConfig::default();
        let keys: Vec<_> = full.texts().iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, KEYS, "one text per key, in KEYS order");
        for key in KEYS {
            let mut limited = full.clone();
            limited.limit(key);
            for want in [&full, &limited] {
                let (_, text) = want.texts().into_iter().find(|(k, _)| k == key).unwrap();
                let mut c = full.clone();
                c.set(key, &text).expect(key);
                assert_eq!(&c, want, "{key} = {text:?}");
            }
        }
        for key in KEYS {
            assert_eq!(
                SupervisorConfig::default_shown(key).is_none(),
                *key == "rules_file",
                "{key}"
            );
        }
        assert_eq!(SupervisorConfig::default_shown("auto_reads"), None);
    }

    #[test]
    fn rebuild_gives_back_the_policy_its_texts_wrote() {
        let full = SupervisorConfig::default();
        let mut policies = vec![full.clone()];
        for key in KEYS {
            policies.push(SupervisorConfig::moved_off_default(key));
        }
        for (agent, _) in full.agent_texts() {
            let mut c = full.clone();
            c.set(&agent, "false").expect("an agent's own switch");
            assert_ne!(c, full, "{agent}");
            policies.push(c);
        }
        for rule in 0..Withheld::default().rules_mut().len() {
            let mut c = full.clone();
            *c.withheld.rules_mut()[rule].1 = true;
            assert_eq!(c.approve, Approve::All, "withheld alone, approve uncapped");
            policies.push(c);
        }
        for want in &policies {
            let texts = want.policy_texts();
            assert_eq!(
                SupervisorConfig::rebuild(&texts),
                Ok(Rebuilt {
                    policy: want.clone(),
                    supplied: Vec::new(),
                    retired: Vec::new(),
                    refused: Vec::new(),
                }),
                "{texts:?}"
            );
        }
    }

    #[test]
    fn rebuild_supplies_what_an_older_builds_texts_did_not_name() {
        let mut written = SupervisorConfig::default();
        written.set("approve", "safe").expect("approve");
        written.set("land_at", "18:00").expect("land_at");
        written.set("codex.enabled", "false").expect("codex");
        let mut older = written.policy_texts();
        older.keys.retain(|(k, _)| k != "land_at");
        older.agents.retain(|(k, _)| k != "codex.enabled");
        let back = SupervisorConfig::rebuild(&older).expect("an older build's texts");
        assert_eq!(back.supplied, ["land_at", "codex.enabled"]);
        assert!(back.retired.is_empty(), "{back:?}");
        assert_eq!(
            back.policy,
            SupervisorConfig {
                land_at: None,
                off: Vec::new(),
                ..written
            },
            "what the texts did not say is the default; what they said stands"
        );
    }

    #[test]
    fn rebuild_reads_a_key_retired_since_as_the_owners_file_does() {
        let texts = SupervisorConfig::default().policy_texts();
        let mut asked_default = texts.clone();
        asked_default
            .keys
            .insert(0, ("rm_breaker".into(), "true".into()));
        let back = SupervisorConfig::rebuild(&asked_default).expect("a retired key");
        assert_eq!(back.policy, SupervisorConfig::default());
        assert!(back.supplied.is_empty(), "{back:?}");
        assert_eq!(back.retired.len(), 1, "{back:?}");
        assert!(
            back.retired[0].starts_with("harness.rm_breaker: "),
            "{back:?}"
        );

        let mut limited = texts;
        limited
            .keys
            .insert(0, ("rm_breaker".into(), "false".into()));
        let back = SupervisorConfig::rebuild(&limited).expect("a retired key");
        assert_eq!(
            back.policy.approve,
            Approve::Safe,
            "capped after `approve=all`"
        );
        assert!(back.policy.withheld.rm_breaker);
        assert_eq!(back.retired.len(), 1, "{back:?}");
    }

    #[test]
    fn rebuild_refuses_texts_this_build_cannot_read() {
        let texts = SupervisorConfig::default().policy_texts();
        let refused = |edit: &dyn Fn(&mut PolicyTexts), names: &str| {
            let mut t = texts.clone();
            edit(&mut t);
            let e = SupervisorConfig::rebuild(&t).expect_err(names);
            assert!(e.contains(names), "{names}: {e}");
        };
        refused(
            &|t| t.keys.push(("approve".into(), "safe".into())),
            "more than once",
        );
        refused(&|t| t.keys.push(("no_such".into(), "1".into())), "no_such");
        refused(
            &|t| t.keys.push(("no_such".into(), "1".into())),
            "a newer build",
        );
        refused(
            &|t| {
                for (k, v) in &mut t.keys {
                    if k == "approve" {
                        *v = "maybe".into();
                    }
                }
            },
            "approve",
        );
        refused(
            &|t| t.agents.push(("nobody.enabled".into(), "true".into())),
            "nobody.enabled",
        );
        refused(
            &|t| t.withheld.push(("rm_breaker".into(), "true".into())),
            "written false",
        );
        refused(
            &|t| t.withheld.push(("trust_dialog".into(), "false".into())),
            "trust_dialog",
        );
    }

    #[test]
    fn rebuild_nearest_reads_a_refused_value_as_the_owners_file_and_names_it() {
        let mut ran = SupervisorConfig::default();
        ran.set("approve", "safe").expect("approve");
        ran.autocompact_offer = Some(450);
        let texts = ran.policy_texts();
        let e = SupervisorConfig::rebuild(&texts).expect_err("450k is on no ladder");
        assert!(e.contains("autocompact_offer"), "{e}");
        let back = SupervisorConfig::rebuild_nearest(&texts).expect("the nearest policy");
        assert_eq!(
            back.policy,
            SupervisorConfig {
                autocompact_offer: Some(AUTOCOMPACT_DEFAULT_K),
                ..ran
            },
            "the refused value keeps its default; what else was said stands"
        );
        assert_eq!(back.refused.len(), 1, "{back:?}");
        assert!(
            back.refused[0].starts_with("harness.autocompact_offer: ")
                && back.refused[0].contains("450k")
                && back.refused[0].ends_with("; this build read autocompact_offer=400k"),
            "what was refused and what was read in its place: {back:?}"
        );
        assert!(
            back.supplied.is_empty() && back.retired.is_empty(),
            "{back:?}"
        );
        let mut newer = texts;
        newer.keys.push(("no_such".into(), "1".into()));
        let e = SupervisorConfig::rebuild_nearest(&newer).expect_err("a newer build's key");
        assert!(e.contains("no_such"), "{e}");
    }

    #[test]
    fn against_default_names_each_limit_and_never_a_text_as_one() {
        let full = SupervisorConfig::default();
        assert_eq!(full.against_default(), Written::default(), "full power");
        for key in KEYS {
            let mut c = full.clone();
            c.limit(key);
            let w = c.against_default();
            assert!(w.own.is_empty(), "{key}: {w:?}");
            if c == full {
                assert!(w.limits.is_empty(), "{key}: {w:?}");
            } else {
                assert_eq!(w.limits.len(), 1, "{key}: {w:?}");
                assert!(w.limits[0].starts_with(&format!("{key}: ")), "{w:?}");
            }
        }
        let w = read(
            "[harness]\napprove = \"safe\"\ncontinue = false\ncontinue_per_hour = 6\n\
             model_fallback = \"\"\nanswer_text = \"ask me later\"\nrules_file = \"/r.md\"\n",
        )
        .against_default();
        assert_eq!(
            w.limits,
            [
                "approve: safe",
                "continue: off",
                "continue_per_hour: 6",
                "model_fallback: off"
            ]
        );
        assert_eq!(w.own, ["answer_text", "rules_file"]);
        let w =
            read("[harness]\nmodel_fallback = \"sonnet\"\nhuman_grace_s = 30\n").against_default();
        assert!(w.limits.is_empty(), "{w:?}");
        assert_eq!(w.own, ["model_fallback", "human_grace_s"]);
        assert_eq!(
            read("[harness]\nhuman_grace_s = 600\n")
                .against_default()
                .limits,
            ["human_grace_s: 600"]
        );
        let roots = "trust_roots = []\n";
        assert!(read(&format!("[harness]\n{roots}")).against_default() == Written::default());
        let w = read(&format!("[harness]\napprove = \"safe\"\n{roots}")).against_default();
        assert_eq!(
            (w.limits, w.own),
            (vec!["approve: safe".to_string()], vec![])
        );
        for more in [
            "trust_roots = [\"~/src/app*\"]\n",
            "trust_roots = [\"~/src/app*\", \"/srv/work\"]\n",
        ] {
            let w = read(&format!("[harness]\napprove = \"safe\"\n{more}")).against_default();
            assert_eq!(
                (w.limits.len(), w.own.clone()),
                (1, vec!["trust_roots"]),
                "{more}"
            );
            assert!(read(&format!("[harness]\n{more}")).against_default() == Written::default());
        }
    }

    #[test]
    fn a_retired_key_written_false_is_read_as_the_limit_it_named() {
        let full = SupervisorConfig::default();
        let one = |text: &str| {
            let (c, notes) = SupervisorConfig::from_aterm_toml(text);
            assert_eq!(notes.len(), 1, "{text}: {notes:?}");
            assert!(
                is_retired_key_note(&notes[0]) && !is_unknown_key_note(&notes[0]),
                "{notes:?}"
            );
            (c, notes[0].clone())
        };
        let (c, note) = one("[harness]
approve_all = false
");
        assert_eq!(
            c,
            SupervisorConfig {
                approve: Approve::Safe,
                ..full.clone()
            }
        );
        assert!(note.contains("approve = \"safe\""), "{note}");
        let (c, _) = one("[harness]
auto_reads = false
");
        assert_eq!(c.approve, Approve::None);
        let (c, _) = one("[harness]
rm_breaker = false
");
        assert_eq!((c.approve, c.withheld.rm_breaker), (Approve::Safe, true));
        let (c, _) = one("[harness]
read_outside_cwd = false
");
        assert_eq!(
            (c.approve, c.withheld.read_outside_cwd),
            (Approve::Safe, true)
        );
        let (c, _) = one("[harness]
trust_roots = [\"~/src/app*\"]
trust_dialog = false
");
        assert_eq!((c.approve, c.trust_roots.len()), (Approve::Safe, 0));
        assert!(!retired_still_limits("rate_nudge", Some(false)));
        assert!(
            !retired_still_limits("approve", Some(false)),
            "no retired key"
        );
        for key in RETIRED_KEYS.iter().filter(|k| **k != "rate_nudge") {
            for text in [
                format!("[harness]\n{key} = false\n"),
                format!("[harness]\n{key} = \"no\"\n"),
                format!("[theme]\nharness.{key} = false\n"),
            ] {
                let c = read(&text);
                assert!(c.limits(&full), "{text}: {c:?}");
            }
            let (c, _) = one(&format!("[harness]\n{key} = true\n"));
            assert_eq!(c, full, "{key} = true changes nothing");
            assert!(retired_still_limits(key, Some(false)), "{key}");
            assert!(!retired_still_limits(key, Some(true)), "{key}");
            assert!(!retired_still_limits(key, None), "{key}");
            for text in [
                format!("[harness]\n{key} = \"maybe\"\n"),
                format!("[harness]\n{key} = 2.5\n"),
                format!("[harness]\n{key} = [false]\n"),
            ] {
                let (c, _) = one(&text);
                assert_eq!(c, full, "{text}");
            }
            assert!(retired_key_note(key, None).is_some(), "{key}");
            for off in ["false", "\"no\"", "0", "\"disabled\""] {
                let (c, note) = one(&format!("[harness]\n{key} = {off}\n"));
                assert!(c.limits(&full), "{key} = {off}");
                assert_eq!(
                    retired_key_note_text(key, Some(off.trim_matches('"'))).as_deref(),
                    Some(note.as_str()),
                    "{key} = {off}"
                );
            }
            assert_eq!(
                retired_key_note(key, Some(false)).as_deref(),
                Some(one(&format!("[harness]\n{key} = false\n")).1.as_str()),
                "the config language says the reader's words"
            );
        }
        let c = read("[harness]\napprove = \"none\"\napprove_all = false\n");
        assert_eq!(c.approve, Approve::None);
        for text in [
            "[harness]\napprove_all = false\napprove = \"all\"\n",
            "[harness]\napprove = \"all\"\napprove_all = false\n",
            "[harness]\napprove_all = false\napprove = \"all\"\n[broken\n",
        ] {
            assert_eq!(read(text).approve, Approve::Safe, "{text}");
        }
        assert_eq!(
            read("[harness]\nrm_breaker = false\n")
                .against_default()
                .limits,
            ["approve: safe", "rm_breaker: off"]
        );
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\napprove_al = false\n");
        assert_eq!(c, full);
        assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
        assert_eq!(retired_key_note("approve_al", Some(false)), None);
    }

    #[test]
    fn a_retired_codex_key_reads_and_changes_nothing() {
        let full = SupervisorConfig::default();
        for text in [
            "[harness]\nrate_nudge = false\n",
            "[harness]\nrate_nudge = true\n",
            "[harness]\nrate_nudge = \"no\"\n",
            "[harness]\nrate_nudge = \"maybe\"\n",
            "[harness]\nrate_nudge = 2.5\n",
            "[theme]\nharness.rate_nudge = false\n",
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(text);
            assert_eq!(c, full, "{text}");
            assert_eq!(notes.len(), 1, "{text}: {notes:?}");
            assert!(
                is_retired_key_note(&notes[0]) && !is_unknown_key_note(&notes[0]),
                "{text}: {notes:?}"
            );
            assert!(notes[0].contains("changes nothing"), "{notes:?}");
        }
        let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nrate_nudge = false\n");
        assert_eq!(
            retired_key_note("rate_nudge", Some(false)).as_ref(),
            notes.first()
        );
        assert!(!KEYS.contains(&"rate_nudge"));
        assert!(full.texts().iter().all(|(k, _)| *k != "rate_nudge"));
        let mut c = full.clone();
        c.limit("rate_nudge");
        assert_eq!(c, full, "its limit is nothing");
        let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nrate_nudg = false\n");
        assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
    }

    #[test]
    fn a_disk_knob_under_harness_is_the_disk_readers_not_an_unknown_key() {
        let text = "[harness]\ndisk.auto_free_gib = 0\n";
        let (_, notes) = SupervisorConfig::from_aterm_toml(text);
        assert!(!notes.iter().any(|n| is_unknown_key_note(n)), "{notes:?}");
        let reader: Vec<String> = crate::harness::cli::disk_faults(text)
            .into_iter()
            .map(|f| f.note)
            .collect();
        assert!(!reader.is_empty(), "the disk reader names it");
        for note in &reader {
            assert!(notes.contains(note), "{note:?} in {notes:?}");
        }
        let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nnot_a_key = 1\n");
        assert_eq!(
            notes.iter().filter(|n| is_unknown_key_note(n)).count(),
            1,
            "{notes:?}"
        );
    }

    #[test]
    fn a_misread_question_switch_changes_nothing() {
        for text in [
            "[harness]\nanswer_questions = \"sometimes\"\n",
            "[harness]\nanswer_questions = 2.5\n",
        ] {
            assert!(read(text).answer_questions, "{text}");
        }
        assert!(!read("[harness]\nanswer_questions = false\n").answer_questions);
    }

    #[test]
    fn from_path_reads_the_file_and_a_missing_one_is_the_defaults() {
        let dir = std::env::temp_dir().join(format!("aterm-harness-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let toml = dir.join("aterm.toml");
        assert_eq!(
            SupervisorConfig::from_path(Some(&toml)),
            (SupervisorConfig::default(), Vec::new())
        );
        assert_eq!(
            SupervisorConfig::from_path(None).0,
            SupervisorConfig::default()
        );
        std::fs::write(&toml, "[harness]\nenabled = false\n").expect("write");
        assert!(!SupervisorConfig::from_path(Some(&toml)).0.enabled);
        std::fs::write(&toml, b"# caf\xe9\n[harness]\nenabled = false\n").expect("write");
        assert!(!SupervisorConfig::from_path(Some(&toml)).0.enabled);
        std::fs::write(&toml, b"# caf\xe9\n[harness]\nenabled = true\n").expect("write");
        assert!(SupervisorConfig::from_path(Some(&toml)).0.enabled);
        let (c, notes) = SupervisorConfig::from_path(Some(&dir));
        assert_eq!(c, SupervisorConfig::default());
        assert!(!notes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_memory_ceiling_reads_auto_off_or_gibibytes_and_limits_to_off() {
        use super::super::memory::MemoryCeiling;
        assert_eq!(read("").memory_ceiling, MemoryCeiling::Auto);
        assert_eq!(
            read("[harness]\nmemory_ceiling = 48\n").memory_ceiling,
            MemoryCeiling::Gib(48)
        );
        assert_eq!(
            read("[harness]\nmemory_ceiling = \"64GB\"\n").memory_ceiling,
            MemoryCeiling::Gib(64)
        );
        assert_eq!(
            read("[harness]\nmemory_ceiling = \"off\"\n").memory_ceiling,
            MemoryCeiling::Off
        );
        let (c, notes) =
            SupervisorConfig::from_aterm_toml("[harness]\nmemory_ceiling = \"lots\"\n");
        assert_eq!(
            c.memory_ceiling,
            MemoryCeiling::Auto,
            "no setting: the default"
        );
        assert!(
            notes.iter().any(|n| n.contains("harness.memory_ceiling")),
            "{notes:?}"
        );
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\nmemory_ceiling = 1.5\n");
        assert_eq!(
            c.memory_ceiling,
            MemoryCeiling::Auto,
            "a float: the default"
        );
        assert!(
            notes.iter().any(|n| n.contains("harness.memory_ceiling")),
            "{notes:?}"
        );
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\nmemory_ceiling = \"no\"\n");
        assert_eq!(c.memory_ceiling, MemoryCeiling::Off, "{notes:?}");
        let c = read("[theme]\nharness.memory_ceiling = \"off\"\n");
        assert_eq!(c.memory_ceiling, MemoryCeiling::Off);
        let c = read("[harness]\nmemory_ceiling = 64\n[theme]\nharness.memory_ceiling = 8\n");
        assert_eq!(c.memory_ceiling, MemoryCeiling::Gib(64));
        let c = read("[harness]\nmemory_ceiling = 8\n[theme]\nharness.memory_ceiling = 64\n");
        assert_eq!(
            c.memory_ceiling,
            MemoryCeiling::Gib(64),
            "a higher one limits"
        );
        let w = read("[harness]\nmemory_ceiling = \"off\"\n").against_default();
        assert_eq!(w.limits, ["memory_ceiling: off"]);
        let w = read("[harness]\nmemory_ceiling = 48\n").against_default();
        assert_eq!(w.own, ["memory_ceiling"]);
        assert_eq!(
            SupervisorConfig::default_shown("memory_ceiling").as_deref(),
            Some("auto")
        );
    }

    #[test]
    fn question_priority_takes_its_three_words_and_a_refused_one_changes_nothing() {
        let full = SupervisorConfig::default();
        assert_eq!(full.question_priority, QuestionPriority::Recommended);
        assert_eq!(
            SupervisorConfig::default_shown("question_priority").as_deref(),
            Some("recommended")
        );
        for (value, want) in [
            ("recommended", QuestionPriority::Recommended),
            ("long-term", QuestionPriority::LongTerm),
            ("short-term", QuestionPriority::ShortTerm),
            ("  Long-Term ", QuestionPriority::LongTerm),
            ("SHORT-TERM", QuestionPriority::ShortTerm),
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(&format!(
                "[harness]\nquestion_priority = \"{value}\"\n"
            ));
            assert!(notes.is_empty(), "{value:?}: {notes:?}");
            assert_eq!(c.question_priority, want, "{value:?}");
            assert!(c.answer_questions, "{value:?}");
            let w = c.against_default();
            assert!(w.limits.is_empty(), "{value:?}: HOW is no limit: {w:?}");
            assert_eq!(
                w.own.contains(&"question_priority"),
                want != QuestionPriority::Recommended,
                "{value:?}: the owner's own value"
            );
        }
        for text in [
            "[harness]\nquestion_priority = \"long_term\"\n",
            "[harness]\nquestion_priority = \"ask\"\n",
            "[harness]\nquestion_priority = true\n",
            "[harness]\nquestion_priority = 2.5\n",
            "font_px = \n[harness]\nquestion_priority = \"sometimes\"\n",
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(text);
            assert_eq!(c, full, "{text:?}: changes nothing");
            let about: Vec<&String> = notes
                .iter()
                .filter(|n| n.starts_with("harness.question_priority:"))
                .collect();
            assert_eq!(about.len(), 1, "{text:?}: {notes:?}");
        }
        let c = read("[theme]\nharness.question_priority = \"long-term\"\n");
        assert_eq!(c, full);
    }

    #[test]
    fn land_at_is_a_known_time_the_reader_validates_and_offers() {
        let full = SupervisorConfig::default();
        assert_eq!(full.land_at, None, "no time offered by default");
        assert_eq!(
            KEYS[KEYS.iter().position(|k| *k == "autopilot").unwrap() + 1],
            "land_at"
        );
        assert_eq!(
            SupervisorConfig::default_shown("land_at").as_deref(),
            Some("none")
        );
        for (value, want) in [
            ("18:00", 18 * 60),
            ("00:00", 0),
            ("23:59", 23 * 60 + 59),
            (" 07:05 ", 7 * 60 + 5),
        ] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\nland_at = \"{value}\"\n"));
            assert!(notes.is_empty(), "{value:?}: {notes:?}");
            assert_eq!(c.land_at.map(LandAt::minute), Some(want), "{value:?}");
            let w = c.against_default();
            assert!(
                w.limits.is_empty(),
                "{value:?}: an offer is no limit: {w:?}"
            );
            assert_eq!(w.own, ["land_at"], "{value:?}: the owner's own value");
            let (_, text) = c
                .texts()
                .into_iter()
                .find(|(k, _)| *k == "land_at")
                .unwrap();
            assert_eq!(text, value.trim(), "EFFECTIVE CONFIGURATION lists it");
            let mut back = full.clone();
            back.set("land_at", &text).unwrap();
            assert_eq!(back, c, "{value:?}: read back");
        }
        for text in [
            "[harness]\nland_at = \"\"\n",
            "[harness]\nland_at = \"off\"\n",
            "[harness]\nland_at = false\n",
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(text);
            assert!(notes.is_empty(), "{text:?}: {notes:?}");
            assert_eq!(c, full, "{text:?}: no time offered");
        }
        for text in [
            "[harness]\nland_at = \"24:00\"\n",
            "[harness]\nland_at = \"18:60\"\n",
            "[harness]\nland_at = \"7:00\"\n",
            "[harness]\nland_at = \"18h\"\n",
            "[harness]\nland_at = \"+2h\"\n",
            "[harness]\nland_at = 18\n",
            "[harness]\nland_at = true\n",
            "font_px = \n[harness]\nland_at = \"tomorrow\"\n",
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(text);
            assert_eq!(c, full, "{text:?}: keeps the default");
            let about: Vec<&String> = notes
                .iter()
                .filter(|n| n.starts_with("harness.land_at:"))
                .collect();
            assert_eq!(about.len(), 1, "{text:?}: {notes:?}");
            assert!(
                !notes.iter().any(|n| is_unknown_key_note(n)),
                "{text:?}: a known key: {notes:?}"
            );
        }
        let c = read("[theme]\nharness.land_at = \"18:00\"\n");
        assert_eq!(c, full);
        let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nland_by = \"18:00\"\n");
        assert!(notes.iter().any(|n| is_unknown_key_note(n)), "{notes:?}");
        assert_eq!(LandAt::of_minute(1440), None);
        assert_eq!(
            LandAt::of_minute(61).map(LandAt::text).as_deref(),
            Some("01:01")
        );
    }

    #[test]
    fn answer_questions_false_wins_over_any_priority_in_either_order() {
        use super::super::run::SuperviseOpts;
        for text in [
            "[harness]\nanswer_questions = false\nquestion_priority = \"long-term\"\n",
            "[harness]\nquestion_priority = \"long-term\"\nanswer_questions = false\n",
        ] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(text);
            assert!(notes.is_empty(), "{notes:?}");
            assert!(!c.answer_questions, "{text:?}");
            assert_eq!(c.question_priority, QuestionPriority::LongTerm);
            let o = SuperviseOpts::hosted_with(&c);
            assert!(!o.policy.answer_questions, "{text:?}");
        }
        let on = read("[harness]\nquestion_priority = \"short-term\"\n");
        let o = SuperviseOpts::hosted_with(&on);
        assert!(o.policy.answer_questions);
        assert_eq!(o.policy.question_priority, QuestionPriority::ShortTerm);
    }

    #[test]
    fn an_agent_table_switches_that_agent_off_and_nothing_else() {
        let full = SupervisorConfig::default();
        assert!(full.off.is_empty());
        assert!(full.on_for(Program::Claude) && full.on_for(Program::Codex));
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness.codex]\nenabled = false\n");
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!c.on_for(Program::Codex) && c.on_for(Program::Claude));
        assert!(c.enabled, "the master switch stays on");
        assert_eq!(
            SupervisorConfig {
                off: Vec::new(),
                ..c
            },
            full,
            "nothing else moved"
        );
        for text in [
            "[harness]\ncodex.enabled = false\n",
            "harness.codex.enabled = false\n",
            "harness = { codex = { enabled = false } }\n",
            "[harness]\ncodex = { enabled = false }\n",
            "[harness.codex]\nenabled = \"no\"\n",
            "[harness.codex]\nenabled = 0\n",
            "[harness.codex]\nenabled = \"none\"\n",
        ] {
            assert_eq!(read(text).off, [Program::Codex], "{text:?}");
        }
        let both = read("[harness.codex]\nenabled = false\n[harness.claude]\nenabled = false\n");
        assert_eq!(both.off, [Program::Claude, Program::Codex]);
        assert_eq!(
            both,
            read("[harness.claude]\nenabled = false\n[harness.codex]\nenabled = false\n"),
            "the same policy in either order"
        );
        assert!(both.enabled && !both.on_for(Program::Claude) && !both.on_for(Program::Codex));
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness.codex]\nenabled = true\n");
        assert_eq!((c, notes), (full.clone(), Vec::new()), "`true` is accepted");
        let c = read("[harness]\nenabled = false\n[harness.codex]\nenabled = true\n");
        assert!(!c.on_for(Program::Codex) && !c.on_for(Program::Claude));
        for (text, off) in [
            ("\"none\"", true),
            ("\"sometimes\"", false),
            ("2.5", false),
            ("[false]", false),
        ] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness.codex]\nenabled = {text}\n"));
            assert_eq!(c.on_for(Program::Codex), !off, "{text}: {notes:?}");
            assert!(
                notes.len() == 1 && notes[0].starts_with("harness.codex.enabled:"),
                "{text}: {notes:?}"
            );
        }
    }

    #[test]
    fn only_enabled_is_per_agent() {
        let full = SupervisorConfig::default();
        let (c, notes) = SupervisorConfig::from_aterm_toml(
            "[harness.codex]\napprove = \"none\"\nenabled = false\ncontinu = false\n",
        );
        assert_eq!(c.approve, Approve::All, "{notes:?}");
        assert_eq!(c.off, [Program::Codex]);
        assert_eq!(notes.len(), 2, "one note per key: {notes:?}");
        for (note, key) in notes.iter().zip(["approve", "continu"]) {
            assert!(note.starts_with(&format!("harness.codex.{key}:")), "{note}");
            assert!(is_unknown_key_note(note), "{note}");
        }
        assert_eq!(
            read("[harness]\napprove = \"none\"\n").approve,
            Approve::None
        );
        let (c, notes) =
            SupervisorConfig::from_aterm_toml("[harness.claude.sub]\nenabled = false\n");
        assert_eq!(c, full);
        assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
        for id in ["no-such-agent", "Codex", "claude-code", "generic", "sub"] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness.{id}]\nenabled = false\n"));
            assert_eq!(c, full, "{id}");
            assert!(
                notes.len() == 1
                    && notes[0].starts_with(&format!("harness.{id}:"))
                    && is_unknown_key_note(&notes[0]),
                "{id}: {notes:?}"
            );
        }
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\ncodex = false\n");
        assert_eq!(c, full);
        assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
    }

    #[test]
    fn a_misplaced_agent_table_only_limits() {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml("[theme]\nharness.codex.enabled = false\n");
        assert_eq!(c.off, [Program::Codex]);
        assert!(
            notes[0].contains("`theme.harness.codex.enabled`"),
            "{notes:?}"
        );
        let c = read("[harness.codex]\nenabled = false\n[theme]\nharness.codex.enabled = true\n");
        assert_eq!(c.off, [Program::Codex]);
        assert_eq!(
            read("[a]\nharness.codex.enabled = true\n[harness.codex]\nenabled = false\n").off,
            [Program::Codex]
        );
        assert_eq!(
            read("[theme.harness.claude]\nenabled = false\n").off,
            [Program::Claude]
        );
        assert_eq!(
            read("[theme.harness.codex]\napprove = \"none\"\n"),
            SupervisorConfig::default()
        );
    }

    #[test]
    fn each_key_of_an_agent_table_is_said_on_its_own() {
        const MISPLACED: &str =
            "[theme]\nharness.codex.enabled = false\nharness.codex.approve = \"none\"\n";
        for text in ["[harness.codex]\nenabled = \"none\"\nfoo = 1\n", MISPLACED] {
            let (c, notes) = SupervisorConfig::from_aterm_toml(text);
            assert_eq!(c.off, [Program::Codex], "{text:?}");
            let (unknown, shown): (Vec<_>, Vec<_>) =
                notes.iter().partition(|n| is_unknown_key_note(n));
            assert!(
                shown.len() == 1 && shown[0].starts_with("harness.codex.enabled:"),
                "{text:?}: {notes:?}"
            );
            assert_eq!(unknown.len(), 1, "{text:?}: {notes:?}");
        }
        let (by_toml, mut toml) = SupervisorConfig::from_aterm_toml(MISPLACED);
        let refused = format!("font_px = \n{MISPLACED}");
        assert!(
            aterm_toml::from_str::<aterm_toml::Value>(&refused).is_err(),
            "the fixture must be one the parser refuses"
        );
        let (by_lines, mut lines) = SupervisorConfig::from_aterm_toml(&refused);
        lines.remove(0);
        assert_eq!(by_toml, by_lines);
        toml.sort();
        lines.sort();
        assert_eq!(toml, lines, "both readers say the same of each key");
    }

    #[test]
    fn a_file_that_is_not_toml_reads_an_agent_table_too() {
        const TYPO: &str = "font_px = \n";
        for tail in [
            "[harness.codex]\nenabled = false\n",
            "[harness]\ncodex.enabled = false\n",
            "harness.codex.enabled = false\n",
            "harness = { codex = { enabled = false } }\n",
            "[harness.codex]\nenabled = false\nenabled = true\n",
            "[harness.codex]\nenabled = true\nenabled = false\n",
            "[theme]\nharness.codex.enabled = false\n",
        ] {
            let text = format!("{TYPO}{tail}");
            assert!(
                aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
                "the fixture must be one the parser refuses: {text:?}"
            );
            let c = read(&text);
            assert_eq!(c.off, [Program::Codex], "{text:?}");
            assert!(c.on_for(Program::Claude), "{text:?}");
        }
        for tail in [
            "[harness.codex]\nenabled = true\n",
            "[harness.no-such-agent]\nenabled = false\n",
            "[[harness.codex]]\nenabled = false\n",
            "[harness.codex]\nnote = \"enabled = false\"\n",
            "# harness.codex.enabled = false\n",
            "[harness.codex]\napprove = \"none\"\n",
        ] {
            let text = format!("{TYPO}{tail}");
            assert_eq!(read(&text), SupervisorConfig::default(), "{text:?}");
        }
        let (_, notes) = SupervisorConfig::from_aterm_toml(&format!(
            "{TYPO}[harness.codex]\napprove = \"none\"\n"
        ));
        assert!(notes.iter().any(|n| is_unknown_key_note(n)), "{notes:?}");
    }

    #[test]
    fn an_agent_switched_off_is_a_limit_and_named_as_one() {
        let full = SupervisorConfig::default();
        let off = read("[harness.codex]\nenabled = false\n");
        assert!(off.limits(&full), "an agent off limits");
        assert!(!full.limits(&off), "an agent back on is no limit");
        assert_eq!(
            off.against_default(),
            Written {
                limits: vec!["codex: off".to_string()],
                own: Vec::new(),
            }
        );
        let text = "[harness]\napprove = \"safe\"\n[harness.codex]\nenabled = false\n\
                    [harness.claude]\nenabled = false\n";
        assert_eq!(
            read(text).against_default().limits,
            ["approve: safe", "claude: off", "codex: off"]
        );
        let mut c = full.clone();
        c.limit("codex.enabled");
        assert_eq!(c, off);
        let mut c = full.clone();
        c.set("codex.enabled", "false").expect("a switch");
        assert_eq!(c, off);
        c.set("codex.enabled", "true").expect("a switch");
        assert_eq!(c, off, "`true` changes nothing");
        assert!(c.set("codex.approve", "none").is_err());
        assert_eq!(c, off);
        assert_eq!(
            SupervisorConfig::default_shown("codex.enabled").as_deref(),
            Some("true")
        );
        for key in ["codex.approve", "no-such-agent.enabled", "codex"] {
            assert_eq!(SupervisorConfig::default_shown(key), None, "{key}");
        }
    }
}
