// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's policy: aterm.toml's `[harness]` table.

use std::path::{Path, PathBuf};

use aterm_phase::{Program, ResetKind};

use super::policy::approval::PriorityWord;

use super::memory::MemoryCeiling;
use super::occupancy::owner_rules::OwnerTables;
use crate::harness::accounts::Mode;
use crate::harness::harness_mark;

/// CRUISE'S CONTINUATION, the default `continue_text` (Autopilot Cruise).
pub const CONTINUE_TEXT: &str = concat!(
    harness_mark!(),
    " Autopilot is Cruise: keep going. Finish, verify and land the work, then find the next \
     improvement. Decide and record why for later AI review; defer unavailable work and \
     advance what can proceed. ",
    super::policy::go_ahead::never_a_need!(),
    " ",
    super::policy::needs::restart_hint!(),
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
    "claim_credits",
    "logins",
    "hosts",
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

/// The proof an `aterm:` fact (a call at no progress, programs held at macOS's program check)
/// may be typed at a break (`[harness] stuck_break_note`,
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
    pub question_priority: PriorityWord,
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
    /// reply cut off, the server), never with an ask, but for agy's non-retryable 4xx, which go
    /// to a person.
    pub retry_api_errors: bool,
    /// Let the window's host probe API reachability (`api.anthropic.com` TLS handshake) while a session waits on a network API error, and watch this Mac's network path and routes (local subscriptions, no traffic), so a lost path holds that wait, for every agent, for up to an hour, and its return or a route change wakes it. Off: the host reads neither, and the wait keeps its retry ladder.
    pub probe_api: bool,
    /// Continue once a session or weekly usage limit has reset.
    pub resume_limits: bool,
    /// At Claude Code's usage-limit dialog (`What do you want to do?`), choose `Wait here, then continue automatically …` by its label (never a spending row, never `Stop`), arming the vendor's own continue for the reset (`limit-wait@v1`). Off: the dialog is a person's.
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
    /// CLAIMING THE FREE CREDITS CLAUDE CODE OFFERS (`crate::harness::credit_offers`): on by default (the owner, 2026-10-26: *"the harness should do this"*), a window's host starts one worker that claims a live offer in the person's own Chrome, never buying, never turning extra usage on; `false` is a limit.
    pub claim_credits: bool,
    /// THE LOGIN LIFECYCLE (`[harness] logins`, `"renew"` or `"off"`; `docs/DESIGN-claude-accounts-v2-2026-10-15.md` §5).
    pub logins: bool,
    /// The machines `aterm harness accounts renew --due` renews on in one sitting and `aterm ai doctor --fleet` runs on, over SSH (`host` or `host:/abs/path/to/aterm`).
    pub hosts: Vec<String>,
    /// Type `/compact` when the worker stops on a full context.
    pub compact_on_context_wall: bool,
    /// Relaunch a crashed agent (Claude Code, Muse Code, agy, or codex-alab) on its validated session or conversation.
    pub relaunch: bool,
    /// How long an agent stands frozen (not reading its input, nobody's hand on it) before the supervisor restarts it (`restart if-stalled`) for its relaunch (`supervise/stall.rs`).
    pub stall_term_after_s: u32,
    /// Restart a frozen agent (`restart pid=<n> if-stalled`) once it has stood [`Self::stall_term_after_s`]: on by default (the owner, 2026-11-02: *"all things on by default"*), so a frozen agent resumes its conversation rather than wait on a warning; `false` is a limit, and the warning.
    pub stall_term: bool,
    /// How much memory a supervised agent's process group (physical footprint) may use before aterm warns.
    pub memory_ceiling: MemoryCeiling,
    /// Restart an idle agent onto a newer installed build of itself. `false` begins nothing (no
    /// notice, no tell, no restart); what the upgrade already did to a conversation is still
    /// finished, which no lane switch holds: a restart already made gets its relaunch and its
    /// carry-on, and an agent already typed a notice gets the line that releases it from it.
    pub upgrade: bool,
    /// The live upgrade asks first.
    pub upgrade_ask: bool,
    /// Seconds the supervisor keeps its hands off a session after a person's keystroke into it.
    /// A choice dialog that grants and spends nothing (a question, a plan-mode switch, a goal, a
    /// model or effort switch, the autocompact offer, the auto-mode setup offer) waits the shorter
    /// of this and `approval_loop::QUESTION_PERSON_GRACE` (15 s): a dialog nobody is answering is
    /// answered, never held for the whole grace. A Claude Code permission box nobody has touched
    /// (no key moved its focus, no other driver held the session) waits only until the person's
    /// last input is `approval_loop::PERSON_SETTLE` (2.5 s) old; another program's untouched box
    /// waits the same 15 s at most. One a person or another driver touched, a held message, an
    /// unrecognized box and the usage-limit menu wait the whole grace.
    pub human_grace_s: u32,
    /// What the harness does with a session's measured wait ([`Occupancy`]).
    pub occupancy: Occupancy,
    /// AN `aterm:` FACT ON A PROMPT THE HARNESS TYPES ANYWAY (`[harness] facts`, system
    /// awareness §14.4): a continuation, a done check, Land's instruction or its nudge may end
    /// with one measured fact of the session's own (a background call of the agent's that has
    /// made no progress, or programs of its calls macOS's program check holds), once per
    /// episode, only where it fits whole. `false` is a limit: no fact rides, and none is typed at
    /// a break either.
    pub facts: bool,
    /// AN `aterm:` FACT AT A BREAK (`[harness] stuck_break_note`, the owner's call O10): at a
    /// break of the agent's own background work, where no turn end comes for a fact to ride, the
    /// session's untold fact (a call at no progress, programs held at macOS's program check) is
    /// typed as one line of its own, once per episode. It ships on (the owner, 2026-11-02, deciding
    /// O10: *"all things on by default"*); `false` is a limit, and `facts = false` withdraws it
    /// ([`Self::stuck_break_grant`]).
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
    /// For each key the texts named that this build has retired, the note [`SupervisorConfig::set`] gave as it read it; for each switch of an agent this build retired ([`aterm_phase::RETIRED_AGENTS`]), a note that it set nothing, with its replacement named.
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
            question_priority: PriorityWord::Recommended,
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
            claim_credits: true,
            logins: true,
            hosts: Vec::new(),
            compact_on_context_wall: true,
            relaunch: true,
            stall_term_after_s: 600,
            stall_term: true,
            memory_ceiling: MemoryCeiling::Auto,
            upgrade: true,
            upgrade_ask: false,
            human_grace_s: 120,
            occupancy: Occupancy::Observe,
            facts: true,
            stuck_break_note: true,
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
            "claim_credits" => switch!(claim_credits),
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
            "question_priority" => match PriorityWord::parse(value) {
                Some(p) => self.question_priority = p,
                None => {
                    return Err(format!(
                        "harness.question_priority: expected {}, got {:?} — no setting, so \
                         question_priority keeps its default",
                        PriorityWord::WORDS.join(", "),
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
            "continue_per_hour" => match crate::parse_uint(value.trim()) {
                Some(n) => self.continue_per_hour = n,
                None => return bad(self, "a count"),
            },
            "human_grace_s" => match crate::parse_uint(value.trim()) {
                Some(n) => self.human_grace_s = n,
                None => return bad(self, "a count of seconds"),
            },
            "rules_file" if flag == Some(true) => {
                return Err(format!(
                    "harness.{key}: expected a name, got {v:?} — no setting, so {key} keeps its \
                     default"
                ));
            }
            "rules_file" if spells_off(v) || v.eq_ignore_ascii_case("none") => {
                self.rules_file = None;
            }
            "stall_term_after_s" => match crate::parse_uint(value.trim()) {
                Some(n) => self.stall_term_after_s = n,
                None => return bad(self, "a count of seconds (0: never)"),
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
            "claim_credits" => self.claim_credits = false,
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

    /// `[harness] accounts` as `agent`'s account pool reads it: [`Mode::Off`] where the harness
    /// is off or `agent` is switched off ([`Self::on_for`]), else [`Self::accounts`]. The one
    /// rotation switch every reader shares: the window's host, which also needs to supervise
    /// the session it moves, and a launch's choice of account (`accounts_pool::Seat::mode_for`).
    /// The sync of a pool's roster reads `[harness] accounts` alone
    /// (`accounts_sync::accounts_sync_off`).
    #[must_use]
    pub fn accounts_for(&self, agent: Program) -> Mode {
        if self.on_for(agent) {
            self.accounts
        } else {
            Mode::Off
        }
    }

    /// `[harness] logins` as `agent`'s account pool reads it: its login lifecycle (the
    /// automatic sign-in and renewal of its rows) runs where `agent` is supervised
    /// ([`Self::on_for`]) and [`Self::logins`] is on. The window's host and the accounts' upkeep
    /// share it, for every pool.
    #[must_use]
    pub fn logins_for(&self, agent: Program) -> bool {
        self.on_for(agent) && self.logins
    }

    /// Whether the usage tap ([`Self::usage_tap`]) is on for Claude Code, the one reading every surface shares: the relaunch lines a pass arms it on (the window's lane, `aterm harness upgrade`), the window's host's own uses of a reading (a wall's timing, the session log's mark, the silent tap's note; what a process launched with the tap reports is recorded with it off too), the footer's reason for a missing week, and `accounts stats`' tap.
    #[must_use]
    pub fn usage_tap_on(&self) -> bool {
        self.on_for(Program::Claude) && self.usage_tap
    }

    /// Whether the limit reset ([`Self::limit_reset`]) is on for Claude Code: the one program
    /// that records a limit-reset offer (`accounts::note_offer`), which `aterm harness accounts`'
    /// rotation and `accounts stats`' `resets` both read, and the switch the window's host claims
    /// a reset under, so Claude Code switched off claims none.
    #[must_use]
    pub fn limit_reset_on(&self) -> bool {
        self.on_for(Program::Claude) && self.limit_reset
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

    /// THE ONE PREDICATE the stuck fact's line at a break passes: a [`StuckBreakGrant`] where
    /// neither [`Self::stuck_break_note`] nor [`Self::facts`] is off,
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
        match aterm_toml::from_str::<aterm_toml::Value>(text) {
            Ok(doc) => Self::from_doc_and_text(&doc, text),
            Err(e) => Self::from_invalid_aterm_toml(text, &e.to_string()),
        }
    }

    /// [`Self::from_aterm_toml`] and the owner's tables ([`OwnerTables::from_doc`]) of one parse
    /// of `text`, so the mode and the tables a window adopts are of one text: the policy's notes,
    /// then the tables'. A text TOML refuses reads no owner table (its lines are read one by
    /// one for the limits alone).
    #[must_use]
    pub fn with_owner_tables(text: &str) -> (Self, OwnerTables, Vec<String>) {
        match aterm_toml::from_str::<aterm_toml::Value>(text) {
            Ok(doc) => {
                let (cfg, mut notes) = Self::from_doc_and_text(&doc, text);
                let (tables, owner_notes) = OwnerTables::from_doc(&doc);
                notes.extend(owner_notes);
                (cfg, tables, notes)
            }
            Err(e) => {
                let (cfg, notes) = Self::from_invalid_aterm_toml(text, &e.to_string());
                (cfg, OwnerTables::default(), notes)
            }
        }
    }

    /// [`Self::with_owner_tables`] over the file at `path`, read as [`Self::from_path`] reads it.
    #[must_use]
    pub fn with_owner_tables_at(path: Option<&Path>) -> (Self, OwnerTables, Vec<String>) {
        let Some(path) = path else {
            return (Self::default(), OwnerTables::default(), Vec::new());
        };
        match aterm_types::dirs::read_aterm_config(path) {
            Ok(Some(bytes)) => Self::with_owner_tables(&String::from_utf8_lossy(&bytes)),
            Ok(None) => (Self::default(), OwnerTables::default(), Vec::new()),
            Err(e) => {
                let (cfg, notes) = Self::unread_config(unread_path_note(path, &e));
                (cfg, OwnerTables::default(), notes)
            }
        }
    }

    pub(crate) fn from_parsed_aterm_toml(
        doc: Option<&aterm_toml::Value>,
        text: &str,
    ) -> (Self, Vec<String>) {
        match doc {
            Some(doc) => Self::from_doc_and_text(doc, text),
            None => Self::from_aterm_toml(text),
        }
    }

    fn from_doc_and_text(doc: &aterm_toml::Value, text: &str) -> (Self, Vec<String>) {
        let (cfg, notes, _) = Self::read_doc(doc, text);
        (cfg, notes)
    }

    /// [`Self::from_doc_and_text`], with each setting that turns one of the harness's switches
    /// off on its own ([`SwitchedOff`]), in the order the reader reads them, kept where the
    /// switch is off in the reading.
    fn read_doc(doc: &aterm_toml::Value, text: &str) -> (Self, Vec<String>, Vec<SwitchedOff>) {
        let mut cfg = Self::default();
        let mut notes = Vec::new();
        let mut off_by = Vec::new();
        let mut switched = |turned: Vec<Switch>, by: &dyn Fn() -> OffSetting| {
            off_by.extend(
                turned
                    .into_iter()
                    .map(|switch| SwitchedOff { switch, by: by() }),
            );
        };
        if let Some(h) = doc.get("harness").filter(|h| h.as_table().is_none()) {
            if one_value_text(h).is_some_and(|t| spells_off(&t)) {
                cfg.enabled = false;
                switched(vec![Switch::Harness], &|| {
                    OffSetting::HarnessWrittenOff(written(h))
                });
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
        harness_tables(doc, String::new(), &mut misplaced);
        let root = doc.get("harness").and_then(aterm_toml::Value::as_table);
        let (retired, current): (Vec<_>, Vec<_>) = root
            .map(entries)
            .unwrap_or_default()
            .into_iter()
            .partition(|(key, _)| RETIRED_KEYS.contains(&key.as_str()));
        let mut disk_notes: Option<Vec<String>> = None;
        for (key, value) in current.into_iter().chain(retired) {
            if key == "disk" {
                let dn = disk_notes.get_or_insert_with(|| {
                    crate::harness::cli::disk_faults(text)
                        .into_iter()
                        .filter(|f| f.path == "harness.disk" || f.path.starts_with("harness.disk."))
                        .map(|f| f.note)
                        .collect()
                });
                if !dn.is_empty() {
                    notes.extend(dn.iter().cloned());
                    continue;
                }
            }
            if OWNER_TABLES.contains(&key.as_str()) {
                continue;
            }
            if let Err(e) = cfg.set_value(&key, value) {
                notes.push(e);
            }
            switched(turned_off_alone(&key, value), &|| OffSetting::Harness {
                key: key.clone(),
                value: written(value),
            });
        }
        for (at, table) in misplaced {
            for (key, value) in entries(table) {
                if OWNER_TABLES.contains(&key.as_str()) {
                    notes.push(misplaced_note(
                        &at,
                        &key,
                        false,
                        Some(OWNER_TABLES_ROOT.into()),
                    ));
                    continue;
                }
                let before = cfg.clone();
                let refused = cfg.set_value(&key, value).err();
                let limits = cfg.limits(&before);
                if !limits {
                    cfg = before;
                }
                notes.push(misplaced_note(&at, &key, limits, refused));
                switched(turned_off_alone(&key, value), &|| OffSetting::Misplaced {
                    at: at.clone(),
                    key: key.clone(),
                    value: written(value),
                });
            }
        }
        let mut scalars = Vec::new();
        harness_scalars(doc, String::new(), &mut scalars);
        for (at, value) in scalars {
            if one_value_text(value).is_some_and(|t| spells_off(&t)) {
                cfg.enabled = false;
                notes.push(misplaced_note(&at, "enabled", true, None));
                switched(vec![Switch::Harness], &|| OffSetting::MisplacedWrittenOff {
                    at: at.clone(),
                    value: written(value),
                });
            }
        }
        if let Some(swarm) = doc.get("swarm") {
            match swarm.as_table() {
                Some(swarm) => {
                    for (key, value) in entries(swarm) {
                        if key == "enabled" {
                            let swarm_off = || {
                                OffSetting::Swarm(format!("[swarm] enabled = {}", written(value)))
                            };
                            match value.as_bool() {
                                Some(false) => {
                                    cfg.accounts = crate::harness::accounts::Mode::Off;
                                    switched(vec![Switch::Accounts], &swarm_off);
                                }
                                Some(true) => {}
                                None if one_value_text(value).is_some_and(|t| spells_off(&t)) => {
                                    cfg.accounts = crate::harness::accounts::Mode::Off;
                                    switched(vec![Switch::Accounts], &swarm_off);
                                    notes.push(
                                        "swarm.enabled: expected a boolean (true or false); \
                                         read as false"
                                            .to_string(),
                                    );
                                }
                                None => notes.push(
                                    "swarm.enabled: expected a boolean (true or false)".to_string(),
                                ),
                            }
                        } else {
                            notes.push(format!("swarm.{key}: unknown key — ignored"));
                        }
                    }
                }
                None if one_value_text(swarm).is_some_and(|t| spells_off(&t)) => {
                    cfg.accounts = crate::harness::accounts::Mode::Off;
                    switched(vec![Switch::Accounts], &|| {
                        OffSetting::Swarm(format!("swarm = {}", written(swarm)))
                    });
                    notes.push(
                        "swarm: written off itself — write `enabled = false` under [swarm]"
                            .to_string(),
                    );
                }
                None => notes.push(format!(
                    "swarm: not a table (a TOML {}) — ignored",
                    swarm.type_str()
                )),
            }
        }
        off_by.retain(|off| !cfg.switch_on(off.switch));
        (cfg, notes, off_by)
    }

    fn from_invalid_aterm_toml(text: &str, err_msg: &str) -> (Self, Vec<String>) {
        let mut cfg = Self::default();
        let mut notes = vec![format!(
            "aterm.toml is not valid TOML ({}); its [harness] lines were read one by one",
            err_msg.lines().next().unwrap_or("").trim()
        )];
        let mut seen: Vec<String> = Vec::new();
        let (retired, current): (Vec<_>, Vec<_>) = assignments(text)
            .into_iter()
            .partition(|(path, _)| path.last().is_some_and(|key| RETIRED_KEYS.contains(key)));
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
                [head @ .., "swarm"] | [head @ .., "swarm", "enabled"]
                    if !value.trim_start().starts_with(['[', '{'])
                        && spells_off(&unquote(value)) =>
                {
                    (head, "accounts".to_string(), "off".to_string())
                }
                _ => continue,
            };
            let key = key.as_str();
            if OWNER_TABLES.contains(&key) {
                if !head.is_empty() {
                    let at = head.join(".").replace(".[]", "[]");
                    notes.push(misplaced_note(
                        &at,
                        key,
                        false,
                        Some(OWNER_TABLES_ROOT.into()),
                    ));
                }
                continue;
            }
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
        (cfg, notes)
    }

    /// Each setting in aterm.toml's `text` that turns one of the harness's switches off on its
    /// own ([`SwitchedOff`]), in the order [`Self::from_aterm_toml`] reads them: the harness
    /// written off itself, the keys of the `[harness]` table (an agent's own table's
    /// included), a `harness` key or table filed below another table's header, and the
    /// `[swarm]` switch. A switch the reading leaves on is named by none; one it leaves off is
    /// named by every setting that turns it off, so a switch is back on once they are all gone.
    ///
    /// # Errors
    ///
    /// Where `text` is not valid TOML, whose lines [`Self::from_aterm_toml`] reads one by one (its
    /// notes say what each did): where the parser stopped and why, on one line (`TOML error at
    /// line 3, column 8: …`).
    pub fn switched_off_by(text: &str) -> Result<Vec<SwitchedOff>, String> {
        let doc = aterm_toml::from_str::<aterm_toml::Value>(text).map_err(|why| {
            let at = why.log_words().to_string();
            match crate::supervise::limit::one_line(why.message_text()) {
                reason if reason.is_empty() => at,
                reason => format!("{at}: {reason}"),
            }
        })?;
        let (_, _, off_by) = Self::read_doc(&doc, text);
        Ok(off_by)
    }

    /// What in aterm.toml's `text` turns account rotation off for `agent` (leaves
    /// [`Self::accounts_for`] at [`crate::harness::accounts::Mode::Off`]), from
    /// [`Self::switched_off_by`]: what turns the harness's master switch off, then `agent`'s own,
    /// then rotation's, each in the order the reader reads them. Empty where none does.
    ///
    /// # Errors
    ///
    /// As [`Self::switched_off_by`], where `text` is not valid TOML.
    pub fn accounts_off_by(text: &str, agent: Program) -> Result<Vec<OffSetting>, String> {
        let off_by = Self::switched_off_by(text)?;
        // One setting turns one switch off, so none is named twice; two settings the reader
        // files alike are each named.
        Ok([Switch::Harness, Switch::Agent(agent), Switch::Accounts]
            .into_iter()
            .flat_map(|switch| {
                off_by
                    .iter()
                    .filter(move |off| off.switch == switch)
                    .map(|off| off.by.clone())
            })
            .collect())
    }

    /// Whether this reading leaves `switch` on.
    #[must_use]
    pub fn switch_on(&self, switch: Switch) -> bool {
        match switch {
            Switch::Harness => self.enabled,
            Switch::Agent(agent) => !self.off.contains(&agent),
            Switch::Accounts => self.accounts != Mode::Off,
            Switch::Logins => self.logins,
        }
    }

    /// [`Self::from_aterm_toml`] over the file at `path`: no path or no file is the defaults.
    #[must_use]
    pub fn from_path(path: Option<&Path>) -> (Self, Vec<String>) {
        let Some(path) = path else {
            return (Self::default(), Vec::new());
        };
        Self::read_path(Some(path))
            .unwrap_or_else(|e| Self::unread_config(unread_path_note(path, &e)))
    }

    /// Read admitted policy while preserving an unavailable configuration as an error.
    ///
    /// # Errors
    /// Returns the common reader's refusal or I/O error for an existing configuration.
    pub(crate) fn read_path(
        path: Option<&Path>,
    ) -> Result<(Self, Vec<String>), aterm_types::dirs::AtermConfigReadError> {
        let Some(path) = path else {
            return Ok((Self::default(), Vec::new()));
        };
        Ok(match aterm_types::dirs::read_aterm_config(path)? {
            Some(bytes) => Self::from_aterm_toml(&String::from_utf8_lossy(&bytes)),
            None => (Self::default(), Vec::new()),
        })
    }

    /// Disable supervision when an existing configuration cannot be admitted or read.
    #[must_use]
    pub fn unread_config(note: String) -> (Self, Vec<String>) {
        (
            Self {
                enabled: false,
                ..Self::default()
            },
            vec![note],
        )
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
            && no_more(self.claim_credits, before.claim_credits)
            && no_more(self.logins, before.logins)
            && self.hosts.iter().all(|h| before.hosts.contains(h))
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
            claim_credits,
            logins,
            hosts,
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
            ("claim_credits", claim_credits.to_string()),
            ("logins", if *logins { "renew" } else { "off" }.to_string()),
            ("hosts", hosts.join(",")),
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
        let mut retired = Vec::new();
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
                } else if what == "agent"
                    && key
                        .split_once('.')
                        .is_some_and(|(head, _)| aterm_phase::retired_agent(head))
                {
                    // An older build's switch for an agent this build retired (Gemini CLI's
                    // `gemini.enabled`): it sets nothing, and the note says so with agy named.
                    retired.push(format!(
                        "harness.{key}: the switch of an agent this build retired, read as nothing — {}",
                        aterm_phase::RETIRED_AGENT
                    ));
                } else {
                    return Err(format!(
                        "harness.{key}: no {what} this build has — a policy a newer build wrote"
                    ));
                }
            }
        }
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
        Program::Claude
        | Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Muse
        | Program::Generic => &[],
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
    let (neg, unsigned) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v.strip_prefix('+').unwrap_or(&v)),
    };
    let (radix, raw_body) = [("0x", 16), ("0o", 8), ("0b", 2)]
        .into_iter()
        .find_map(|(p, r)| unsigned.strip_prefix(p).map(|b| (r, b)))
        .unwrap_or((10, unsigned));
    let body = raw_body.replace('_', "");
    if body.is_empty() || !body.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    match u64::from_str_radix(&body, radix).ok()? {
        0 => Some(false),
        1 if !neg => Some(true),
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
    let n: u32 = crate::parse_uint(v.strip_suffix('k').unwrap_or(&v))?;
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

/// One of the harness's switches a setting in aterm.toml can turn off
/// ([`SupervisorConfig::switched_off_by`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// The master switch ([`SupervisorConfig::enabled`]).
    Harness,
    /// One agent's own ([`SupervisorConfig::off`]).
    Agent(Program),
    /// Account rotation ([`SupervisorConfig::accounts`] at [`Mode::Off`]).
    Accounts,
    /// The login lifecycle ([`SupervisorConfig::logins`]).
    Logins,
}

/// A setting in aterm.toml ([`OffSetting`]) that turns one of the harness's switches off on its
/// own, and that switch ([`SupervisorConfig::switched_off_by`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchedOff {
    /// The switch it turns off.
    pub switch: Switch,
    /// The setting.
    pub by: OffSetting,
}

/// A setting in aterm.toml that turns one of the harness's switches off
/// ([`SupervisorConfig::switched_off_by`]). Each value is spelled as TOML reads it, which may
/// differ from the file's own spelling of an equal value (`0x0` reads `0`, `'off'` reads
/// `"off"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffSetting {
    /// A key of the `[harness]` table, its value as TOML reads it: `<agent>.<leaf>` is the key
    /// `<leaf>` of the agent's own table, `[harness.<agent>]`. `"early"` or `"wall"` in place of
    /// an `accounts` there turns rotation back on, where nothing else turns it off.
    Harness {
        /// The key, as the reader files it.
        key: String,
        /// Its value as TOML reads it.
        value: String,
    },
    /// A key of a `harness` table filed below the header of the table at `at`, which TOML files
    /// as `<at>.harness.<key>` and which limits all the same; its value as TOML reads it.
    Misplaced {
        /// The table it is filed in.
        at: String,
        /// The key, as the reader files it.
        key: String,
        /// Its value as TOML reads it.
        value: String,
    },
    /// The harness written off itself (`harness = false`), its value as TOML reads it.
    HarnessWrittenOff(String),
    /// The same below the header of the table at `at`, which TOML files as `<at>.harness` and
    /// which turns the harness off all the same; its value as TOML reads it.
    MisplacedWrittenOff {
        /// The table it is filed in.
        at: String,
        /// Its value as TOML reads it.
        value: String,
    },
    /// The swarm switch, as TOML reads it (`[swarm] enabled = false`, `swarm = false`).
    Swarm(String),
}

impl OffSetting {
    /// The setting as aterm.toml says it, in backticks.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Harness { key, value } => match key.split_once('.') {
                Some((agent, leaf)) => format!("`[harness.{agent}] {leaf} = {value}`"),
                None => format!("`[harness] {key} = {value}`"),
            },
            Self::Misplaced { at, key, value } => format!(
                "`{at}.harness.{key} = {value}` (a `harness.{key}` below another table's header)"
            ),
            Self::HarnessWrittenOff(value) => format!("`harness = {value}`"),
            Self::MisplacedWrittenOff { at, value } => {
                format!("`{at}.harness = {value}` (a `harness` below another table's header)")
            }
            Self::Swarm(written) => format!("`{written}`"),
        }
    }

    /// Whether this is `[harness] accounts` itself, whose `"early"` or `"wall"` turns rotation
    /// back on.
    #[must_use]
    pub fn is_harness_accounts(&self) -> bool {
        matches!(self, Self::Harness { key, .. } if key == "accounts")
    }
}

/// The switches `key = value` turns off on its own as a key of the `[harness]` table, as the
/// reader reads it ([`SupervisorConfig::set_value`]; a refused value may still read as its
/// limit).
fn turned_off_alone(key: &str, value: &aterm_toml::Value) -> Vec<Switch> {
    let base = SupervisorConfig::default();
    let mut alone = base.clone();
    let _ = alone.set_value(key, value);
    let mut turned = Vec::new();
    if base.enabled && !alone.enabled {
        turned.push(Switch::Harness);
    }
    turned.extend(
        alone
            .off
            .iter()
            .filter(|agent| !base.off.contains(agent))
            .map(|agent| Switch::Agent(*agent)),
    );
    if base.accounts != Mode::Off && alone.accounts == Mode::Off {
        turned.push(Switch::Accounts);
    }
    if base.logins && !alone.logins {
        turned.push(Switch::Logins);
    }
    turned
}

/// `value` as TOML reads it, in TOML's spelling: a text quoted, a boolean or number bare (a
/// number in decimal, whatever base the file wrote it in).
fn written(value: &aterm_toml::Value) -> String {
    match value {
        aterm_toml::Value::String(text) => format!("{text:?}"),
        other => scalar_text(other).unwrap_or_else(|| other.type_str().to_string()),
    }
}

/// The root `[harness]` keys only the owner writes: the mode and the owner's own source and rule
/// tables (`docs/DESIGN-harness-occupancy-2026-10-20.md` §1.1, §3.3).
pub const OWNER_KEYS: [&str; 3] = ["occupancy", OWNER_TABLES[0], OWNER_TABLES[1]];

/// The owner's source and rule tables: [`super::occupancy::owner_rules::OwnerTables`]' to read,
/// never [`SupervisorConfig`]'s (not keys of it, so never `unknown key` either).
const OWNER_TABLES: [&str; 2] = super::occupancy::owner_rules::TABLES;

/// Why an owner table below another table's header is read nowhere.
const OWNER_TABLES_ROOT: &str =
    "the owner's rule tables are read from the root [harness] table alone";

/// WHAT ONE TEXT OF aterm.toml SAYS IN THE OWNER'S KEYS ([`OWNER_KEYS`]), read so two texts
/// compare: the mode [`SupervisorConfig::from_aterm_toml`] reads off it (a misplaced limit and an
/// invalid file's lines included), and the keys as written. Two texts whose readings are equal
/// say the same there; the GUI refuses a save of the aterm.toml in force whose reading differs
/// from the file's when a control client made the save or an edit in it.
#[derive(Debug, PartialEq)]
pub struct OwnerKeys {
    mode: Occupancy,
    written: OwnerKeysWritten,
}

/// [`OwnerKeys`]'s keys as one text writes them.
#[derive(Debug, PartialEq)]
enum OwnerKeysWritten {
    /// A TOML document: the root `[harness]` table's value of each key, `None` where absent.
    Root([Option<aterm_toml::Value>; 3]),
    /// Not TOML: every assignment the line-by-line reader files under one of the keys, at any
    /// depth, as its path and its value's text.
    Lines(Vec<(Vec<String>, String)>),
}

impl OwnerKeys {
    /// The reading of `text`, the whole aterm.toml.
    #[must_use]
    pub fn of(text: &str) -> Self {
        let mode = SupervisorConfig::from_aterm_toml(text).0.occupancy;
        let written = match aterm_toml::from_str::<aterm_toml::Value>(text) {
            Ok(doc) => {
                let root = doc.get("harness").and_then(aterm_toml::Value::as_table);
                OwnerKeysWritten::Root(OWNER_KEYS.map(|key| root.and_then(|t| t.get(key)).cloned()))
            }
            Err(_) => OwnerKeysWritten::Lines(
                assignments(text)
                    .into_iter()
                    .filter(|(path, _)| {
                        path.windows(2)
                            .any(|pair| pair[0] == "harness" && OWNER_KEYS.contains(&pair[1]))
                    })
                    .map(|(path, value)| {
                        (
                            path.into_iter().map(str::to_string).collect(),
                            value.to_string(),
                        )
                    })
                    .collect(),
            ),
        };
        Self { mode, written }
    }
}

/// The note of an aterm.toml at `path` that exists and cannot be read.
fn unread_path_note(path: &Path, e: &aterm_types::dirs::AtermConfigReadError) -> String {
    format!(
        "{}: cannot be read ({e}); the harness is disabled",
        path.display()
    )
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
#[path = "config_tests.rs"]
mod tests;
