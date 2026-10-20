// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's policy: aterm.toml's `[harness]` table.
//!
//! The rule: batteries included, on by default; configuration limits power,
//! it never enables it. Unless the owner has written otherwise, every agent
//! session aterm hosts is supervised in fully automatic mode: nobody is at
//! the keyboard, so nothing it meets may wait on a person (owner, 2026-09-24).
//! [`SupervisorConfig::default`] is every power on, and every key the owner
//! can write takes power away:
//!
//! * a key missing is its default: full power;
//! * only an explicit setting takes power away. A switch reads
//!   `true`/`false` and other plain spellings of them (`"yes"`/`"no"`,
//!   `"on"`/`"off"`, `1`/`0`, any case); `approve` reads `all`, `safe`,
//!   `none` (any case) and an off spelling as `none`. A known key with a
//!   value that is neither its own nor an off (`approve = "maybe"`, `enabled
//!   = 2.5`, a table) keeps its default and is reported;
//! * an unknown key is reported and changes nothing;
//! * `harness` written as an off itself (`harness = false`) is the master
//!   switch off; any other value that is not a table is ignored;
//! * `harness.<key>` written under another table (`[theme]` then
//!   `harness.enabled = false`) still limits, and only limits, so a kill
//!   switch works wherever in the file it was written;
//! * a file that is not TOML at all is read line by line inside its
//!   `[harness]` section (`assignments`; a key said twice there may only
//!   limit the second time), so a typo elsewhere never re-arms a limit here;
//! * a retired key ([`RETIRED_KEYS`]: 0.93.0's approval switches) written
//!   `false` is read as the limit it named ([`SupervisorConfig::retired`])
//!   and said so; written `true` it asked for the default, does nothing, and
//!   is said to (owner, 2026-09-25); `rate_nudge`, the retired Codex
//!   harness's, does nothing either way and is said to;
//! * `[harness.<agent>]`, where `<agent>` is a supervisable program's own
//!   name (`claude`, `codex`), takes one key: `enabled = false` switches that
//!   agent's sessions off ([`SupervisorConfig::off`], read through
//!   [`SupervisorConfig::on_for`]); `true` changes nothing beyond the master
//!   switch; any other key there is reported and changes nothing. It limits
//!   wherever it is written, as every key does. The window's host is its one
//!   reader: the engine and the launcher read neither it nor the master
//!   switch.
//!
//! Stops are warnings by default: where the harness would halt, end or wait
//! on an agent on its own judgment, it says so and goes on. Each stop is one
//! opt-in switch away: `upgrade_ask` (the live upgrade's notice and readiness
//! answer), `stall_term` (a frozen agent restarted with `restart if-stalled`),
//! `rm_breaker_ask` (under `approve = "safe"`, an unproven rm circuit breaker
//! handed to a person). Written `true`, each is a limit by
//! [`SupervisorConfig::limits`]'s measure (it takes the harness's own move
//! away), except `stall_term`, whose `true` is more power, so it is never
//! taken from a misplaced table. What a person set up stays a stop
//! (owner, 2026-09-29):
//! `approve = "none"`, their own Claude Code ask rules and hooks, a hold they
//! pressed.
//!
//! One more key's `true` is more power, and it ships `false` too:
//! `limit_reset`, the taking of a limit reset the plan offers at Claude
//! Code's usage-limit dialog. A reset is the owner's scarce purchase, and the
//! rule that takes one has never been run against a real wall, so it is held
//! as `stall_term` is: `false` unless the root `[harness]` table says `true`,
//! never taken from a misplaced table, and left `false` by its limit.
//! `usage_tap` is held the same way: its `true` puts Claude Code's own
//! status-line setting on the relaunch lines aterm types (the usage tap,
//! `crate::harness::tap`), which changes what a session's argv carries, so it
//! ships `false` and only the root table's `true` turns it on.
//!
//! `usage_notes` ships `false` as well, held the same way: on a machine that
//! lists no accounts, its `true` keeps a record of the one account's usage —
//! usage credits carrying its sessions, the vendor's own warning that a window
//! is filling — and raises the two digest lines they make (accounts v2 §3.3,
//! §5.4). A new alert on a machine that did not ask for it is the owner's to
//! turn on; with accounts listed the record is kept unless `accounts` is
//! `off` (a wall's is kept under `off` too).
//!
//! `usage_credits` ships `false` and is held the same way: its `true` lets
//! the harness carry a Claude Code session onto usage credits — the consent
//! box that names them taken, the bucket that asks for one continued into
//! it, its own turns typed while credits carry the session, a model billed
//! to them chosen — and its default keeps the session on the subscription
//! (owner, 2026-10-19: *"for continuing on credits, that's an optional
//! additional feature that's off by default. use subscriptions."*): but for
//! one probe turn past the reset the credits' notice named, or at the hold's
//! bound where none is named, which another window may still bill to
//! credits. Claude Code saves a model's consent for good per model and
//! account, so its `true` is the owner's to write, and only the root table's
//! turns it on. A PURCHASE is no part of it: nothing bought, whatever the
//! key says.
//!
//! One key says how far the harness carries a session's work: `autopilot`
//! ([`Autopilot`]; `docs/DESIGN-harness-autopilot-2026-10-15.md` §2),
//! `cruise` (the default: keep the work moving until the agent's clear final
//! statement), `land` (wrap the work up cleanly, then type nothing more but
//! the land answer to a question asked in prose) or `off` (hands off: plain
//! Claude Code), any case, and the owner's own words for them
//! (`forever`/`infinity`, `wrap-up`/`wrapup`/`wrap`). Each step down
//! takes power away, so `land` and `off` are limits and `off` (any off
//! spelling) is the key's; a value that is none of them keeps `cruise` and
//! is said. The key is the DEFAULT: a session's own word (`aterm ctl @<sid>
//! meta set autopilot off|cruise|land`, owned by aterm-gui, read by key off
//! the bare `meta` reply, [`SupervisorConfig::autopilot_for`]) decides that
//! session over it, and `meta unset autopilot` hands it back. The master
//! switch (`enabled = false`) wins over both: no session is supervised at
//! all, whatever its word.
//!
//! One key only offers: `land_at`, a local time `HH:MM`
//! ([`LandAt`]; `docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.4), the
//! time the window's Autopilot menu and Settings ▸ Harness offer a timed
//! Cruise to land at. It is never armed by itself, so it neither limits nor
//! grants: a misplaced one changes nothing, and a value that is no time
//! keeps the default (none offered) and is said. No engine path reads it.
//!
//! One key says how rather than how much: `question_priority`
//! ([`QuestionPriority`]), how a question is answered once `answer_questions`
//! answers it: `recommended` (the default: a dialog's recommended option, a
//! question in prose `answer_text`), or `long-term` / `short-term` (the
//! worker decides by that word's fixed policy text, written into a dialog's
//! free-text row and typed in place of `answer_text`). Like a text it
//! neither limits nor grants: a misplaced one, or a value this reader cannot
//! take, changes nothing, and says so (owner, 2026-09-25).
//!
//! [`SupervisorConfig::from_aterm_toml`] is the one reader of that table: the
//! window's host, `aterm drive answer` (its grace) and the hand-run `aterm
//! harness upgrade` all call it, so none of them can disagree on a key, a
//! default or a refusal.

use std::path::{Path, PathBuf};

use aterm_phase::Program;

use super::policy::approval::QuestionPriority;

use super::memory::MemoryCeiling;
use crate::harness::accounts::Mode;

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
];

/// The `[harness]` keys this reader retired: 0.93.0's four approval
/// switches, folded into [`KEYS`]' `approve`, and `approve_all`, which
/// `approve = "all"` is — each still limits when written `false`
/// ([`SupervisorConfig::retired`]) — and 0.105.0's `rate_nudge`, the Codex
/// harness's rate-limit nudge, which no supervisor answers any more: it
/// changes nothing either way. None is a key a new file should write; a file
/// that names one still reads, and is told so.
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
    /// Every box: its one-shot allow (never a "don't ask again" option, which
    /// would rewrite the agent's own settings). The default.
    All,
    /// Only a box [`super::policy::approval::decide`] proves safe: a read-only
    /// command, a trust dialog under [`SupervisorConfig::trust_roots`], a read
    /// under an allowed root and outside the secrets list
    /// ([`super::policy::ApprovalCtx::read_roots`]), an rm under a scratch root.
    Safe,
    /// None: every box goes to the owner.
    None,
}

impl Approve {
    /// How much this level answers: `none` < `safe` < `all`.
    fn rank(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Safe => 1,
            Self::All => 2,
        }
    }

    /// This level, capped at `cap`: a limit can lower it, never raise it.
    #[must_use]
    pub fn at_most(self, cap: Self) -> Self {
        if cap.rank() < self.rank() { cap } else { self }
    }

    /// `all`, `safe` or `none`, in any case; an off spelling is `none` only
    /// by [`SupervisorConfig::set`]'s rule for every key ([`spells_off`]).
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
    pub fn word(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Safe => "safe",
            Self::None => "none",
        }
    }
}

/// AUTOPILOT: how far the harness carries a session's work at its turn ends
/// (`[harness] autopilot`, the default; a session's own `autopilot` word over
/// it, [`SupervisorConfig::autopilot_for`]). The owner's three-way control
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §2.2): the window's chip
/// cycles it `cruise` → `land` → `off`. Power runs `off` < `land` < `cruise`,
/// so a key may only move it down that order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Autopilot {
    /// Keep the work moving: every point answered, the work continued until
    /// the agent's clear final statement (a done reply to the done check, or
    /// a legacy explicit-finish report); a final report that still names open
    /// work, a blocked item included, or a session repository that still
    /// holds uncommitted, untracked, stashed or unpushed work is challenged
    /// at every done report while it is left (`policy::turn_end`'s
    /// `RULE_OPEN_ITEMS` and `RULE_UNCOMMITTED`). The harness as it ran before
    /// Autopilot. The default.
    #[default]
    Cruise,
    /// Wrap the work up cleanly, then stop: the turn-end policy types the
    /// land instruction once (finish the step in hand, start nothing new,
    /// leave it verified, committed and pushed, write down what is left,
    /// report the outcome in ordinary language), at most `LAND_NUDGES`
    /// nudges after it, and then nothing but the land answer, which a
    /// question asked in prose at a turn end gets whether or not the nudges
    /// are spent (`policy::turn_end`'s `RULE_LAND`, `RULE_LAND_NUDGE`,
    /// `RULE_LAND_ANSWER`). A question DIALOG is answered as under `Cruise`
    /// (the session's `questions` word, else `question_priority`: the land
    /// answer for a dialog is deferred, the design's §2.2), and so are boxes
    /// and walls: the wrap-up still has to finish.
    Land,
    /// Hands off: the harness answers nothing and types nothing into the
    /// session — plain Claude Code, the owner's A/B baseline. The loop parks
    /// while the session's word says so (`Session::park_while_off`), and the
    /// window's host stops the session's worker (aterm-gui).
    Off,
}

impl Autopilot {
    /// Every mode, in the order the chip cycles them.
    pub const ALL: [Autopilot; 3] = [Autopilot::Cruise, Autopilot::Land, Autopilot::Off];

    /// The mode's one word, as the table, the session's `meta` word and the
    /// chip spell it: `cruise`, `land` or `off`. STABLE: [`Self::parse`]
    /// reads it back.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Autopilot::Cruise => "cruise",
            Autopilot::Land => "land",
            Autopilot::Off => "off",
        }
    }

    /// A mode from its word ([`Self::word`]) or the owner's own words for it
    /// (`forever`, `infinity`: [`Autopilot::Cruise`]; `wrap-up`, `wrapup`,
    /// `wrap`: [`Autopilot::Land`]), trimmed, in any case. `None` for anything
    /// else: an on/off switch word other than `off` is no mode.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "cruise" | "forever" | "infinity" => Some(Autopilot::Cruise),
            "land" | "wrap-up" | "wrapup" | "wrap" => Some(Autopilot::Land),
            "off" => Some(Autopilot::Off),
            _ => None,
        }
    }

    /// How much this mode carries: `off` < `land` < `cruise`.
    fn rank(self) -> u8 {
        match self {
            Autopilot::Off => 0,
            Autopilot::Land => 1,
            Autopilot::Cruise => 2,
        }
    }
}

/// How much `[harness] accounts` rotates: `off` < `wall` < `early` (a move at
/// the limit is one of the moves `early` makes).
fn accounts_rank(mode: Mode) -> u8 {
    match mode {
        Mode::Off => 0,
        Mode::Wall => 1,
        Mode::Early => 2,
    }
}

/// The owner's policy for every supervised agent session. Every field's
/// default is full power; see the module doc for how a written key limits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// The master switch (Settings: search "harness"). Off: no session is supervised.
    pub enabled: bool,
    /// The agents whose sessions are not supervised: each one a
    /// `[harness.<agent>]` table switched off (`enabled = false`), named once,
    /// in name order. Read through [`Self::on_for`], under the master switch.
    pub off: Vec<Program>,
    /// Supervise the sessions of a headless instance too.
    pub headless: bool,
    /// THE DEFAULT AUTOPILOT ([`Autopilot`], `[harness] autopilot`): how far
    /// the harness carries a session that has no `autopilot` word of its own
    /// ([`Self::autopilot_for`]). [`Autopilot::Cruise`] by default; `off`
    /// (any off spelling) is its limit.
    pub autopilot: Autopilot,
    /// THE TIME A TIMED CRUISE IS OFFERED TO LAND AT (`[harness] land_at`,
    /// local `HH:MM`, `docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.4):
    /// the Autopilot chip's menu and Settings ▸ Harness offer "Cruise until
    /// 18:00, then land" with it. Only offered, never armed by itself, so it
    /// takes no power and gives none: a known key this reader validates (a
    /// malformed time keeps the default, unset, and is said) and lists, and
    /// no engine path reads. `None` (the default): no time is offered.
    pub land_at: Option<LandAt>,
    /// What a permission box may be answered with, one level for every
    /// agent. The safe rules judge Claude Code's boxes: under
    /// [`Approve::Safe`] any other agent's box goes to a person.
    pub approve: Approve,
    /// Folders whose trust dialog is answered under [`Approve::Safe`] —
    /// Claude Code's. A leading `~/` is the
    /// home directory; a trailing `*` matches any suffix of the last
    /// component (`~/src/app*` is every folder in `~/src` whose name starts
    /// `app`, a checkout and its worktrees beside it). None by default: no
    /// fact about a session or the vendor makes a folder safe to trust, so
    /// under `safe` every trust dialog goes to a person until the person
    /// names their own folders here. A trust root is also a Read root for
    /// Claude Code's Read box.
    pub trust_roots: Vec<String>,
    /// Under `approve = "safe"`, hand an unproven rm circuit breaker to a
    /// person. Off (the default): Claude Code's `rm` on a possibly-empty
    /// variable box that the safe rules cannot prove is approved with its
    /// reason kept as a warning
    /// ([`super::policy::approval::RULE_RM_BREAKER_WARN`]: the ledger's
    /// `unproven`, and one `WARN` line said). `approve = "none"`, the owner's
    /// own ask rules and hooks, and the retired `rm_breaker = false` still
    /// send the box to a person whatever this says (owner, 2026-09-29).
    pub rm_breaker_ask: bool,
    /// Answer a question, a choice between options or a request for a
    /// decision with [`Self::answer_text`] instead of holding it for a person.
    pub answer_questions: bool,
    /// How a question is answered while [`Self::answer_questions`] answers it
    /// (`[harness] question_priority`): [`QuestionPriority::Recommended`], the
    /// default (a dialog's recommended option, a question in prose
    /// [`Self::answer_text`]), or, `long-term` / `short-term`, the worker
    /// decides by that word's fixed policy text, written into a dialog's
    /// free-text row and typed in place of `answer_text` in prose. Never
    /// whether: `answer_questions = false` answers nothing whatever this says.
    /// A session's own `questions` word decides over it (`aterm ctl @<sid>
    /// meta set questions …`, read by the loop's question policy). A value
    /// the reader cannot take changes nothing.
    pub question_priority: QuestionPriority,
    /// What a question is answered with.
    pub answer_text: String,
    /// Dismiss Claude Code's rating surveys — the session survey, the long-context,
    /// plugin and memory cards — with a guarded `0`, never a rating.
    pub dismiss_surveys: bool,
    /// Type a continuation when a worker ends a turn with work left.
    pub continue_policy: bool,
    /// What a continuation types when the worker offers no suggestion of its own.
    pub continue_text: String,
    /// At most this many continuations per session per hour; `0` is no cap.
    /// Without a cap, a worker that keeps stopping short is continued on a
    /// growing back-off, never left waiting for a person.
    pub continue_per_hour: u32,
    /// A file whose text is appended to a continuation (standing rules).
    pub rules_file: Option<PathBuf>,
    /// Answer an API error or an overload, however many, by what it says went
    /// wrong (`policy::turn_end`'s module docs: the network, a reply cut off,
    /// the server), never with an ask. Off: each is escalated.
    pub retry_api_errors: bool,
    /// Let the window's host measure the API's reach while a session waits at
    /// an API error the network caused (a resolve, a connect and a verified
    /// TLS handshake to `api.anthropic.com`, no request sent; aterm-agent
    /// `harness::netwatch`), so the wait at an unreachable API ends within
    /// about a minute of it being back, not at the ladder's next rung. Off:
    /// nothing is probed, and such a wall is tried on the time ladder alone.
    pub probe_api: bool,
    /// Continue once a session or weekly usage limit has reset.
    pub resume_limits: bool,
    /// At Claude Code's usage-limit dialog (`What do you want to do?`), choose
    /// `Wait here, then continue automatically …` by its label (never a
    /// spending row, never `Stop`), so the vendor's own continue is armed for
    /// the reset (`limit-wait@v1`). Off: the dialog is a person's
    /// (owner, 2026-09-24).
    pub limit_wait: bool,
    /// At Claude Code's autocompact offer (`We recommend compacting
    /// earlier`, 2.1.302), choose `Autocompact at ‹ <size> › tokens` at this
    /// size, in kilo-tokens (`autocompact-offer@v1`): the focus moved onto
    /// that row, the size stepped to it, then Enter — never the keep row,
    /// never Esc. The one standing Claude Code setting the harness chooses,
    /// by the owner's direction (2026-10-16: "this should select to auto
    /// compact at 400 as recommended … without interrupting"). `None`
    /// (`"off"`): the dialog is a person's.
    pub autocompact_offer: Option<u32>,
    /// Which sessions are rotated across the owner's listed Claude Code
    /// accounts (`accounts.toml` beside this file, `aterm harness accounts`;
    /// `docs/DESIGN-claude-accounts-2026-10-01.md` §3, §6): a session whose
    /// turn ends on a usage limit is relaunched on its own conversation under
    /// the next runnable account ([`Mode::Wall`]), and ahead of the limit too
    /// ([`Mode::Early`], the default: at a plain turn end, off an account
    /// another session found at its limit, or one the accounts probe's
    /// `/usage` sample shows low; only `early` probes: `/usage` typed at an
    /// idle turn end, the panel read and closed). [`Mode::Off`], its limit,
    /// moves no session. Inert with no roster, so full power needs no
    /// exception to the rule. The turn-end policy reads only whether it is off;
    /// which account and when is the host's
    /// (`crate::harness::accounts::advise`, `IdleHost::account_probe`).
    pub accounts: Mode,
    /// Take a limit reset the plan offers at Claude Code's usage-limit
    /// dialog, ahead of [`Self::limit_wait`]'s row (`limit-reset@v1`;
    /// `docs/SPEC-claude-accounts-resets-2026-10-02.md`): keys only, through
    /// the dialog's reset row matched by its exact label, then its
    /// confirmation read whole. No command is ever typed for it, and a
    /// confirmation that does not read exactly is closed with the reset kept. Off by default: a reset is the owner's
    /// scarce purchase, so `true` is more power, as [`Self::stall_term`]'s
    /// is: [`Self::limit`] leaves it off and a misplaced table never turns it
    /// on. Acts only with
    /// [`Self::resume_limits`] on (the loop's gate; this reader takes the key
    /// as written). Never yet run against a real wall: its screens were
    /// composed from the vendor's bundle, so the default stays `false` until
    /// that spec's live check (§6) passes.
    pub limit_reset: bool,
    /// The usage tap (`crate::harness::tap`; the accounts design v2 §3.1,
    /// ruling R-7): put Claude Code's own status-line setting — an inline
    /// `--settings` whose command is `<aterm> harness tap` — on the relaunch
    /// lines aterm types for a Claude Code on the machine's own login, so its
    /// usage figures are read after every reply and recorded for that login.
    /// The readings move no session. Off by default: it changes the argv a
    /// relaunched session carries, so `true` is more power, as
    /// [`Self::limit_reset`]'s is — [`Self::limit`] leaves it off and a
    /// misplaced table never turns it on. Read live, independent of
    /// [`Self::accounts`]: a line typed after an edit follows it, and a
    /// session tapped before keeps its tap, and is recorded, until its next
    /// relaunch.
    pub usage_tap: bool,
    /// On a machine that lists no Claude Code accounts, keep a record of the
    /// one account's usage as its sessions' screens say it — usage credits
    /// carrying them past a limit, the vendor's own warning that a window is
    /// filling — and raise the digest's two lines they make: credits in use,
    /// once a day, and the week nearly used with credits off (accounts v2
    /// §3.3, §5.4). Off by default: a line of the digest on a machine that did
    /// not ask for it is the owner's to turn on, so `true` is more power, held
    /// as [`Self::limit_reset`] is. With accounts listed the record is kept
    /// unless [`Self::accounts`] is `off` (a wall's is kept under `off` too).
    /// Nothing is typed for it either way: credits are work going on, never a
    /// wall.
    pub usage_notes: bool,
    /// CONTINUING ON USAGE CREDITS (owner, 2026-10-19: *"for continuing on
    /// credits, that's an optional additional feature that's off by default.
    /// use subscriptions."*). `true`: the harness may carry a Claude Code
    /// session onto usage credits — full power takes a consent box that names
    /// them (`Continue with …` under `<model> now uses usage credits`, the
    /// Usage credits dialog's `Continue with usage credits`; the approval
    /// policy's `credits_consent`, `credits-consent@v1`), and a model bucket
    /// that asks for that consent is continued into it (the turn-end policy's
    /// `consent-accept@v1`). Claude Code saves a model's consent (`Continue
    /// with <M>`) for good per model and account, so later requests on the
    /// model go to credits with no box; the Usage credits dialog's `Continue
    /// with usage credits` saves nothing and carries the session onto credits
    /// already on. `false`, the default: the session stays on the
    /// subscription — such a box is cancelled with Esc (`credits-decline@v1`),
    /// or goes to a person where no Esc is read, and the bucket gets what
    /// `continue = false` gives it (another account, the fallback model, the
    /// reset waited out); while credits carry the session with no box (`Now
    /// using usage credits`, `<M> is drawing from usage credits`) the policy
    /// types none of its own turns (the turn-end policy's `credits-hold@v1`)
    /// but one probe turn past the reset their notice named or at the hold's
    /// bound, which another window may still bill to credits; and no upgrade,
    /// fallback or return moves it onto a model the account's profile bills
    /// to credits, or onto Fable where the profile cannot say
    /// (`crate::harness::upgrade_models::credits_billed`). Off by
    /// default, so `true` is more power, held as [`Self::limit_reset`] is:
    /// [`Self::limit`] leaves it off and a misplaced table never turns it on.
    /// Nothing is ever BOUGHT either way (the approval policy's `buys`).
    pub usage_credits: bool,
    /// THE LOGIN LIFECYCLE (`[harness] logins`, `"renew"` or `"off"`;
    /// `docs/DESIGN-claude-accounts-v2-2026-10-15.md` §5): a session whose
    /// login is a roster row with an email (`accounts.toml`, `aterm harness
    /// accounts renew`) FOLLOWS ITS RENEWAL — relaunched on its own
    /// conversation under the renewed login at its next idle turn end, or at
    /// its lost login where a renewal already replaced it. A lost login no
    /// renewal replaced is signed in where it stands either way (`/login`
    /// once, the browser sign-in the owner's, its notice says so — the
    /// owner's ruling of 2026-10-19). Inert with no such row. Apart from
    /// [`Self::accounts`], so `accounts = "off"` keeps renewal working.
    /// `false` (`"off"`, its limit): no session follows a renewal.
    pub logins: bool,
    /// The machines `aterm harness accounts renew --due` renews on next, over
    /// SSH (`host` or `host:/abs/path/to/aterm`), in one sitting. Read by
    /// that command alone, never by a supervisor, nor by the run a sitting
    /// makes on another machine (`--here`); empty (the default) runs no
    /// `ssh`, and so does its limit.
    pub hosts: Vec<String>,
    /// Claude Code's model fallback: the model the agent is relaunched on at
    /// a model-bucket limit (`--model`, session-only — never Claude's
    /// `/model`, which saves the person's default), and back from at its
    /// reset; set, full power also switches Claude Code's model-refusal pause
    /// (`Session paused`, exactly its switch and its retry). No other
    /// vendor's box is ever answered with a model switch under it. `None`:
    /// wait for the reset instead, and leave the pause to a person.
    pub model_fallback: Option<String>,
    /// Type `/compact` when the worker stops on a full context.
    pub compact_on_context_wall: bool,
    /// Relaunch a Claude Code that crashed (its session record left behind,
    /// read as the exit is seen) on its own conversation, a Muse Code that
    /// crashed (its exit status as its shell recorded it, by Muse Code's own
    /// table) on its own session, an agy that crashed (its shell's exit status
    /// and the crash file its dead pid left) on its own conversation, a
    /// Gemini CLI that crashed (its shell's exit status, by its own table) on
    /// the one session of its project begun since it started, where its own
    /// resume would load every turn, and a codex-alab on the line aterm's
    /// copy of its own record validated; a graceful exit is someone's
    /// decision, a start Muse Code, agy or Gemini CLI refused is never
    /// retried, a Ctrl-Z'd agy is no exit, and a vendor `codex` is never
    /// relaunched. Its limit limits the stall's remedy too: an agent
    /// frozen past its bound is ended only where it is relaunched
    /// (`supervise/stall.rs`).
    pub relaunch: bool,
    /// How long an agent stands frozen (not reading its input, nobody's hand
    /// on it) before the supervisor restarts it (`restart if-stalled`) for its
    /// relaunch (`supervise/stall.rs`); `0` never. Its own limit, apart from
    /// [`Self::relaunch`]: the automatic end of a frozen worker can be
    /// switched off while crashes are still relaunched.
    pub stall_term_after_s: u32,
    /// Restart a frozen agent (`restart pid=<n> if-stalled`) once it has stood
    /// [`Self::stall_term_after_s`]. Off (the default): the same bound takes a
    /// warning instead (one line said and one record in the window's
    /// Messages, `supervise/stall.rs`), and the agent is left running for a
    /// person to end or wait on (owner, 2026-09-29).
    pub stall_term: bool,
    /// How much memory a supervised agent's process group may use (its
    /// physical footprint, as the window's host measures it) before aterm
    /// warns about it (one Messages row an episode, with the tab's own `claude
    /// --resume <id>`). The agent is restarted on its conversation at its next
    /// idle point only when the machine is in trouble (critical memory
    /// pressure, or warn while it swaps) and this agent holds the most memory,
    /// under [`Self::relaunch`]; else the person is handed its resume line
    /// (`supervise/memory.rs`). `auto` (the default): an eighth of physical
    /// memory plus half the memory available now; a number of GiB; `off` (its
    /// limit): no warning and no restart.
    pub memory_ceiling: MemoryCeiling,
    /// Restart an idle agent onto a newer installed build of itself.
    pub upgrade: bool,
    /// The live upgrade asks first: type a notice asking the agent to get to a
    /// stopping point and confirm readiness in ordinary language, and restart
    /// only on that answer. Off (the default): nothing is typed before the
    /// move; a session is restarted onto the newer build only at a point where it is
    /// genuinely idle (its turn over, nothing running under it, no background
    /// work), and until then its tab only says that a newer build is installed
    /// and that it moves over when idle (owner, 2026-09-29).
    pub upgrade_ask: bool,
    /// Seconds the supervisor keeps its hands off a session after a person's
    /// keystroke into it — all but the folder-trust dialog of a launch the
    /// server witnessed (`approval_loop.rs`, THE LAUNCH).
    pub human_grace_s: u32,
    /// Safe rules a retired key switched off ([`RETIRED_KEYS`]); no key of
    /// its own sets it.
    pub withheld: Withheld,
}

/// A LOCAL CLOCK TIME, `HH:MM` in 24-hour time (`[harness] land_at`,
/// [`SupervisorConfig::land_at`]): the minute of the day a timed Cruise is
/// offered to land at. Which instant it names is the window's to resolve
/// when a person picks it (today's, else tomorrow's), never this reader's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LandAt {
    minute: u16,
}

impl LandAt {
    /// `HH:MM` with two digits each, `00:00` to `23:59`, surrounding blanks
    /// ignored; `None` for anything else (`7:00`, `24:00`, `18:60`, `18h`).
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
    pub fn minute(self) -> u16 {
        self.minute
    }

    /// `HH:MM`, as [`Self::parse`] takes it back.
    #[must_use]
    pub fn text(self) -> String {
        format!("{:02}:{:02}", self.minute / 60, self.minute % 60)
    }
}

/// The two safe rules only a retired key can take away ([`RETIRED_KEYS`]),
/// each read under `approve = "safe"`, the level the same key caps `approve`
/// at: `rm_breaker = false` leaves the rm circuit breaker no scratch root,
/// and `read_outside_cwd = false` leaves a Read box no root, so either box
/// goes to the owner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Withheld {
    /// `rm_breaker = false`: an rm is never proven under a scratch root.
    pub rm_breaker: bool,
    /// `read_outside_cwd = false`: a Read box is never proven.
    pub read_outside_cwd: bool,
}

impl Withheld {
    /// Each rule under the retired key that withholds it, as a place to read
    /// or set: the ONE list of them, which both
    /// [`SupervisorConfig::withheld_texts`] and [`SupervisorConfig::rebuild`]
    /// go through, so the two cannot name a rule differently. The destructure
    /// names every field: a rule added without its key here does not build.
    fn rules_mut(&mut self) -> [(&'static str, &mut bool); 2] {
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

/// [`SupervisorConfig::policy_texts`]: the whole policy as the texts
/// [`SupervisorConfig::set`] takes back, for a tape's header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyTexts {
    /// [`SupervisorConfig::texts`]: every key of [`KEYS`], in its order.
    pub keys: Vec<(String, String)>,
    /// [`SupervisorConfig::agent_texts`]: `<agent>.enabled` per
    /// supervisable program.
    pub agents: Vec<(String, String)>,
    /// [`SupervisorConfig::withheld_texts`]: each withheld rule's retired
    /// key, `false`.
    pub withheld: Vec<(String, String)>,
}

/// [`SupervisorConfig::rebuild`]'s answer: the policy, and what of it the
/// texts did not say — what THIS build chose for a run another build wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebuilt {
    pub policy: SupervisorConfig,
    /// Each key of [`KEYS`] and each agent's switch the texts did not name
    /// (texts from a build before it), left at its full-power default.
    pub supplied: Vec<String>,
    /// For each key the texts named that this build has retired, the note
    /// [`SupervisorConfig::set`] gave as it read it.
    pub retired: Vec<String>,
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
            continue_text: "keep going".to_string(),
            continue_per_hour: 0,
            rules_file: None,
            retry_api_errors: true,
            probe_api: true,
            resume_limits: true,
            limit_wait: true,
            autocompact_offer: Some(AUTOCOMPACT_DEFAULT_K),
            accounts: Mode::Early,
            limit_reset: false,
            usage_tap: false,
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
            withheld: Withheld::default(),
        }
    }
}

impl SupervisorConfig {
    /// Set one key from its value's text: an on/off ([`switch_word`]) for a
    /// switch, a decimal for a count, the string for a text, a path or
    /// `approve`, and for `trust_roots` the array's elements joined by `,`. A
    /// value that is not the key's own: an explicit off ([`spells_off`]) is
    /// that key's limiting value ([`Self::limit`]); anything else leaves the
    /// key at its default; both are said in the `Err`. An unknown key changes
    /// nothing and says so (owner, 2026-09-25).
    ///
    /// `<agent>.<key>` is a key of the `[harness.<agent>]` table, `<agent>` a
    /// supervisable program's own name: `claude.enabled` and `codex.enabled`
    /// are switches whose `false` switches that agent off ([`Self::off`]) and
    /// whose `true` changes nothing; any other key there is refused.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let v = value.trim();
        // An off limits a key it is not the key's own value for, except a
        // count, where "off" could as well mean "no cap" or "no grace" (more
        // power): there it is no setting either.
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
            "approve" => match Approve::parse(value) {
                Some(a) => self.approve = a,
                None => return bad(self, "all, safe or none"),
            },
            "autopilot" => match Autopilot::parse(value) {
                Some(a) => self.autopilot = a,
                None => return bad(self, "cruise, land or off"),
            },
            // A time, or none: empty (the default's text) and an off spelling
            // offer no time. It neither limits nor grants, so a value it
            // cannot read is no setting, never a limit.
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
            // An off (`false`, `"none"`, `""`) is no path and no model: the
            // fallback goes, never a model named "false". An on-word names no
            // model or file either: no setting, never a model named "true".
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

    /// A retired key ([`RETIRED_KEYS`]) read as what 0.93.0 made it mean, and
    /// the note that says so. `limits` (written as an off, [`retired_reading`];
    /// a value that is no on or off is no setting and never reaches here)
    /// applies the limit it named:
    ///
    /// * `approve_all`: `approve` at most `safe` (its rules answer);
    /// * `auto_reads`: `approve = "none"` (not even a read is answered);
    /// * `rm_breaker`, `read_outside_cwd`, `trust_dialog`: `approve` at most
    ///   `safe`, with that rule's roots taken away ([`Withheld`];
    ///   `trust_roots` emptied).
    ///
    /// `true` asked for what is the default: nothing changes. `rate_nudge`
    /// changes nothing either way: no supervisor answers Codex's nudge.
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

    /// Set `key` to its limiting value: the least power it can give. A switch
    /// is off, `approve` is `none`, the autopilot is `off`, a cap is one an
    /// hour, the grace is an hour, a frozen agent is never ended
    /// (`stall_term_after_s = 0`), no session is moved between accounts
    /// (`accounts = "off"`), no limit reset is taken (`limit_reset = false`,
    /// its default), no line carries the usage tap (`usage_tap = false`, its
    /// default), no record of the one account's usage is kept (`usage_notes =
    /// false`, its default), no session is carried onto usage credits
    /// (`usage_credits = false`, its default), an agent's own switch (`codex.enabled`) is that
    /// agent off. A text and `question_priority` (HOW a question is answered)
    /// keep their defaults: neither carries power.
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
            // An opt-in: its least power is its default, no reset taken.
            "limit_reset" => self.limit_reset = false,
            // An opt-in too: its least power is its default, no tap added.
            "usage_tap" => self.usage_tap = false,
            // An opt-in too: no record and no line.
            "usage_notes" => self.usage_notes = false,
            // An opt-in too: the subscription, no session onto credits.
            "usage_credits" => self.usage_credits = false,
            "model_fallback" => self.model_fallback = None,
            "compact_on_context_wall" => self.compact_on_context_wall = false,
            "relaunch" => self.relaunch = false,
            "stall_term_after_s" => self.stall_term_after_s = 0,
            "memory_ceiling" => self.memory_ceiling = MemoryCeiling::Off,
            "upgrade" => self.upgrade = false,
            // The three switches that restore a stop: the limit is the stop
            // (the upgrade asks first, the box goes to a person), except the
            // stall's, whose least power is no end at all.
            "upgrade_ask" => self.upgrade_ask = true,
            "rm_breaker_ask" => self.rm_breaker_ask = true,
            "stall_term" => self.stall_term = false,
            "human_grace_s" => self.human_grace_s = 3600,
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

    /// Tests: the full-power default with `key` alone moved off it — by its
    /// limit, else by the first of a few values that is the key's own (an
    /// opt-in's `true`, a text, the priority's other word, a path or list, a
    /// time). A key none of them moves fails here by name, so every test that
    /// walks [`KEYS`] through it takes a new key in by construction.
    #[cfg(test)]
    pub(crate) fn moved_off_default(key: &str) -> Self {
        let full = Self::default();
        let mut moved = full.clone();
        moved.limit(key);
        for value in ["true", "long-term", "~/flip/root", "18:00"] {
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

    /// Whether `agent`'s sessions are supervised: the master switch on and
    /// that agent not switched off by its own table ([`Self::off`]).
    #[must_use]
    pub fn on_for(&self, agent: Program) -> bool {
        self.enabled && !self.off.contains(&agent)
    }

    /// Switch `agent` off ([`Self::off`]), keeping the list one entry per
    /// agent in name order, so two files that switch off the same agents read
    /// as the same policy whichever table comes first.
    fn switch_off(&mut self, agent: Program) {
        if !self.off.contains(&agent) {
            self.off.push(agent);
            self.off.sort_by_key(|p| p.name());
        }
    }

    /// A session's Autopilot ([`Autopilot`]): its own `autopilot` word
    /// (`word`, read by key off the bare `meta` reply; `None` when the reply
    /// has none, it is unset, or it is no word [`Autopilot::parse`] reads),
    /// else this table's default ([`Self::autopilot`]). The one resolver:
    /// a session's `cruise` under a default of `off` is the owner opting that
    /// session in by hand. The master switch is not read here: with
    /// `enabled = false` no session is supervised at all, so no loop asks.
    #[must_use]
    pub fn autopilot_for(&self, word: Option<Autopilot>) -> Autopilot {
        word.unwrap_or(self.autopilot)
    }

    /// The reader of aterm.toml's `[harness]` table (module docs): the policy
    /// and one line per key it refused, could not take, or found outside the
    /// root table. `text` is the whole file.
    ///
    /// The root `[harness]` is applied first and every `harness` table TOML
    /// files under another one after it, each of whose keys may only limit
    /// (`Self::limits`), so a misplaced key never undoes the root table's
    /// limit, whichever comes first in the file.
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
                // A retired key after every current one: the limit it names
                // holds whatever `approve` says beside it.
                let (retired, current): (Vec<_>, Vec<_>) = root
                    .map(entries)
                    .unwrap_or_default()
                    .into_iter()
                    .partition(|(key, _)| RETIRED_KEYS.contains(&key.as_str()));
                // `disk` under [harness] is the disk reader's, not an unknown
                // harness key: a `[disk]` knob TOML files there turns every
                // automatic removal off ([`crate::harness::cli::disk_faults`]),
                // so the reader's own note stands in for "not a harness key,
                // ignored", which would invite the deletion that turns removal
                // back on.
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
                // `harness = false` filed under another table: the master
                // switch written off, which limits wherever it was written.
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
                // A retired key after every current one, as above.
                let (retired, current): (Vec<_>, Vec<_>) =
                    assignments(text).into_iter().partition(|(path, _)| {
                        path.last().is_some_and(|key| RETIRED_KEYS.contains(key))
                    });
                for (path, value) in current.into_iter().chain(retired) {
                    // `harness = false`: the master switch written off; at the
                    // root the key `enabled`, said once (a later `enabled =
                    // true` is then a repeat that may only limit); under
                    // another header a misplaced one, which limits. A key of
                    // an agent's table is `<agent>.<key>`, as `set` reads it.
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
                    let list = matches!(key, "trust_roots" | "hosts");
                    let refused = if value.trim_start().starts_with('[') && !list {
                        Some(no_shape(key, "array"))
                    } else {
                        cfg.set_scalar(key, &one_value, quoted || list).err()
                    };
                    // A key said twice — which may be why the file is not
                    // TOML — or said under another header may only limit.
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

    /// [`Self::from_aterm_toml`] over the file at `path`: no path or no file
    /// is the defaults; a file that exists and cannot be read is the defaults
    /// and a note saying so.
    #[must_use]
    pub fn from_path(path: Option<&Path>) -> (Self, Vec<String>) {
        let Some(path) = path else {
            return (Self::default(), Vec::new());
        };
        // Bytes, decoded lossily: a stray non-UTF-8 byte in a comment must not
        // hide the explicit off beside it (the file is then read line by line).
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

    /// [`Self::set`] from a parsed TOML value: a value of a shape no key takes
    /// (a table, a float, a date) is no setting and changes nothing, as any
    /// value that is neither the key's own nor an off (owner, 2026-09-25).
    /// An agent's table arrives key by key, as `<agent>.<key>` ([`entries`]).
    fn set_value(&mut self, key: &str, value: &aterm_toml::Value) -> Result<(), String> {
        use aterm_toml::Value;
        match value {
            Value::String(text) => self.set_scalar(key, text, true),
            Value::Boolean(_) | Value::Integer(_) => {
                self.set_scalar(key, &scalar_text(value).unwrap_or_default(), false)
            }
            // An array is `trust_roots`' shape alone.
            Value::Array(_) if matches!(key, "trust_roots" | "hosts") => match scalar_text(value) {
                Some(text) => self.set(key, &text),
                None => Err(no_shape(key, value.type_str())),
            },
            _ if RETIRED_KEYS.contains(&key) => Err(retired_no_setting(key)),
            _ if KEYS.contains(&key) => Err(no_shape(key, value.type_str())),
            _ => match agent_key(key) {
                Some((_, "enabled")) => Err(no_shape(key, value.type_str())),
                Some(_) => Err(per_agent_only(key)),
                None => Err(unknown_key(key)),
            },
        }
    }

    /// [`Self::set`] for a one-value text whose TOML shape is known: `quoted`
    /// (a string) or a bare boolean or number. A bare boolean or number is no
    /// text (`continue_text = false` is not the word "false" typed into a
    /// worker), while a quoted string is always one, `"yes"` included.
    fn set_scalar(&mut self, key: &str, text: &str, quoted: bool) -> Result<(), String> {
        if !quoted && matches!(key, "answer_text" | "continue_text") {
            return Err(format!(
                "harness.{key}: expected a text, got a bare {text} — no setting, so {key} keeps \
                 its default"
            ));
        }
        self.set(key, text)
    }

    /// Whether `self` is `before` with power taken away and none given: some
    /// field moved, and no switch is on that was off, no agent back on, no
    /// autopilot higher, no approval looser, no cap higher or absent, no grace
    /// or stall bound shorter, no account mode wider, no trust root or
    /// fallback model new, and no text or question priority changed (neither
    /// limits nor grants, so where a key may only limit, neither is taken).
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
            && (self.withheld.rm_breaker || !before.withheld.rm_breaker)
            && (self.withheld.read_outside_cwd || !before.withheld.read_outside_cwd)
    }

    /// Every key's value, in [`KEYS`] order, as the text [`Self::set`] takes
    /// back. The destructure names every field, so a field added without a
    /// key here does not build (and `texts_are_the_keys` holds the order).
    /// Settings ▸ Harness lists it, each value against the full-power
    /// default's, as the effective `[harness]` table.
    #[must_use]
    pub fn texts(&self) -> [(&'static str, String); 36] {
        let Self {
            enabled,
            // No key in [`KEYS`]: the agents' own tables ([`Self::agent_texts`];
            // `against_default` names them).
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
            // No key of its own: a retired key's ([`Self::withheld_texts`];
            // `against_default` names it).
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
        ]
    }

    /// EACH AGENT'S OWN SWITCH as its `[harness.<agent>]` table spells it —
    /// `(<agent>.enabled, true|false)`, for every supervisable program in
    /// [`Program::ALL`]'s order, so a new agent has its row with no list
    /// here: the key [`Self::set`] takes back, no key of [`KEYS`]
    /// ([`Self::texts`] leaves the tables out, and `against_default` names
    /// one off as `<agent>: off`). Settings ▸ Harness lists them beside
    /// [`Self::texts`] in the effective table.
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

    /// EACH SAFE RULE A RETIRED KEY TOOK AWAY, under the key that took it
    /// (`rm_breaker`, `false`) — none where none did ([`Withheld`]): what
    /// it withholds beyond the `approve` cap it also sets has no key of
    /// [`KEYS`] to show it by.
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

    /// THE POLICY AS TEXT, WHOLE: [`Self::texts`], [`Self::agent_texts`] and
    /// [`Self::withheld_texts`]. What a tape's header writes of the policy a loop ran
    /// under, so a replay — on this build or a later one — rebuilds it through
    /// the same [`Self::set`] the owner's file goes through
    /// ([`Self::rebuild`]), never through a second encoding of the fields
    /// that could drift from the reader's.
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

    /// The policy [`Self::policy_texts`] wrote — on this build or an older
    /// one — rebuilt the way the owner's file is read: every text through
    /// [`Self::set`], applied to the full-power default.
    ///
    /// A key of [`KEYS`] or an agent's switch the texts do not name is a
    /// key the writing build did not have yet: it keeps its full-power
    /// default, exactly as an owner's file written before the key is read
    /// (`[harness]`'s first rule, a key missing is its default), and is named
    /// in [`Rebuilt::supplied`] so whoever reads the replay knows this build,
    /// not the run, chose it. Keys are added every week; refusing every
    /// older tape at each one would make a newer build unable to replay any
    /// run it did not record. A key the texts name that this build has
    /// retired ([`RETIRED_KEYS`]) is read as `set` reads it in an owner's
    /// file — after every live key, so the cap it may set holds over the
    /// written `approve` — and its note kept in [`Rebuilt::retired`]. Any
    /// other key is refused by name: texts from a NEWER build, which this
    /// one cannot represent and must not read as a default.
    ///
    /// The withheld rules are NOT replayed through `set`: their retired key
    /// also caps `approve` at `safe`, which the written `approve` already
    /// says, and a policy whose `approve` was put back above the cap
    /// afterwards (as a test builds one) would come back capped. No live
    /// key's `set` touches another's field, so the order is the texts' own.
    ///
    /// # Errors
    ///
    /// A key said twice or unknown to this build, a value [`Self::set`]
    /// refuses, or a withheld rule this build does not have — each named.
    pub fn rebuild(texts: &PolicyTexts) -> Result<Rebuilt, String> {
        let PolicyTexts {
            keys,
            agents,
            withheld,
        } = texts;
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
                    cfg.set(key, value)
                        .map_err(|e| format!("{e} (as the policy wrote it)"))?;
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
            // `set` answers a retired key with the note that says what it now
            // means (and applies that reading); it never takes one silently.
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
        })
    }

    /// `key`'s full-power default as Settings and the help show it (`true`,
    /// `all`, `[]`, `0 (no cap)`), or `None` for a key with no default value
    /// (`rules_file`) or no such key. An agent's switch (`codex.enabled`) shows
    /// `true`.
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
            // No time offered.
            "land_at" if text.is_empty() => Some("none".to_string()),
            _ if text.is_empty() => None,
            "continue_per_hour" if full.continue_per_hour == 0 => Some(format!("{text} (no cap)")),
            "stall_term_after_s" => Some(format!("{text} (0: never)")),
            _ => Some(text),
        }
    }

    /// What this policy writes against the full-power default, key by key in
    /// [`KEYS`] order. A key whose value takes power away (by the same measure
    /// a misplaced key is held to, [`Self::limits`]) is a limit; any other
    /// change (a text, a question priority, a longer list of trust roots,
    /// another fallback model, a shorter grace) is the owner's own value,
    /// which neither limits nor grants. `trust_roots` is read only under
    /// `approve = "safe"`, the one level that consults it. After the keys,
    /// each agent switched off by its own table, as `<agent>: off`.
    #[must_use]
    pub fn against_default(&self) -> Written {
        let full = Self::default();
        let mut written = Written::default();
        for ((key, value), (_, default)) in self.texts().into_iter().zip(full.texts()) {
            if value == default || key == "trust_roots" && self.approve != Approve::Safe {
                continue;
            }
            // `self` with only this key back at its default: whether the move
            // from there to `self` limits is whether this key's value does.
            let mut at_default = self.clone();
            if at_default.set(key, &default).is_err() || !self.limits(&at_default) {
                written.own.push(key);
                continue;
            }
            let shown = match value.as_str() {
                "false" | "" => "off".to_string(),
                _ if key == "trust_roots" => self.trust_roots.join(", "),
                _ if key == "hosts" => self.hosts.join(", "),
                _ => value,
            };
            written.limits.push(format!("{key}: {shown}"));
        }
        // A rule only a retired key takes away, under the name it was written.
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
    /// Each key that takes power away, as `<key>: <value>` (`approve: safe`,
    /// `continue: off`, `continue_per_hour: 6`).
    pub limits: Vec<String>,
    /// Each key changed without taking power away: the owner's own text or
    /// choice (`answer_text`, `question_priority`, `rules_file`).
    pub own: Vec<&'static str>,
}

/// The note for a key that is not a `[harness]` key.
fn unknown_key(key: &str) -> String {
    format!(
        "harness.{key}: {UNKNOWN_KEY}, ignored (known: {})",
        KEYS.join(", ")
    )
}

/// The note for a key of an agent's table other than its switch: refused in
/// the unknown key's words ([`is_unknown_key_note`]), with where it goes.
fn per_agent_only(key: &str) -> String {
    format!(
        "harness.{key}: {UNKNOWN_KEY}, ignored — only `enabled` is per agent; write it under \
         `[harness]`"
    )
}

/// The program with a harness table ([`Program::harness_table`]) `id` is the
/// exact name of (`claude`, `codex`: [`Program::name`]), whose
/// `[harness.<id>]` table is that agent's; `None`
/// for any other word, another spelling of one (`claude-code`, `Codex`)
/// included.
fn agent_named(id: &str) -> Option<Program> {
    aterm_phase::program_of(id).filter(|p| p.name() == id && p.harness_table())
}

/// A key of an agent's table, `<agent>.<key>` ([`agent_named`]): the agent
/// and the key under it.
fn agent_key(key: &str) -> Option<(Program, &str)> {
    let (id, leaf) = key.split_once('.')?;
    Some((agent_named(id)?, leaf))
}

/// The words every unknown-key note carries ([`is_unknown_key_note`]).
const UNKNOWN_KEY: &str = "not a harness key";

/// The words every retired-key note carries ([`is_retired_key_note`]).
const RETIRED_KEY: &str = "a retired harness key";

/// Where 0.93.0's retired keys were last current.
const RETIRED_IN: &str = "0.93.0";

/// Where the retired `key` was last current ([`RETIRED_KEYS`]).
fn retired_in(key: &str) -> &'static str {
    match key {
        "rate_nudge" => "0.105.0",
        _ => RETIRED_IN,
    }
}

/// Whether the retired `[harness] <key>` written `value` still LIMITS the
/// supervisor ([`SupervisorConfig::retired`]): one of 0.93.0's approval
/// switches written off. `rate_nudge`, the retired Codex harness's, limits
/// nothing whatever it is written; nor does any key written on, or no on or
/// off at all.
#[must_use]
pub fn retired_still_limits(key: &str, value: Option<bool>) -> bool {
    RETIRED_KEYS.contains(&key) && key != "rate_nudge" && value == Some(false)
}

/// Whether `note` (one of [`SupervisorConfig::from_aterm_toml`]'s) is a
/// retired key's ([`RETIRED_KEYS`]), which the window's config language says
/// itself, in the same words ([`retired_key_note`]).
#[must_use]
pub fn is_retired_key_note(note: &str) -> bool {
    note.contains(RETIRED_KEY)
}

/// What the reader says of the retired `[harness] <key>` written `value`
/// (`None`: a value that is no switch, which changes nothing): the words the
/// window's config language shows on that line.
#[must_use]
pub fn retired_key_note(key: &str, value: Option<bool>) -> Option<String> {
    RETIRED_KEYS.contains(&key).then(|| match value {
        Some(on) => SupervisorConfig::default().retired(key, !on),
        None => retired_no_setting(key),
    })
}

/// [`retired_key_note`] from the value's one-value text (`None`: a shape that
/// has none, such as a table, an array, a float), read by the reader's own
/// rule ([`retired_reading`]) so the config language and the reader never
/// disagree on what the line did.
#[must_use]
pub fn retired_key_note_text(key: &str, text: Option<&str>) -> Option<String> {
    retired_key_note(key, text.and_then(retired_reading))
}

/// A retired key's value as an on or an off: an on-word ([`switch_word`]) is
/// on, any off spelling ([`spells_off`], `"disabled"` too, as for every key)
/// is off, and anything else is no setting.
#[must_use]
pub fn retired_reading(value: &str) -> Option<bool> {
    if spells_off(value) {
        Some(false)
    } else {
        switch_word(value)
    }
}

/// The note for a retired key whose value is no on or off.
fn retired_no_setting(key: &str) -> String {
    format!(
        "harness.{key}: {RETIRED_KEY} ({}), and its value is no on or off — no setting, so it \
         changes nothing; remove it",
        retired_in(key)
    )
}

/// The note for a known key whose value is of a shape it does not take.
fn no_shape(key: &str, shape: &str) -> String {
    format!(
        "harness.{key}: a TOML {shape} is not a value it takes — no setting, so it changes nothing"
    )
}

/// A TOML value's text when it is one value — a boolean, an integer or a
/// string — and `None` for every other shape (an array is no switch).
fn one_value_text(v: &aterm_toml::Value) -> Option<String> {
    use aterm_toml::Value;
    match v {
        Value::Boolean(_) | Value::Integer(_) | Value::String(_) => scalar_text(v),
        _ => None,
    }
}

/// Every non-table `harness` value in `v` below its root, with the dotted
/// path of the table that holds it (`[x]` then `harness = false`).
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

/// A value's text as an explicit on or off: `true`/`false`, `yes`/`no`,
/// `on`/`off`, `1`/`0` (and TOML's other spellings of the integers 0 and 1:
/// `+0`, `0x1`, `0b0`…), in any case; `None` for anything else: no setting.
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

/// `[harness] autocompact_offer`'s default size, in kilo-tokens: the one
/// Claude Code 2.1.302 recommends (its offer's `defaultTokens`).
pub const AUTOCOMPACT_DEFAULT_K: u32 = 400;

/// An `autocompact_offer` value: `Some(None)` for an off, `Some(Some(k))` for
/// a size on Claude Code's ladder (`400k`, `400`, `400000`, `true` for the
/// default),
/// `None` for anything else.
fn autocompact_size(value: &str) -> Option<Option<u32>> {
    let v = value.trim().to_ascii_lowercase();
    if spells_off(&v) || v == "off" {
        return Some(None);
    }
    if switch_word(&v) == Some(true) {
        return Some(Some(AUTOCOMPACT_DEFAULT_K));
    }
    let n: u32 = v.strip_suffix('k').unwrap_or(&v).parse().ok()?;
    // A count of tokens (`400000`) is the same size.
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

/// Whether a value's text says off for any key: a switch's off
/// ([`switch_word`]), or `none` / `never` / `disabled`, the one kind of value
/// that limits a key it is not the key's own value for (only an explicit
/// setting takes power away) (owner, 2026-09-25).
#[must_use]
pub fn spells_off(value: &str) -> bool {
    switch_word(value) == Some(false)
        || matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "none" | "never" | "disabled"
        )
}

/// Whether `note` (one of [`SupervisorConfig::from_aterm_toml`]'s) says a key
/// is not a `[harness]` key, which a reader that already names every unknown
/// key of the file (the window's config language) leaves out.
#[must_use]
pub fn is_unknown_key_note(note: &str) -> bool {
    note.contains(UNKNOWN_KEY)
}

/// What a `harness.<key>` TOML files under `at` did: limited, or nothing (a
/// key there may only limit), and why its value was refused if it was.
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

/// A `harness` table's entries as the keys [`SupervisorConfig::set`] reads:
/// each as written, but an agent's table (`[harness.codex]`, `codex = { … }`)
/// opened into its keys as `<agent>.<key>` ([`agent_named`]), so each of
/// those is set, held to the limit rule and said on its own, as a root key is.
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

/// Every `harness` table in `v` below its root, with the dotted path of the
/// table that holds it (`theme`, `keybind[0]`), at any depth, arrays of
/// tables included.
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

/// A TOML value as the text [`SupervisorConfig::set`] reads, or `None` for a
/// shape no key takes (a table, a float, a date).
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

/// Every one-line `key = value` of a file the TOML parser refuses, as its
/// whole key path and its value's text: [`SupervisorConfig::from_aterm_toml`]'s
/// reading of a file nothing can parse.
///
/// A `[a.b]` header names the table the keys below it belong to, a `[[a]]`
/// array-of-tables header names an element of `a` (the path segment `[]`, so
/// its own `enabled` is never `a.enabled`), and `k = v` is `<header>.<k>`.
/// Every one-line spelling TOML gives a key is read (dotted, spaces around a
/// dot allowed; quoted segments; one-line inline tables, `harness = { enabled
/// = false }`, flattened into their keys), cutting a line only where TOML
/// would ([`top_level`]), `#` comments ignored. A multi-line string
/// (`"""`/`'''`) or array is skipped whole, its lines never read as keys or
/// headers: a `[harness]` or an `enabled = false` quoted inside an
/// `answer_text` is no setting.
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
    // The table path the next keys belong to.
    let mut header: Vec<&str> = Vec::new();
    // Inside a multi-line string (its closing quotes) or array (its depth).
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

/// How many `[` a line leaves open, counted where TOML would: outside quoted
/// strings and after a `#` comment's cut.
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

/// A one-line TOML value's text as [`SupervisorConfig::set`] reads it: a
/// quoted string unquoted, an array's items unquoted and joined by `,`,
/// anything else as written.
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

/// A TOML key as its segments: cut at the dots [`top_level`] sees, each one
/// trimmed and stripped of one layer of basic or literal quotes, so
/// `"harness" . 'enabled'` is `[harness, enabled]` and `"a.b"` is one segment.
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

/// `s` cut at every `sep` TOML itself would see on one line: never inside a
/// quoted string (a basic string's `\"` escape included), never inside a
/// nested `{…}` or `[…]`.
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

    /// `autocompact_offer`: 400k by default (the size Claude Code
    /// recommends), any size on its ladder by any spelling, an off as `None`
    /// (its limit), and anything else no setting — the default kept.
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
        // Off the ladder, or no size at all: no setting.
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
        // Stops are warnings by default: the upgrade asks nothing first, a
        // frozen agent is warned about and left running, and an unproven rm
        // breaker under `safe` is approved with a warning, each one switch
        // away from the stop.
        assert!(!full.upgrade_ask && !full.stall_term && !full.rm_breaker_ask);
        let strict =
            read("[harness]\nupgrade_ask = true\nstall_term = true\nrm_breaker_ask = true\n");
        assert!(strict.upgrade_ask && strict.stall_term && strict.rm_breaker_ask);
        // And no limit reset is taken unasked.
        assert!(!full.limit_reset, "a limit reset is an opt-in");
        for key in KEYS {
            let mut c = full.clone();
            c.limit(key);
            // A text has no limit, and neither has how a question is answered
            // (`question_priority`): no power to take; nor `stall_term`, whose
            // default (the warning) is already its least, nor `limit_reset`,
            // whose default (no reset taken) is, nor `usage_tap` (no tap), nor
            // `usage_notes` (no record), nor `usage_credits` (the
            // subscription), nor `hosts`, whose default (no machine) is, nor
            // `land_at`, which only offers a time.
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
                    | "usage_tap"
                    | "usage_notes"
                    | "usage_credits"
                    | "hosts"
            );
            assert_eq!(c == full, text_key, "{key}: only a text has no limit");
            assert!(
                c == full || c.limits(&full),
                "{key}: a limit never adds power"
            );
        }
        // Negative control: the check sees power added.
        let mut c = full.clone();
        c.limit("enabled");
        assert!(!full.limits(&c));
    }

    /// Only an explicit setting takes power away: a value that is neither the
    /// key's own nor an off (a word it cannot read, a float, a table) keeps
    /// the key's default and is named, so `approve = "everything"` never stops
    /// every box being answered and `enabled = "yes"` never turns the harness
    /// off. An off, however it is spelled, is still the limit, and an unknown
    /// key changes nothing (owner, 2026-09-25).
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
        // No setting at all: every one of these is the full default.
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
        // The owner's explicit spellings, in any case, are read as written.
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
        // A quoted string is a text, whatever it says.
        assert_eq!(
            read("[harness]\ncontinue_text = \"yes\"\n").continue_text,
            "yes"
        );
        assert_eq!(read("[harness]\nanswer_text = \"1\"\n").answer_text, "1");
        // An off is no model and no path, never a model named "false"; the
        // whole table written off is the master switch, wherever it is filed.
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
        // A table under the root one is an unknown key.
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

    /// Every spelling TOML reads as `[harness] <key>` is that key here: an
    /// inline table, quoted or spaced keys, a quoted `"false"`.
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
        // Not the key: another table's, an array of tables, a near miss.
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

    /// `harness.<key>` written below another table's header (TOML files it
    /// under that table) still limits, and never grants: a kill switch works
    /// wherever in the file it was written, and a stray line never undoes the
    /// root table's limit, whichever comes first (the root table is applied
    /// first). Each such line is named.
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
        // A table that sorts before `harness` is still applied after it.
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
        // Negative controls: a misplaced key that would grant — a switch back
        // on, a new trust root, a text — changes nothing.
        let c = read(
            "[theme]\nharness.enabled = true\nharness.trust_roots = [\"/\"]\n\
             harness.continue_text = \"x\"\nharness.continue_per_hour = 0\n",
        );
        assert_eq!(c, SupervisorConfig::default());
        let c = read("[harness]\ncontinue_per_hour = 3\n[x]\nharness.continue_per_hour = 9\n");
        assert_eq!(c.continue_per_hour, 3, "a higher cap is not a limit");
    }

    /// A file the TOML parser refuses (a typo three tables away) is read line
    /// by line: every limit its `[harness]` lines write holds, in every
    /// one-line spelling, and nothing else changes. Said twice, a key may only
    /// limit, so one `false` beats a `true` in either order.
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
        // Negative controls: none of these is `[harness] enabled = false`, and
        // a refused file is no reason to take power either.
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
        // A byte-order mark in front of a refused file does not hide its
        // header, and a switch set ON there is ON.
        assert!(!read(&format!("\u{feff}[harness]\nenabled = false\n{TYPO}")).enabled);
        assert!(read(&format!("{TYPO}[harness]\nupgrade = true\n")).upgrade);
        // The whole table written off is off there too; a value that is no
        // setting is none there either (owner, 2026-09-25).
        assert!(!read(&format!("{TYPO}harness = false\n")).enabled);
        assert!(!read(&format!("{TYPO}[x]\nharness = false\n")).enabled);
        // The whole table off, then a `[harness]` saying `enabled = true` (a
        // redefinition, which is why TOML refuses the file): the later line
        // is a repeat of the master switch and may only limit.
        let redefined = "harness = false\n[harness]\nenabled = true\n";
        assert!(aterm_toml::from_str::<aterm_toml::Value>(redefined).is_err());
        assert!(!read(redefined).enabled);
        // A multi-line string or array is skipped whole: the `[harness]` and
        // the off quoted inside it are no setting.
        for text in [
            "[other]\nnote = \"\"\"\n[harness]\nenabled = false\n\"\"\"\n",
            "[other]\nnote = '''\nharness.approve = \"none\"\n'''\n",
            "[other]\nlist = [\n  \"[harness]\",\n]\nenabled = false\n",
        ] {
            let text = format!("{TYPO}{text}");
            let c = read(&text);
            assert!(c.enabled && c.approve == Approve::All, "{text:?}");
        }
        // A multi-line array is not taken, the lines after it still are.
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

    /// `[harness] accounts` (the accounts design, §3): three words, `early`
    /// the full-power default and `off` its limit; `wall` rotates less than
    /// `early`, so it limits too. A word it cannot read keeps the default, an
    /// off however spelled is `off`, and written below another table's
    /// header it may only limit. NEGATIVE CONTROLS: an on-word names no
    /// mode, and a misplaced `early` does not undo the root table's `wall`.
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
        // What it writes against the default: a limit, by its own word.
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
        // Misplaced: it limits, and never grants.
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

    /// `[harness] autopilot` (the Autopilot design, §2.1): three words and the
    /// owner's own for them, any case, `cruise` the full-power default and
    /// `off` its limit; `land` carries less than `cruise`, so it limits too.
    /// An off however spelled is `off` and said; a value that is no mode (an
    /// on-word, a number, a word it does not know) keeps `cruise` and is
    /// said. Written below another table's header it may only limit.
    /// NEGATIVE CONTROLS: an on-word names no mode, a misplaced `cruise` does
    /// not undo the root table's `land`, and the session's word decides over
    /// the table's in both directions ([`SupervisorConfig::autopilot_for`]).
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
        // The words, the owner's words, and what they read back as.
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
        // The table: its own words without a note; an off however spelled is
        // `off`, said; anything else keeps `cruise`, said.
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
        // Power only goes down the order: each step down is a limit, and
        // its limit is `off`.
        let land = read("[harness]\nautopilot = \"land\"\n");
        let off = read("[harness]\nautopilot = \"off\"\n");
        assert!(land.limits(&full) && off.limits(&full) && off.limits(&land));
        assert!(!full.limits(&land) && !land.limits(&off), "never up");
        let mut c = full.clone();
        c.limit("autopilot");
        assert_eq!(c, off);
        // What it writes against the default: a limit, by its own word.
        assert_eq!(land.against_default().limits, ["autopilot: land"]);
        assert_eq!(off.against_default().limits, ["autopilot: off"]);
        assert!(full.texts().contains(&("autopilot", "cruise".to_string())));
        // Misplaced: it limits, and never grants.
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
        // A file that is not TOML: read line by line, the same words.
        assert_eq!(
            read("font_px = \n[harness]\nautopilot = \"wrap-up\"\n").autopilot,
            Autopilot::Land
        );
        // The session's word decides over the table's, up or down; no word is
        // the table's.
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

    /// `[harness] limit_reset`: a limit reset is the owner's scarce purchase,
    /// and the rule that takes one has never been run against a real wall,
    /// so the key SHIPS `false` and only the root table's own `true` turns
    /// it on: no other key, no limit, no misplaced table and no repeat does.
    /// Its `true` is more power (the measure `stall_term` is held to), so it
    /// is the owner's own choice against the default, never a limit; an off
    /// however spelled, and a value that is no switch, leave it off. The
    /// reader takes the key as written, whatever `resume_limits` says: that
    /// gate is the loop's. NEGATIVE CONTROLS: the root table's `true` IS
    /// read (so every `false` above it is a reading, not a dead key), and a
    /// policy that has it on is limited by its `false`.
    #[test]
    fn limit_reset_ships_off_and_only_the_root_table_turns_it_on() {
        let full = SupervisorConfig::default();
        assert!(!full.limit_reset, "the default is off");
        assert!(!read("").limit_reset && !read("[harness]\n").limit_reset);
        assert_eq!(
            SupervisorConfig::default_shown("limit_reset").as_deref(),
            Some("false")
        );
        // No other key turns it on, written as an on, as its own default or
        // as its limit.
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
        // The root table's `true`, in every spelling of an on: that field
        // and no other.
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
        // An off, and a value that is no switch: off, the second one said.
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
        // The reader takes the key as written beside `resume_limits = false`
        // (the loop's gate), and the master switch off is still off.
        let c = read("[harness]\nlimit_reset = true\nresume_limits = false\n");
        assert!(c.limit_reset && !c.resume_limits);
        let c = read("[harness]\nlimit_reset = true\nenabled = false\n");
        assert!(c.limit_reset && !c.enabled);
        // Its `true` is more power: a limit takes it away and never gives it.
        assert!(!on.limits(&full), "on is no limit of off");
        assert!(full.limits(&on), "off limits on");
        let mut c = on.clone();
        c.limit("limit_reset");
        assert_eq!(c, full, "its limit is off");
        // So against the default it is the owner's own choice, never a limit,
        // and its text reads back.
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
        // Misplaced, it never grants; it still limits.
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
        // A file that is not TOML: the root section's first word is taken, a
        // repeat may only limit, and another header's grants nothing.
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

    /// `[harness] usage_notes` (accounts v2 S5): the record of a machine's one
    /// account's usage and the two digest lines it makes are a new alert, so
    /// the key SHIPS `false`; the root table's own `true` turns it on, its
    /// `true` is the owner's own choice against the default and never a
    /// limit, an off however spelled leaves it off, and a misplaced `true`
    /// grants nothing. NEGATIVE CONTROLS: the root table's `true` IS read, a
    /// misplaced `false` still limits, and no other key turns it on.
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

    /// `[harness] usage_tap`: its `true` changes the argv a relaunched
    /// Claude Code carries, so the key SHIPS `false`, only the root table's
    /// own `true` turns it on, and against the default it is the owner's own
    /// choice (`yours: usage_tap`), never a limit. NEGATIVE CONTROLS: the
    /// root table's `true` IS read, `accounts = "off"` beside it leaves it on
    /// (the tap is a recorder, independent of the accounts), a policy that has
    /// it on is limited by its `false`, and a misplaced `true` grants nothing.
    #[test]
    fn usage_tap_ships_off_and_only_the_root_table_turns_it_on() {
        let full = SupervisorConfig::default();
        assert!(!full.usage_tap, "the default is off");
        assert!(!read("").usage_tap && !read("[harness]\n").usage_tap);
        assert_eq!(
            SupervisorConfig::default_shown("usage_tap").as_deref(),
            Some("false")
        );
        for key in KEYS.iter().filter(|k| **k != "usage_tap") {
            let mut c = full.clone();
            let _ = c.set(key, "true");
            assert!(!c.usage_tap, "{key} = true");
            c.limit(key);
            assert!(!c.usage_tap, "{key} at its limit");
        }
        let on = SupervisorConfig {
            usage_tap: true,
            ..full.clone()
        };
        for text in ["true", "\"yes\"", "1"] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\nusage_tap = {text}\n"));
            assert_eq!(c, on, "{text}");
            assert!(notes.is_empty(), "{text}: {notes:?}");
        }
        for text in ["false", "\"no\"", "0"] {
            assert_eq!(read(&format!("[harness]\nusage_tap = {text}\n")), full);
        }
        let c = read("[harness]\nusage_tap = true\naccounts = \"off\"\n");
        assert!(c.usage_tap, "the accounts' switch does not silence the tap");
        assert!(!on.limits(&full), "on is no limit of off");
        assert!(full.limits(&on), "off limits on");
        let mut c = on.clone();
        c.limit("usage_tap");
        assert_eq!(c, full, "its limit is off");
        assert_eq!(
            on.against_default(),
            Written {
                limits: Vec::new(),
                own: vec!["usage_tap"],
            }
        );
        let (c, notes) = SupervisorConfig::from_aterm_toml("[theme]\nharness.usage_tap = true\n");
        assert_eq!(c, full, "a misplaced true grants nothing");
        assert!(notes[0].contains("`theme.harness.usage_tap`"), "{notes:?}");
        assert!(
            !read("[harness]\nusage_tap = true\n[theme]\nharness.usage_tap = false\n").usage_tap,
            "a misplaced off limits all the same"
        );
    }

    /// `[harness] usage_credits` (owner, 2026-10-19: *"for continuing on
    /// credits, that's an optional additional feature that's off by default.
    /// use subscriptions."*): the key SHIPS `false` and only the root
    /// table's own `true` turns it on: no other key, no limit, no misplaced
    /// table and no repeat does. Its `true` is more power, so it is the
    /// owner's own choice against the default, never a limit; an off however
    /// spelled, and a value that is no switch, leave it off. NEGATIVE
    /// CONTROLS: the root table's `true` IS read (so every `false` above it
    /// is a reading, not a dead key), and a policy that has it on is limited
    /// by its `false`.
    #[test]
    fn usage_credits_ships_off_and_only_the_root_table_turns_it_on() {
        let full = SupervisorConfig::default();
        assert!(!full.usage_credits, "the default is off");
        assert!(!read("").usage_credits && !read("[harness]\n").usage_credits);
        assert_eq!(
            SupervisorConfig::default_shown("usage_credits").as_deref(),
            Some("false")
        );
        // No other key turns it on, written as an on, as its own default or
        // as its limit.
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
        // The root table's `true`, in every spelling of an on: that field
        // and no other.
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
        // An off, and a value that is no switch: off, the second one said.
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
        // Its `true` is more power: a limit takes it away and never gives it.
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
        // Misplaced, it never grants; it still limits.
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
        // A file that is not TOML: the root section's first word is taken, a
        // repeat may only limit, and another header's grants nothing.
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

    /// THE AGENTS' OWN SWITCHES, as the effective table lists them: one per
    /// supervisable program, in `Program::ALL`'s order, each `true` at the
    /// default, and each text set back onto a default reproduces the policy.
    /// NEGATIVE CONTROL: one agent's switch Off changes only its own row.
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
        // A retired key's withheld rule is shown under the key that took it.
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
        // Each default's text, and each LIMITED value's text, set back onto
        // the default is that value: `against_default` probes with these.
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
        // Every key but the path shows a default; an unknown key shows none.
        for key in KEYS {
            assert_eq!(
                SupervisorConfig::default_shown(key).is_none(),
                *key == "rules_file",
                "{key}"
            );
        }
        assert_eq!(SupervisorConfig::default_shown("auto_reads"), None);
    }

    /// THE HEADER'S POLICY: [`SupervisorConfig::rebuild`] gives back exactly
    /// the policy [`SupervisorConfig::policy_texts`] wrote, with nothing
    /// supplied — the default, and the default with any ONE thing moved: each
    /// key to a value of its own that is not the default's
    /// ([`SupervisorConfig::moved_off_default`]), each agent switched off,
    /// and each rule a retired key withholds (alone: `approve` left above the
    /// cap that key would also set).
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
                }),
                "{texts:?}"
            );
        }
    }

    /// TEXTS AN OLDER BUILD WROTE: a key or an agent's switch that build did
    /// not have yet is not in them. Each comes back at its full-power
    /// default — as an owner's file written before the key reads — and is
    /// named as supplied, never refused: a newer build replays an older
    /// build's run. Every other key keeps what the texts said. (This build's
    /// `land_at` arrived after the first tapes were written, without them.)
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

    /// A KEY RETIRED SINCE the texts were written is read as this build reads
    /// it in an owner's file, after every live key: written `true`, it asked
    /// for the default and changes nothing; written `false`, it applies the
    /// limit it now names — capping the `approve` the texts wrote, whatever
    /// order they came in — and its note is kept.
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

    /// A policy text this build cannot read is refused by name, never read
    /// as the default it would otherwise leave: a key said twice, a key or an
    /// agent this build does not have (a newer build's), a value `set`
    /// refuses, a withheld rule written anything but `false` or unknown.
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
    fn against_default_names_each_limit_and_never_a_text_as_one() {
        let full = SupervisorConfig::default();
        assert_eq!(full.against_default(), Written::default(), "full power");
        // Every key's limiting value is named as a limit, alone. A key whose
        // limit is its default (a text, the list, the path) names nothing.
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
        // Negative control: a text, a path, another model and a shorter grace
        // change the policy but take no power away: the owner's own.
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
        // trust_roots is read only where `safe` consults it. None is the
        // default, so `[]` names nothing there, and a person's own folders
        // are the owner's own; under `all` neither.
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

    /// Owner decision D9: 0.93.0 shipped `approve_all`, `auto_reads`, `rm_breaker`,
    /// `read_outside_cwd` and `trust_dialog`. One written `false` is a limit
    /// the owner wrote, so it is read as the limit it named (never lifted as
    /// "not a harness key") and said so, with the key to write instead;
    /// written `true` it asked for the default and changes nothing. Negative
    /// control: a misspelling of one is an unknown key, and changes nothing.
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
        // A root written beside it, since the default holds none: the limit
        // takes away the roots the same file names.
        let (c, _) = one("[harness]
trust_roots = [\"~/src/app*\"]
trust_dialog = false
");
        assert_eq!((c.approve, c.trust_roots.len()), (Approve::Safe, 0));
        // Each limits, and only limits — written `false` or any off spelling;
        // a misplaced one still limits. (`rate_nudge` limits nothing:
        // `a_retired_codex_key_reads_and_changes_nothing`; nor does Settings
        // say it does.)
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
            // The word Settings ▸ Modified says of the row is the reader's.
            assert!(retired_still_limits(key, Some(false)), "{key}");
            assert!(!retired_still_limits(key, Some(true)), "{key}");
            assert!(!retired_still_limits(key, None), "{key}");
            // A value that is no on or off is no setting: it changes nothing
            // (owner, 2026-09-25).
            for text in [
                format!("[harness]\n{key} = \"maybe\"\n"),
                format!("[harness]\n{key} = 2.5\n"),
                format!("[harness]\n{key} = [false]\n"),
            ] {
                let (c, _) = one(&text);
                assert_eq!(c, full, "{text}");
            }
            assert!(retired_key_note(key, None).is_some(), "{key}");
            // Any off spelling limits — and the config language, reading the
            // same text, says the reader's own words for it.
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
        // A retired key never raises a level the table lowered further, and
        // its limit holds beside `approve`, written before it or after, in a
        // file that loads and in one read line by line.
        let c = read("[harness]\napprove = \"none\"\napprove_all = false\n");
        assert_eq!(c.approve, Approve::None);
        for text in [
            "[harness]\napprove_all = false\napprove = \"all\"\n",
            "[harness]\napprove = \"all\"\napprove_all = false\n",
            "[harness]\napprove_all = false\napprove = \"all\"\n[broken\n",
        ] {
            assert_eq!(read(text).approve, Approve::Safe, "{text}");
        }
        // Named where Settings names the policy.
        assert_eq!(
            read("[harness]\nrm_breaker = false\n")
                .against_default()
                .limits,
            ["approve: safe", "rm_breaker: off"]
        );
        // Negative control: a misspelling is no retired key.
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\napprove_al = false\n");
        assert_eq!(c, full);
        assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
        assert_eq!(retired_key_note("approve_al", Some(false)), None);
    }

    /// The Codex harness's `rate_nudge`, retired: a file that still names it
    /// reads, whatever its value, misplaced or not, and the key changes
    /// nothing — no limit, no power — and is said as retired (never "not a
    /// harness key"), in the words the config language shows on its line.
    /// Gone from [`KEYS`], the Settings rows and the `harness policy` texts.
    /// Negative control: a misspelling of it is an unknown key.
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

    /// `disk` under [harness] is the disk reader's: a `[disk]` knob TOML files
    /// there turns every automatic removal off, so its note is the reader's,
    /// never "not a harness key, ignored" (which invites the deletion that
    /// turns removal back on). Control: an unknown key beside it still is one.
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

    /// An `answer_questions` value the reader cannot take is no setting: the
    /// switch keeps its default (owner, 2026-09-25). Negative control: the
    /// switch spelled right, off, turns it off.
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
        // A stray non-UTF-8 byte hides no explicit off.
        std::fs::write(&toml, b"# caf\xe9\n[harness]\nenabled = false\n").expect("write");
        assert!(!SupervisorConfig::from_path(Some(&toml)).0.enabled);
        std::fs::write(&toml, b"# caf\xe9\n[harness]\nenabled = true\n").expect("write");
        assert!(SupervisorConfig::from_path(Some(&toml)).0.enabled);
        // A path that exists and cannot be read as a file: the defaults, said.
        let (c, notes) = SupervisorConfig::from_path(Some(&dir));
        assert_eq!(c, SupervisorConfig::default());
        assert!(!notes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `memory_ceiling`: `auto` by default; a number of GiB (a TOML integer or
    /// a text with a unit) or `off` taken. A value it cannot take is no
    /// setting: the default stands and the note says so (only an off-word
    /// reads as a key's limit, [`SupervisorConfig::set`]). Written below
    /// another table, `off` limits and a lower number never grants (a lower
    /// ceiling warns and restarts more) (owner, 2026-09-25).
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
        // Negative control: an off-word the key cannot take whole is still
        // its limit (`no`, which the parser does not read).
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\nmemory_ceiling = \"no\"\n");
        assert_eq!(c.memory_ceiling, MemoryCeiling::Off, "{notes:?}");
        // Misplaced: off limits; a lower number does not grant.
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
        // What the policy writes against the default.
        let w = read("[harness]\nmemory_ceiling = \"off\"\n").against_default();
        assert_eq!(w.limits, ["memory_ceiling: off"]);
        let w = read("[harness]\nmemory_ceiling = 48\n").against_default();
        assert_eq!(w.own, ["memory_ceiling"]);
        assert_eq!(
            SupervisorConfig::default_shown("memory_ceiling").as_deref(),
            Some("auto")
        );
    }

    /// `[harness] question_priority` is how, beside `answer_questions`'s
    /// whether (the key right after it in [`KEYS`]): its three words, trimmed
    /// and case folded, each the owner's own value (never a limit, never a
    /// grant); anything else (`long_term`, `ask`, which is a session's word for
    /// not answering, a switch's `true`, a float) is no setting and changes
    /// nothing, and one note says so. `recommended` by default
    /// (owner, 2026-09-25).
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
        // A misplaced priority is no limit: it changes nothing.
        let c = read("[theme]\nharness.question_priority = \"long-term\"\n");
        assert_eq!(c, full);
    }

    /// `[harness] land_at` (design §9.3.4) is a KNOWN key: the time the
    /// window offers a timed Cruise to land at, right after `autopilot` in
    /// [`KEYS`]. `HH:MM` from `00:00` to `23:59` is the owner's own value
    /// (never a limit, never a grant), listed in the effective table and read
    /// back by [`SupervisorConfig::set`]; empty and an off spelling offer no
    /// time (the default). Anything else keeps the default and ONE note says
    /// so in this key's own words — never `not a harness key`, which a CLI
    /// `supervise` would otherwise print for a key the window reads. A
    /// misplaced one changes nothing. Negative control: a key this reader
    /// does not know is still said to be no harness key.
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
        // A misplaced time offers nothing: neither a limit nor a grant.
        let c = read("[theme]\nharness.land_at = \"18:00\"\n");
        assert_eq!(c, full);
        // Negative control: an unknown key is still said to be one.
        let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nland_by = \"18:00\"\n");
        assert!(notes.iter().any(|n| is_unknown_key_note(n)), "{notes:?}");
        assert_eq!(LandAt::of_minute(1440), None);
        assert_eq!(
            LandAt::of_minute(61).map(LandAt::text).as_deref(),
            Some("01:01")
        );
    }

    /// The priority never turns answering on: `answer_questions = false` with
    /// `question_priority = long-term` answers no question, whichever key the
    /// file sets first, the priority kept as written. Negative control: with the switch on, the priority rides the
    /// hosted options as written.
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

    /// `[harness.<agent>] enabled = false` switches that agent's sessions off
    /// and nothing else: the master switch and the other agent stay on, in
    /// every spelling TOML reads as that table's key, and two agents off read
    /// as one policy whichever table comes first. `true` is accepted and
    /// changes nothing; the master switch off wins over it. An off however
    /// spelled is the agent's limit; a value that is no switch keeps it on
    /// and is said. NEGATIVE CONTROL: the default supervises both.
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

    /// Only `enabled` is per agent: any other key in an agent's table — a
    /// `[harness]` key or not — is refused in the unknown key's words, with
    /// where to write it, and changes nothing, while the `enabled` beside it
    /// holds. A table under any word that is not a supervisable program's
    /// exact name is an unknown key, as it always was, and so is an agent's
    /// name written as a value. NEGATIVE CONTROL: the same key under
    /// `[harness]` is read.
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

    /// Written below another table's header, an agent's table may only limit,
    /// like every `[harness]` key: its `false` switches the agent off all the
    /// same, and its `true` never undoes the root table's `false`, whichever
    /// comes first in the file. Each such key is named.
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

    /// Each key of an agent's table is said on its own, as a root key is: the
    /// note for a key that was read never rides with a refused one beside it,
    /// so a reader that leaves unknown-key notes out (the window's) still
    /// shows it, at the root and below another header; and the TOML reader
    /// and the line-by-line one say the same of each misplaced key.
    /// NEGATIVE CONTROL: the refused key's own note is an unknown key's.
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

    /// A file the TOML parser refuses is read line by line for an agent's
    /// table too, in every one-line spelling; said twice, its key may only
    /// limit, and another header's still limits. NEGATIVE CONTROLS: an agent
    /// switched on, a table for no agent, an array of tables, an off quoted
    /// in a text or a comment, and another key there change nothing.
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

    /// An agent switched off is a limit, by the measure every misplaced key is
    /// held to, and back on is none; against the default it is named as
    /// `<agent>: off`, after the keys. Its switch is also a key of
    /// `SupervisorConfig::set`, `limit` and `default_shown`, as
    /// `<agent>.enabled`.
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
