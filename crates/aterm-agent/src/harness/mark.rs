// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MARK — presence and control for the aterm wrapper
//! (design `docs/DESIGN-aterm-wrapper-2026-09-17.md` §4.6).
//!
//! Two owner asks, both P0 (design §4.6, quoting 2026-09-19): *"there should
//! be a nice harness indicator showing that aterm is doing 'something' over on
//! top of claude code and a way in the settings to disable the harness."* The
//! moment aterm answers a prompt or types a command inside someone else's
//! program, "who did that?" and "how do I stop it?" must have answers that are
//! visible without being asked for.
//!
//! # One state, rendered the same everywhere
//!
//! [`Mark`] has exactly four values and [`presence`] is the ONE function that
//! decides which one a session is in. The tab label, the status-bar row, the
//! menu-bar item and `harness status` all render the same [`Presence`], so
//! they cannot disagree — the failure this module exists to prevent is two
//! surfaces claiming different things about the same session.
//!
//! # The correction this module is built on (design §4.6, 2026-09-19)
//!
//! The first draft derived `armed` from "hooks confirmed" and fired
//! `bypassed` on a missing hook — i.e. it reported the HOOK's liveness as the
//! HARNESS's liveness. Under the central law (design §0.2, §4.2, §4.7) the
//! harness is fully alive with ZERO hooks: it observes, classifies, warns and
//! journals off the grid spine ([`super::observe`]). Rendering `bypassed`
//! there would be a false statement about the product.
//!
//! So this module reports TWO facts and keeps them split:
//!
//! * [`Mark`] reports the HOST — is the harness live, and is it acting?
//! * [`Presence::hooks`] / [`Presence::hooks_absent`] report the ENRICHMENT
//!   channel separately, so "the harness did nothing", "the harness was not
//!   there" and "the harness is watching but the vendor's events are not
//!   arriving" stay three different answers.
//!
//! DEVIATION, reported deliberately: the Stage-3 task text asked for
//! `bypassed` "whenever hooks are unconfirmed". That is the pre-correction
//! rule, and design §4.6 names it a false statement about the product in so
//! many words. This module follows the design. What the task's other half —
//! *never `armed` on an assumption* — costs is paid in full: `armed` requires
//! [`Inputs::spine`], which the caller may only set from a watch it has SEEN
//! answer, and an unconfirmed spine renders [`Degrade::SpineDown`], never
//! `armed`.
//!
//! # Nothing polls, and nothing here opens a socket
//!
//! There is no timer, no sleep and no loop in this file. [`presence`] is a
//! pure function of injected facts including the clock; the `acting` hold
//! (§4.6.1's two seconds) is a COMPARISON against an injected `now_ms`, not a
//! wait. The three rendering helpers ([`Presence::meta_set_icon`],
//! [`Presence::meta_set_description`], [`Presence::appnotice`]) return the
//! control-protocol command LINES a host sends; this module sends nothing,
//! exactly as [`super`]'s module law requires.
//!
//! # In-band attribution
//!
//! [`attribution`] is the one helper every actuation uses, so a line the
//! harness causes inside the vendor's own transcript names the harness and its
//! rule id. A reader scrolling back a week later must not have to guess
//! whether a person, the model, or aterm produced a line.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested, including every mark
//! state, both bypass-by-switch paths, the attribution helper's injection
//! cases, and the capability store's round trip. UNVERIFIED: no GUI surface
//! renders [`Presence`] yet — the tab label, the status lane and the menu-bar
//! row of design §4.6.1 are TARGET.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::truncate_bytes;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// How long `acting` holds past the verdict (design §4.6.1: *"holds for
/// `ACT_HOLD` (2 s) past the verdict row so a fast act is still seen"*).
pub const ACT_HOLD_MS: u64 = 2_000;

/// The stem every in-band attribution opens with.
pub const STEM: &str = "aterm harness";

/// The byte cap on a rule name quoted into an attribution. A rule name is
/// first-party, but attribution text lands inside someone else's transcript,
/// so it is bounded like every other quoted field in this module tree.
pub const RULE_CAP: usize = 64;

/// The `meta set icon` cap (control_verbs.rs: *"icon 64B"*, VERIFIED).
pub const ICON_CAP: usize = 64;

/// The `meta set description` cap (control_verbs.rs: *"description 1024B"*,
/// VERIFIED).
pub const DESCRIPTION_CAP: usize = 1024;

/// The lane `appnotice` posts the harness row on.
///
/// MEASURED (`aterm_types::control_verbs`, the `appnotice` entry): the verb
/// takes `toolchain|update` and nothing else. Design §4.6.1 wants a
/// `Lane::Harness` of its own, which is a GUI change under D6; until that
/// lands the harness rides `toolchain` — named here rather than hidden, so
/// the row's home is a stated deviation and not an accident.
pub const APPNOTICE_LANE: &str = "toolchain";

/// The file the per-capability off switches live in, inside the harness state
/// directory.
///
/// DEVIATION from design §3.7, reported: the design calls this store the
/// harness's own `config.toml`. `aterm-agent` has no TOML parser and the code
/// rules forbid a new third-party dependency, so the store is JSON read
/// through [`aterm_json`], which is already in the shipped graph. What the
/// design actually binds is the SCOPE — capability state lives in the
/// harness's own home and the master switch lives in `aterm.toml` — and that
/// is honoured exactly.
pub const CAPS_FILE: &str = "capabilities.json";

// ---------------------------------------------------------------------------
// The four states
// ---------------------------------------------------------------------------

/// The mark: one glyph, four states, the same on every surface (§4.6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// A harness is live and idle — watching, nothing to say. Provable with
    /// ZERO hooks, so it stays true under `--bare` and on an adopted session.
    Armed,
    /// The harness is taking an action right now. Held [`ACT_HOLD_MS`] past
    /// the verdict so a fast act is still seen.
    Acting,
    /// Live, but reduced: the spine watch is down or a capability is off.
    Degraded,
    /// The harness is NOT live and is observing nothing. **Not** a missing
    /// hook (§4.6.1, corrected).
    Bypassed,
}

impl Mark {
    /// The glyph design §4.6.1 assigns this state.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Mark::Armed => "◇",
            Mark::Acting => "◆",
            Mark::Degraded => "◈",
            Mark::Bypassed => "◌",
        }
    }

    /// The schema-1 spelling (`mark=` in `harness status`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mark::Armed => "armed",
            Mark::Acting => "acting",
            Mark::Degraded => "degraded",
            Mark::Bypassed => "bypassed",
        }
    }
}

/// Why the harness is not live (§4.6.3's `bypassed=env|config|no-prelude`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bypass {
    /// `harness.enabled = false` in `aterm.toml` — durable, this machine,
    /// every session.
    Config,
    /// `$ATERM_NO_HARNESS` is engaged for this session.
    Env,
    /// Nothing attached a harness to this session: no prelude exported the
    /// capability set and no bridge is installed.
    NoPrelude,
}

impl Bypass {
    /// The schema-1 spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Bypass::Config => "config",
            Bypass::Env => "env",
            Bypass::NoPrelude => "no-prelude",
        }
    }

    /// The one sentence an operator can act on — what to undo.
    #[must_use]
    pub fn remedy(self) -> &'static str {
        match self {
            Bypass::Config => "harness.enabled = false in aterm.toml",
            Bypass::Env => "$ATERM_NO_HARNESS is set for this session",
            Bypass::NoPrelude => "no harness is attached to this session",
        }
    }
}

/// Why the harness is live but reduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Degrade {
    /// The caller could not confirm a live watch on the grid spine. This is
    /// the honest answer to *never `armed` on an assumption*: an unconfirmed
    /// spine is reduced, not armed, and not bypassed either — the harness is
    /// switched on, its eyes are shut.
    SpineDown,
    /// At least one capability is off (a probe failed, or an operator turned
    /// it off).
    CapsReduced,
}

impl Degrade {
    /// The schema-1 spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Degrade::SpineDown => "spine-down",
            Degrade::CapsReduced => "caps-reduced",
        }
    }
}

/// Why the ENRICHMENT channel is quiet (§4.6.3's `hooks_absent_cause`). A
/// value here never changes [`Mark`]; it is reported beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HooksAbsent {
    /// A per-session environment bypass suppressed the hooks.
    Env,
    /// The session was launched `--bare`, which removes hooks, plugins AND
    /// the statusLine in one flag (MEASURED, design §4.5).
    Bare,
    /// The vendor settings file carries no harness entries.
    Settings,
    /// Entries exist, but nothing has fired yet.
    Timeout,
}

impl HooksAbsent {
    /// The schema-1 spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HooksAbsent::Env => "env",
            HooksAbsent::Bare => "bare",
            HooksAbsent::Settings => "settings",
            HooksAbsent::Timeout => "timeout",
        }
    }
}

/// How a harness is attached to this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// The `__harness` prelude ran and exported the capability set.
    Prelude,
    /// No prelude, but a bridge is installed in the harness state directory —
    /// a hand `aterm harness install`. Still attached.
    Installed,
    /// Nothing attached: [`Bypass::NoPrelude`].
    None,
}

// ---------------------------------------------------------------------------
// The act in flight
// ---------------------------------------------------------------------------

/// The act the harness is taking, as the journal row that was written BEFORE
/// it records it (§4.3's order: journal, act, verdict).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Act {
    /// What is being done, in the voice the tab label shows (`approving rm`).
    pub verb: String,
    /// The ledger row that explains it.
    pub journal: u64,
    /// When the journal row was written.
    pub began_ms: u64,
    /// When the verdict landed; `None` while the act is still open.
    pub verdict_ms: Option<u64>,
}

impl Act {
    /// Is this act still on screen at `now_ms`? An open act always is; a
    /// closed one holds [`ACT_HOLD_MS`] past its verdict.
    ///
    /// A clock that went BACKWARDS (`now_ms < verdict_ms`) is treated as
    /// still-showing: every tie in this module breaks toward the safe answer,
    /// and the safe answer about "is aterm acting?" is yes.
    #[must_use]
    pub fn showing(&self, now_ms: u64) -> bool {
        match self.verdict_ms {
            None => true,
            Some(v) => now_ms < v.saturating_add(ACT_HOLD_MS),
        }
    }

    /// Milliseconds since the journal row, saturating at 0 on a backwards
    /// clock.
    #[must_use]
    pub fn since_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.began_ms)
    }
}

// ---------------------------------------------------------------------------
// The decision
// ---------------------------------------------------------------------------

/// Everything [`presence`] needs, injected so the decision is a pure function
/// of its arguments.
#[derive(Debug, Clone, Default)]
pub struct Inputs<'a> {
    /// `harness.enabled` from `aterm.toml`. `None` = the key is unset, which
    /// is the default: ON. The key lives in `aterm.toml` and NOT in the
    /// harness's own config precisely so the kill switch never depends on the
    /// thing it kills (§4.6.2).
    pub enabled: Option<bool>,
    /// The raw `$ATERM_NO_HARNESS` value; `None` when unset.
    pub no_harness: Option<&'a str>,
    /// How this session is attached.
    pub attach: Option<Attach>,
    /// A live watch on the grid spine that the caller has SEEN answer.
    /// `false` is the honest default: nothing here assumes a watch.
    pub spine: bool,
    /// Every capability this harness declares.
    pub caps: &'a [&'a str],
    /// The capabilities that are currently off, by name.
    pub caps_off: &'a [String],
    /// The act in flight, if any.
    pub act: Option<Act>,
    /// The millisecond clock the `acting` hold is compared against.
    pub now_ms: u64,
    /// Has a vendor hook ever fired into this state directory?
    pub hooks: bool,
    /// Why not, when it has not.
    pub hooks_absent: Option<HooksAbsent>,
}

/// What every surface renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presence {
    /// The one state.
    pub mark: Mark,
    /// Set exactly when `mark` is [`Mark::Bypassed`].
    pub bypass: Option<Bypass>,
    /// Set exactly when `mark` is [`Mark::Degraded`].
    pub degrade: Option<Degrade>,
    /// Set exactly when `mark` is [`Mark::Acting`].
    pub acting: Option<Act>,
    /// Milliseconds since the acting journal row; 0 when not acting.
    pub since_ms: u64,
    /// The ENRICHMENT channel, reported beside the mark and never folded into
    /// it (§4.6.1, corrected).
    pub hooks: bool,
    /// Why the enrichment channel is quiet.
    pub hooks_absent: Option<HooksAbsent>,
    /// How many capabilities are live.
    pub caps_live: usize,
    /// How many capabilities this harness declares.
    pub caps_total: usize,
}

/// THE decision. Total, pure, and ordered so that every tie breaks toward the
/// safe answer.
///
/// The order, and why:
///
/// 1. the durable off switch, then the per-session one, then "nothing is
///    attached" — all three are [`Mark::Bypassed`], and the CAUSE reported is
///    the widest true one, because that is the one that must be undone for
///    the next launch to differ;
/// 2. an act in flight outranks every reduction: what aterm is doing right
///    now is the fact the indicator exists for;
/// 3. an unconfirmed spine, then a reduced capability set, are
///    [`Mark::Degraded`];
/// 4. only then [`Mark::Armed`] — never on an assumption.
#[must_use]
pub fn presence(inp: &Inputs) -> Presence {
    let caps_total = inp.caps.len();
    let caps_live = inp
        .caps
        .iter()
        .filter(|c| !inp.caps_off.iter().any(|off| off == *c))
        .count();
    let base = Presence {
        mark: Mark::Armed,
        bypass: None,
        degrade: None,
        acting: None,
        since_ms: 0,
        hooks: inp.hooks,
        hooks_absent: if inp.hooks { None } else { inp.hooks_absent },
        caps_live,
        caps_total,
    };
    let bypass = if inp.enabled == Some(false) {
        Some(Bypass::Config)
    } else if env_engaged(inp.no_harness) {
        Some(Bypass::Env)
    } else if matches!(inp.attach, None | Some(Attach::None)) {
        Some(Bypass::NoPrelude)
    } else {
        None
    };
    if let Some(bypass) = bypass {
        return Presence {
            mark: Mark::Bypassed,
            bypass: Some(bypass),
            ..base
        };
    }
    if let Some(act) = inp.act.as_ref().filter(|a| a.showing(inp.now_ms)) {
        return Presence {
            mark: Mark::Acting,
            acting: Some(act.clone()),
            since_ms: act.since_ms(inp.now_ms),
            ..base
        };
    }
    if !inp.spine {
        return Presence {
            mark: Mark::Degraded,
            degrade: Some(Degrade::SpineDown),
            ..base
        };
    }
    if caps_live < caps_total {
        return Presence {
            mark: Mark::Degraded,
            degrade: Some(Degrade::CapsReduced),
            ..base
        };
    }
    base
}

/// THE reading of a boolean veto variable: engaged only by a value that is
/// non-empty and not `"0"`.
///
/// A std-only MIRROR of `aterm_types::control_socket::env_flag_engaged`, which
/// `aterm-agent` cannot call because it does not depend on `aterm-types` — the
/// same shape and the same reason as `aterm-uds`'s token-filename mirror
/// (`aterm-ctl/src/lib.rs`, `uds_token_name_mirror_matches_aterm_types`).
/// UNVERIFIED here: no test in this crate can pin the agreement, because
/// neither crate can see the other; the rule is three lines and its whole
/// content is stated in this doc comment.
///
/// WHY IT IS NOT `is_some()`: empty environment variables travel, so a shell
/// exporting `ATERM_NO_HARNESS=` would hand every descendant a veto nothing
/// intended. That exact species disabled the seamless updater twice on
/// 2026-09-01.
#[must_use]
pub fn env_engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

// ---------------------------------------------------------------------------
// Rendering — one Presence, every surface
// ---------------------------------------------------------------------------

impl Presence {
    /// The tab-label prefix: the glyph and one space (§4.6.1 place 1 — the
    /// mark is PREPENDED to the existing `<phase glyph> <subject> · <state>`).
    #[must_use]
    pub fn tab_prefix(&self) -> String {
        let mut s = String::from(self.mark.glyph());
        s.push(' ');
        s
    }

    /// The one sentence every prose surface shows.
    #[must_use]
    pub fn sentence(&self) -> String {
        let mut s = String::from(STEM);
        s.push(' ');
        s.push_str(self.mark.as_str());
        match (self.mark, self.bypass, self.degrade, self.acting.as_ref()) {
            (Mark::Bypassed, Some(b), _, _) => {
                s.push_str(" · ");
                s.push_str(b.remedy());
            }
            (Mark::Degraded, _, Some(Degrade::SpineDown), _) => {
                s.push_str(" · no confirmed watch on the grid spine");
            }
            (Mark::Degraded, _, Some(Degrade::CapsReduced), _) => {
                s.push_str(" · ");
                s.push_str(&self.caps_live.to_string());
                s.push('/');
                s.push_str(&self.caps_total.to_string());
                s.push_str(" capabilities live");
            }
            (Mark::Acting, _, _, Some(act)) => {
                s.push_str(" · ");
                s.push_str(truncate_bytes(&act.verb, RULE_CAP));
                s.push_str(" · journal #");
                s.push_str(&act.journal.to_string());
            }
            _ => s.push_str(" · watching"),
        }
        // The enrichment channel is named beside the mark, never folded into
        // it: a reader must be able to tell "reduced" from "unenriched".
        if !self.hooks {
            s.push_str(" · hooks=absent");
            if let Some(cause) = self.hooks_absent {
                s.push('(');
                s.push_str(cause.as_str());
                s.push(')');
            }
        }
        super::one_line(&s, usize::MAX)
    }

    /// The `meta set icon` command line a host sends (§4.6.1's *"From the CLI
    /// side use meta set icon"* — no GUI change needed).
    #[must_use]
    pub fn meta_set_icon(&self) -> String {
        let mut s = String::from("meta set icon ");
        s.push_str(truncate_bytes(self.mark.glyph(), ICON_CAP));
        s
    }

    /// The `meta set description` command line a host sends.
    #[must_use]
    pub fn meta_set_description(&self) -> String {
        let mut s = String::from("meta set description ");
        s.push_str(truncate_bytes(&self.sentence(), DESCRIPTION_CAP));
        s
    }

    /// The `appnotice` command line for one act (§4.6.1 place 2). See
    /// [`APPNOTICE_LANE`] for which lane it rides and why.
    #[must_use]
    pub fn appnotice(&self) -> String {
        let mut s = String::from("appnotice ");
        s.push_str(APPNOTICE_LANE);
        s.push(' ');
        s.push_str(&self.sentence());
        s
    }

    /// The `mark=…` tokens `harness status` appends to its line.
    #[must_use]
    pub fn status_fields(&self) -> String {
        let mut s = String::from("mark=");
        s.push_str(self.mark.as_str());
        if let Some(b) = self.bypass {
            s.push_str(" bypassed=");
            s.push_str(b.as_str());
        }
        if let Some(d) = self.degrade {
            s.push_str(" degraded=");
            s.push_str(d.as_str());
        }
        if let Some(act) = self.acting.as_ref() {
            s.push_str(" acting=");
            s.push_str(&token(&act.verb));
            s.push_str(" since_ms=");
            s.push_str(&self.since_ms.to_string());
            s.push_str(" journal=");
            s.push_str(&act.journal.to_string());
        }
        s.push_str(" hooks=");
        s.push_str(if self.hooks { "present" } else { "absent" });
        if let Some(cause) = self.hooks_absent {
            s.push_str(" hooks_absent_cause=");
            s.push_str(cause.as_str());
        }
        s.push_str(" caps_live=");
        s.push_str(&self.caps_live.to_string());
        s.push('/');
        s.push_str(&self.caps_total.to_string());
        s
    }

    /// The same facts as a JSON object, for `harness status --json`.
    #[must_use]
    pub fn json(&self) -> Value {
        let mut o = Map::new();
        o.insert(
            "mark".to_owned(),
            Value::from(self.mark.as_str().to_owned()),
        );
        o.insert(
            "glyph".to_owned(),
            Value::from(self.mark.glyph().to_owned()),
        );
        if let Some(b) = self.bypass {
            o.insert("bypassed".to_owned(), Value::from(b.as_str().to_owned()));
        }
        if let Some(d) = self.degrade {
            o.insert("degraded".to_owned(), Value::from(d.as_str().to_owned()));
        }
        if let Some(act) = self.acting.as_ref() {
            let mut a = Map::new();
            a.insert(
                "verb".to_owned(),
                Value::from(truncate_bytes(&act.verb, RULE_CAP).to_owned()),
            );
            a.insert("journal".to_owned(), Value::from(act.journal));
            a.insert("since_ms".to_owned(), Value::from(self.since_ms));
            o.insert("acting".to_owned(), Value::Object(a));
        }
        o.insert("hooks".to_owned(), Value::from(self.hooks));
        if let Some(cause) = self.hooks_absent {
            o.insert(
                "hooks_absent_cause".to_owned(),
                Value::from(cause.as_str().to_owned()),
            );
        }
        o.insert("caps_live".to_owned(), Value::from(self.caps_live as u64));
        o.insert("caps_total".to_owned(), Value::from(self.caps_total as u64));
        o.insert("sentence".to_owned(), Value::from(self.sentence()));
        Value::Object(o)
    }
}

/// A single whitespace-free token for a `key=value` status field.
fn token(s: &str) -> String {
    let cleaned: String = truncate_bytes(s, RULE_CAP)
        .chars()
        .map(|c| {
            if c.is_whitespace() || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() {
        "-".to_owned()
    } else {
        cleaned
    }
}

// ---------------------------------------------------------------------------
// In-band attribution
// ---------------------------------------------------------------------------

/// Where an attribution is going, which is what decides its framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    /// A machine-read reason field on a hook decision
    /// (`permissionDecisionReason`). The bare stem, no punctuation.
    Field,
    /// Prose the harness sends into the vendor's own transcript — a Stop-block
    /// reply. Opens with the stem and a colon (§4.6.1: *"a Stop-block reply
    /// opens `aterm harness:`"*).
    Reply,
    /// A command line the harness TYPES. The stem goes on its own line BEFORE
    /// the command, never inside it, so nothing the harness types can change
    /// what the command means.
    Typed,
}

/// THE attribution helper every actuation uses (§4.6.1: *"In-band attribution
/// is the deepest half, and it is not optional"*).
///
/// `rule` names the rule that caused the act (`rm policy`); `id` is the
/// ledger row that explains it, so a reader can go from a line in someone
/// else's transcript to the journal row behind it.
///
/// SAFETY of the output, not of memory: `rule` is bounded to [`RULE_CAP`]
/// bytes on a character boundary and every control character in it is folded
/// to a space, so an attribution can never inject a line break into the
/// transcript it lands in. `id` is a `u64`, so it cannot.
#[must_use]
pub fn attribution(voice: Voice, rule: &str, id: u64) -> String {
    let rule = super::one_line(rule, RULE_CAP);
    let rule = rule.trim();
    let mut s = String::from(STEM);
    if !rule.is_empty() {
        s.push(' ');
        s.push_str(rule);
    }
    s.push_str(" id=");
    s.push_str(&id.to_string());
    match voice {
        Voice::Field => s,
        Voice::Reply => {
            s.push_str(": ");
            s
        }
        Voice::Typed => {
            s.push('\n');
            s
        }
    }
}

// ---------------------------------------------------------------------------
// The per-capability off switches
// ---------------------------------------------------------------------------

/// Where the per-capability switches live inside the harness state directory.
#[must_use]
pub fn caps_path(state: &Path) -> PathBuf {
    state.join(CAPS_FILE)
}

/// The store's on-disk text for `disabled` (sorted and deduplicated, so the
/// file is a function of the SET and two equal sets are byte-identical).
#[must_use]
pub fn caps_json(disabled: &BTreeSet<String>) -> String {
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert(
        "disabled".to_owned(),
        Value::Array(
            disabled
                .iter()
                .map(|c| Value::from(c.clone()))
                .collect::<Vec<_>>(),
        ),
    );
    let mut s = aterm_json::to_string(&Value::Object(o)).unwrap_or_default();
    s.push('\n');
    s
}

/// The disabled set in `text`. A malformed or truncated file reads as EMPTY —
/// the tie breaks toward the safe answer, which for a capability switch is
/// "nothing was deliberately turned off", because silently disabling a
/// capability is the failure an operator cannot see.
#[must_use]
pub fn parse_caps_json(text: &str) -> BTreeSet<String> {
    let Ok(doc) = aterm_json::from_str::<Value>(text) else {
        return BTreeSet::new();
    };
    doc.get("disabled")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Read the disabled set out of `state`. A missing file is an empty set.
#[must_use]
pub fn read_disabled(state: &Path) -> BTreeSet<String> {
    match std::fs::read_to_string(caps_path(state)) {
        Ok(text) => parse_caps_json(&text),
        Err(_) => BTreeSet::new(),
    }
}

/// Write the disabled set into `state`, creating the directory if needed.
///
/// The write goes to a sibling temporary file and is renamed over the target,
/// so a reader never sees a half-written store and a crash mid-write leaves
/// the previous set intact.
///
/// # Errors
///
/// The directory cannot be created, or the file cannot be written or renamed.
pub fn write_disabled(state: &Path, disabled: &BTreeSet<String>) -> io::Result<()> {
    std::fs::create_dir_all(state)?;
    let target = caps_path(state);
    let mut tmp = target.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, caps_json(disabled))?;
    std::fs::rename(&tmp, &target)
}

/// The verdict of one `enable`/`disable` request: what the set was, what it
/// became, and whether anything actually changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switch {
    /// The set after the request.
    pub disabled: BTreeSet<String>,
    /// Did the set change?
    pub changed: bool,
    /// The capabilities the request named that this harness does not declare.
    pub unknown: Vec<String>,
}

/// Apply one enable/disable request to `current`.
///
/// `caps` is the declared capability set; a name outside it is REPORTED in
/// [`Switch::unknown`] and never written, so a typo cannot quietly park a
/// switch nothing will ever read. An empty `names` means every declared
/// capability — the bare `aterm harness disable` form.
#[must_use]
pub fn switch(current: &BTreeSet<String>, caps: &[&str], names: &[String], enable: bool) -> Switch {
    let mut unknown = Vec::new();
    let targets: Vec<String> = if names.is_empty() {
        caps.iter().map(|c| (*c).to_owned()).collect()
    } else {
        let mut ok = Vec::new();
        for n in names {
            if caps.contains(&n.as_str()) {
                ok.push(n.clone());
            } else {
                unknown.push(n.clone());
            }
        }
        ok
    };
    let mut next = current.clone();
    for t in targets {
        if enable {
            next.remove(&t);
        } else {
            next.insert(t);
        }
    }
    Switch {
        changed: next != *current,
        disabled: next,
        unknown,
    }
}

#[path = "mark_tests.rs"]
#[cfg(test)]
mod tests;
