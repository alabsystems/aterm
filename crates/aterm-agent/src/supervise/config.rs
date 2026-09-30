// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's policy: aterm.toml's `[harness]` table.
//!
//! THE RULE (owner, 2026-09-24): *batteries included, on by default;
//! configuration limits power, it never enables it.* Unless the owner has
//! written otherwise, every agent session aterm hosts is supervised in FULLY
//! AUTOMATIC mode — nobody is at the keyboard, so nothing it meets may wait on
//! a person. [`SupervisorConfig::default`] is therefore every power on, and
//! every key the owner can write takes power away:
//!
//! * a key missing is its default — full power;
//! * ONLY AN EXPLICIT SETTING TAKES POWER AWAY (owner ruling, 2026-09-25:
//!   "all such dialogs must be approved by default unless there is a setting
//!   added later by the user explicitly to NOT do this"). A switch reads
//!   `true`/`false` and the owner's other plain spellings of them (`"yes"`/
//!   `"no"`, `"on"`/`"off"`, `1`/`0`, any case); `approve` reads `all`,
//!   `safe`, `none` (any case) and an off spelling as `none`. A known key
//!   with a value that is NEITHER its own nor an off (`approve = "maybe"`,
//!   `enabled = 2.5`, a table) KEEPS ITS DEFAULT and is reported — until the
//!   ruling it was set to its limit ("the owner evidently meant to limit
//!   it"), so `approve = "everything"` stopped every box being answered;
//! * an unknown key is reported and changes nothing — it cannot say what to
//!   limit;
//! * `harness` written as an off itself (`harness = false`) is the master
//!   switch off; any other value that is not a table is ignored;
//! * `harness.<key>` written under another table (`[theme]` then
//!   `harness.enabled = false`) still limits, and only limits — a kill switch
//!   must not stop working because of where in the file it was written;
//! * a file that is not TOML at all is read line by line inside its
//!   `[harness]` section (`assignments`; a key said twice there may only
//!   limit the second time), so a typo elsewhere never re-arms a limit here;
//! * a RETIRED key ([`RETIRED_KEYS`]: 0.93.0's approval switches) written
//!   `false` is read as the limit it named ([`SupervisorConfig::retired`]),
//!   and said so — a limit the owner wrote holds through the rename; written
//!   `true` it asked for the default, does nothing, and is said to.
//!
//! [`SupervisorConfig::from_aterm_toml`] is the ONE reader of that table: the
//! window's host, `aterm drive watch|supervise` and the hand-run `aterm harness
//! upgrade` all call it, so no two of them can disagree on a key, a default or
//! a refusal.

use std::path::{Path, PathBuf};

/// Every `[harness]` key, in the order the help and Settings list them.
pub const KEYS: &[&str] = &[
    "enabled",
    "headless",
    "approve",
    "trust_roots",
    "answer_questions",
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
    "model_fallback",
    "rate_nudge",
    "compact_on_context_wall",
    "relaunch",
    "stall_term_after_s",
    "upgrade",
    "human_grace_s",
];

/// The `[harness]` keys 0.93.0 shipped and this reader retired: its four
/// approval switches, folded into [`KEYS`]' `approve`, and `approve_all`,
/// which `approve = "all"` is. Each still LIMITS when written `false`
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

/// The owner's policy for every supervised agent session. Every field's
/// default is full power; see the module doc for how a written key limits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// The master switch (Settings: search "harness"). Off: no session is supervised.
    pub enabled: bool,
    /// Supervise the sessions of a HEADLESS instance too.
    pub headless: bool,
    /// What a permission box may be answered with.
    pub approve: Approve,
    /// Folders whose trust dialog is answered under [`Approve::Safe`]. A
    /// leading `~/` is the home directory; a trailing `*` matches any suffix
    /// of the last component (`~/aterm*` is every aterm worktree).
    pub trust_roots: Vec<String>,
    /// Answer a question, a choice between options or a request for a
    /// decision with [`Self::answer_text`] instead of holding it for a person.
    pub answer_questions: bool,
    /// What a question is answered with.
    pub answer_text: String,
    /// Dismiss Claude Code's session survey (`0`, never a rating).
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
    /// Answer an API error or an overload, however many, by what it says
    /// went wrong (`policy::turn_end`'s module header: the network, a reply
    /// cut off, the server) — never with an ask. Off: each is escalated.
    pub retry_api_errors: bool,
    /// Let the window's host MEASURE the API's reach while a session waits
    /// at an API error the network caused — a resolve, a connect and a
    /// verified TLS handshake to `api.anthropic.com`, no request sent
    /// (aterm-agent `harness::netwatch`) — so the wait at an unreachable API
    /// ends within about a minute of it being back (the probe's cadence and
    /// the loop's wait step), not at the ladder's next rung. Off: nothing is
    /// probed, the reach is never measured, and such a wall is tried on the
    /// time ladder alone.
    pub probe_api: bool,
    /// Continue once a session or weekly usage limit has reset.
    pub resume_limits: bool,
    /// At Claude Code's usage-limit dialog (`What do you want to do?`),
    /// choose `Wait here, then continue automatically …` — by its label,
    /// never a spending row, never `Stop` — so the vendor's own continue is
    /// armed for the reset (`limit-wait@v1`; the owner, 2026-09-24: "you
    /// should not depend on the user to choose"). Off: the dialog is a
    /// person's.
    pub limit_wait: bool,
    /// CLAUDE CODE'S model fallback: the model the agent is RELAUNCHED on at
    /// a model-bucket limit (`--model`, session-only — never Claude's
    /// `/model`, which saves the person's default), and back from at its
    /// reset; set, full power also switches Claude Code's model-refusal pause
    /// (`Session paused`, exactly its switch and its retry). No other
    /// vendor's box is ever answered with a model switch under it — Codex's
    /// rate-limit nudge is [`Self::rate_nudge`]'s. `None`: wait for the reset
    /// instead, and leave the pause to a person.
    pub model_fallback: Option<String>,
    /// CODEX'S RATE-LIMIT NUDGE (`Approaching rate limits`): answered by full
    /// power — its switch to the cheaper model ONLY when Codex's own usage
    /// reading shows the window at 90% or more, and then only to save the
    /// work (commit and push, then stop) before the session is put back on
    /// its own model and held until the window resets; any other nudge is
    /// kept on its model. Off: the nudge is a person's.
    pub rate_nudge: bool,
    /// Type `/compact` when the worker stops on a full context.
    pub compact_on_context_wall: bool,
    /// Relaunch a Claude Code or a Codex that crashed (Claude Code's session
    /// record left behind, a Codex's exit status as its shell recorded it —
    /// read as the exit is seen), on its own conversation; a graceful exit is
    /// someone's decision. Its limit limits the stall's remedy too: an agent
    /// frozen past its bound is ended only where it is relaunched
    /// (`supervise/stall.rs`).
    pub relaunch: bool,
    /// How long an agent stands frozen — not reading its input, nobody's
    /// hand on it — before the supervisor ends it with `signal term` for its
    /// relaunch (`supervise/stall.rs`); `0` never. Its own limit, apart
    /// from [`Self::relaunch`]: the automatic end of a frozen worker can be
    /// switched off while crashes are still relaunched.
    pub stall_term_after_s: u32,
    /// Restart an idle agent onto a newer installed build of itself.
    pub upgrade: bool,
    /// After a person's keystroke into a session, the supervisor keeps its
    /// hands off that session for this many seconds.
    pub human_grace_s: u32,
    /// Safe rules a retired key switched off ([`RETIRED_KEYS`]); no key of
    /// its own sets it.
    pub withheld: Withheld,
    /// Whether a SESSION's own `questions` word (`aterm ctl @<sid> meta set
    /// questions ask|recommended`, Owner-only, read by key off the bare
    /// `meta` reply) decides that session's question dialogs over
    /// [`Self::answer_questions`] — the owner's "global, and then session"
    /// (2026-09-23): `ask` hands them to a person (how a controller takes a
    /// worker's questions, `aterm drive answer`), `recommended` answers them
    /// where the table's switch is off. On by default; off where a limit is
    /// not the table's to be overridden by a session: a loop's own
    /// `--no-answer`. (An `answer_questions` value the reader could not take
    /// took it away too until the owner ruling of 2026-09-25; such a value is
    /// no setting now, and changes nothing.) Not a `[harness]` key.
    pub session_questions: bool,
}

/// The two safe rules only a retired key can take away ([`RETIRED_KEYS`]),
/// each read under `approve = "safe"` — the level the same key caps
/// `approve` at: `rm_breaker = false` leaves the rm circuit breaker no
/// scratch root, and `read_outside_cwd = false` leaves a Read box no root,
/// so either box goes to the owner.
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
            trust_roots: ["~/aterm*", "~/ay*", "$HOME/trust*", "/private/tmp/claude-*"]
                .map(String::from)
                .to_vec(),
            answer_questions: true,
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
            model_fallback: Some("opus".to_string()),
            rate_nudge: true,
            compact_on_context_wall: true,
            relaunch: true,
            stall_term_after_s: 600,
            upgrade: true,
            human_grace_s: 120,
            withheld: Withheld::default(),
            session_questions: true,
        }
    }
}

impl SupervisorConfig {
    /// Set one key from its value's text — an on/off ([`switch_word`]) for a
    /// switch, a decimal for a count, the string for a text, a path or
    /// `approve`, and for `trust_roots` the array's elements joined by `,`.
    /// A value that is not the key's own: an explicit off ([`spells_off`]) is
    /// that key's LIMITING value ([`Self::limit`]); anything else leaves the
    /// key at its default (owner ruling of 2026-09-25) — both said in the
    /// `Err`. An unknown key changes nothing and says so.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let v = value.trim();
        // An off limits a key it is not the key's own value for — except a
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
        let text = {
            let v = value.trim();
            (!v.is_empty()).then(|| v.to_string())
        };
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
            "rate_nudge" => switch!(rate_nudge),
            "compact_on_context_wall" => switch!(compact_on_context_wall),
            "relaunch" => switch!(relaunch),
            "upgrade" => switch!(upgrade),
            "approve" => match Approve::parse(value) {
                Some(a) => self.approve = a,
                None => return bad(self, "all, safe or none"),
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

    /// A RETIRED key ([`RETIRED_KEYS`]) read as what 0.93.0 made it mean,
    /// and the note that says so. `limits` (written as an off —
    /// [`retired_reading`]; a value that is no on or off is no setting and
    /// never reaches here) applies the limit it named:
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

    /// Set `key` to its LIMITING value: the least power it can give. A switch
    /// is off, `approve` is `none`, a cap is one an hour, the grace is an
    /// hour, a frozen agent is never ended (`stall_term_after_s = 0`);
    /// a text keeps its default (no text limits more than another).
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
            "model_fallback" => self.model_fallback = None,
            "rate_nudge" => self.rate_nudge = false,
            "compact_on_context_wall" => self.compact_on_context_wall = false,
            "relaunch" => self.relaunch = false,
            "stall_term_after_s" => self.stall_term_after_s = 0,
            "upgrade" => self.upgrade = false,
            "human_grace_s" => self.human_grace_s = 3600,
            retired if RETIRED_KEYS.contains(&retired) => {
                let _ = self.retired(retired, true);
            }
            _ => {}
        }
    }

    /// THE reader of aterm.toml's `[harness]` table (module doc): the policy
    /// and one line per key it refused, could not take, or found outside the
    /// root table. `text` is the whole file.
    ///
    /// The root `[harness]` is applied first and every `harness` table TOML
    /// files under another one after it, each of whose keys may only limit
    /// (`Self::limits`) — so a misplaced key never undoes the root
    /// table's limit, whichever comes first in the file.
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
                for (key, value) in current.into_iter().chain(retired) {
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
                    // `harness = false`: the master switch written off — at the
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

    /// [`Self::set`] from a parsed TOML value: a value of a shape no key
    /// takes (a table, a float, a date) is no setting — it changes nothing,
    /// as any value that is neither the key's own nor an off (owner ruling of
    /// 2026-09-25; it was the key's limit until then).
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
    /// text — `continue_text = false` is not the word "false" typed into a
    /// worker — while a quoted string is always one, `"yes"` included.
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
    /// cap higher or absent, no grace or stall bound shorter, no trust root or fallback model
    /// new, and no text changed (a text neither limits nor grants, so where a
    /// key may only limit, a text is not taken).
    fn limits(&self, before: &Self) -> bool {
        let cap = |n: u32| if n == 0 { u32::MAX } else { n };
        let no_more = |now: bool, then: bool| !now || then;
        self != before
            && no_more(self.enabled, before.enabled)
            && no_more(self.headless, before.headless)
            && self.approve.rank() <= before.approve.rank()
            && self
                .trust_roots
                .iter()
                .all(|r| before.trust_roots.contains(r))
            && no_more(self.answer_questions, before.answer_questions)
            && no_more(self.session_questions, before.session_questions)
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
            && (self.model_fallback.is_none() || self.model_fallback == before.model_fallback)
            && no_more(self.rate_nudge, before.rate_nudge)
            && no_more(self.compact_on_context_wall, before.compact_on_context_wall)
            && no_more(self.relaunch, before.relaunch)
            && cap(self.stall_term_after_s) >= cap(before.stall_term_after_s)
            && no_more(self.upgrade, before.upgrade)
            && self.human_grace_s >= before.human_grace_s
            && (self.withheld.rm_breaker || !before.withheld.rm_breaker)
            && (self.withheld.read_outside_cwd || !before.withheld.read_outside_cwd)
    }

    /// Every key's value, in [`KEYS`] order, as the text [`Self::set`] takes
    /// back. The destructure names every field, so a field added without a
    /// key here does not build (and `texts_are_the_keys` holds the order).
    fn texts(&self) -> [(&'static str, String); 22] {
        let Self {
            enabled,
            headless,
            approve,
            trust_roots,
            answer_questions,
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
            model_fallback,
            rate_nudge,
            compact_on_context_wall,
            relaunch,
            stall_term_after_s,
            upgrade,
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
            ("answer_questions", answer_questions.to_string()),
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
            ("model_fallback", model_fallback.clone().unwrap_or_default()),
            ("rate_nudge", rate_nudge.to_string()),
            (
                "compact_on_context_wall",
                compact_on_context_wall.to_string(),
            ),
            ("relaunch", relaunch.to_string()),
            ("stall_term_after_s", stall_term_after_s.to_string()),
            ("upgrade", upgrade.to_string()),
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
    /// [`KEYS`] order. A key whose value takes power away — by the same
    /// measure a misplaced key is held to ([`Self::limits`]) — is a LIMIT;
    /// any other change (a text, a longer list of trust roots, another
    /// fallback model, a shorter grace) is the owner's OWN value, which
    /// neither limits nor grants. `trust_roots` is read only under
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
    /// choice (`answer_text`, `rules_file`).
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
/// retired key's ([`RETIRED_KEYS`]) — which the window's config language
/// says itself, in the same words ([`retired_key_note`]).
#[must_use]
pub fn is_retired_key_note(note: &str) -> bool {
    note.contains(RETIRED_KEY)
}

/// What the reader says of the retired `[harness] <key>` written `value`
/// (`None`: a value that is no switch — no setting, which changes nothing):
/// the words the window's config language shows on that line.
#[must_use]
pub fn retired_key_note(key: &str, value: Option<bool>) -> Option<String> {
    RETIRED_KEYS.contains(&key).then(|| match value {
        Some(on) => SupervisorConfig::default().retired(key, !on),
        None => retired_no_setting(key),
    })
}

/// [`retired_key_note`] from the value's one-value text (`None`: a shape
/// that has none — a table, an array, a float), read by the reader's own
/// rule ([`retired_reading`]) so the config language and the reader never
/// disagree on what the line did.
#[must_use]
pub fn retired_key_note_text(key: &str, text: Option<&str>) -> Option<String> {
    retired_key_note(key, text.and_then(retired_reading))
}

/// A retired key's value as an on or an off: an on-word ([`switch_word`]) is
/// on, any off spelling ([`spells_off`] — `"disabled"` too, as for every
/// key) is off, and anything else is no setting.
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

/// A TOML value's text when it is ONE value — a boolean, an integer or a
/// string — and `None` for every other shape (an array is no switch).
fn one_value_text(v: &aterm_toml::Value) -> Option<String> {
    use aterm_toml::Value;
    match v {
        Value::Boolean(_) | Value::Integer(_) | Value::String(_) => scalar_text(v),
        _ => None,
    }
}

/// Every NON-table `harness` value in `v` below its root, with the dotted
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

/// A value's text as an EXPLICIT on or off: `true`/`false`, `yes`/`no`,
/// `on`/`off`, `1`/`0` (and TOML's other spellings of the integers 0 and 1:
/// `+0`, `0x1`, `0b0`…), in any case; `None` for anything else — no setting.
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

/// Whether a value's text says OFF for any key: a switch's off
/// ([`switch_word`]), or `none` / `never` / `disabled` — the one kind of
/// value that limits a key it is not the key's own value for (owner ruling
/// of 2026-09-25: only an explicit setting takes power away).
#[must_use]
pub fn spells_off(value: &str) -> bool {
    switch_word(value) == Some(false)
        || matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "none" | "never" | "disabled"
        )
}

/// Whether `note` (one of [`SupervisorConfig::from_aterm_toml`]'s) says a
/// key is not a `[harness]` key — which a reader that already names every
/// unknown key of the file (the window's config language) leaves out.
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
/// whole key path and its value's text — the reading of a file nothing can
/// parse ([`SupervisorConfig::from_aterm_toml`], and `aterm harness disk`'s
/// `disk.apply`).
///
/// A `[a.b]` header names the table the keys below it belong to, a `[[a]]`
/// array-of-tables header names an ELEMENT of `a` (the path segment `[]`, so
/// its own `enabled` is never `a.enabled`), and `k = v` is `<header>.<k>`.
/// Every one-line spelling TOML gives a key is read — dotted (spaces around a
/// dot allowed), quoted segments, and one-line inline tables (`harness = {
/// enabled = false }`, flattened into their keys) — cutting a line only where
/// TOML would ([`top_level`]), `#` comments ignored. A multi-line string
/// (`"""`/`'''`) or array is SKIPPED whole, its lines never read as keys or
/// headers: a `[harness]` or an `enabled = false` quoted inside an
/// `answer_text` is no setting (the review of 2026-09-26).
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
/// trimmed and stripped of ONE layer of basic or literal quotes, so
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
        for key in KEYS {
            let mut c = full.clone();
            c.limit(key);
            let text_key = matches!(
                *key,
                "trust_roots" | "answer_text" | "continue_text" | "rules_file"
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

    /// THE OWNER RULING OF 2026-09-25: only an explicit setting takes power
    /// away. A value that is neither the key's own nor an off — a word it
    /// cannot read, a float, a table — KEEPS the key's default and is named;
    /// until the ruling it was the key's limit, so `approve = "everything"`
    /// stopped every box being answered and `enabled = "yes"` turned the
    /// harness off. An off, however it is spelled, is still the limit, and an
    /// unknown key changes nothing.
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

    /// `harness.<key>` written below another table's header — TOML files it
    /// under that table — still LIMITS, and never grants: a kill switch must
    /// not stop working because of where in the file it was written, and a
    /// stray line must not undo the root table's limit, whichever comes
    /// first (the root table is applied first). Each such line is named.
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

    /// A file the TOML parser refuses — a typo three tables away — is read
    /// line by line: every limit its `[harness]` lines write holds, in every
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
        // setting is none there either (owner ruling of 2026-09-25).
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
        // NEGATIVE CONTROL: a text, a path, another model and a shorter grace
        // change the policy but take no power away — the owner's own.
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
        let roots = "trust_roots = [\"~/aterm*\"]\n";
        assert!(read(&format!("[harness]\n{roots}")).against_default() == Written::default());
        assert_eq!(
            read(&format!("[harness]\napprove = \"safe\"\n{roots}"))
                .against_default()
                .limits,
            ["approve: safe", "trust_roots: ~/aterm*"]
        );
        let more = "trust_roots = [\"~/aterm*\", \"~/ay*\", \"$HOME/trust*\", \"/private/tmp/claude-*\", \"~/x\"]\n";
        let w = read(&format!("[harness]\napprove = \"safe\"\n{more}")).against_default();
        assert_eq!((w.limits.len(), w.own), (1, vec!["trust_roots"]));
    }

    /// D9 (the reconciliation of 2026-09-25): 0.93.0 shipped `approve_all`,
    /// `auto_reads`, `rm_breaker`, `read_outside_cwd` and `trust_dialog`.
    /// One written `false` was a limit the owner wrote, so it is read as the
    /// limit it named — never lifted as "not a harness key" — and said so,
    /// with the key to write instead; written `true` it asked for the
    /// default and changes nothing. NEGATIVE CONTROL: a misspelling of one
    /// is an unknown key, and changes nothing.
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
            // (owner ruling of 2026-09-25; it was the limit until then).
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
        // NEGATIVE CONTROL: a misspelling is no retired key.
        let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\napprove_al = false\n");
        assert_eq!(c, full);
        assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
        assert_eq!(retired_key_note("approve_al", Some(false)), None);
    }

    /// A SESSION'S `questions` word keeps its say unless a loop's own flag
    /// takes it (2026-09-25): an `answer_questions` value the reader cannot
    /// take is no setting (owner ruling of 2026-09-25) — the switch keeps its
    /// default and the sessions their word; until the ruling it silenced both.
    /// NEGATIVE CONTROL: the switch spelled right, on or off, or absent, or an
    /// unknown key, leaves the session's word its say too.
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
    }
}
