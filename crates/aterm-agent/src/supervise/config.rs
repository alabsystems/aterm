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
//!   is said to (owner, 2026-09-25).
//!
//! Stops are warnings by default: where the harness would halt, end or wait
//! on an agent on its own judgment, it says so and goes on. Each stop is one
//! opt-in switch away: `upgrade_ask` (the live upgrade's notice and READY
//! handshake), `stall_term` (a frozen agent restarted with `restart if-stalled`),
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
//! window's host, `aterm drive watch|supervise` and the hand-run `aterm
//! harness upgrade` all call it, so none of them can disagree on a key, a
//! default or a refusal.

use std::path::{Path, PathBuf};

use super::policy::approval::QuestionPriority;

use super::memory::MemoryCeiling;
use crate::harness::accounts::Mode;

/// Every `[harness]` key, in the order the help and Settings list them.
pub const KEYS: &[&str] = &[
    "enabled",
    "headless",
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
    "model_fallback",
    "rate_nudge",
    "compact_on_context_wall",
    "relaunch",
    "stall_term_after_s",
    "stall_term",
    "memory_ceiling",
    "upgrade",
    "upgrade_ask",
    "human_grace_s",
];

/// The `[harness]` keys 0.93.0 shipped and this reader retired: its four
/// approval switches, folded into [`KEYS`]' `approve`, and `approve_all`,
/// which `approve = "all"` is. Each still limits when written `false`
/// ([`SupervisorConfig::retired`]); none is a key a new file should write.
pub const RETIRED_KEYS: &[&str] = &[
    "approve_all",
    "auto_reads",
    "rm_breaker",
    "read_outside_cwd",
    "trust_dialog",
];

/// What the supervisor may approve on a permission box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approve {
    /// Every box: its one-shot allow (never a "don't ask again" option, which
    /// would rewrite the agent's own settings). The default.
    All,
    /// Only a box [`super::policy::approval::decide`] proves safe: a read-only
    /// command, a trust dialog under [`SupervisorConfig::trust_roots`], a read
    /// outside the secrets list, an rm under a scratch root.
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
    /// Supervise the sessions of a headless instance too.
    pub headless: bool,
    /// What a permission box may be answered with.
    pub approve: Approve,
    /// Folders whose trust dialog is answered under [`Approve::Safe`]. A
    /// leading `~/` is the home directory; a trailing `*` matches any suffix
    /// of the last component (`~/Developer/aterm*` is every aterm worktree).
    pub trust_roots: Vec<String>,
    /// Under `approve = "safe"`, hand an unproven rm circuit breaker to a
    /// person. Off (the default): the vendor's `rm` on a possibly-empty
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
    /// A session's own `questions` word decides over it
    /// ([`Self::session_questions`]). A value the reader cannot take changes
    /// nothing.
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
    /// Claude Code's model fallback: the model the agent is relaunched on at
    /// a model-bucket limit (`--model`, session-only — never Claude's
    /// `/model`, which saves the person's default), and back from at its
    /// reset; set, full power also switches Claude Code's model-refusal pause
    /// (`Session paused`, exactly its switch and its retry). No other
    /// vendor's box is ever answered with a model switch under it — Codex's
    /// rate-limit nudge is [`Self::rate_nudge`]'s. `None`: wait for the reset
    /// instead, and leave the pause to a person.
    pub model_fallback: Option<String>,
    /// Codex's rate-limit nudge (`Approaching rate limits`): answered by full
    /// power, its switch to the cheaper model only when Codex's own usage
    /// reading shows the window at 90% or more, and then only to save the
    /// work (commit and push, then stop) before the session is put back on its
    /// own model and held until the window resets; any other nudge is kept on
    /// its model. Off: the nudge is a person's.
    pub rate_nudge: bool,
    /// Type `/compact` when the worker stops on a full context.
    pub compact_on_context_wall: bool,
    /// Relaunch a Claude Code or a Codex that crashed (Claude Code's session
    /// record left behind, a Codex's exit status as its shell recorded it,
    /// read as the exit is seen), on its own conversation; a graceful exit is
    /// someone's decision. Its limit limits the stall's remedy too: an agent
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
    /// stopping point and answer `ATERM-UPGRADE-READY-…`, and restart only on
    /// that answer. Off (the default): nothing is typed before the move; a
    /// session is restarted onto the newer build only at a point where it is
    /// genuinely idle (its turn over, nothing running under it, no background
    /// work), and until then its tab only says that a newer build is installed
    /// and that it moves over when idle (owner, 2026-09-29).
    pub upgrade_ask: bool,
    /// Seconds the supervisor keeps its hands off a session after a person's
    /// keystroke into it.
    pub human_grace_s: u32,
    /// Safe rules a retired key switched off ([`RETIRED_KEYS`]); no key of
    /// its own sets it.
    pub withheld: Withheld,
    /// Whether a session's own `questions` word (`aterm ctl @<sid> meta set
    /// questions ask|recommended|long-term|short-term`, Owner-only, read by key
    /// off the bare `meta` reply) decides that session's questions over
    /// [`Self::answer_questions`] and [`Self::question_priority`] ("global, and
    /// then session"): a priority word answers its dialogs and its questions
    /// in prose by that priority, where the table's switch is off too; `ask`
    /// hands its dialogs to a person (how a controller takes a worker's
    /// questions, `aterm drive answer`). On by default; off under a loop's own
    /// `--no-answer`. Not a `[harness]` key (owner, 2026-09-23).
    pub session_questions: bool,
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

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            headless: true,
            approve: Approve::All,
            trust_roots: [
                "~/Developer/aterm*",
                "~/Developer/ay*",
                "~/Developer/trust*",
                "/private/tmp/claude-*",
            ]
            .map(String::from)
            .to_vec(),
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
            accounts: Mode::Early,
            limit_reset: false,
            model_fallback: Some("opus".to_string()),
            rate_nudge: true,
            compact_on_context_wall: true,
            relaunch: true,
            stall_term_after_s: 600,
            stall_term: false,
            memory_ceiling: MemoryCeiling::Auto,
            upgrade: true,
            upgrade_ask: false,
            human_grace_s: 120,
            withheld: Withheld::default(),
            session_questions: true,
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
            "rate_nudge" => switch!(rate_nudge),
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
            "accounts" => match Mode::parse(&v.to_ascii_lowercase()) {
                Some(mode) => self.accounts = mode,
                None => return bad(self, "early, wall or off"),
            },
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
    /// `true` asked for what is the default: nothing changes.
    fn retired(&mut self, key: &str, limits: bool) -> String {
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
    /// is off, `approve` is `none`, a cap is one an hour, the grace is an
    /// hour, a frozen agent is never ended (`stall_term_after_s = 0`), no
    /// session is moved between accounts (`accounts = "off"`), no limit reset
    /// is taken (`limit_reset = false`, its default). A text and
    /// `question_priority` (HOW a question is answered) keep their defaults:
    /// neither carries power.
    pub fn limit(&mut self, key: &str) {
        match key {
            "enabled" => self.enabled = false,
            "headless" => self.headless = false,
            "approve" => self.approve = Approve::None,
            "answer_questions" => self.answer_questions = false,
            "dismiss_surveys" => self.dismiss_surveys = false,
            "continue" => self.continue_policy = false,
            "continue_per_hour" => self.continue_per_hour = 1,
            "retry_api_errors" => self.retry_api_errors = false,
            "probe_api" => self.probe_api = false,
            "resume_limits" => self.resume_limits = false,
            "limit_wait" => self.limit_wait = false,
            "accounts" => self.accounts = Mode::Off,
            // An opt-in: its least power is its default, no reset taken.
            "limit_reset" => self.limit_reset = false,
            "model_fallback" => self.model_fallback = None,
            "rate_nudge" => self.rate_nudge = false,
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
            _ => {}
        }
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
                    .into_iter()
                    .flatten()
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
                    if let Err(e) = cfg.set_value(key, value) {
                        notes.push(e);
                    }
                }
                for (at, table) in misplaced {
                    for (key, value) in table {
                        let before = cfg.clone();
                        let refused = cfg.set_value(key, value).err();
                        let limits = cfg.limits(&before);
                        if !limits {
                            cfg = before;
                        }
                        notes.push(misplaced_note(&at, key, limits, refused));
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
                let mut seen = Vec::new();
                // A retired key after every current one, as above.
                let (retired, current): (Vec<_>, Vec<_>) =
                    assignments(text).into_iter().partition(|(path, _)| {
                        path.last().is_some_and(|key| RETIRED_KEYS.contains(key))
                    });
                for (path, value) in current.into_iter().chain(retired) {
                    // `harness = false`: the master switch written off; at the
                    // root the key `enabled`, said once (a later `enabled =
                    // true` is then a repeat that may only limit); under
                    // another header a misplaced one, which limits.
                    let (head, key, one_value): (&[&str], &str, String) = match path.as_slice() {
                        [head @ .., "harness"] if !value.trim_start().starts_with(['[', '{']) => {
                            if !spells_off(&unquote(value)) {
                                continue;
                            }
                            (head, "enabled", "false".to_string())
                        }
                        [head @ .., "harness", key] => (head, key, unquote(value)),
                        _ => continue,
                    };
                    let quoted = value.trim_start().starts_with(['"', '\'']);
                    let before = cfg.clone();
                    let refused = if value.trim_start().starts_with('[') && key != "trust_roots" {
                        Some(no_shape(key, "array"))
                    } else {
                        cfg.set_scalar(key, &one_value, quoted || key == "trust_roots")
                            .err()
                    };
                    // A key said twice — which may be why the file is not
                    // TOML — or said under another header may only limit.
                    let limits = cfg.limits(&before);
                    let repeat = head.is_empty() && seen.contains(&key);
                    if head.is_empty() {
                        seen.push(key);
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
    fn set_value(&mut self, key: &str, value: &aterm_toml::Value) -> Result<(), String> {
        use aterm_toml::Value;
        match value {
            Value::String(text) => self.set_scalar(key, text, true),
            Value::Boolean(_) | Value::Integer(_) => {
                self.set_scalar(key, &scalar_text(value).unwrap_or_default(), false)
            }
            // An array is `trust_roots`' shape alone.
            Value::Array(_) if key == "trust_roots" => match scalar_text(value) {
                Some(text) => self.set(key, &text),
                None => Err(no_shape(key, value.type_str())),
            },
            _ if RETIRED_KEYS.contains(&key) => Err(retired_no_setting(key)),
            _ if KEYS.contains(&key) => Err(no_shape(key, value.type_str())),
            _ => Err(unknown_key(key)),
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
    /// field moved, and no switch is on that was off, no approval looser, no
    /// cap higher or absent, no grace or stall bound shorter, no account mode
    /// wider, no trust root or fallback model new, and no text or question
    /// priority changed (neither limits nor grants, so where a key may only
    /// limit, neither is taken).
    fn limits(&self, before: &Self) -> bool {
        let cap = |n: u32| if n == 0 { u32::MAX } else { n };
        let no_more = |now: bool, then: bool| !now || then;
        self != before
            && no_more(self.enabled, before.enabled)
            && no_more(self.headless, before.headless)
            && self.approve.rank() <= before.approve.rank()
            && no_more(!self.rm_breaker_ask, !before.rm_breaker_ask)
            && self
                .trust_roots
                .iter()
                .all(|r| before.trust_roots.contains(r))
            && no_more(self.answer_questions, before.answer_questions)
            && no_more(self.session_questions, before.session_questions)
            && self.question_priority == before.question_priority
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
            && accounts_rank(self.accounts) <= accounts_rank(before.accounts)
            && no_more(self.limit_reset, before.limit_reset)
            && (self.model_fallback.is_none() || self.model_fallback == before.model_fallback)
            && no_more(self.rate_nudge, before.rate_nudge)
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
    fn texts(&self) -> [(&'static str, String); 29] {
        let Self {
            enabled,
            headless,
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
            accounts,
            limit_reset,
            model_fallback,
            rate_nudge,
            compact_on_context_wall,
            relaunch,
            stall_term_after_s,
            stall_term,
            memory_ceiling,
            upgrade,
            upgrade_ask,
            human_grace_s,
            // No key of its own: a retired key's (`against_default` names it).
            withheld: _,
            // No key: a session's own word, and a flag's limit on it.
            session_questions: _,
        } = self;
        let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
        let approve = match approve {
            Approve::All => "all",
            Approve::Safe => "safe",
            Approve::None => "none",
        };
        [
            ("enabled", enabled.to_string()),
            ("headless", headless.to_string()),
            ("approve", approve.to_string()),
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
            ("model_fallback", model_fallback.clone().unwrap_or_default()),
            ("rate_nudge", rate_nudge.to_string()),
            (
                "compact_on_context_wall",
                compact_on_context_wall.to_string(),
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

    /// `key`'s full-power default as Settings and the help show it (`true`,
    /// `all`, `[~/aterm*, …]`, `0 (no cap)`), or `None` for a key with no
    /// default value (`rules_file`) or no such key.
    #[must_use]
    pub fn default_shown(key: &str) -> Option<String> {
        let full = Self::default();
        let (_, text) = full.texts().into_iter().find(|(k, _)| *k == key)?;
        match key {
            _ if text.is_empty() => None,
            "trust_roots" => Some(format!("[{}]", full.trust_roots.join(", "))),
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
    /// `approve = "safe"`, the one level that consults it.
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
                _ => value,
            };
            written.limits.push(format!("{key}: {shown}"));
        }
        // A rule only a retired key takes away, under the name it was written.
        let Withheld {
            rm_breaker,
            read_outside_cwd,
        } = self.withheld;
        for (key, off) in [
            ("rm_breaker", rm_breaker),
            ("read_outside_cwd", read_outside_cwd),
        ] {
            if off {
                written.limits.push(format!("{key}: off"));
            }
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

/// The words every unknown-key note carries ([`is_unknown_key_note`]).
const UNKNOWN_KEY: &str = "not a harness key";

/// The words every retired-key note carries ([`is_retired_key_note`]).
const RETIRED_KEY: &str = "a retired harness key";

/// Where the retired keys were last current.
const RETIRED_IN: &str = "0.93.0";

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
        "harness.{key}: {RETIRED_KEY} ({RETIRED_IN}), and its value is no on or off — no \
         setting, so it changes nothing; remove it"
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
            // whose default (no reset taken) is.
            let text_key = matches!(
                *key,
                "trust_roots"
                    | "answer_text"
                    | "question_priority"
                    | "continue_text"
                    | "rules_file"
                    | "stall_term"
                    | "limit_reset"
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
        assert!(
            notes
                .iter()
                .any(|n| n.contains("approve:") && n.contains("keeps its default")),
            "{notes:?}"
        );
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
        assert!(
            !c.enabled && notes[0].contains("master switch is off"),
            "{notes:?}"
        );
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
        assert!(notes[0].contains("not a table"), "{notes:?}");
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
        assert!(
            notes.iter().any(|n| n.contains("`theme.harness.enabled`")
                && n.contains("limits all the same")),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("`theme.harness.relaunch`") && n.contains("changed nothing")),
            "{notes:?}"
        );
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
        assert!(notes[0].contains("not valid TOML"), "{notes:?}");
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
        let at = KEYS.iter().position(|k| *k == "accounts").expect("a key");
        assert_eq!(
            (KEYS[at - 1], KEYS[at + 1]),
            ("limit_wait", "limit_reset"),
            "among the other answers to a limit"
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
        let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\naccounts = false\n");
        assert!(
            notes[0].contains("expected early, wall or off") && notes[0].contains("its limit"),
            "{notes:?}"
        );
        let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\naccounts = \"sometimes\"\n");
        assert!(notes[0].contains("keeps its default"), "{notes:?}");
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
        let at = KEYS
            .iter()
            .position(|k| *k == "limit_reset")
            .expect("a key");
        assert_eq!(
            (KEYS[at - 1], KEYS[at + 1]),
            ("accounts", "model_fallback"),
            "after the accounts, before the model's answer to a limit"
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
            ("false", None),
            ("\"no\"", None),
            ("0", None),
            ("\"never\"", Some("its limit")),
            ("\"none\"", Some("its limit")),
            ("\"sometimes\"", Some("keeps its default")),
            ("3", Some("keeps its default")),
            ("2.5", Some("no setting")),
            ("[true]", Some("no setting")),
        ] {
            let (c, notes) =
                SupervisorConfig::from_aterm_toml(&format!("[harness]\nlimit_reset = {text}\n"));
            assert_eq!(c, full, "{text}");
            match said {
                None => assert!(notes.is_empty(), "{text}: {notes:?}"),
                Some(word) => assert!(
                    notes.len() == 1 && notes[0].contains(word),
                    "{text}: {notes:?}"
                ),
            }
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
            notes[0].contains("`theme.harness.limit_reset`")
                && notes[0].contains("changed nothing"),
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
        assert_eq!(
            SupervisorConfig::default_shown("approve").as_deref(),
            Some("all")
        );
        assert_eq!(
            SupervisorConfig::default_shown("continue_per_hour").as_deref(),
            Some("0 (no cap)")
        );
        assert_eq!(SupervisorConfig::default_shown("auto_reads"), None);
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
        // trust_roots is read only where `safe` consults it: fewer roots
        // there are a limit, more are the owner's own; under `all` neither.
        let roots = "trust_roots = [\"~/Developer/aterm*\"]\n";
        assert!(read(&format!("[harness]\n{roots}")).against_default() == Written::default());
        assert_eq!(
            read(&format!("[harness]\napprove = \"safe\"\n{roots}"))
                .against_default()
                .limits,
            ["approve: safe", "trust_roots: ~/Developer/aterm*"]
        );
        let more = "trust_roots = [\"~/Developer/aterm*\", \"~/Developer/ay*\", \
                    \"~/Developer/trust*\", \"/private/tmp/claude-*\", \"~/x\"]\n";
        let w = read(&format!("[harness]\napprove = \"safe\"\n{more}")).against_default();
        assert_eq!((w.limits.len(), w.own), (1, vec!["trust_roots"]));
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
        assert!(
            note.contains("read as the limit it named")
                && note.contains("write `approve = \"safe\"` instead"),
            "{note}"
        );
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
trust_dialog = false
");
        assert_eq!((c.approve, c.trust_roots.len()), (Approve::Safe, 0));
        // Each limits, and only limits — written `false` or any off spelling;
        // a misplaced one still limits.
        for key in RETIRED_KEYS {
            for text in [
                format!("[harness]\n{key} = false\n"),
                format!("[harness]\n{key} = \"no\"\n"),
                format!("[theme]\nharness.{key} = false\n"),
            ] {
                let c = read(&text);
                assert!(c.limits(&full), "{text}: {c:?}");
            }
            let (c, note) = one(&format!("[harness]\n{key} = true\n"));
            assert_eq!(c, full, "{key} = true changes nothing");
            assert!(note.contains("changes nothing"), "{note}");
            // A value that is no on or off is no setting: it changes nothing
            // (owner, 2026-09-25).
            for text in [
                format!("[harness]\n{key} = \"maybe\"\n"),
                format!("[harness]\n{key} = 2.5\n"),
                format!("[harness]\n{key} = [false]\n"),
            ] {
                let (c, note) = one(&text);
                assert_eq!(c, full, "{text}");
                assert!(note.contains("changes nothing"), "{note}");
            }
            assert!(
                retired_key_note(key, None).is_some_and(|n| n.contains("changes nothing")),
                "{key}"
            );
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
            assert!(
                retired_key_note_text(key, Some("maybe"))
                    .is_some_and(|n| n.contains("changes nothing")),
                "{key}"
            );
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

    /// A session's `questions` word keeps its say unless a loop's own flag
    /// takes it: an `answer_questions` value the reader cannot take is no
    /// setting, so the switch keeps its default and the sessions their word
    /// (owner, 2026-09-25).
    /// Negative control: the switch spelled right, on or off, or absent, or
    /// an unknown key, leaves the session's word its say too.
    #[test]
    fn a_misread_question_switch_changes_nothing() {
        for text in [
            "[harness]\nanswer_questions = \"sometimes\"\n",
            "[harness]\nanswer_questions = 2.5\n",
        ] {
            let c = read(text);
            assert!(c.answer_questions && c.session_questions, "{text}");
        }
        for text in [
            "[harness]\nanswer_questions = true\n",
            "[harness]\nanswer_questions = false\n",
            "[harness]\nanswer_question = true\n",
            "",
        ] {
            assert!(read(text).session_questions, "{text:?}");
        }
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
        assert!(notes[0].contains("cannot be read"), "{notes:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hosted_opts_follow_the_policy() {
        use super::super::run::SuperviseOpts;
        let o = SuperviseOpts::hosted_with(&SupervisorConfig::default());
        assert_eq!(o.policy, SupervisorConfig::default());
        let none = read("[harness]\napprove = \"none\"\nresume_limits = false\n");
        let o = SuperviseOpts::hosted_with(&none);
        assert_eq!(
            o.policy, none,
            "the whole config rides in the options: the one gate"
        );
        // The opt-in rides there as written, and is off where it is not.
        assert!(!SuperviseOpts::hosted().policy.limit_reset);
        let on = read("[harness]\nlimit_reset = true\n");
        assert!(SuperviseOpts::hosted_with(&on).policy.limit_reset);
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
            notes.iter().any(|n| n.contains("harness.memory_ceiling")
                && n.contains("no setting, so memory_ceiling keeps its default")),
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
            KEYS[KEYS.iter().position(|k| *k == "answer_questions").unwrap() + 1],
            "question_priority"
        );
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
            assert!(c.answer_questions && c.session_questions, "{value:?}");
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

    /// The priority never turns answering on: `answer_questions = false` with
    /// `question_priority = long-term` answers no question, whichever key the
    /// file sets first, the priority kept as written and the session's word
    /// its say. Negative control: with the switch on, the priority rides the
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
            assert!(
                c.session_questions,
                "a switch written right keeps the session's say"
            );
            let o = SuperviseOpts::hosted_with(&c);
            assert!(!o.policy.answer_questions, "{text:?}");
        }
        let on = read("[harness]\nquestion_priority = \"short-term\"\n");
        let o = SuperviseOpts::hosted_with(&on);
        assert!(o.policy.answer_questions);
        assert_eq!(o.policy.question_priority, QuestionPriority::ShortTerm);
    }
}
