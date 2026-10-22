// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session user metadata and event timeline: two small stores on [`crate::SessionCtx`],
//! shared by `Arc` between the `Session`, its registry
//! [`crate::session_store::SessionHandle`] and each control connection.
//!
//! * [`SessionMeta`]: what a driver calls the session (`meta set <field> …`), not what its
//!   program sets. The title outranks the OSC 0/2 title in tab labels; `role operator`
//!   names the fleet operator; a non-empty `attention` badges the menu-bar status item;
//!   `questions` tells the in-window supervisor how to answer Claude Code's question
//!   dialog ([`QUESTIONS_POLICY_WORDS`]); `autopilot` (`cruise`, `land` or `off`) sets how
//!   far the harness carries its work.
//! * [`SessionTimeline`]: a bounded drop-oldest ring of [`TimelineEvent`]s plus the
//!   server's published program and agent verdict ([`AgentPublication`]), built like
//!   [`crate::turn_ledger::TurnLedger`] (same cap, monotonic-ms clock and clamp). Read by
//!   the `timeline` verb and the `subscribe … events` digest.
//!
//! * [`SessionMeta`]: the user-settable identity a driver stamps on a session
//!   over the control socket (`meta set title|description|icon|role|attention
//!   …`): a display title that outranks the OSC 0/2 title in tab labels, a
//!   free-text description, an icon token, a typed `role` (a label the
//!   menus and the pill name the session by; it elects nothing), a typed `attention` escalation
//!   (an ask ⇒ the menu-bar status item badges), and a typed `questions` policy word
//!   for how the in-window supervisor answers Claude Code's question dialog
//!   ([`QUESTIONS_POLICY_WORDS`], [`normalize_questions_value`]), and its
//!   typed `autopilot` word for how far the harness carries its work
//!   (`cruise`, `land` or `off`; [`normalize_autopilot_value`]). This is what
//!   the operator calls the session, not what the running program sets.
//! * [`SessionTimeline`]: a bounded, drop-oldest ring of lifecycle events
//!   (`spawned`, `state-change`, `title-change`, `cwd-change`, `meta-change`,
//!   `agent-change`, `status-change`, `human`, `harness-act`, `program-state`),
//!   plus the server's published
//!   program and agent verdict ([`AgentPublication`]), modeled on
//!   [`crate::turn_ledger::TurnLedger`] (same cap, monotonic-ms clock and
//!   clamp). Read by the `timeline` verb and scanned by the `subscribe …
//!   events` digest for `EVENT <sid> meta …` pushes.
//!
//! ## Why `SessionCtx`, not `SessionHandle`
//!
//! A `SessionHandle` is a clone in the registry, cloned out per control request
//! under the store lock; state in its fields would fork on every clone and force
//! hot-path readers (the per-frame tab-label refill) through a `Store` read
//! lock. `SessionCtx` holds one copy per session, reachable without the store
//! lock from the pool session, the registry handle and the control thread. Both
//! locks are leaves: taken briefly, never across a `Terminal` or `Store` lock.

use std::collections::VecDeque;
use std::sync::Arc;

use aterm_grapheme::GraphemeClusters;

use crate::turn_ledger::now_ms;

/// Byte cap for `meta set title` (after trim). Small: it is a tab label.
pub(crate) const META_TITLE_MAX: usize = 120;
/// Byte cap for `meta set description` (after trim) — a paragraph, not a doc.
pub(crate) const META_DESCRIPTION_MAX: usize = 1024;
/// Byte cap for `meta set icon` (after trim) — an emoji / short token.
pub(crate) const META_ICON_MAX: usize = 64;
/// Byte cap for `meta set role` (after trim). Every value is stored verbatim:
/// a role is a label, and no value elects anything.
pub(crate) const META_ROLE_MAX: usize = 64;
/// Byte cap for `meta set attention` (after trim): a one-line needs-human message.
pub(crate) const META_ATTENTION_MAX: usize = 256;
/// Byte cap for one keyed attention entry (`meta set attention owner=<k>`), after trim.
/// Tighter than the bare field's: several owners share one badge.
pub(crate) const META_ATTENTION_KEYED_MAX: usize =
    aterm_types::control_verbs::META_ATTENTION_KEYED_MAX;
/// How many attention owners one session holds at once, the bare `-` owner
/// included (its slot is always kept). A write from a new keyed owner past
/// that is refused, never evicting another owner's escalation.
pub(crate) const ATTENTION_OWNERS_MAX: usize = 8;
/// Byte cap for an attention owner key or a supervisor holder name.
pub(crate) const META_OWNER_MAX: usize = 64;
/// The owner a bare `meta set attention <text>` writes as.
pub(crate) const BARE_ATTENTION_OWNER: &str = "-";
/// Byte cap for `meta set questions` (the stored, normalized value). A storage
/// bound only: the field is a closed set ([`QUESTIONS_POLICY_WORDS`]), so the
/// grammar refuses first.
pub(crate) const META_QUESTIONS_MAX: usize = 16;

/// The policy words `meta set questions` accepts: this session's word for how
/// the in-window supervisor treats its questions (Claude Code's question
/// dialog, and by the priority words a question asked in prose), overriding
/// the global `[harness] answer_questions` and `[harness] question_priority`.
/// Their meaning belongs to the supervisor (`aterm_agent::supervise`):
/// - `ask` leaves the dialog to a person (escalated like any box); a prose
///   question stays the global switch's;
/// - `recommended` answers the dialog with its recommended option(s) and a
///   prose question with `answer_text`;
/// - `long-term` and `short-term` answer by the worker's own choice under that
///   priority, from a fixed text the supervisor writes into the free-text row
///   (`aterm_agent::supervise::QuestionPriority::WORDS`, which this list is
///   built from).
///
/// This store holds the word only. Unset (`questions=-`), the global switch
/// decides; a build restoring a word it does not know drops it.
pub(crate) const QUESTIONS_POLICY_WORDS: [&str;
    1 + aterm_agent::supervise::QuestionPriority::WORDS.len()] = {
    let w = aterm_agent::supervise::QuestionPriority::WORDS;
    ["ask", w[0], w[1], w[2]]
};

/// The one usage line every refusal of a `meta set questions` value prints
/// (after the `ERR ` prefix), built from [`QUESTIONS_POLICY_WORDS`].
pub(crate) fn questions_usage() -> String {
    format!(
        "usage: meta set questions {}",
        QUESTIONS_POLICY_WORDS.join("|")
    )
}

/// Normalize a `questions` value to its policy word, or `None` when it is not
/// one: whitespace is trimmed and ASCII case folded (`Recommended` stores
/// `recommended`), so every reader compares one spelling. Anything else is
/// `None`, and the wire answers [`questions_usage`]. Pure: the control verb,
/// the restore sanitizer and [`SessionMeta::set`]'s second gate all use it.
#[must_use]
pub(crate) fn normalize_questions_value(value: &str) -> Option<&'static str> {
    let value = value.trim();
    QUESTIONS_POLICY_WORDS
        .into_iter()
        .find(|word| word.eq_ignore_ascii_case(value))
}

/// Byte cap for `meta set autopilot` (the stored word). A storage bound only, as
/// [`META_QUESTIONS_MAX`] is: the closed set ([`normalize_autopilot_value`]) refuses first.
pub(crate) const META_AUTOPILOT_MAX: usize = 16;

/// The one usage line every refusal of a `meta set autopilot` value prints
/// (after the `ERR ` prefix, and after the reason where one is named:
/// `ERR until= goes with cruise only: usage: …`). It spells the three stored
/// words, in the order the chip cycles them, and the timed Cruise's
/// `until=` (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.5: a local
/// `HH:MM`, or `+<n>h`, `+<n>m`, `+<n>h<n>m` of elapsed time); the owner's
/// own words for two of them (`forever`, `infinity`, `wrap-up`, `wrapup`,
/// `wrap`) are admitted too and stored as the word they stand for
/// ([`normalize_autopilot_value`]).
pub(crate) const AUTOPILOT_USAGE: &str =
    "usage: meta set autopilot cruise [until=HH:MM|+DURATION]|land|off";

/// Normalize an `autopilot` value to its stored word, or `None` when it names no mode: read
/// by the supervisor's own reader ([`aterm_agent::supervise::Autopilot::parse`]: trimmed,
/// any case, aliases included) and stored as that mode's word
/// ([`aterm_agent::supervise::Autopilot::word`]), so a bare `meta` replies with exactly what
/// the supervisor's loop reads back. The control verb, the restore sanitizer,
/// [`SessionMeta::set`]'s second gate and the window's writer (`App::set_session_autopilot`)
/// all use it.
#[must_use]
pub(crate) fn normalize_autopilot_value(value: &str) -> Option<&'static str> {
    aterm_agent::supervise::Autopilot::parse(value).map(aterm_agent::supervise::Autopilot::word)
}

/// True for characters that must never reach chrome from user metadata: the engine's one table
/// ([`aterm_messages::text::is_hidden_char`]), shared with the message band and the presence
/// row. ZWJ/ZWNJ and the variation selectors pass, so emoji, joining scripts and ideographic
/// variants survive.
pub(crate) fn is_forbidden_metadata_char(ch: char) -> bool {
    aterm_messages::text::is_hidden_char(ch)
}

#[must_use]
pub(crate) fn metadata_has_forbidden_formatting(value: &str) -> bool {
    value.chars().any(is_forbidden_metadata_char)
}

/// Canonical single-line presentation sanitizer shared by restored metadata
/// and every user-metadata chrome boundary. Unsafe controls are removed and
/// the byte cap is applied only between grapheme clusters, so a combining mark
/// or ZWJ emoji is never bisected.
#[must_use]
pub(crate) fn sanitize_presentation_line(value: &str, max_bytes: usize) -> String {
    let filtered: String = value
        .chars()
        .filter(|ch| !is_forbidden_metadata_char(*ch))
        .collect();
    let value = filtered.trim();
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut sanitized = String::with_capacity(max_bytes);
    for grapheme in value.graphemes() {
        if sanitized.len().saturating_add(grapheme.len()) > max_bytes {
            break;
        }
        sanitized.push_str(grapheme);
    }
    let trimmed_len = sanitized.trim_end().len();
    sanitized.truncate(trimmed_len);
    sanitized
}

/// Sanitize one named user metadata field for presentation and persistence. Unknown
/// fields and values that become empty are unset.
#[must_use]
pub(crate) fn sanitize_metadata_value(field: &str, value: &str) -> Option<String> {
    let cap = SessionMeta::cap(field)?;
    // `questions` is a closed set: a value from a restore manifest or an older
    // caller is normalized or dropped, never cut to the cap (which could turn
    // one word into another).
    if field == MetaField::Questions.wire_name() {
        return normalize_questions_value(value).map(str::to_owned);
    }
    // So is `autopilot`: a word this build cannot read is dropped (the table's default
    // then decides), never stored.
    if field == MetaField::Autopilot.wire_name() {
        return normalize_autopilot_value(value).map(str::to_owned);
    }
    let value = sanitize_presentation_line(value, cap);
    (!value.is_empty()).then_some(value)
}

/// The bare-attention prefix the retired aterm-link Claude Code hooks wrote (`claude needs
/// approval: <message>`). Nothing writes or clears it now (`aterm link hook` writes no
/// meta), so a carried one would outlive the supervisor's own clear: it does not travel.
pub(crate) const RETIRED_HOOK_BADGE: &str = "claude needs approval:";

/// [`sanitize_metadata_value`] for an attention carried past this process (a
/// handoff, a restore, the crash journal) or past its shell (a closed tab
/// reopened on a fresh one): the retired hooks' badge ([`RETIRED_HOOK_BADGE`])
/// does not travel. A live `meta set attention` is not filtered.
#[must_use]
pub(crate) fn carried_attention(value: &str) -> Option<String> {
    sanitize_metadata_value("attention", value)
        .filter(|value| !value.starts_with(RETIRED_HOOK_BADGE))
}

/// How many timeline events a session retains (drop-oldest past this), sized
/// like the turn ledger: a long session stays readable, the ring never grows.
pub(crate) const TIMELINE_CAP: usize = 512;

/// Byte cap for one event's stored payload: a short `k=v` wire token string
/// (values already pct-encoded); the clamp guards against a pathological
/// title/cwd bloating the ring.
const MAX_PAYLOAD: usize = 256;

/// The user-settable per-session metadata (`meta set`/`meta unset`). All
/// fields are `None` until a driver sets them; `user_title` (when set +
/// non-empty) outranks the live OSC title in tab labels and stays until unset.
#[derive(Default, Clone, Debug)]
pub(crate) struct SessionMeta {
    /// Operator-chosen display title, the top rung of the tab-label chain.
    pub user_title: Option<String>,
    /// Free-text purpose/notes for the session (agents leave context here).
    pub description: Option<String>,
    /// Icon token (emoji or short name), shown in the pane header and session chrome.
    pub icon: Option<String>,
    /// Typed role token: a label, the first rung the menus and the pill
    /// name the session by. No value elects anything.
    pub role: Option<String>,
    /// Typed needs-human escalation: non-empty ⇒ this session wants a human,
    /// and the value is the one-line reason in the status-item menu. Only it
    /// raises that row: a title starting `⚠` raises none.
    ///
    /// This is the effective value, the most recently set entry of
    /// [`Self::attention_owners`], so every reader (status item, tab chrome,
    /// `meta`, `sessions meta=1`, the `meta-change` event) sees one line.
    /// Written only through [`Self::set`] and [`Self::set_attention_owned`],
    /// which keep it in step with the map.
    pub attention: Option<String>,
    /// Keyed attention (`meta set attention owner=<k> <text>`): one entry per
    /// owner, bounded at [`ATTENTION_OWNERS_MAX`], so a supervisor and a human
    /// (or two supervisors) raise and clear their own escalations independently.
    /// The bare form is owner [`BARE_ATTENTION_OWNER`]. Not part of equality;
    /// only the bare owner's entry is carried across a restore (see
    /// [`Self::sanitized`]).
    pub(crate) attention_owners: AttentionOwners,
    /// Who is supervising this session (`meta set supervisor <holder>`), shown
    /// in `status`, `sessions` and `meta`. Not user identity: never restored,
    /// not part of equality, not `meta=1`.
    pub(crate) supervisor: Option<SupervisorClaim>,
    /// Bumped by every supervisor set or clear, renewals included: an off-lock record read or
    /// kernel proof commits only while the revision it read still stands. Process-local;
    /// never restored, not part of equality.
    pub(crate) supervisor_revision: u64,
    /// Typed question policy: one of [`QUESTIONS_POLICY_WORDS`], stored lowercase (only ever a
    /// word [`normalize_questions_value`] produced). Unset, the `[harness]` switches decide.
    /// Like `role`: part of equality, counted by `sessions meta=1`, carried across a restore and
    /// a handoff.
    pub questions: Option<String>,
    /// The session's Autopilot word (`meta set autopilot cruise|land|off`;
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §2.1), only ever a word
    /// [`normalize_autopilot_value`] produced. Unset, `[harness] autopilot` decides
    /// ([`aterm_agent::supervise::SupervisorConfig::autopilot_for`]); `[harness] enabled =
    /// false` overrides both. Read typed through [`Self::autopilot_word`]. Like `questions`:
    /// part of equality, counted by `sessions meta=1`, carried across a restore and a handoff.
    pub autopilot: Option<String>,
    /// AUTOPILOT'S TIMER (`docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §9.3.6): the instant, unix ms, a timed Cruise lands — only ever beside
    /// the word `cruise`, and written with it by one method. `meta`,
    /// `status` and `ls` print it (`land_at=`, unix seconds, `-` while
    /// unset). It is no [`MetaField`]: it rides the autopilot field. Written
    /// only with the word, by [`Self::set_autopilot`]: every write of the
    /// word by hand, the same word included, drops it. Part of equality,
    /// like the word it goes with.
    pub(crate) land_at: Option<i64>,
    /// WHO SET THE AUTOPILOT PAIR (`docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §11.7.3, item 1): the writer of the last write that moved the word or
    /// its timer — the session log's actor, the window surface pressed and
    /// when ([`AutopilotSetBy`]) — stored with the pair under the same meta
    /// guard ([`apply_autopilot`], the timer's fire and its cancel), so the
    /// word and its setter never disagree. `None` before any such write, and
    /// for a word carried from a leaf that names no setter (`meta`'s
    /// `autopilot_by=restored`). Provenance like [`Self::driver_writes`], so
    /// not part of equality; carried across a restore and a handoff with the
    /// word it goes with, and dropped where the word is ([`Self::sanitized`]).
    pub(crate) autopilot_set_by: Option<AutopilotSetBy>,
    /// How many writes of the Autopilot pair moved it in this process: one
    /// more at every write that stores a setter here ([`apply_autopilot`],
    /// the fire, the cancel), none at a restore's seed or graft, which carry
    /// the setter of a write made in another process. The window's record of
    /// a wire write (`App::on_meta_changed`) compares it with the last it
    /// recorded, so a wake for another field — or one after a restore —
    /// records nothing. Process-local: never restored, not part of equality.
    pub(crate) autopilot_revision: u64,
    /// Provenance, not value: one bit per [`MetaField`] a driver has written in
    /// this process (every write [`apply_meta_value`] accepted, including a
    /// re-set to the same value and a clear of an unset field). A restore seed
    /// ([`Self::set`]) marks nothing. [`restore_carried_meta`] reads it so a
    /// carried identity never overwrites a write a driver made here after the
    /// identity was captured. Not persisted, not part of equality. Write it only
    /// through [`apply_meta_value`]; read it through [`Self::driver_wrote`].
    pub(crate) driver_writes: u8,
}

/// Equality is over the eight values (what a tab label, a `meta` reader and a
/// restore capture see); `driver_writes` records how a value got there, and
/// `autopilot_set_by` who wrote the Autopilot pair (§11.7.3, item 1: two
/// sessions in the same mode are in the same mode, whoever set each).
/// Destructured, so a new field forces a decision about which side it is on.
impl PartialEq for SessionMeta {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            user_title,
            description,
            icon,
            role,
            attention,
            attention_owners: _,
            supervisor: _,
            supervisor_revision: _,
            questions,
            autopilot,
            land_at,
            autopilot_set_by: _,
            autopilot_revision: _,
            driver_writes: _,
        } = self;
        *user_title == other.user_title
            && *description == other.description
            && *icon == other.icon
            && *role == other.role
            && *attention == other.attention
            && *questions == other.questions
            && *autopilot == other.autopilot
            && *land_at == other.land_at
    }
}

impl Eq for SessionMeta {}

impl SessionMeta {
    /// Whether any field is set: the `sessions` listing's `meta=<1|0>` bit.
    /// `questions` and `autopilot` count like every other user field.
    #[must_use]
    pub(crate) fn any_set(&self) -> bool {
        self.user_title.is_some()
            || self.description.is_some()
            || self.icon.is_some()
            || self.role.is_some()
            || self.attention.is_some()
            || self.questions.is_some()
            || self.autopilot.is_some()
    }

    /// The session's Autopilot word, typed ([`Self::autopilot`]): `None` while unset (the
    /// table's default decides) or for a stored spelling this build cannot read. The stored
    /// word is matched first, without allocating, since the footer reads it every frame
    /// (`App::session_autopilot`); any other spelling goes through the supervisor's reader.
    #[must_use]
    pub(crate) fn autopilot_word(&self) -> Option<aterm_agent::supervise::Autopilot> {
        use aterm_agent::supervise::Autopilot;
        let stored = self.autopilot.as_deref()?;
        Autopilot::ALL
            .into_iter()
            .find(|mode| mode.word() == stored)
            .or_else(|| Autopilot::parse(stored))
    }

    /// Whether a driver wrote `field` on this process (see
    /// [`Self::driver_writes`]).
    #[must_use]
    pub(crate) const fn driver_wrote(&self, field: MetaField) -> bool {
        self.driver_writes & field.bit() != 0
    }

    /// The named field's current value (`None` for an unknown field name —
    /// callers validate names before writing; reading is total).
    #[must_use]
    pub fn get(&self, field: &str) -> Option<&str> {
        match field {
            "title" => self.user_title.as_deref(),
            "description" => self.description.as_deref(),
            "icon" => self.icon.as_deref(),
            "role" => self.role.as_deref(),
            "attention" => self.attention.as_deref(),
            "questions" => self.questions.as_deref(),
            "autopilot" => self.autopilot.as_deref(),
            _ => None,
        }
    }

    /// The named field after the canonical single-line presentation policy, which also covers
    /// a value that bypassed the setter (a hand-edited restore manifest).
    #[must_use]
    pub(crate) fn presentation_value(&self, field: &str) -> Option<String> {
        self.get(field)
            .and_then(|value| sanitize_metadata_value(field, value))
    }

    /// [`Self::presentation_value`] without allocating, for the render path:
    /// writes the canonical value into `out` and returns whether the field is
    /// set (and non-empty after the policy). `out` is left untouched when the
    /// answer is `false`, so the tab-strip refill's try-lock fallback can keep
    /// a stale value in a resident slot.
    pub(crate) fn presentation_value_into(&self, field: &str, out: &mut String) -> bool {
        let Some(cap) = Self::cap(field) else {
            return false;
        };
        let Some(value) = self.get(field) else {
            return false;
        };
        // Fixpoint of `sanitize_presentation_line`: nothing to filter or trim
        // and within the cap. Every production writer sanitizes through `set`,
        // so this is the steady-state path: one scan, no allocation.
        if value.len() <= cap
            && value.trim().len() == value.len()
            && !metadata_has_forbidden_formatting(value)
        {
            if value.is_empty() {
                return false;
            }
            out.clear();
            out.push_str(value);
            return true;
        }
        // Non-canonical (an older in-process caller, a hand-edited manifest):
        // the full sanitizer, which is the bidi-override / invisible-character
        // guard.
        match sanitize_metadata_value(field, value) {
            Some(sanitized) => {
                out.clear();
                out.push_str(&sanitized);
                true
            }
            None => false,
        }
    }

    /// A canonical bounded copy for restore/handoff persistence, values only
    /// (driver provenance does not travel).
    ///
    /// Attention travels as the bare owner's entry only: one carried under the bare owner
    /// could never be cleared by its real owner. Keyed entries cross a seamless update
    /// separately ([`Self::handoff_attention_owners`]); after a restore their driver
    /// re-asserts them. The retired hooks' badge does not travel ([`carried_attention`]).
    #[must_use]
    pub(crate) fn sanitized(&self) -> Self {
        let attention = if self.attention_owners.is_empty() {
            self.attention.as_deref()
        } else {
            self.attention_owners.get(BARE_ATTENTION_OWNER)
        }
        .and_then(carried_attention);
        let (autopilot, land_at) = self.carried_autopilot();
        // Its setter goes with the word, and where the word is dropped, so
        // is it (§11.7.3, item 5: `carried_land_at`'s rule).
        let autopilot_set_by = self.autopilot_set_by.filter(|_| autopilot.is_some());
        Self {
            user_title: self.presentation_value("title"),
            description: self.presentation_value("description"),
            icon: self.presentation_value("icon"),
            role: self.presentation_value("role"),
            attention,
            attention_owners: AttentionOwners::default(),
            supervisor: None,
            supervisor_revision: 0,
            questions: self.presentation_value("questions"),
            autopilot,
            land_at,
            autopilot_set_by,
            autopilot_revision: 0,
            driver_writes: 0,
        }
    }

    /// THE AUTOPILOT PAIR AS A RESTORE LAYOUT CARRIES IT: the word in its
    /// presentation form, and the timer beside it — which travels with the
    /// word it goes with, and only with `cruise` (§9.3.6, item 2). The one
    /// projection [`Self::sanitized`] (every capture's) and the crash
    /// journal's check of what its last image carries
    /// (`App::journal_owner_words`) both read, so the two never disagree.
    #[must_use]
    pub(crate) fn carried_autopilot(&self) -> (Option<String>, Option<i64>) {
        (
            self.presentation_value("autopilot"),
            self.land_at.filter(|_| {
                self.autopilot_word() == Some(aterm_agent::supervise::Autopilot::Cruise)
            }),
        )
    }

    /// Set (or with `None`, clear) `owner`'s attention entry and re-project
    /// [`Self::attention`]. `value` is stored as given (the caller validated it:
    /// [`validated_attention_value`]). Returns whether the effective value
    /// moved, or [`AttentionOwnersFull`] when `owner` is new and the map is at
    /// [`ATTENTION_OWNERS_MAX`] (nothing changed).
    pub(crate) fn set_attention_owned(
        &mut self,
        owner: &str,
        value: Option<String>,
    ) -> Result<bool, AttentionOwnersFull> {
        self.set_attention_kind(owner, value, None)
            .map(|moved| moved.effective)
    }

    /// [`Self::set_attention_owned`] with the entry's kind (`meta set
    /// attention owner=<k> kind=<ask|wait>`; `None`: the writer named none,
    /// which the need classifier reads by its legacy rule). The bare owner's
    /// entry is always an ask. Returns what moved ([`AttentionMove`]): the
    /// effective value, and any entry at all, which the need reads.
    pub(crate) fn set_attention_kind(
        &mut self,
        owner: &str,
        value: Option<String>,
        kind: Option<aterm_types::needs::AttentionKind>,
    ) -> Result<AttentionMove, AttentionOwnersFull> {
        self.adopt_literal_attention();
        let entry = self.attention_owners.put(owner, value, kind)?;
        let effective = self.attention_owners.effective().map(str::to_owned);
        let changed = self.attention != effective;
        self.attention = effective;
        Ok(AttentionMove {
            effective: changed,
            entry,
        })
    }

    /// The bare field (`meta set|unset attention` with no `owner=`): the bare
    /// owner's entry, `value` sanitized as every field is. The bare owner is
    /// never refused for capacity.
    pub(crate) fn set_bare_attention(&mut self, value: Option<String>) -> AttentionMove {
        let value = value.and_then(|value| sanitize_metadata_value("attention", &value));
        self.set_attention_kind(BARE_ATTENTION_OWNER, value, None)
            .unwrap_or_default()
    }

    /// A meta built as a struct literal (a restore seed, a test) carries its
    /// attention in [`Self::attention`] with an empty map. Before the first
    /// keyed write, that value becomes the bare owner's entry, so a keyed
    /// write on top of it — and the clear after — cannot lose it.
    fn adopt_literal_attention(&mut self) {
        if self.attention_owners.is_empty()
            && let Some(value) = self.attention.clone()
        {
            let _ = self
                .attention_owners
                .put(BARE_ATTENTION_OWNER, Some(value), None);
        }
    }

    /// The supervisor's holder name while its claim is live at `now_us`
    /// ([`crate::metrics::now_us`]); a lapsed `ttl=` claim reads as none.
    #[must_use]
    pub(crate) fn live_supervisor(&self, now_us: u64) -> Option<&str> {
        self.live_claim(now_us).map(|claim| claim.holder.as_str())
    }

    /// The live claim itself at `now_us` (a lapsed `ttl=` claim reads as
    /// none): its holder, binding and proof.
    #[must_use]
    pub(crate) fn live_claim(&self, now_us: u64) -> Option<&SupervisorClaim> {
        self.supervisor
            .as_ref()
            .filter(|claim| claim.expires_us.is_none_or(|at| at > now_us))
    }

    /// Replace the claim and bump [`Self::supervisor_revision`], invalidating reads admitted
    /// under the old one. Returns the new revision.
    fn set_supervisor(&mut self, claim: Option<SupervisorClaim>) -> u64 {
        self.supervisor = claim;
        self.supervisor_revision = self.supervisor_revision.wrapping_add(1);
        self.supervisor_revision
    }

    /// Whether the live claim at `now_us` is a proven self-claim ([`crate::self_claim`]):
    /// `status` and `meta` print `supervisor_proven=<0|1>`.
    #[must_use]
    pub(crate) fn supervisor_proven(&self, now_us: u64) -> bool {
        self.live_claim(now_us)
            .is_some_and(|claim| claim.proven.is_some())
    }

    /// When the `ttl=` claim lapses ([`crate::metrics::now_us`]), live or not;
    /// `None` without a claim or for a connection-bound one. The presence timer
    /// wakes at it ([`lapse_supervisor`]).
    #[must_use]
    pub(crate) fn supervisor_expiry(&self) -> Option<u64> {
        self.supervisor.as_ref().and_then(|claim| claim.expires_us)
    }

    /// The claim a seamless update carries at `now_us`, as `(row, claim_known)` for the
    /// handoff record (`SessionRecord::supervisor` / `claim_known`):
    ///
    /// * no live claim, or one held by `own_holder` (this process's in-GUI
    ///   host, whose successor host takes the session at Commit):
    ///   `(None, true)`, so the successor may supervise at once;
    /// * a live `ttl=` lease: `(Some("<holder> <remaining_ms>"), true)`, put
    ///   back on the successor's session before it is registered, so its holder
    ///   renews it there and the successor's host parks behind it;
    /// * a connection-bound claim: `(None, false)`; the connection ends with
    ///   this process, and `false` gives the holder time to claim again from
    ///   the successor ([`crate::harness_host`]'s adopted-claim grace).
    ///
    /// Carrying our own host's lease would park the successor's host behind a
    /// dead process's claim.
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) fn handoff_supervisor(
        &self,
        now_us: u64,
        own_holder: &str,
    ) -> (Option<String>, bool) {
        let Some(claim) = self
            .supervisor
            .as_ref()
            .filter(|claim| claim.expires_us.is_none_or(|at| at > now_us))
        else {
            return (None, true);
        };
        if claim.holder == own_holder {
            return (None, true);
        }
        // A proven self-claim is never carried: its proof is this instance's connection and
        // process. Its holder claims again from the successor inside the adopted-claim
        // grace, proven there.
        if claim.proven.is_some() {
            return (None, false);
        }
        match claim.expires_us {
            // Rounded up to a whole ms, so a live lease never carries as 0.
            Some(at) => (
                Some(format!(
                    "{} {}",
                    claim.holder,
                    at.saturating_sub(now_us).div_ceil(1000)
                )),
                true,
            ),
            None => (None, false),
        }
    }

    /// The keyed escalations a seamless update carries: every owner's attention
    /// entry, `"<owner> [kind=<ask|wait>] <pct-text>"`, in write order (oldest
    /// first, so the successor shows the same one), the bare owner's included,
    /// but never [`crate::input_stall::SERVER_ATTENTION_OWNER`]'s, which the
    /// successor's input watch derives again. An escalation is a request for a
    /// person, so it must survive the update; and so must its kind: a wait read
    /// with no kind is an ask to every reader but the window harness's own
    /// (the need classifier's legacy rule), so a limit the harness or another
    /// supervisor waits out would need the owner in the successor until its
    /// writer said it again. A map built as a literal (its bare value in
    /// [`Self::attention`] alone) carries nothing here; that value crosses in
    /// the restore leaf.
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) fn handoff_attention_owners(&self) -> Vec<String> {
        self.attention_owners
            .in_write_order()
            .into_iter()
            .filter(|(owner, _, _)| *owner != crate::input_stall::SERVER_ATTENTION_OWNER)
            .map(|(owner, text, kind)| {
                let text = crate::control::pct_encode(text);
                // The bare owner's entry is an ask by construction: its row
                // stays the one every producer wrote.
                match kind.filter(|_| owner != BARE_ATTENTION_OWNER) {
                    Some(kind) => format!("{owner} kind={} {text}", kind.word()),
                    None => format!("{owner} {text}"),
                }
            })
            .collect()
    }

    /// The byte cap for a named field, or `None` for an unknown field name.
    #[must_use]
    pub(crate) fn cap(field: &str) -> Option<usize> {
        match field {
            "title" => Some(META_TITLE_MAX),
            "description" => Some(META_DESCRIPTION_MAX),
            "icon" => Some(META_ICON_MAX),
            "role" => Some(META_ROLE_MAX),
            "attention" => Some(META_ATTENTION_MAX),
            "questions" => Some(META_QUESTIONS_MAX),
            "autopilot" => Some(META_AUTOPILOT_MAX),
            _ => None,
        }
    }

    /// Set (or with `None`, unset) a named field. Returns `Some(changed)` for a
    /// known field (`changed`: the stored value moved, so callers act only on a
    /// real change), `None` for an unknown name.
    pub(crate) fn set(&mut self, field: &str, value: Option<String>) -> Option<bool> {
        if field == "attention" {
            // The bare field is the bare owner: existing callers (the wire's bare
            // form, a restore seed) keep their meaning, and keyed entries are
            // untouched.
            return Some(self.set_bare_attention(value).effective);
        }
        if field == "autopilot" {
            // The word alone is a hand write of it: its timer goes with it
            // (§9.3.6, item 1).
            return Some(self.set_autopilot(value, None));
        }
        let slot = match field {
            "title" => &mut self.user_title,
            "description" => &mut self.description,
            "icon" => &mut self.icon,
            "role" => &mut self.role,
            "questions" => &mut self.questions,
            _ => return None,
        };
        // User-facing callers reject unsafe/over-cap values visibly; this second
        // gate makes older/internal callers store the same canonical form.
        let value = value.and_then(|value| sanitize_metadata_value(field, &value));
        let changed = *slot != value;
        *slot = value;
        Some(changed)
    }
}

impl SessionMeta {
    /// THE ONE WRITER OF THE AUTOPILOT PAIR (§9.3.6, item 1): store `word`
    /// (`None`: unset; sanitized as [`Self::set`] sanitizes) and `land_at`
    /// (unix ms) together. The timer is kept only beside the word `cruise`
    /// — a `land_at` under any other word, or none, is dropped — so a timer
    /// never outlives its Cruise. Whether EITHER half moved: a write of the
    /// same word that drops a timer is a change, so its record, its wake and
    /// the index follow it. The host's ring follows the word alone
    /// ([`apply_autopilot`]).
    pub(crate) fn set_autopilot(&mut self, word: Option<String>, land_at: Option<i64>) -> bool {
        let word = word.and_then(|value| sanitize_metadata_value("autopilot", &value));
        let cruise = word.as_deref().and_then(normalize_autopilot_value)
            == Some(aterm_agent::supervise::Autopilot::Cruise.word());
        let land_at = land_at.filter(|_| cruise);
        let changed = self.autopilot != word || self.land_at != land_at;
        self.autopilot = word;
        self.land_at = land_at;
        changed
    }

    /// Store `set_by` as the Autopilot pair's setter
    /// ([`Self::autopilot_set_by`]) and step [`Self::autopilot_revision`]:
    /// a write here that moved the pair (§11.7.3, item 1). Called only beside
    /// a [`Self::set_autopilot`] that returned `true`, under the same guard.
    pub(crate) fn note_autopilot_set_by(&mut self, set_by: AutopilotSetBy) {
        self.autopilot_set_by = Some(set_by);
        self.autopilot_revision = self.autopilot_revision.wrapping_add(1);
    }
}

/// One owner's attention entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AttentionEntry {
    owner: String,
    text: String,
    /// What its writer said it is (`kind=`); the bare owner's is always
    /// [`aterm_types::needs::AttentionKind::Ask`]. `None`: a keyed entry
    /// written with no kind (an older writer, a carried row).
    kind: Option<aterm_types::needs::AttentionKind>,
    /// Write order within this map: the highest is the most recent, and the
    /// most recent entry is the effective one.
    stamp: u64,
}

/// A new owner found the map at [`ATTENTION_OWNERS_MAX`]: nothing was stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AttentionOwnersFull;

/// What one attention write moved ([`SessionMeta::set_attention_kind`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AttentionMove {
    /// The effective (most recently written) value moved: what `attention=`
    /// shows and `meta-change field=attention` records.
    pub(crate) effective: bool,
    /// Some entry moved: one came or went, or its text or kind changed. The
    /// need classifier reads every entry, so this alone moves a session's
    /// need (an older ask cleared under a newer wait moves no effective
    /// value).
    pub(crate) entry: bool,
}

impl AttentionMove {
    /// Whether anything a reader of the entries reads moved.
    #[must_use]
    pub(crate) const fn any(self) -> bool {
        self.effective || self.entry
    }
}

/// The keyed attention map ([`SessionMeta::attention_owners`]): at most
/// [`ATTENTION_OWNERS_MAX`] owners, one line each.
///
/// Readers see the most recently written entry: the texts are free form, so
/// recency is the only order a human can predict, and a clear falls back to
/// the next most recent, so one owner's clear never hides another's
/// escalation. Re-setting an entry to its current text does not move it.
#[derive(Clone, Debug, Default)]
pub(crate) struct AttentionOwners {
    entries: Vec<AttentionEntry>,
    next_stamp: u64,
}

impl AttentionOwners {
    /// Whether no owner holds an entry.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many owners hold an entry.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// `owner`'s entry, if it holds one.
    #[must_use]
    pub(crate) fn get(&self, owner: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.owner == owner)
            .map(|entry| entry.text.as_str())
    }

    /// The most recently written entry's text — what [`SessionMeta::attention`]
    /// projects.
    #[must_use]
    pub(crate) fn effective(&self) -> Option<&str> {
        self.top().map(|entry| entry.text.as_str())
    }

    /// The owner of the effective entry (`meta attention_owner=`).
    #[must_use]
    pub(crate) fn effective_owner(&self) -> Option<&str> {
        self.top().map(|entry| entry.owner.as_str())
    }

    fn top(&self) -> Option<&AttentionEntry> {
        self.entries.iter().max_by_key(|entry| entry.stamp)
    }

    /// The most recently written entry's text that `wanted` takes.
    #[must_use]
    pub(crate) fn most_recent_where(&self, wanted: impl Fn(&str) -> bool) -> Option<&str> {
        self.entries
            .iter()
            .filter(|entry| wanted(&entry.text))
            .max_by_key(|entry| entry.stamp)
            .map(|entry| entry.text.as_str())
    }

    /// Every entry as the need classifier reads it (`App::need_facts`),
    /// most recent first.
    #[must_use]
    pub(crate) fn kinded(&self) -> Vec<crate::needs::AttentionFact> {
        let mut entries: Vec<&AttentionEntry> = self.entries.iter().collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.stamp));
        entries
            .into_iter()
            .map(|entry| crate::needs::AttentionFact {
                owner: entry.owner.clone(),
                text: entry.text.clone(),
                kind: entry.kind,
            })
            .collect()
    }

    /// `meta`'s `attention_kinds=`: `<kind>:<owner>` per entry in write order,
    /// oldest first, `,`-joined (`-` for a kind its writer did not name), or
    /// `-` with no entry. An owner token holds no `,`, and the kind word no
    /// `:`, so the first `:` splits each.
    #[must_use]
    pub(crate) fn kinds_field(&self) -> String {
        let mut entries: Vec<&AttentionEntry> = self.entries.iter().collect();
        entries.sort_by_key(|entry| entry.stamp);
        let field: Vec<String> = entries
            .into_iter()
            .map(|entry| {
                format!(
                    "{}:{}",
                    entry
                        .kind
                        .map_or("-", aterm_types::needs::AttentionKind::word),
                    entry.owner
                )
            })
            .collect();
        if field.is_empty() {
            "-".to_string()
        } else {
            field.join(",")
        }
    }

    /// Every `(owner, text, kind)` in write order, oldest first: written again
    /// into an empty map it gives the same effective entry. What a handoff
    /// carries ([`SessionMeta::handoff_attention_owners`]).
    #[cfg(any(unix, test))]
    fn in_write_order(&self) -> Vec<(&str, &str, Option<aterm_types::needs::AttentionKind>)> {
        let mut entries: Vec<&AttentionEntry> = self.entries.iter().collect();
        entries.sort_by_key(|entry| entry.stamp);
        entries
            .into_iter()
            .map(|entry| (entry.owner.as_str(), entry.text.as_str(), entry.kind))
            .collect()
    }

    /// Store (`Some`) or clear (`None`) `owner`'s entry, of `kind` (the bare
    /// owner's is always an ask). Returns whether any entry moved: one was
    /// removed or added, or a standing one's text or kind changed.
    fn put(
        &mut self,
        owner: &str,
        value: Option<String>,
        kind: Option<aterm_types::needs::AttentionKind>,
    ) -> Result<bool, AttentionOwnersFull> {
        let kind = if owner == BARE_ATTENTION_OWNER {
            Some(aterm_types::needs::AttentionKind::Ask)
        } else {
            kind
        };
        let at = self.entries.iter().position(|entry| entry.owner == owner);
        match (at, value) {
            (Some(at), None) => {
                self.entries.remove(at);
                Ok(true)
            }
            (None, None) => Ok(false),
            (Some(at), Some(text)) => {
                let kind_moved = self.entries[at].kind != kind;
                self.entries[at].kind = kind;
                let text_moved = self.entries[at].text != text;
                if text_moved {
                    self.next_stamp += 1;
                    self.entries[at].text = text;
                    self.entries[at].stamp = self.next_stamp;
                }
                Ok(kind_moved || text_moved)
            }
            (None, Some(text)) => {
                // The bare owner always has its slot: it is the field every
                // pre-keyed caller writes, and refusing it would break them.
                if owner != BARE_ATTENTION_OWNER
                    && self
                        .entries
                        .iter()
                        .filter(|entry| entry.owner != BARE_ATTENTION_OWNER)
                        .count()
                        >= ATTENTION_OWNERS_MAX - 1
                {
                    return Err(AttentionOwnersFull);
                }
                self.next_stamp += 1;
                self.entries.push(AttentionEntry {
                    owner: owner.to_string(),
                    text,
                    kind,
                    stamp: self.next_stamp,
                });
                Ok(true)
            }
        }
    }
}

/// A live supervisor's claim on a session ([`SessionMeta::supervisor`]).
///
/// With `ttl=<ms>` it is a lease that lapses at `expires_us` unless the holder re-sets it (how
/// a one-request-per-connection driver like `aterm ctl` keeps it). Without `ttl=` it is bound
/// to the connection that set it (`conn`) and cleared when that connection stops serving
/// requests (closes, or turns into a `subscribe` stream), so a dead supervisor stops showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorClaim {
    /// The holder name the claim was made under.
    pub(crate) holder: String,
    /// The control connection it is bound to, when it has no `ttl=`.
    pub(crate) conn: Option<u64>,
    /// When a `ttl=` claim lapses ([`crate::metrics::now_us`]).
    pub(crate) expires_us: Option<u64>,
    /// The proof, when the kernel proved the claim is its agent's own
    /// ([`crate::self_claim::prove`]); never set from the holder's spelling. `None` for an
    /// ordinary claim.
    pub(crate) proven: Option<crate::self_claim::Proven>,
}

/// Whether `token` is a well-formed attention owner key or supervisor holder name: one wire
/// token of 1..=[`META_OWNER_MAX`] bytes from `[A-Za-z0-9._:@/-]`, so it prints verbatim and
/// cannot be read as a field or an option. A lone `-` passes: it is the bare attention owner,
/// and holder callers refuse it.
#[must_use]
pub(crate) fn valid_owner_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= META_OWNER_MAX
        && token.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'/' | b'-')
        })
}

/// The user-metadata fields as a closed type. Everything past the wire parse
/// boundary carries this instead of a `&str` name, so the unknown-field case is
/// unreachable and the `meta-change` record prints a `'static` token directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaField {
    /// `title` — the operator's display title, top rung of the label chain.
    Title,
    /// `description` — free-text purpose/notes an agent leaves behind.
    Description,
    /// `icon` — an emoji / short token for the strip.
    Icon,
    /// `role` — typed role token (a label; no value is special).
    Role,
    /// `attention` — typed needs-human escalation message (non-empty ⇒ badge).
    Attention,
    /// `questions` — the typed question policy the in-window supervisor reads for this
    /// session (a closed set, [`QUESTIONS_POLICY_WORDS`]).
    Questions,
    /// `autopilot` — how far the harness carries the session's work (a closed set,
    /// [`normalize_autopilot_value`]).
    Autopilot,
}

impl MetaField {
    /// Every field, in the order `meta` prints them.
    pub(crate) const ALL: [Self; 7] = [
        Self::Title,
        Self::Description,
        Self::Icon,
        Self::Role,
        Self::Attention,
        Self::Questions,
        Self::Autopilot,
    ];

    /// The field list an unknown-field refusal names (`meta set|unset <field>`), in
    /// [`Self::ALL`]'s order: one spelling for the wire and its tests.
    pub(crate) const NAMES: &'static str =
        "title|description|icon|role|attention|questions|autopilot";

    /// This field's bit in [`SessionMeta::driver_writes`].
    const fn bit(self) -> u8 {
        1 << self as u8
    }

    /// Parse a wire field token, or `None` for an unknown name: the one door from the
    /// string vocabulary into the typed one.
    #[must_use]
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "title" => Self::Title,
            "description" => Self::Description,
            "icon" => Self::Icon,
            "role" => Self::Role,
            "attention" => Self::Attention,
            "questions" => Self::Questions,
            "autopilot" => Self::Autopilot,
            _ => return None,
        })
    }

    /// The stable token [`SessionMeta::set`]/[`SessionMeta::get`] key on and the
    /// `meta-change` payload prints. A closed vocabulary, safe to print verbatim.
    #[must_use]
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Description => "description",
            Self::Icon => "icon",
            Self::Role => "role",
            Self::Attention => "attention",
            Self::Questions => "questions",
            Self::Autopilot => "autopilot",
        }
    }

    /// This field's byte cap, applied to the trimmed value.
    #[must_use]
    pub(crate) const fn cap(self) -> usize {
        match self {
            Self::Title => META_TITLE_MAX,
            Self::Description => META_DESCRIPTION_MAX,
            Self::Icon => META_ICON_MAX,
            Self::Role => META_ROLE_MAX,
            Self::Attention => META_ATTENTION_MAX,
            Self::Questions => META_QUESTIONS_MAX,
            Self::Autopilot => META_AUTOPILOT_MAX,
        }
    }
}

/// What a metadata write intends. `Clear` is distinct from `Set("")`: both
/// store `None`, but they record different timeline payloads (`value=-` is the
/// cleared marker; `value=` is not), which `events` consumers distinguish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaEdit<'a> {
    /// Store this value, after the full validation ladder.
    Set(&'a str),
    /// Unset the field — labels fall back down the chain.
    Clear,
}

/// Why a metadata write was refused. Every variant is user-visible: the control
/// arm renders it as its `ERR` line, a GUI caller as an inline rejection. A
/// value is never silently truncated or stripped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaWriteError {
    /// The value was empty (or whitespace-only). `Set` cannot clear.
    Empty,
    /// Control, bidi, or invisible formatting characters were present.
    ForbiddenFormatting,
    /// Over the field's byte cap (which is carried so the caller can name it).
    TooLong {
        /// The exceeded [`MetaField::cap`].
        cap: usize,
    },
    /// A `questions` value outside its closed set
    /// ([`normalize_questions_value`]); the wire answers [`questions_usage`].
    NotAQuestionsPolicy,
    /// An `autopilot` value that names no mode
    /// ([`normalize_autopilot_value`]); the wire answers [`AUTOPILOT_USAGE`].
    NotAnAutopilot,
}

/// Pure validation: the `meta set` ladder minus the store (trim, empty
/// rejection, forbidden-formatting rejection, byte cap, then canonicalization
/// to the stored representation). No locks and no ctx.
///
/// `Ok(None)` comes only from [`MetaEdit::Clear`]: a `Set` that survives the
/// ladder always has a canonical value.
pub(crate) fn validated_meta_value(
    field: MetaField,
    edit: MetaEdit<'_>,
) -> Result<Option<String>, MetaWriteError> {
    let MetaEdit::Set(value) = edit else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(MetaWriteError::Empty);
    }
    // The two typed-vocabulary fields: each one's closed set is the whole
    // ladder, and it stores the normalized spelling, not the typed one.
    if field == MetaField::Questions {
        return normalize_questions_value(value)
            .map(|word| Some(word.to_string()))
            .ok_or(MetaWriteError::NotAQuestionsPolicy);
    }
    if field == MetaField::Autopilot {
        return normalize_autopilot_value(value)
            .map(|word| Some(word.to_string()))
            .ok_or(MetaWriteError::NotAnAutopilot);
    }
    if metadata_has_forbidden_formatting(value) {
        return Err(MetaWriteError::ForbiddenFormatting);
    }
    let cap = field.cap();
    if value.len() > cap {
        return Err(MetaWriteError::TooLong { cap });
    }
    Ok(Some(
        sanitize_metadata_value(field.wire_name(), value)
            .expect("non-empty validated metadata has a canonical value"),
    ))
}

/// Apply one validated metadata mutation and, on an actual change, record the
/// `meta-change` timeline event (`field=<f> value=<pct|->`). Returns whether
/// the stored value moved, the caller's gate for the wake and subscriber
/// fan-out (see the `meta` dispatch arm and the GUI rename commit). For
/// `attention` that is any entry moving ([`AttentionMove::any`]): clearing
/// the bare entry under a newer keyed one moves no effective value, so no
/// event, but it moves the need, which only the wake republishes.
///
/// Every call, a no-op one included, marks the field in
/// [`SessionMeta::driver_writes`]: each such write was answered `OK`.
///
/// Atomicity: the timeline record happens while the meta guard is held (the
/// one sanctioned meta→timeline nesting, documented on `SessionCtx`), so two
/// racing `meta set`s on the control worker threads cannot record their events
/// in the opposite order to their stores. Deadlock-free: the one other nested
/// site ([`restore_carried_meta`]) takes them in the same order, and timeline
/// is a leaf everywhere.
///
/// The record is taken under the meta guard (the one sanctioned meta→timeline nesting,
/// documented on `SessionCtx`), so racing `meta set`s cannot record events in the opposite
/// order to their stores; [`restore_carried_meta`] nests in the same order and timeline is
/// a leaf. Guards are released on return: a GUI caller refreshes tab chrome right after,
/// re-taking `ctx.meta`, and `std::sync::Mutex` is not reentrant.
///
/// An Autopilot word that moved rings the window's host ([`crate::harness_host::ring`],
/// after the guards are released), whoever wrote it (the wire, or the window's chip and
/// Settings, [`crate::autopilot`]): on the pass the ring brings, the host stops the
/// session's worker for `off` and starts one for `cruise` or `land`. The footer repaint is
/// the caller's, which knows the session's local id.
pub(crate) fn apply_meta_value(
    ctx: &crate::SessionCtx,
    field: MetaField,
    value: Option<String>,
    by: aterm_slog::Actor,
) -> bool {
    if field == MetaField::Autopilot {
        // The word written by hand: its timer, if any, goes with it.
        // A wire write (or an in-process one through it): no window surface.
        return apply_autopilot(ctx, value, None, by, None).moved;
    }
    let payload = meta_change_payload(field, value.as_deref());
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.driver_writes |= field.bit();
    let (changed, moved) = if field == MetaField::Attention {
        let moved = meta.set_bare_attention(value);
        (moved.effective, moved.any())
    } else {
        let changed = meta.set(field.wire_name(), value).unwrap_or(false);
        (changed, changed)
    };
    if changed {
        let change = MetaChange::written_by(field.wire_name(), meta.get(field.wire_name()), by);
        record_meta(ctx, payload, change, &meta);
    }
    drop(meta);
    if changed && field == MetaField::Autopilot {
        crate::harness_host::ring();
    }
    moved
}

/// What one write of the Autopilot pair did ([`apply_autopilot`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AutopilotWrite {
    /// Either half moved: the record, the wake, the footer and the timer's
    /// index follow it.
    pub(crate) moved: bool,
    /// The word moved: the window's host was rung.
    pub(crate) word_moved: bool,
    /// The session's [`SessionMeta::autopilot_revision`] after the write:
    /// the one this write stored where it `moved`.
    pub(crate) revision: u64,
}

/// WHO SET A SESSION'S AUTOPILOT PAIR, AND WHEN
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.7.3, item 1;
/// [`SessionMeta::autopilot_set_by`]). Named `set_by`, never "hand": the
/// timer's model keeps "hand" for a write with no timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AutopilotSetBy {
    /// The writer as the session log names it ([`aterm_slog::Actor`]):
    /// `Human` for the window, `Ctl` an Owner client over `aterm ctl`,
    /// `Harness` aterm's own harness, `Bridge` the fabric, `Aterm` the
    /// timer's fire, a session's sid its edge connection.
    pub(crate) actor: aterm_slog::Actor,
    /// The window surface pressed, for a window write; `None` for every
    /// other writer.
    pub(crate) surface: Option<crate::autopilot::Surface>,
    /// When, unix ms: the wall clock at the write.
    pub(crate) since_ms: i64,
}

impl AutopilotSetBy {
    /// A write by `actor` from `surface`, now.
    #[must_use]
    pub(crate) fn now(
        actor: aterm_slog::Actor,
        surface: Option<crate::autopilot::Surface>,
    ) -> Self {
        Self {
            actor,
            surface,
            since_ms: crate::autopilot_timers::wall_ms(),
        }
    }

    /// The setter's word, as `meta`'s `autopilot_by=` and the restore leaf
    /// write it (§11.7.2): `human:chip`, `human:border`, `human:menu`,
    /// `human:settings`, `human:every`; else the actor's own word (`ctl`,
    /// `harness`, `bridge`, `aterm`, a session's sid, `-`).
    #[must_use]
    pub(crate) fn word(&self) -> String {
        match (self.actor, self.surface) {
            (aterm_slog::Actor::Human, Some(surface)) => format!("human:{}", surface.word()),
            (actor, _) => actor.word().to_string(),
        }
    }

    /// As the restore leaf carries it (§11.7.3, item 5): `<setter> <unix
    /// ms>`, the setter by [`Self::word`].
    #[must_use]
    pub(crate) fn carried(&self) -> String {
        format!("{} {}", self.word(), self.since_ms)
    }

    /// A leaf's `autopilot_by` read back ([`Self::carried`]'s form): `None`
    /// where it does not parse — a hand-edited or unknown setter, a surface
    /// this build does not know — so the word reads as restored with no
    /// setter, never as one it was not.
    #[must_use]
    pub(crate) fn parse_carried(text: &str) -> Option<Self> {
        let (setter, since) = text.trim().split_once(' ')?;
        let since_ms = since.trim().parse::<i64>().ok()?;
        let (actor, surface) = match setter.strip_prefix("human:") {
            Some(surface) => (
                aterm_slog::Actor::Human,
                Some(crate::autopilot::Surface::from_word(surface)?),
            ),
            // A known writer's word, or a minted sid (an edge writer,
            // `session_log::sid_actor`'s rule); any other word is unknown.
            None => match aterm_slog::Actor::from_word(setter) {
                aterm_slog::Actor::Sid(_) if !aterm_session::is_minted_sid(setter) => {
                    return None;
                }
                aterm_slog::Actor::Nobody if setter != "-" => return None,
                actor => (actor, None),
            },
        };
        Some(Self {
            actor,
            surface,
            since_ms,
        })
    }
}

/// Write session `ctx`'s AUTOPILOT PAIR (§9.3.6, item 1): its word (`None`:
/// unset; a value [`validated_meta_value`] produced) and the instant a timed
/// Cruise lands (`land_at`, unix ms; kept only beside `cruise`), as ONE step
/// under the meta guard ([`SessionMeta::set_autopilot`]). The wire's `meta
/// set|unset autopilot … [until=]` and the window's own writer
/// (`App::set_session_autopilot_timed`) both come here. Marks the autopilot
/// field driver-written — the pair rides its bit — and, when either half
/// moved, records the `meta-change` (`field=autopilot value=<word|->`, with
/// ` land_at=<unix s>` while a timer is armed) under the guard, as
/// [`apply_meta_value`] does. A word that moved rings the window's host
/// after the guards are released; a timer alone does not (the loop reads
/// the word, never the timer).
///
/// WHO SET IT (§11.7.3, item 1): where the pair moved, `by` and the window
/// `surface` pressed (`None` for a wire write) are stored as its setter
/// ([`SessionMeta::autopilot_set_by`]) under the same guard, and the
/// session's [`SessionMeta::autopilot_revision`] steps; a write that moves
/// nothing (the same word again) stores neither.
pub(crate) fn apply_autopilot(
    ctx: &crate::SessionCtx,
    word: Option<String>,
    land_at: Option<i64>,
    by: aterm_slog::Actor,
    surface: Option<crate::autopilot::Surface>,
) -> AutopilotWrite {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.driver_writes |= MetaField::Autopilot.bit();
    let before = meta.autopilot.clone();
    let moved = meta.set_autopilot(word, land_at);
    let word_moved = meta.autopilot != before;
    if moved {
        meta.note_autopilot_set_by(AutopilotSetBy::now(by, surface));
        record_autopilot(ctx, &meta, by);
    }
    let revision = meta.autopilot_revision;
    drop(meta);
    if word_moved {
        crate::harness_host::ring();
    }
    AutopilotWrite {
        moved,
        word_moved,
        revision,
    }
}

/// What THE FIRE of a timed Cruise found (§9.3.6, item 5; [`fire_land_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fired {
    /// The stored pair was the timer's, and its instant had come: `land`
    /// is stored, with no timer, at this [`SessionMeta::autopilot_revision`]
    /// (its setter `aterm`, the timer's: §11.7.3, item 1).
    Landed { revision: u64 },
    /// The stored pair is the timer's, but the wall clock is short of its
    /// instant by this many ms (set back, or the monotonic deadline ran
    /// early): nothing written; indexed again from the remainder.
    NotYet(i64),
    /// The stored pair is no longer the timer's — the word is not `cruise`,
    /// or the instant moved — so nothing is written; indexed again from what
    /// is stored (`Some`), or not at all.
    Moved(Option<i64>),
}

/// THE FIRE IS A COMPARE-AND-SET (§9.3.6, item 5): under session `ctx`'s
/// meta guard, write `land` (and drop the timer) only where the STORED pair
/// is still `cruise` with `land_at == expected_ms` and the wall clock
/// (`wall_ms`, read once by the caller) is at or past it. The App's index
/// says only when to look: a hand write stored before this — `meta set
/// autopilot off` over the wire whose wake the App has not applied yet, the
/// same word again, a new arm — leaves a pair this does not match, so the
/// hand always wins. A write here is this process's: it marks the
/// autopilot `driver_writes` bit, so a later graft of a carried identity
/// never reverts it, and records the `meta-change` under the guard as
/// [`apply_meta_value`] does. The word moved, so the host is rung after the
/// guard is released; the rest of the fan-out (the footer, the A/B arm, the
/// page, the message) is the App's.
pub(crate) fn fire_land_at(ctx: &crate::SessionCtx, expected_ms: i64, wall_ms: i64) -> Fired {
    use aterm_agent::supervise::Autopilot;
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let ours =
        meta.autopilot_word() == Some(Autopilot::Cruise) && meta.land_at == Some(expected_ms);
    if !ours {
        return Fired::Moved(meta.land_at);
    }
    if wall_ms < expected_ms {
        return Fired::NotYet(expected_ms - wall_ms);
    }
    meta.driver_writes |= MetaField::Autopilot.bit();
    meta.set_autopilot(Some(Autopilot::Land.word().to_string()), None);
    // The timer's write: aterm's, stored as its setter at the fire's wall.
    meta.note_autopilot_set_by(AutopilotSetBy {
        actor: aterm_slog::Actor::Aterm,
        surface: None,
        since_ms: wall_ms,
    });
    record_autopilot(ctx, &meta, aterm_slog::Actor::Aterm);
    let revision = meta.autopilot_revision;
    drop(meta);
    crate::harness_host::ring();
    Fired::Landed { revision }
}

/// THE CANCEL IS A COMPARE-AND-SET TOO (§9.3.3, §9.3.4: "Cancel landing"
/// is offered only while a timer is armed): under session `ctx`'s meta
/// guard, drop the timer only where the STORED pair is still `cruise` with
/// a `land_at` — the very instant `expected` names, where the row that asked
/// named one — and keep the word as stored. Anything else (a hand write of
/// `off` or `land` stored first, the timer's own fire, a new arm the row
/// never showed) writes nothing, so a cancel offered by a page or a menu
/// that has gone stale can never write `cruise` over a word the person or
/// the timer wrote. A drop is a hand write of the pair: it marks the
/// autopilot `driver_writes` bit and records the `meta-change` under the
/// guard, as [`apply_autopilot`] does, with the person and the `surface`
/// the cancel was pressed on as its setter; the word did not move, so the
/// host is not rung. What it did.
pub(crate) fn cancel_land_at(
    ctx: &crate::SessionCtx,
    expected: Option<i64>,
    surface: crate::autopilot::Surface,
) -> AutopilotWrite {
    use aterm_agent::supervise::Autopilot;
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let armed = meta.autopilot_word() == Some(Autopilot::Cruise)
        && meta
            .land_at
            .is_some_and(|at| expected.is_none_or(|expected| expected == at));
    if !armed {
        return AutopilotWrite {
            revision: meta.autopilot_revision,
            ..AutopilotWrite::default()
        };
    }
    meta.driver_writes |= MetaField::Autopilot.bit();
    let word = meta.autopilot.clone();
    let moved = meta.set_autopilot(word, None);
    if moved {
        // A page's or a menu's cancel: the person's.
        meta.note_autopilot_set_by(AutopilotSetBy::now(aterm_slog::Actor::Human, Some(surface)));
        record_autopilot(ctx, &meta, aterm_slog::Actor::Human);
    }
    AutopilotWrite {
        moved,
        word_moved: false,
        revision: meta.autopilot_revision,
    }
}

/// The `meta-change` payload for the Autopilot pair as `meta` now stores it:
/// [`meta_change_payload`]'s `field=autopilot value=<pct|->`, and
/// ` land_at=<unix s>` after it while a timer is armed — so a write that
/// only drops a timer reads as the change it is.
fn autopilot_change_payload(meta: &SessionMeta) -> String {
    let mut payload = meta_change_payload(MetaField::Autopilot, meta.autopilot.as_deref());
    if let Some(ms) = meta.land_at {
        payload.push_str(" land_at=");
        payload.push_str(&ms.div_euclid(1000).to_string());
    }
    payload
}

/// Why a keyed attention write was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttentionWriteError {
    /// The value failed the ordinary metadata ladder.
    Meta(MetaWriteError),
    /// `owner` is new and [`ATTENTION_OWNERS_MAX`] owners already hold one.
    OwnersFull,
}

/// Pure validation for one owner's attention text: [`validated_meta_value`]'s
/// ladder, with the keyed cap ([`META_ATTENTION_KEYED_MAX`]) for a keyed owner
/// and the bare field's cap for the bare one.
pub(crate) fn validated_attention_value(
    owner: &str,
    edit: MetaEdit<'_>,
) -> Result<Option<String>, MetaWriteError> {
    let value = validated_meta_value(MetaField::Attention, edit)?;
    match value {
        Some(value) if owner != BARE_ATTENTION_OWNER && value.len() > META_ATTENTION_KEYED_MAX => {
            Err(MetaWriteError::TooLong {
                cap: META_ATTENTION_KEYED_MAX,
            })
        }
        value => Ok(value),
    }
}

/// `meta set|unset attention owner=<k> …`: validate, store `owner`'s entry and,
/// when the effective value moved, record `meta-change field=attention` under
/// the meta guard as [`apply_meta_value`] does. Marks the attention field
/// driver-written. The supervisor's entry ([`crate::harness_host::ATTENTION_OWNER`])
/// also moves a task record's rev ([`SessionTimeline::note_task_attention`]).
/// Returns whether any entry (the need reads them all: [`AttentionMove`]) or
/// the task record moved — what wakes the session's waiters and its need.
pub(crate) fn write_attention_owned(
    ctx: &crate::SessionCtx,
    owner: &str,
    edit: MetaEdit<'_>,
    by: aterm_slog::Actor,
) -> Result<bool, AttentionWriteError> {
    write_attention_kind(ctx, owner, edit, None, by)
}

/// [`write_attention_owned`] with the entry's kind (`kind=`, `None` when the
/// writer named none): a kind that moves alone is news too (the session's
/// need moves).
pub(crate) fn write_attention_kind(
    ctx: &crate::SessionCtx,
    owner: &str,
    edit: MetaEdit<'_>,
    kind: Option<aterm_types::needs::AttentionKind>,
    by: aterm_slog::Actor,
) -> Result<bool, AttentionWriteError> {
    let value = validated_attention_value(owner, edit).map_err(AttentionWriteError::Meta)?;
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let moved = meta
        .set_attention_kind(owner, value, kind)
        .map_err(|AttentionOwnersFull| AttentionWriteError::OwnersFull)?;
    let changed = moved.effective;
    meta.driver_writes |= MetaField::Attention.bit();
    // The supervisor's own entry, and a worker's NEEDS YOU entry, are also
    // its task record's news (`task_record`): raised or cleared, a tasked
    // session's rev moves, under this meta guard, so `await task` sees the
    // edge the flag records (it stands while either entry does).
    let supervisor = crate::task_record::owner_moves_task(owner)
        .then(|| crate::task_record::supervisor_attention(&meta));
    let (mut task_moved, mut log_owed) = (false, false);
    if changed || supervisor.is_some() {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        if changed {
            let payload = meta_change_payload(MetaField::Attention, meta.attention.as_deref());
            let change = MetaChange::written_by(
                MetaField::Attention.wire_name(),
                meta.attention.as_deref(),
                by,
            );
            timeline.record_meta_change(payload, change, &meta);
        }
        if let Some(raised) = supervisor {
            task_moved = timeline.note_task_attention(raised);
        }
        // The log's `meta` and `task` marks owe its readers a wake: taken in
        // this hold, sent by the caller after release.
        log_owed = timeline.slog_take_wake();
    }
    drop(meta);
    Ok(moved.any() || task_moved || log_owed)
}

/// THE SUPERVISOR'S ATTENTION ENTRY WRITTEN INSIDE A TASK MARK
/// (`task_record::mark_session_task_with`): `value` (sanitized to one line
/// within [`META_ATTENTION_KEYED_MAX`]; empty is a clear) stored as the
/// [`crate::harness_host::ATTENTION_OWNER`]'s entry under the meta guard and
/// the timeline lock the caller holds (meta → timeline, as
/// [`write_attention_owned`] takes them), the field marked driver-written and
/// `meta-change field=attention` recorded when the effective value moved —
/// what the `meta set attention owner=supervisor` verb does, minus the task
/// record's rev, which the caller's mark moves once with its state. Returns
/// whether the supervisor's entry moved; a map already at
/// [`ATTENTION_OWNERS_MAX`] owners refuses it (`false`, nothing written).
pub(crate) fn put_supervisor_attention(
    meta: &mut SessionMeta,
    timeline: &mut SessionTimeline,
    value: Option<&str>,
) -> bool {
    let owner = crate::harness_host::ATTENTION_OWNER;
    let value = value
        .map(|value| sanitize_presentation_line(value, META_ATTENTION_KEYED_MAX))
        .filter(|value| !value.is_empty());
    if meta.attention_owners.get(owner) == value.as_deref() {
        return false;
    }
    let Ok(changed) = meta.set_attention_owned(owner, value) else {
        return false;
    };
    meta.driver_writes |= MetaField::Attention.bit();
    if changed {
        let payload = meta_change_payload(MetaField::Attention, meta.attention.as_deref());
        // The window host's own deadline wrote it, not a client: aterm's.
        let change = MetaChange::written_by(
            MetaField::Attention.wire_name(),
            meta.attention.as_deref(),
            aterm_slog::Actor::Aterm,
        );
        timeline.record_meta_change(payload, change, meta);
    }
    true
}

/// Store (`Some`) or clear (`None`) the server's own attention entry under `owner` (the
/// input watch's [`crate::input_stall::SERVER_ATTENTION_OWNER`]), recording `meta-change
/// field=attention` when the effective value moves, as [`write_attention_owned`] does. Marks
/// no [`SessionMeta::driver_writes`]. The text quotes a program name, so it is held to the
/// keyed cap and the presentation sanitizer. Returns whether the effective value moved; a
/// full map refuses a new owner (the stall still reads on `status input=` and `agent=`).
pub(crate) fn write_server_attention(
    ctx: &crate::SessionCtx,
    owner: &str,
    value: Option<String>,
) -> bool {
    let value = value
        .map(|value| sanitize_presentation_line(&value, META_ATTENTION_KEYED_MAX))
        .filter(|value| !value.is_empty());
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let Ok(changed) = meta.set_attention_owned(owner, value) else {
        return false;
    };
    if changed {
        record_attention(ctx, &meta, aterm_slog::Actor::Aterm);
    }
    drop(meta);
    changed
}

/// [`claim_supervisor_as`] without a proof, for tests: claim the session for `holder`. `conn`
/// binds a claim with no TTL to the setting connection ([`SupervisorClaim`]); `expires_us` is
/// a TTL claim's lapse instant.
///
/// A live claim by another holder, or a proven one, is refused with its name (`Err`), like
/// `lease acquire`; the same holder renews. Returns whether the shown holder moved; a move
/// records `meta-change field=supervisor value=<holder>` under the meta guard.
#[cfg(test)]
pub(crate) fn claim_supervisor(
    ctx: &crate::SessionCtx,
    holder: &str,
    conn: Option<u64>,
    expires_us: Option<u64>,
    now_us: u64,
) -> Result<bool, String> {
    claim_supervisor_as(
        ctx,
        holder,
        conn,
        expires_us,
        None,
        now_us,
        aterm_slog::Actor::Ctl,
    )
    .map(|c| c.changed)
}

/// What a granted claim did ([`claim_supervisor_as`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claimed {
    /// Whether what `status` and `meta` show moved: the holder, or whether it is proven.
    pub(crate) changed: bool,
    /// The holder a proven self-claim replaced, if any: the window host's own loop (whose
    /// connection is then cut at once) or a proven claim whose process is gone.
    pub(crate) replaced: Option<String>,
    /// The granted request's revision, for its off-lock record read.
    pub(crate) revision: u64,
}

/// How a claim request meets the claim standing ([`claim_rule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaimRule {
    /// No live claim: taken.
    Take,
    /// The same holder: renewed (binding, expiry and proof replaced).
    Renew,
    /// A proven self-claim over the window host's claim, or over a proven
    /// claim whose process is gone.
    Replace,
    /// Refused: `ERR busy supervisor=<holder>`.
    Busy,
}

/// The claim rule (pure). `standing` is the live claim's holder and whether it is proven,
/// `standing_gone` whether a proven standing claim's process is gone, `proven` whether this
/// request was proven. A proven claim is renewed only by a proven request (an unproven one
/// under the same name is a forgery and is refused); a proven request replaces the window
/// host's claim (`aterm-harness@…`, never proven) and a proven claim whose process is gone;
/// every other replacement is refused.
#[must_use]
pub(crate) fn claim_rule(
    standing: Option<(&str, bool)>,
    standing_gone: bool,
    holder: &str,
    proven: bool,
) -> ClaimRule {
    match standing {
        None => ClaimRule::Take,
        Some((h, standing_proven)) if h == holder => {
            if standing_proven && !proven {
                ClaimRule::Busy
            } else {
                ClaimRule::Renew
            }
        }
        Some((h, standing_proven)) => {
            let host = !standing_proven && aterm_agent::harness::upgrade_drive::is_host_holder(h);
            let dead = standing_proven && standing_gone;
            if proven && (host || dead) {
                ClaimRule::Replace
            } else {
                ClaimRule::Busy
            }
        }
    }
}

/// `meta set supervisor <holder> [ttl=<ms>]`: claim the session for `holder` with the
/// request's proof, if any ([`crate::self_claim`]), under [`claim_rule`]. The standing proven
/// claim's process is looked at before the meta guard is taken (a syscall, or `ps` off
/// macOS: never under a leaf lock), and that look counts only while the claim under the
/// guard is still the one looked at; a claim that moved in between counts its process as
/// running (fail closed: busy, never a replacement). Every move, a proof gained or lost by
/// a renewal included, records `meta-change field=supervisor value=<holder>` under the meta
/// guard.
pub(crate) fn claim_supervisor_as(
    ctx: &crate::SessionCtx,
    holder: &str,
    conn: Option<u64>,
    expires_us: Option<u64>,
    proven: Option<crate::self_claim::Proven>,
    now_us: u64,
    by: aterm_slog::Actor,
) -> Result<Claimed, String> {
    let looked = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        meta.live_claim(now_us).and_then(|c| c.proven.clone())
    };
    let looked_gone = looked.as_ref().is_some_and(|p| !p.alive());
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let standing = meta
        .live_claim(now_us)
        .map(|c| (c.holder.clone(), c.proven.clone()));
    let gone = looked_gone
        && standing
            .as_ref()
            .is_some_and(|(_, p)| p.is_some() && *p == looked);
    let rule = claim_rule(
        standing.as_ref().map(|(h, p)| (h.as_str(), p.is_some())),
        gone,
        holder,
        proven.is_some(),
    );
    if rule == ClaimRule::Busy {
        return Err(standing.map(|(h, _)| h).unwrap_or_default());
    }
    let was_proven = standing.as_ref().is_some_and(|(_, p)| p.is_some());
    let now_proven = proven.is_some();
    let renewed_identity_changed = rule == ClaimRule::Renew
        && standing
            .as_ref()
            .is_some_and(|(_, old)| old.as_ref() != proven.as_ref());
    let revision = meta.set_supervisor(Some(SupervisorClaim {
        holder: holder.to_string(),
        conn,
        expires_us,
        proven,
    }));
    let changed = match rule {
        ClaimRule::Renew => was_proven != now_proven,
        ClaimRule::Take | ClaimRule::Replace | ClaimRule::Busy => true,
    };
    if changed {
        record_supervisor(ctx, &meta, Some(holder), by);
    } else if renewed_identity_changed {
        // The existing meta-change rule counts proof presence, not identity.
        // A reused holder spelling under a different birth still owns none
        // of the earlier proven process's pending faces.
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pending_self_tasks
            .clear();
    }
    // The card engine's `holder-claim` dev row (design §3.5, `claim_lead`):
    // a claim taken or taken over, on the present ordinal and process clock
    // the card's rows carry. Under the meta guard, as the row above.
    if crate::holder_image::DEV_ROWS && matches!(rule, ClaimRule::Take | ClaimRule::Replace) {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record(
                "holder-claim",
                format!(
                    "holder={} {}",
                    crate::control::pct_encode(holder),
                    crate::holder_image::dev_tail()
                ),
            );
    }
    // A replacement is answered only once the writes the replaced holder's
    // fence admitted have landed (`FencedWrites`); every later one reads
    // the new holder and is refused.
    let meta = if rule == ClaimRule::Replace {
        settle_fenced_writes(ctx, meta)
    } else {
        meta
    };
    drop(meta);
    let replaced = (rule == ClaimRule::Replace)
        .then(|| standing.map(|(h, _)| h))
        .flatten();
    Ok(Claimed {
        changed,
        replaced,
        revision,
    })
}

/// The `if-supervisor=<holder>` fence, read under the meta lock: `None` while `holder` holds
/// the session's live claim, else the refusal naming who holds it now, `ERR busy
/// supervisor=<holder|->`, which a supervisor's loop reads as another holder's claim
/// (`aterm_agent`'s `claim.rs`) and so watches instead. A write it admits is made under a
/// [`FencedWrite`], never on this answer alone.
#[must_use]
pub(crate) fn supervisor_fence(
    ctx: &crate::SessionCtx,
    holder: &str,
    now_us: u64,
) -> Option<String> {
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    fence_refusal(&meta, holder, now_us)
}

fn fence_refusal(meta: &SessionMeta, holder: &str, now_us: u64) -> Option<String> {
    match meta.live_supervisor(now_us) {
        Some(live) if live == holder => None,
        live => Some(format!(
            "ERR busy supervisor={}\n",
            live.map_or_else(|| "-".to_string(), crate::control::pct_encode)
        )),
    }
}

/// The session's fenced writes in flight: every write an `if-supervisor=` fence admitted
/// that has not finished ([`FencedWrite`]). A proven claim's replacement waits for them
/// ([`claim_supervisor_as`]), so no keystroke the replaced holder was admitted to type lands
/// after the replacement is answered, and every write admitted after it reads the new holder
/// and is refused. The count is raised only under the meta guard that read the fence, and
/// `settled` is waited on only with that guard held.
#[derive(Debug, Default)]
pub(crate) struct FencedWrites {
    inflight: std::sync::atomic::AtomicU32,
    settled: std::sync::Condvar,
}

/// One admitted fenced write, counted in [`FencedWrites`] from the fence's read until it
/// drops. Taken around the write itself (a verb's whole write, or one phase of a `turn`,
/// never a `turn`'s waits), and again on the main thread around a fenced
/// [`crate::Wake::Input`] batch, which re-reads the fence there, so a hop that outlived its
/// worker's deadline is checked when it is applied, not when it was posted.
pub(crate) struct FencedWrite<'a> {
    ctx: &'a crate::SessionCtx,
}

impl<'a> FencedWrite<'a> {
    /// Admit one write under `holder`'s fence: counted, or the refusal
    /// [`supervisor_fence`] gives.
    pub(crate) fn enter(
        ctx: &'a crate::SessionCtx,
        holder: &str,
        now_us: u64,
    ) -> Result<Self, String> {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(refusal) = fence_refusal(&meta, holder, now_us) {
            return Err(refusal);
        }
        ctx.fenced_writes
            .inflight
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        drop(meta);
        Ok(Self { ctx })
    }
}

impl Drop for FencedWrite<'_> {
    fn drop(&mut self) {
        let writes = &self.ctx.fenced_writes;
        writes
            .inflight
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        // Taken between the count and the notify, so a replacement that read
        // a non-zero count under this guard is already waiting: no lost wake.
        let meta = self.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        writes.settled.notify_all();
        drop(meta);
    }
}

/// Wait, under the meta guard `meta`, until no fenced write admitted before
/// it is still in flight. Each one ends within its own write's deadline (a
/// main-thread hop's reply deadline, a background write's receipt wait).
fn settle_fenced_writes<'g>(
    ctx: &crate::SessionCtx,
    meta: std::sync::MutexGuard<'g, SessionMeta>,
) -> std::sync::MutexGuard<'g, SessionMeta> {
    let writes = &ctx.fenced_writes;
    writes
        .settled
        .wait_while(meta, |_| {
            writes.inflight.load(std::sync::atomic::Ordering::SeqCst) > 0
        })
        .unwrap_or_else(|p| p.into_inner())
}

/// Clear a proven claim whose process has exited or no longer leads the foreground group
/// `foreground` (read by the status sweep as the group moved): recorded `meta-change
/// field=supervisor value=-`, and the window host is told, as for any release. Any other
/// claim, or a proven one whose process still leads, is left alone (`false`). The process
/// is looked at outside the meta guard, and only the very claim looked at is cleared.
pub(crate) fn release_proven_if_gone(
    ctx: &crate::SessionCtx,
    foreground: i32,
    now_us: u64,
) -> bool {
    let proven = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        meta.live_claim(now_us).and_then(|c| c.proven.clone())
    };
    let Some(proven) = proven else {
        return false;
    };
    if i64::from(foreground) == i64::from(proven.pid) && proven.alive() {
        return false;
    }
    // Its process left the foreground: aterm's.
    release_supervisor_if(ctx, now_us, aterm_slog::Actor::Aterm, |claim| {
        claim.proven.as_ref() == Some(&proven)
    })
}

/// Clear the session's supervisor claim: every claim when `conn` is `None`
/// (`meta unset supervisor`), or only one bound to connection `conn`. Returns
/// whether a live holder stopped being shown; that move records `meta-change
/// field=supervisor value=-`.
pub(crate) fn release_supervisor(
    ctx: &crate::SessionCtx,
    conn: Option<u64>,
    now_us: u64,
    by: aterm_slog::Actor,
) -> bool {
    release_supervisor_if(ctx, now_us, by, |claim| {
        conn.is_none() || claim.conn == conn
    })
}

/// `meta unset supervisor holder=<h>`: clear the claim only while `holder`
/// holds it, so a supervisor giving back its own claim (after its connection
/// was lost and another holder claimed the session) never clears the other's.
/// Returns what [`release_supervisor`] returns.
pub(crate) fn release_supervisor_held_by(
    ctx: &crate::SessionCtx,
    holder: &str,
    now_us: u64,
    by: aterm_slog::Actor,
) -> bool {
    release_supervisor_if(ctx, now_us, by, |claim| claim.holder == holder)
}

/// Clear the claim when `ours` says so, under the one meta guard that also
/// read it (no gap for another claim between the test and the clear).
fn release_supervisor_if(
    ctx: &crate::SessionCtx,
    now_us: u64,
    by: aterm_slog::Actor,
    ours: impl FnOnce(&SupervisorClaim) -> bool,
) -> bool {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let Some(claim) = meta.supervisor.as_ref() else {
        return false;
    };
    if !ours(claim) {
        return false;
    }
    let was_live = meta.live_supervisor(now_us).is_some();
    let proven = meta.supervisor.take().and_then(|claim| claim.proven);
    let revision = meta.set_supervisor(None);
    if was_live {
        record_supervisor(ctx, &meta, None, by);
    }
    drop(meta);
    // A proven claim's end is told to its copy and the window host
    // (`self_claim::released`), outside every lock: it reads a process.
    if let Some(proven) = proven {
        crate::self_claim::released(ctx, &proven, revision);
    }
    // The in-GUI host claims a session it found held by another supervisor
    // once that claim goes (`harness_host`), so it is told of every release,
    // shown or not (a lapsed TTL claim included).
    crate::harness_host::note_claim_released();
    was_live
}

/// Remove a `ttl=` claim lapsed at `now_us` and record `meta-change field=supervisor value=-`,
/// as a release does: nothing writes when a supervisor dies, so the presence timer calls this
/// at the expiry. A live claim, a connection-bound one or none is left alone (`false`).
pub(crate) fn lapse_supervisor(ctx: &crate::SessionCtx, now_us: u64) -> bool {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if meta.supervisor_expiry().is_none_or(|at| at > now_us) {
        return false;
    }
    let proven = meta.supervisor.take().and_then(|claim| claim.proven);
    let revision = meta.set_supervisor(None);
    // The lease's lapse: aterm's.
    record_supervisor(ctx, &meta, None, aterm_slog::Actor::Aterm);
    drop(meta);
    if let Some(proven) = proven {
        crate::self_claim::released(ctx, &proven, revision);
    }
    crate::harness_host::note_claim_released();
    true
}

/// The longest `ttl=` a `meta set supervisor` accepts (ms): the verb's own
/// bound, and the most a carried lease is seeded with.
pub(crate) const SUPERVISOR_TTL_MAX_MS: u64 = 600_000;

/// A carried supervisor row (`SessionRecord::supervisor`, `"<holder>
/// <remaining_ms>"`), read as the `meta set supervisor … ttl=` verb reads its
/// arguments: the holder one owner token and never `-`, the lease at least a
/// millisecond and cut to [`SUPERVISOR_TTL_MAX_MS`]. `None` for anything else.
#[must_use]
pub(crate) fn parse_carried_supervisor(row: &str) -> Option<(&str, u64)> {
    let (holder, ms) = row.split_once(' ')?;
    if holder == "-" || !valid_owner_token(holder) {
        return None;
    }
    let ms = ms.parse::<u64>().ok().filter(|ms| *ms > 0)?;
    Some((holder, ms.min(SUPERVISOR_TTL_MAX_MS)))
}

/// A carried attention row (`SessionRecord::attention_owners`, `"<owner>
/// [kind=<ask|wait>] <pct-text>"`): the owner one owner token (the bare `-`
/// included) and never the server's own; the kind, where the row names one
/// (a producer before the kind names none, and the entry then reads by the
/// need classifier's legacy rule); the text decoded and put through the
/// chrome's presentation sanitizer at its owner's cap (the keyed cap, or the
/// bare field's). The text is one space-free token (`pct_encode`), so a
/// `kind=` token is told from it by the space after it. A kind outside the
/// set, or one on the bare owner's row, is dropped and the entry kept: a
/// fault in the kind never costs the escalation. `None` when nothing
/// presentable is left.
#[must_use]
pub(crate) fn parse_carried_attention(
    row: &str,
) -> Option<(&str, Option<aterm_types::needs::AttentionKind>, String)> {
    let (owner, rest) = row.split_once(' ')?;
    if !valid_owner_token(owner) || owner == crate::input_stall::SERVER_ATTENTION_OWNER {
        return None;
    }
    let (kind, text) = match rest.split_once(' ') {
        Some((head, text)) if head.starts_with("kind=") => (
            head.strip_prefix("kind=")
                .and_then(aterm_types::needs::AttentionKind::parse)
                .filter(|_| owner != BARE_ATTENTION_OWNER),
            text,
        ),
        _ => (None, rest),
    };
    let cap = if owner == BARE_ATTENTION_OWNER {
        META_ATTENTION_MAX
    } else {
        META_ATTENTION_KEYED_MAX
    };
    let text = sanitize_presentation_line(&aterm_control::wire::pct_decode(text), cap);
    (!text.is_empty()).then_some((owner, kind, text))
}

/// Put back the claim and escalations a seamless update carried onto an adopted session's
/// fresh meta, before `App::register_session`, so no reader sees the session without them
/// and no second supervisor can claim it in between.
///
/// * `supervisor` (a carried `ttl=` lease) becomes the same lease with its
///   remaining time counted from `now_us`: its holder's next renewal renews it
///   here, and the successor's own host is refused as in the parent. A row that
///   does not parse ([`parse_carried_supervisor`]) seeds nothing; the
///   successor's host still waits its grace (`SessionRecord::claim_grace`).
/// * each `attention_owners` row is written again in order, of the kind it
///   names, so the same entry is effective and a wait is still a wait; a row
///   that does not parse ([`parse_carried_attention`]) is
///   dropped, and past [`ATTENTION_OWNERS_MAX`] a new owner is refused as a
///   live write would be.
///
/// A seed marks no [`SessionMeta::driver_writes`]; moves are recorded as ordinary
/// `meta-change` events under the meta guard.
pub(crate) fn seed_carried_claims(
    meta: &std::sync::Mutex<SessionMeta>,
    timeline: &std::sync::Mutex<SessionTimeline>,
    supervisor: Option<&str>,
    attention_owners: &[String],
    now_us: u64,
) {
    let claim = supervisor.and_then(parse_carried_supervisor);
    if claim.is_none() && attention_owners.is_empty() {
        return;
    }
    let mut meta = meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut attention_moved = false;
    for (owner, kind, text) in attention_owners
        .iter()
        .filter_map(|row| parse_carried_attention(row))
    {
        attention_moved |= meta
            .set_attention_kind(owner, Some(text), kind)
            .is_ok_and(|moved| moved.effective);
    }
    if let Some((holder, ms)) = claim {
        meta.set_supervisor(Some(SupervisorClaim {
            holder: holder.to_string(),
            conn: None,
            expires_us: Some(now_us.saturating_add(ms.saturating_mul(1000))),
            // A carried lease is never proven: no proof crosses an update.
            proven: None,
        }));
    }
    // A seed is aterm's (no driver's write), and derives `escalated` too.
    let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
    let by = aterm_slog::Actor::Aterm;
    if attention_moved {
        let payload = meta_change_payload(MetaField::Attention, meta.attention.as_deref());
        let change = MetaChange::written_by(
            MetaField::Attention.wire_name(),
            meta.attention.as_deref(),
            by,
        );
        timeline.record_meta_change(payload, change, &meta);
    }
    if let Some((holder, _)) = claim {
        let change = MetaChange::written_by(SUPERVISOR_FIELD, Some(holder), by);
        timeline.record_meta_change(supervisor_change_payload(Some(holder)), change, &meta);
    }
    drop(timeline);
    drop(meta);
}

/// The `meta-change` kind ([`SessionTimeline::record_meta_change`] alone
/// writes it).
pub(crate) const META_CHANGE: &str = "meta-change";

/// The supervisor key's wire name, as its `meta-change` names it.
pub(crate) const SUPERVISOR_FIELD: &str = "supervisor";

/// One `meta-change`'s typed facts as its writer knows them, for the session
/// log's mark ([`SessionTimeline::record_meta_change`]): the key, the value
/// it now stores (`None`: unset) and who wrote it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MetaChange<'a> {
    pub(crate) field: &'static str,
    pub(crate) value: Option<&'a str>,
    pub(crate) by: aterm_slog::Actor,
}

impl<'a> MetaChange<'a> {
    /// `field` now storing `value`, written by `by`. Named for itself: the
    /// lock-order census resolves a call by its name, and a bare `of` met
    /// `session_store`'s `Drawn::of`, which takes the meta and timeline
    /// locks this writer's callers already hold.
    pub(crate) fn written_by(
        field: &'static str,
        value: Option<&'a str>,
        by: aterm_slog::Actor,
    ) -> Self {
        MetaChange { field, value, by }
    }
}

/// [`SessionTimeline::record_meta_change`] on `ctx`'s timeline, the meta
/// guard (`meta`) held: the one meta → timeline nesting.
fn record_meta(
    ctx: &crate::SessionCtx,
    payload: String,
    change: MetaChange<'_>,
    meta: &SessionMeta,
) {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record_meta_change(payload, change, meta);
}

/// The Autopilot pair's `meta-change` as `meta` now stores it, by `by`.
fn record_autopilot(ctx: &crate::SessionCtx, meta: &SessionMeta, by: aterm_slog::Actor) {
    let change = MetaChange::written_by(
        MetaField::Autopilot.wire_name(),
        meta.autopilot.as_deref(),
        by,
    );
    record_meta(ctx, autopilot_change_payload(meta), change, meta);
}

/// The effective attention's `meta-change` as `meta` now stores it, by `by`.
fn record_attention(ctx: &crate::SessionCtx, meta: &SessionMeta, by: aterm_slog::Actor) {
    let payload = meta_change_payload(MetaField::Attention, meta.attention.as_deref());
    let change = MetaChange::written_by(
        MetaField::Attention.wire_name(),
        meta.attention.as_deref(),
        by,
    );
    record_meta(ctx, payload, change, meta);
}

/// The supervisor key's `meta-change` (`holder`, or `None` for none), by `by`.
fn record_supervisor(
    ctx: &crate::SessionCtx,
    meta: &SessionMeta,
    holder: Option<&str>,
    by: aterm_slog::Actor,
) {
    let change = MetaChange::written_by(SUPERVISOR_FIELD, holder, by);
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    // A logical claim change ends the former holder's pending faces. A
    // renewal of the same proven claim records no change here and keeps them.
    timeline.pending_self_tasks.clear();
    timeline.record_meta_change(supervisor_change_payload(holder), change, meta);
}

/// The `meta-change` payload for the supervisor key: `field=supervisor
/// value=<pct|->`, the attention record's shape.
fn supervisor_change_payload(holder: Option<&str>) -> String {
    let value = holder.map_or_else(|| "-".to_string(), crate::control::pct_encode);
    format!("field=supervisor value={value}")
}

/// The `meta-change` record's payload for `field` now storing `value`:
/// `field=<f> value=<pct|->`. One spelling for both recorders, so a restored
/// field reads on the `events` digest exactly like a `meta set` one.
fn meta_change_payload(field: MetaField, value: Option<&str>) -> String {
    let value = value.map_or_else(|| "-".to_string(), crate::control::pct_encode);
    format!("field={} value={value}", field.wire_name())
}

/// Put a carried identity (the seven user fields a restore leaf captured in the previous
/// process) back onto a session already live here, which a reader may have seen bare and a
/// driver may have written since:
///
/// * A driver's write wins: a field a driver wrote in this process
///   ([`SessionMeta::driver_writes`]) keeps the driver's value, set or
///   cleared, as the newer intent.
/// * Every other field takes the carried value exactly (absent means unset),
///   and a field that moves records the ordinary `meta-change` event under the
///   meta guard, as [`apply_meta_value`] does, so watchers see each field
///   arrive as a `meta set` would, and the chrome cache's `high_id` key moves.
/// * An Autopilot word that moves rings the window's host, as
///   [`apply_meta_value`] does: a session carried in `off` is let alone from
///   the host's next pass.
/// * The Autopilot word and its timer (`land_at`) are ONE step
///   ([`SessionMeta::set_autopilot`]), under the autopilot field's bit: a
///   driver's write of the word here set or cleared the timer with it, so
///   it wins for both halves; else both take the carried pair. The App
///   indexes the session's timer after this (`App::index_autopilot_timer`).
/// * The pair's setter ([`SessionMeta::autopilot_set_by`]) follows the pair
///   (§11.7.3, item 5): a driver's write here keeps its own setter, and
///   otherwise the carried setter is stored, with no revision step.
///
/// Returns whether any stored value moved: the caller's gate for waking `events` watchers.
pub(crate) fn restore_carried_meta(ctx: &crate::SessionCtx, carried: &SessionMeta) -> bool {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut moved = false;
    let mut autopilot_moved = false;
    for field in MetaField::ALL {
        if meta.driver_wrote(field) {
            continue;
        }
        let name = field.wire_name();
        if field == MetaField::Autopilot {
            // The PAIR, as one step (§9.3.6, item 2): the word and its timer,
            // both kept where a driver wrote the word here (its write set or
            // cleared the timer with it).
            let before = meta.autopilot.clone();
            let pair_moved = meta.set_autopilot(carried.autopilot.clone(), carried.land_at);
            // ITS SETTER GOES WITH IT (§11.7.3, item 5): the carried one,
            // whether or not the pair moved, and none beside no word — as a
            // driver's write here, its setter included, wins for all three.
            // Provenance: no value moved, and no revision steps (the write it
            // names was made in another process).
            meta.autopilot_set_by = carried
                .autopilot_set_by
                .filter(|_| meta.autopilot.is_some());
            if !pair_moved {
                continue;
            }
            moved = true;
            autopilot_moved |= meta.autopilot != before;
            // A restore is aterm's.
            record_autopilot(ctx, &meta, aterm_slog::Actor::Aterm);
            continue;
        }
        if !meta
            .set(name, carried.get(name).map(str::to_owned))
            .unwrap_or(false)
        {
            continue;
        }
        moved = true;
        // The STORED value, which `set`'s sanitizer may have narrowed from
        // what the manifest spelled.
        let payload = meta_change_payload(field, meta.get(name));
        let change = MetaChange::written_by(name, meta.get(name), aterm_slog::Actor::Aterm);
        record_meta(ctx, payload, change, &meta);
    }
    drop(meta);
    if autopilot_moved {
        crate::harness_host::ring();
    }
    moved
}

/// Validate then apply: the entry point for non-wire callers. The wire handler
/// splits the two only to render each refusal as its own `ERR` line.
pub(crate) fn write_session_meta(
    ctx: &crate::SessionCtx,
    field: MetaField,
    edit: MetaEdit<'_>,
    by: aterm_slog::Actor,
) -> Result<bool, MetaWriteError> {
    let value = validated_meta_value(field, edit)?;
    Ok(apply_meta_value(ctx, field, value, by))
}

/// What an adopted session's recorders say about the time before the seamless
/// update that adopted it. The timeline, asciicast and temporal spine start
/// empty in the successor, so each opens with this gap: the timeline's first
/// row is `handoff carried=0 from_build=<n>`, the cast header carries
/// `aterm_handoff` beside `aterm_truncated`/`aterm_dropped`, and `temporal
/// status` appends ` carried=0 from_build=<n>` after `dropped_events=`.
/// `from_build` is the manifest's `outgoing_build`; a producer that wrote none
/// reads `-` (`null` in the cast's JSON).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HandoffGap {
    /// The build number of the process that handed the session across.
    pub(crate) from_build: Option<u64>,
}

impl HandoffGap {
    /// The `k=v` tail every text surface prints: `carried=0 from_build=<n|->`.
    #[must_use]
    pub(crate) fn payload(self) -> String {
        match self.from_build {
            Some(build) => format!("carried=0 from_build={build}"),
            None => "carried=0 from_build=-".to_string(),
        }
    }
}

/// One recorded lifecycle event. `kind` is a closed vocabulary:
/// - lifecycle kinds this module's recorders write (`spawned`, `state-change`,
///   `title-change`, `cwd-change`, `meta-change`, `agent-change`,
///   `task-change` ([`crate::task_record`]), `closing`, an adopted session's
///   opening `handoff` row, [`HandoffGap`]) and the status sweep's
///   `status-change` (`crate::session_status::STATUS_CHANGE`);
/// - fabric kinds (`inbox`, `inbox-seen`, `post`, `fetch`, `post-landed`,
///   `hold`, `topic`: `crate::fabric::FABRIC_EVENT_KINDS`, also their digest
///   spelling);
/// - `render`, the resize ledger's entry into and exit from `desync-risk`
///   (`desync-risk run= displaced= net= at=` / `healed run= after_ms=`, pushed
///   as `EVENT <local> render …`; [`crate::resize_ledger`]);
/// - `in-doubt`, written by `crate::pty_idem` when an input verb with an `id=`
///   key failed without saying whether its bytes reached the PTY. It has no
///   wire form: it is one driver's unresolved write, and the fabric `EVENT`
///   names are pinned.
///
/// `payload` is a short `k=v` string, free-text values pct-encoded at record time, printed
/// verbatim by the `timeline` verb and the events digest (whose wire names are
/// `crate::subscribe::timeline_wire_kind`'s).
///
/// The `timeline` verb lists every kind a live session recorded. The last two
/// rows (`closing reason= by=`, when the close path said why, and
/// `state-change state=closed`) are written as the store deregisters the
/// session, when the sid stops resolving, so the verb cannot reach them. The
/// `subscribe … events` digest pushes `meta-change` as `EVENT <local> meta …`,
/// `agent-change` ([`SessionTimeline::publish_agent`]) as `EVENT <local> agent
/// <word> rev=<n>`, `task-change` ([`SessionTimeline::mark_task`]) as `EVENT
/// <local> task rev=<n> state=<s> …`, `status-change` as `EVENT <local> status
/// …`, and `closing`
/// as `EVENT <local> closing …` (the dying watch's final pass, before its
/// `exited` frame); the final `state-change` has no wire form (`exited` is that
/// fact). The instance's `exits` ledger keeps the reason and actor afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TimelineEvent {
    /// Per-session monotonic id (1-based), the `since=<id>` resume key.
    pub id: u64,
    /// Milliseconds since the process epoch ([`now_ms`]): monotonic, aligned
    /// with the turn ledger's clock, not wall time.
    pub t_ms: u64,
    /// The event kind token.
    pub kind: &'static str,
    /// Pre-encoded `k=v` tail, clamped to [`MAX_PAYLOAD`] bytes. Shared, so
    /// a reader copying events out of the timeline lock bumps a refcount
    /// instead of copying the text.
    pub payload: Arc<str>,
}

/// The timeline kind of an act a self-supervising agent recorded
/// ([`SessionTimeline::note_harness_act`]).
pub(crate) const HARNESS_ACT: &str = "harness-act";

/// The timeline kind of a move of the program's own state report as the
/// status sweep honoured it (OSC 7501, [`SessionTimeline::note_program_state`]).
pub(crate) const PROGRAM_STATE: &str = "program-state";

/// How many [`PROGRAM_STATE`] rows the ring holds at once: half of
/// [`TIMELINE_CAP`]. Past it a new row evicts the OLDEST `program-state` row,
/// not the oldest row, so a program that flaps its report every sweep keeps
/// its newest moves and, in a full ring, ages out at most this many of the
/// session's other rows: the newest `TIMELINE_CAP - PROGRAM_STATE_ROWS_MAX`
/// of them always stay. Below the share a new row is an ordinary one and a
/// full ring drops its oldest row, whatever its kind. 256 rows are some sixty
/// turns of `working`, `blocked`, `working`, `done`.
pub(crate) const PROGRAM_STATE_ROWS_MAX: usize = TIMELINE_CAP / 2;

/// Published foreground custody for an input decision, independent of
/// ordinary phase or screen revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundStamp {
    generation: u64,
    pgid: i32,
    holder: Option<u64>,
    agent: aterm_phase::Program,
}

/// A session's bounded event timeline, newest-last, drop-oldest at
/// [`TIMELINE_CAP`], with per-session monotonic ids minted here.
#[derive(Default)]
pub(crate) struct SessionTimeline {
    events: VecDeque<TimelineEvent>,
    next_id: u64,
    /// The last cwd a `cwd-change` was recorded for — the dedup watermark that
    /// makes [`Self::record_cwd_change`] idempotent per actual change (the
    /// observer runs once per output wake, not once per `cd`).
    last_cwd: Option<String>,
    /// What the server reads this session to be running, and its agent verdict
    /// ([`AgentPublication`]). Kept beside the ring the events digest scans: the
    /// status sweep writes it on the main thread, the program resolver thread
    /// writes `program`, and `sessions`, `await agent` and the push loop read it
    /// from the control threads.
    agent: AgentPublication,
    /// Input custody, independent of busy/idle and ordinary verdict revisions.
    /// Observed foreground, program, reader and image changes cannot revive
    /// an older press by returning to the same published identity.
    foreground_generation: u64,
    /// The Claude Code footer's facts (`aterm_agent::harness::footer`), with the foreground
    /// group and kernel start they were read for. Host-side only: painted into the rule under
    /// Claude Code's input box (`crate::claude_footer`). Written by the footer resolver thread.
    claude_footer: Option<(i32, Option<u64>, aterm_agent::harness::footer::FooterFacts)>,
    /// Last Muse account named in this terminal. Its anonymous usage cards
    /// cannot be attributed after a known account switch, even if a pane is
    /// rebuilt or the foreground process changes: the old glass survives both.
    muse_card_account: Option<String>,
    /// Sticky for the terminal's life, also set when old glass is adopted.
    muse_cards_need_account: bool,
    /// Those facts' `version` as a build, parsed once when they were stored:
    /// what the card's gate holds an image of unknown build to
    /// ([`Self::card_gate`], every frame, with no parse).
    claude_footer_build: Option<atpkg::vendor_direct::Version>,
    /// The drawer image in front (its token) when those facts were stored,
    /// `None` when none of their group's was: whose row they are when an
    /// image of that group is published again
    /// ([`Self::offer_holder_image`]).
    claude_footer_image: Option<u64>,
    /// The newest launch card the frame compose read for the process those
    /// facts are (its group, kernel start and owner: floor and session id). The
    /// card names the starting model and effort until the transcript or its
    /// `--model` decides them (`FooterFacts::filled_from`). A newer reading
    /// replaces it; a frame with no card to read does not (the card scrolls
    /// away long before the transcript decides).
    claude_card: Option<(
        i32,
        Option<u64>,
        aterm_agent::harness::footer::FactsOwner,
        aterm_phase::LaunchCard,
    )>,
    /// The shell's PATH, measured from the environment of a foreground leader
    /// whose parent is the session's shell (`session_status::program::
    /// leader_facts`): a fact about the shell, so it outlives the job it was
    /// read from. `None` until such a leader ran.
    path: Option<PathReading>,
    /// `copy=` of the current foreground leader and the group it was read
    /// for (`managed`/`foreign`, an agent only).
    leader_copy: Option<(i32, &'static str)>,
    /// The live agent upgrade of the conversation in this tab, as the window's
    /// upgrade host last reported it (`upgrade=`), or `None`.
    upgrade: Option<aterm_agent::harness::upgrade_drive::Row>,
    /// That upgrade's note for the person here, read off the row when it was
    /// handed over (`Row::pause_note`): what the Claude Code footer leads
    /// with while the move waits only on them.
    upgrade_note: Option<aterm_agent::harness::footer::UpgradeNote>,
    /// The resize ledger ([`crate::resize_ledger`]): every grid resize this session took and
    /// whether the app drew over content one displaced. Kept beside the ring its `render`
    /// transitions are recorded on.
    resizes: crate::resize_ledger::ResizeLedger,
    /// The id a seamless update's manifest told its successor to count on
    /// from ([`Self::handoff_id`]): this process may hand out ids up to it
    /// without either process naming one event twice. `None` until a park
    /// draws a manifest.
    #[cfg(any(unix, test))]
    handoff_ceiling: Option<u64>,
    /// THE TASK RECORD ([`crate::task_record`]), `None` for a session with no
    /// task. Here, beside the ring its `task-change` rows are recorded on and
    /// the agent publication its replies join, for the reason `agent` is:
    /// the one per-session leaf every reader (`task`, `status`, `sessions`,
    /// `await task`, the `events` digest) reaches lock-disjointly.
    task: Option<crate::task_record::TaskRecord>,
    /// WHAT A WORKER'S LAUNCHER DID WITH ITS LINE, and when
    /// (`task_launch::note_launch`): typed, with what the input receipt said
    /// and the program it runs, or never typed and why. `None` where no
    /// launcher of this process got that far. The startup deadline's stall
    /// says it in the log (`harness_host::startup_mark_with`).
    worker_launch: Option<(std::time::Instant, String)>,
    /// THE SURVEY'S EXCERPTS of the Claude Code process in front
    /// ([`crate::survey::ClaudeExcerpts`]) and the group they were read for:
    /// written by the footer resolver from its own tail read with every
    /// footer read (so a process that reuses the group has its own by the
    /// next one), read by the `survey` verb, so a survey reads no transcript.
    /// Never painted, so a new excerpt repaints nothing.
    claude_excerpts: Option<(i32, crate::survey::ClaudeExcerpts)>,
    /// THE FOOTER'S SIDE of the Claude Code process in front
    /// ([`crate::claude_footer::FactSide`]: its directories and traffic) and
    /// the group it was read for, written by the footer resolver with every
    /// footer read, read by the fact menus and the traffic card. Never
    /// painted and never in the facts, so a new side repaints nothing.
    claude_side: Option<(i32, std::sync::Arc<crate::claude_footer::FactSide>)>,
    /// When the footer resolver last read facts for the group in front, and
    /// that group: each read folds the session's usage anew, whether or not
    /// anything moved, so the Stats Deck takes its token rates between two
    /// reads (`crate::vitals_host`). Kept apart from the facts, so a read
    /// that moved nothing repaints nothing.
    claude_footer_read: Option<(i32, std::time::Instant)>,
    /// What the agent's own screen counted at the status sweep's last read
    /// of it ([`AgentGauges`]): the survey's `ctx_left_pct=` and `bg=`.
    gauges: AgentGauges,
    /// When a move of the screen alone last woke the supervisor host
    /// ([`Self::publish_agent`]): at most once per
    /// `harness_host::STATUS_AFTER_SCREEN`.
    screen_rung_at: Option<std::time::Instant>,
    /// The session log ([`crate::session_log`]): the hierarchical record of what this session
    /// does (session › run › turn › step), kept under this leaf lock with the producers that
    /// write it.
    slog: crate::session_log::SessionLog,
    /// THE NEED VERDICT ([`crate::needs::Verdict`]): what the session needs of
    /// its owner, what aterm waits out for it and whose hold stands, written
    /// once per change on the main thread (`App::publish_need`) and read by
    /// every surface — `status needs=`, the survey's `needs=`, the menu-bar
    /// item and its notifications — so none of them classifies again. Here
    /// for the reason `agent` is: the leaf every reader reaches.
    need: crate::needs::Verdict,
    /// The last act an agent that supervises itself recorded on this session
    /// (`meta set harness_act <act>`, [`Self::note_harness_act`]): `status
    /// last_act=`. Kept beside the ring, which may age its `harness-act` row out.
    last_act: Option<String>,
    /// Admitted self-claim acts not yet shown by the app thread, in commit
    /// order. A SelfTask wake drains this queue, not its possibly late face.
    /// One face per outstanding act, including an act whose lane has not
    /// posted its wake yet. It is not aged out by the bounded timeline ring;
    /// a logical claim change clears it, as the old holder's news.
    pending_self_tasks: VecDeque<aterm_agent::harness::acts::TaskFace>,
    /// What the latest landing a proven self-claim's holder recorded (or
    /// point it left to a person) was recorded over ([`LandingMark`],
    /// [`Self::mark_landing`]): `None` before one.
    landing_mark: Option<LandingMark>,
    /// The task's face under Land, as this window's host last heard it from the session's
    /// loop, and the wall time of the move (unix ms): `status` and `ls` print it (`task=`,
    /// only while this window's host holds the live claim) and Settings ▸ Harness's state line
    /// reads it. `None`: nobody has told it, or the loop that did has ended. Written only by
    /// `App::apply_harness_fact`.
    task_face: Option<(aterm_agent::harness::acts::TaskFace, i64)>,
    /// The session's watchers ([`crate::SessionCtx::watchers`]): every
    /// [`Self::record`] rings them, so whatever reads this ring (the `events`
    /// digest, a parked `await`, `mail` on an `inbox` row) is woken by the
    /// write itself. A default timeline rings a list of its own that nobody
    /// watches; a session's is wired by [`crate::session_watch::Observed`].
    watchers: Arc<crate::session_watch::Watchers>,
    /// The face and its time as they stood when the host told "unknown"
    /// (the loop ended: a restart, a panicked body, an Off), kept so the next
    /// loop's start face, where it is the same (a landing seeded across the
    /// restart), RESTATES it with its first time rather than moving it
    /// ([`Self::set_task_face`]). Gone at the first face told after.
    task_face_held: Option<(aterm_agent::harness::acts::TaskFace, i64)>,
    /// THE LOOP'S PAUSE AFTER SHORT TURNS, as this window's host last heard
    /// it (`docs/DESIGN-harness-autopilot-2026-10-15.md` §10.5.3, item 4):
    /// what `status` and `ls` print (`pause=`, only while this window's host
    /// holds the live claim). `None`: none stands. Written only by
    /// `App::apply_harness_fact`.
    pause: Option<crate::PausePlan>,
    /// THE WORKER'S NEEDS YOU, WHOLE, as its loop told this window's host
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.2.3 item 3, slice
    /// 2c): the set told last and the outgoing set whose entry still stands
    /// until the new one's request lands ([`crate::needs::ToldNeeds`]), each
    /// with the needs owner's entry text it was told with. Written only by
    /// the host's live hook (`harness_host::store_needs`), on the loop's
    /// thread, before the entry's `meta set` (or `unset`) is sent; a set is
    /// read only beside the entry it was told with
    /// ([`crate::needs::NeedFacts::told_needs`]). No stamp: the menu row's
    /// age is the entry's, and no surface reads a need's own.
    harness_needs: crate::needs::ToldNeeds,
    /// WHY LAND STILL HOLDS, as the session's loop told this window's host
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.3.3 item 3, slice
    /// 3c; `IdleHost::land_held`): what `status land_held= land_nudges=
    /// land_since=` print (only while this window's host holds the live
    /// claim and the session runs under Land), and what the window's glass
    /// copies (`harness_glass::on_land_held`). `None`: none stands. Written
    /// only by the host's live hook (`harness_host::store_land_held`), on
    /// the loop's thread, and cleared by `App::apply_harness_fact` beside a
    /// proven self-claim and at a person's own exchange. `Copy`, no text.
    land_held: Option<aterm_agent::harness::acts::LandHeld>,
    /// Which foreground group was in front when the status sweep first saw
    /// the engine's current cwd report: `(Terminal::reported_cwd_seq, pgid)`
    /// ([`Self::witness_cwd_report`]). A report stands only under the group it
    /// was made under: an ssh's remote shell's OSC 7 says nothing once the
    /// local shell is back in front (`App::resolve_citation`).
    cwd_witness: Option<(u64, i32)>,
    /// THE HOLDER IMAGE (card engine §3.4, I1): the image the PTY reader last
    /// published as the foreground holder's, before the hold of the slice
    /// that starts at its cut ([`Self::offer_holder_image`]). Only card-relevant
    /// edges publish, so between them it is the last image that entered or
    /// left a card drawer.
    holder_image: Option<std::sync::Arc<crate::holder_image::HolderImage>>,
    /// The token whose `card-seen` dev row was recorded
    /// ([`Self::note_card_seen`]).
    card_seen: Option<u64>,
    /// What the card passes did with this session's card, for the dev rows
    /// ([`Self::note_card_frame`]).
    card_frames: CardFrameMemo,
    /// What [`Self::take_registry_drift`] judges against and last said.
    registry_drift: RegistryDriftMemo,
}

/// [`SessionTimeline::note_card_frame`]'s memo (card engine §8's judge).
#[derive(Default)]
struct CardFrameMemo {
    /// The drawer token whose `card-decorated` row was recorded.
    decorated: Option<u64>,
    /// The banner row its card was last decorated at (`card-row`).
    decorated_row: Option<i64>,
    /// The bare frames counted for the token in front since its card was
    /// seen or read, before it was decorated.
    bare: BareFrames,
    /// The last `card-bare` row's token, reason and row.
    said: Option<(u64, crate::claude_card::Bare, Option<i64>)>,
    /// The window pass count of the last card pass that judged this
    /// session's card (`card-seen after=`).
    composed: u64,
    /// The `card-seen` row's token, banner row and `after=`: the passes
    /// composed after it and before the same card's decoration showed it
    /// bare, whatever the gate read (a predecessor's card above it, say).
    seen: Option<(u64, i64, u64)>,
}

/// The frames one drawer's card was shown bare, by where (§8's
/// `bare_frames`): a refused card is counted at its banner's absolute row,
/// so a predecessor's card refused above the drawer's own (an inline
/// relaunch, a `cat`'d card) is not the drawer's card's bare time; a frame
/// with the card seen but no card read on the pane counts for whichever
/// card is decorated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BareFrames {
    token: u64,
    /// The row of the card refused most recently, and its frames there.
    row: Option<i64>,
    at_row: u32,
    /// Frames with the drawer's card seen but none read.
    unplaced: u32,
}

impl BareFrames {
    /// One more bare frame of `token`'s, its refused card at `row`.
    fn count(&mut self, token: u64, row: Option<i64>) {
        if self.token != token {
            *self = Self {
                token,
                ..Self::default()
            };
        }
        match row {
            Some(r) if self.row == Some(r) => self.at_row = self.at_row.saturating_add(1),
            Some(r) => {
                self.row = Some(r);
                self.at_row = 1;
            }
            None => self.unplaced = self.unplaced.saturating_add(1),
        }
    }

    /// The frames `token`'s card decorated at `row` was shown bare first.
    fn of(&self, token: u64, row: i64) -> u32 {
        if self.token != token {
            return 0;
        }
        let here = if self.row == Some(row) {
            self.at_row
        } else {
            0
        };
        self.unplaced.saturating_add(here)
    }
}

/// [`SessionTimeline::take_registry_drift`]'s memo (card engine §3.10).
#[derive(Default)]
struct RegistryDriftMemo {
    /// A drawer image's token, and the owner of the facts that stood for its
    /// group when it was published and are not its own: facts of that owner
    /// are a row from before its `exec` (the image it replaced wrote it),
    /// never judged against it, nor held to as its build
    /// (`SessionTimeline::card_gate`). Which standing facts those are,
    /// `SessionTimeline::offer_holder_image` says.
    stale: Option<(u64, Option<aterm_agent::harness::footer::FactsOwner>)>,
    /// The image token and the reported build last said.
    said: Option<(u64, String)>,
}

/// A Claude Code process whose registry row reports a build its Store or
/// Native drawer image does not name (card engine §3.10): a fact for later,
/// never a gate input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistryDrift {
    /// The group the image and the row are of.
    pub(crate) pgid: i32,
    /// The build the image names: its store directory's, or its installer
    /// version folder's.
    pub(crate) image: String,
    /// Where the image runs from (Store or Native).
    pub(crate) source: atpkg::image::ImageSource,
    /// The build the registry row reports (`FooterFacts::version`).
    pub(crate) reported: String,
}

/// What an identified agent's screen counts, read with its verdict by the
/// status sweep (`session_status::AgentStep::gauges`) and published for the
/// survey. `None` for what the screen did not show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentGauges {
    /// The context left before auto-compact, in percent, where the agent
    /// shows it (`aterm_phase::Reading::context_left`).
    pub(crate) ctx_pct: Option<u8>,
    /// How many background shells it counts (`aterm_phase::shells_running`).
    pub(crate) shells: Option<u32>,
}

/// How many ids a seamless update leaves this process between the park's draw
/// and its `_exit` ([`SessionTimeline::handoff_id`]). Events keep being
/// recorded until Commit, so the successor counts on from above them;
/// `timeline since=` filters by id, so the jump is invisible. A process that
/// records more than this in one overlap refuses its Commit (a lossless retry)
/// rather than hand an id out twice.
#[cfg(any(unix, test))]
pub(crate) const HANDOFF_ID_RESERVE: u64 = 1 << 16;

/// One measurement of the shell's PATH ([`SessionTimeline::set_leader`]).
///
/// The evidence is a child's exec environment, not the shell's: a one-off
/// `PATH=/usr/bin:/bin tool` reads frozen from a shell whose PATH is live. So a
/// frozen reading is believed only once settled (a second job agrees, or the
/// carried adoption mark does, [`path_columns`]) and never raises the mark. A
/// live reading settles at once (putting aterm's own `agents/` first is no
/// accident) and lowers the mark ([`SessionTimeline::take_path_lowered`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PathReading {
    /// What the PATH does with `claude`/`codex`.
    pub(crate) verdict: crate::session_status::program::PathVerdict,
    /// When it was read, on the ledger clock ([`now_ms`]).
    pub(crate) at_ms: u64,
    /// The foreground group it was read from: a reading from another group with the
    /// same verdict is another job agreeing.
    pgid: i32,
    /// Whether another job's reading agreed with this verdict, with no other
    /// verdict between them.
    agreed: bool,
    /// Whether the registry's frozen mark has been lowered for this live
    /// reading yet ([`SessionTimeline::take_path_lowered`]).
    reported: bool,
}

impl PathReading {
    /// Whether the reading is the shell's, not one job's: live, or frozen
    /// with a second job agreeing.
    pub(crate) fn settled(&self) -> bool {
        self.verdict.word() == "live" || self.agreed
    }
}

/// `(path=, path_evidence=)` from the registry's adoption `mark` and the last `reading`
/// (`(word, age_ms, settled)`, [`SessionTimeline::path_reading`]). One job's unsettled frozen
/// reading on an unmarked shell reads `live`, `unconfirmed:<age_ms>`: it may be a per-command
/// `PATH=` override.
pub(crate) fn path_columns(
    mark: bool,
    reading: Option<(&'static str, u64, bool)>,
) -> (&'static str, String) {
    match reading {
        Some((word, age_ms, settled)) if settled || mark => (word, format!("measured:{age_ms}")),
        Some((_, age_ms, _)) => ("live", format!("unconfirmed:{age_ms}")),
        None if mark => ("frozen", "carried".to_string()),
        None => ("live", "-".to_string()),
    }
}

/// The server-published identity and agent verdict of one session: `status`
/// and every `sessions` row carry it as `program= agent= agent_detail=
/// agent_rev= agent_since_ms=`, `subscribe … events` pushes `EVENT <local>
/// agent <word> rev=<n>` on every move, and `await agent` parks on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentPublication {
    /// The argv[0] basename of the PTY's foreground process-group leader
    /// (`claude`, `zsh`, `sleep`), or `None` while unresolved or unknowable.
    pub(crate) program: Option<String>,
    /// The agent the last verdict's reader identified (by `program`'s name or, for a runtime
    /// like `node`, by its screen), or `None`. The in-GUI supervisor host attaches by it
    /// (`harness_host::agent_of`).
    pub(crate) reader: Option<aterm_phase::Program>,
    /// The foreground process group `program` belongs to (`0` = none seen
    /// yet). A resolution answered for an older group is dropped.
    pub(crate) program_pgid: i32,
    /// `busy|prompt|question|wall:<kind>|idle|survey|unknown`, or `-` when the
    /// session is not an identified agent. The effective word: the screen's
    /// verdict ([`Self::screen_word`]), or `wall:unresponsive` while the program
    /// has stopped reading its input ([`Self::input`]).
    pub(crate) word: &'static str,
    /// The verdict's detail (`bash:not-read-only`, a limit's reset), wire-safe
    /// (pct-encoded at the edge), or `None`.
    pub(crate) detail: Option<String>,
    /// Bumps each time `word` or `detail` moves; `0` = never published.
    pub(crate) rev: u64,
    /// How many times the verdict moved as the PROGRAM's: [`Self::rev`]'s
    /// moves, less the screen reads within a re-grid's settle
    /// ([`GridRead::settling`]) — a width change reflows the screen, the
    /// read of the reflowed grid can parse it otherwise (a box's detail lost)
    /// and the program's repaint parse it as before, with nothing of the
    /// program's own moving — plus one at the first read past the settle
    /// whose verdict is not the one that stood before the re-grid
    /// ([`GridRead::before`]). An input stall's move always counts. A count
    /// that only grows, like [`Self::text_moves`]: the held deadline's
    /// verdict evidence (`harness_host::Progress::verdict`), where `rev` is
    /// the wire's.
    pub(crate) verdict_moves: u64,
    /// The approval box's command or path while `word` is `prompt`, folded to
    /// one clipped line. Host-side only (menu-bar row and notification); the
    /// wire never carries it. Moving it bumps no `rev`.
    pub(crate) subject: Option<String>,
    /// When `word`/`detail` last moved, on the ledger clock ([`now_ms`]).
    pub(crate) changed_ms: u64,
    /// The same moment on the clock that counts through sleep
    /// (`aterm_uds::process::continuous_ms`), where this platform has one:
    /// what the survey's `agent_ms=` is taken on ([`Self::age_ms`]).
    pub(crate) changed_continuous_ms: Option<u64>,
    /// The screen the verdict stands on: the stamp of the last read of the live
    /// zone (an unchanged re-read re-confirms the verdict and moves the stamp).
    /// `None` until the first read.
    pub(crate) stamp: Option<AgentStamp>,
    /// The screen's verdict as last published, kept apart from the effective
    /// [`Self::word`] so a stall that ends hands the word back to it.
    pub(crate) screen_word: &'static str,
    /// The screen verdict's detail, likewise.
    pub(crate) screen_detail: Option<String>,
    /// The published input stall ([`crate::input_stall::InputStallFact`]): while
    /// set on an identified agent the word is `wall:unresponsive` whatever the
    /// screen says, since a frozen program's screen keeps its last verdict.
    pub(crate) input: Option<crate::input_stall::InputStallFact>,
    /// How many times a read of the screen found its TEXT changed from the
    /// read before ([`AgentStamp::fp`] moved; colours alone never move it):
    /// a count that only grows, so a screen that changed and changed back
    /// between two looks still reads as moved. A RE-GRID IS NO MOVE: a read
    /// within [`REGRID_SETTLE`] of the first read that found the grid resized
    /// ([`Self::grid`]) is not counted, since a resize changes every row's
    /// text and the program's repaint after it changes it again with nothing
    /// of the program's own moving — the presence row born or folded, a tab
    /// brought to the front, the window dragged wider or taller, a pane
    /// split, the font zoomed; the verdict a re-grid moves is settled the
    /// same way ([`Self::verdict_moves`]). A grid that keeps moving less than
    /// [`REGRID_SETTLE`] apart (an edge being dragged) withholds this text
    /// evidence, and the verdict's, until it settles; input and the
    /// transcript still count. The held deadline's screen evidence
    /// (`harness_host::look_at_startups`).
    pub(crate) text_moves: u64,
    /// The grid as the screen's reads found it ([`GridRead`]; `None` until
    /// the sweep's first read names it): what keeps a re-grid out of
    /// [`Self::text_moves`].
    pub(crate) grid: Option<GridRead>,
    /// THE READS WHOSE LIVE ZONE MOVED UNDER A VERDICT THAT STOOD while the
    /// agent was not busy (LC-1 of ATERM-KI-1015-1,
    /// [`SessionTimeline::publish_agent`]): an in-app `/clear` moves its
    /// conversation and no verdict, only this. Host-side only, like
    /// [`Self::subject`]: the supervisor host follows Claude Code's record of
    /// the agent when it moves (`harness_host::FollowStamp::screen`). A busy
    /// agent's screen (its spinner) moves it never.
    pub(crate) screen_moves: u64,
    /// How many reads of the screen found the agent BUSY after a read that
    /// did not ([`Self::screen_word`] moved into `busy`; a read within a
    /// re-grid's settle is none, and an input stall, which moves only the
    /// effective word, neither begins nor ends one): the turns the screen saw
    /// begin, a count that only grows. A proven self-claim's landing reopens
    /// at one past its mark ([`LandingMark::busy_entries`]). The screen is
    /// read at sweeps, so a turn no read found busy is not counted; Codex's
    /// screen is never read (`aterm_phase::CodexReader`), and its own report
    /// counts its turns ([`LandingMark::program_turns`]).
    pub(crate) busy_entries: u64,
}

/// WHAT A PROVEN SELF-CLAIM'S LANDING WAS RECORDED OVER (FOREVER LAND §10.4):
/// the session's two counts of turns begun, as they stood when its holder's
/// `meta set harness_act landed@v1` was served (the `meta` arm,
/// `control::serve_meta`, which records the mark before it posts the
/// landing's face). A point it left to a person (`left-to-person@v1`) is
/// marked the same way and reopens the same way
/// (`harness_task::holds_until_another_turn`). A turn begun after the landing that a count sees moves
/// it past its mark, and the landing reopens
/// (`App::reopen_self_claim_landing`) — however short the turn, whenever the
/// status sweep reads it, and whatever the report standing then says. What
/// each count cannot see is the reopen's to say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LandingMark {
    /// The program's own count (OSC 7501,
    /// `aterm_core::terminal::program_state::ProgramStateReport::turns`),
    /// read from the engine as the act arrived, before it was admitted: every
    /// report the engine had parsed by then is the landing's or earlier, so a
    /// sweep that had not yet read the landing turn's reports reads none of
    /// them as after it, and a report parsed while the act was admitted (a
    /// turn begun after the holder decided) stays past it. One parsed between
    /// the holder's decision and the act's arrival is in it: the act's trip
    /// from the holder is a window no read here closes.
    pub(crate) program_turns: u64,
    /// The screen's count ([`AgentPublication::busy_entries`]), read with the
    /// program's.
    pub(crate) busy_entries: u64,
}

/// How long after the first read that finds a session's grid resized a text
/// change is the re-grid's, not the program's ([`AgentPublication::text_moves`]):
/// the program's repaint for the new size lands within the resize ledger's
/// [`crate::resize_ledger::RISK_AFTER_MS`] (Claude Code's in 5-30 ms), and
/// the sweep reads a moved screen within
/// [`crate::session_status::AGENT_MIN_INTERVAL`] of it; two seconds holds
/// both with room. What the program itself changes in that time is not
/// counted, and its next change is; its verdict, if it is not the one that
/// stood before the re-grid, counts once at the first read past the settle
/// ([`AgentPublication::verdict_moves`]). Each new resize starts the settle
/// again, so a grid that keeps moving less than this apart withholds the
/// screen's evidence until it rests: the discount is per burst, not per
/// resize.
pub(crate) const REGRID_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

/// THE GRID AT A READ OF THE SCREEN ([`SessionTimeline::note_grid`]): which
/// resize the engine had last taken, and since when the reads have found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GridRead {
    /// The engine's `(Terminal::resize_lifetime, Terminal::resize_ordinal)`
    /// at the read: a new value is a resize (or a new engine) since the last.
    pub(crate) resize: (u64, u64),
    /// The first read that found the grid at `resize` after another, or
    /// `None` where the grid has not moved since the first read.
    pub(crate) since: Option<std::time::Instant>,
    /// The read itself.
    pub(crate) at: std::time::Instant,
    /// The effective verdict (word, detail) that stood when the first
    /// re-grid of a burst was read, kept until the first read past the
    /// settle compares it ([`AgentPublication::verdict_moves`]); `None`
    /// outside a burst.
    pub(crate) before: Option<(&'static str, Option<String>)>,
}

impl GridRead {
    /// Whether a text change at this read is the re-grid's: the read is
    /// within [`REGRID_SETTLE`] of the first that found the grid resized.
    #[must_use]
    pub(crate) fn settling(&self) -> bool {
        self.since
            .is_some_and(|since| self.at.saturating_duration_since(since) < REGRID_SETTLE)
    }
}

/// Which screen an agent verdict was read from: its generation and FNV-1a-64
/// hash, as `status gen=`/`hash=` print them. A press decided from the verdict
/// fences on these (`key if-gen=<agent_gen> if-fp=<agent_fp>`), never on a
/// fresh `status gen=`, which can be up to one sweep newer than the verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AgentStamp {
    /// The screen generation of the read.
    pub(crate) generation: crate::control::ScreenGen,
    /// FNV-1a-64 of the untrimmed visible screen at the read.
    pub(crate) fp: u64,
}

impl Default for AgentPublication {
    fn default() -> Self {
        Self {
            program: None,
            reader: None,
            program_pgid: 0,
            word: "-",
            detail: None,
            rev: 0,
            verdict_moves: 0,
            subject: None,
            changed_ms: now_ms(),
            changed_continuous_ms: aterm_uds::process::continuous_ms(),
            stamp: None,
            screen_word: "-",
            screen_detail: None,
            input: None,
            text_moves: 0,
            grid: None,
            screen_moves: 0,
            busy_entries: 0,
        }
    }
}

impl AgentPublication {
    /// Milliseconds since the verdict last moved (`agent_since_ms=`).
    pub(crate) fn since_ms(&self) -> u64 {
        now_ms().saturating_sub(self.changed_ms)
    }

    /// [`Self::since_ms`] counted THROUGH SLEEP, as the survey reports it: a
    /// laptop that slept since the verdict moved reads the time that passed,
    /// not only the time awake. The awake count where the platform has no
    /// such clock.
    pub(crate) fn age_ms(&self) -> u64 {
        match (
            self.changed_continuous_ms,
            aterm_uds::process::continuous_ms(),
        ) {
            (Some(at), Some(now)) => now.saturating_sub(at),
            _ => self.since_ms(),
        }
    }

    /// The effective word and detail: `wall:unresponsive` (detail `stopped`
    /// for a stopped job) while an input stall is published on an identified
    /// agent, the screen's verdict otherwise. A session that is no agent keeps
    /// `-`: its stall is the server's attention entry, not an agent word.
    fn effective(&self) -> (&'static str, Option<String>) {
        match &self.input {
            Some(stall) if self.reader.is_some() => (
                "wall:unresponsive",
                stall.stopped.then(|| "stopped".to_string()),
            ),
            _ => (self.screen_word, self.screen_detail.clone()),
        }
    }

    /// The seven wire fields, space-separated, in their fixed order: `program=`
    /// `agent=` `agent_detail=` `agent_rev=` `agent_since_ms=` `agent_gen=`
    /// `agent_fp=`. Free text is pct-encoded; unknown is `-`.
    pub(crate) fn wire_fields(&self) -> String {
        let enc = |v: Option<&str>| v.map_or_else(|| "-".to_string(), crate::control::pct_encode);
        let (generation, fp) = self.stamp_fields();
        format!(
            "program={} agent={} agent_detail={} agent_rev={} agent_since_ms={} \
             agent_gen={generation} agent_fp={fp}",
            enc(self.program.as_deref()),
            self.word,
            enc(self.detail.as_deref()),
            self.rev,
            self.since_ms(),
        )
    }

    /// `agent_gen`/`agent_fp` as wire text (`-` before the first read).
    fn stamp_fields(&self) -> (String, String) {
        self.stamp.map_or_else(
            || ("-".to_string(), "-".to_string()),
            |s| (s.generation.to_string(), format!("{:016x}", s.fp)),
        )
    }
}

/// How long a person's hands must be off a session before their next input opens a new burst
/// (one `EVENT <local> human` push, one `human` row), which bounds those rows in the shared
/// [`TIMELINE_CAP`] ring to 120 an hour. The supervisor's `[harness] human_grace_s` (120 s by
/// default) is longer. Bursts are [`crate::human_input::HumanInputStamp`]'s.
pub(crate) const HUMAN_BURST_GAP_MS: u64 = 30_000;

impl SessionTimeline {
    /// An empty timeline that rings `watchers` on every record.
    pub(crate) fn watched_by(watchers: Arc<crate::session_watch::Watchers>) -> Self {
        SessionTimeline {
            watchers,
            ..SessionTimeline::default()
        }
    }

    /// The list this timeline rings: `SessionStore::register` checks it is the
    /// context's.
    pub(crate) fn watchers(&self) -> &Arc<crate::session_watch::Watchers> {
        &self.watchers
    }

    /// An act the session's proven self-claim's holder took (`meta set
    /// harness_act <act>`, the native harness contract §5): one `harness-act`
    /// row (`act=<act> holder=<holder>`), and the act kept for `status
    /// last_act=`. `act` and `holder` are owner tokens, printed verbatim. In
    /// the same hold, the session log's `act` mark by `by`, the connection's
    /// actor (plan 5C-2, [`crate::session_log::SessionLog::self_claim_act`]):
    /// the caller takes the log's wake before it releases the lock.
    pub(crate) fn note_harness_act(
        &mut self,
        act: &str,
        holder: &str,
        by: aterm_slog::Actor,
    ) -> u64 {
        self.last_act = Some(act.to_string());
        let id = self.record(HARNESS_ACT, format!("act={act} holder={holder}"));
        let at = self.slog.now();
        let _ = self.slog.self_claim_act(act, holder, by, at);
        id
    }

    /// The last act [`Self::note_harness_act`] kept, if any.
    pub(crate) fn last_act(&self) -> Option<&str> {
        self.last_act.as_deref()
    }

    /// Queue the face of an admitted act in the hold that commits its row.
    pub(crate) fn queue_self_task(&mut self, face: aterm_agent::harness::acts::TaskFace) {
        self.pending_self_tasks.push_back(face);
    }

    /// Take the finite batch committed before this read. Later acts keep
    /// their own wakes; a continuous writer cannot grow this app turn.
    pub(crate) fn take_self_tasks(&mut self) -> VecDeque<aterm_agent::harness::acts::TaskFace> {
        std::mem::take(&mut self.pending_self_tasks)
    }

    /// A proven self-claim's holder recorded its landing (or left a point to
    /// a person) over `mark` (the session's counts as its act arrived,
    /// [`LandingMark`]), replacing any mark before it.
    pub(crate) fn mark_landing(&mut self, mark: LandingMark) {
        self.landing_mark = Some(mark);
    }

    /// The latest landing's mark ([`Self::mark_landing`]), `None` before one.
    pub(crate) fn landing_mark(&self) -> Option<LandingMark> {
        self.landing_mark
    }

    /// A move of what the program in front says about itself, as the status
    /// sweep's gate HONOURED it (OSC 7501, `crate::program_state::ProgramStateGate`:
    /// only under a `codex`, only a report newer than the last foreground
    /// move): one `program-state` row, `state=<idle|working|blocked|done|error>
    /// kind=<permission|question|auth|->`, or `state=- kind=-` when the shown
    /// report went away (a `clear`, a full reset, the program leaving the
    /// front). Read with the rows' own `t=`, `timeline` is the program's turn
    /// ledger: a turn starts at `working` (or `blocked`), ends at `done`,
    /// `idle` or `error`, and a `blocked` row to the next row is a blocked span.
    ///
    /// IN THE SAME HOLD, THE SESSION LOG'S PROGRAM MARK (phase 6, 6F-4;
    /// rulings U17, U39, T24's reversal): `mark <run|-> program state=
    /// [ask=] [coalesced=] src=engine conf=claim`, through the run's
    /// coalescing bucket (`SessionLog::observe_program`), written before the
    /// row so the row's wake finds it. So `await log kind=program` and
    /// `subscribe … log` see each move; the log's fold never reads it, `brief`
    /// prints no line of it, and it stays on this host (no bus class).
    ///
    /// The row itself is still read by no decision: no agent verdict, act,
    /// supervisor or alert reads it, the `events` digest does not carry it
    /// (`crate::subscribe::timeline_wire_kind`) and the tab's hover tail
    /// leaves it out (`crate::session_chrome::timeline_row_shown`).
    ///
    /// Bounded like every row: the gate moves at most once per status sweep,
    /// the payload is two closed-set words under [`MAX_PAYLOAD`], and the ring
    /// stays at [`TIMELINE_CAP`] with these rows held to
    /// [`PROGRAM_STATE_ROWS_MAX`] of it; the log keeps its own rule (64 a
    /// run, `aterm_slog::program_share`, and a flap past the bucket coalesced
    /// to its latest state): `Diff::Retention`, not an exact pair.
    pub(crate) fn note_program_state(
        &mut self,
        shown: Option<aterm_core::terminal::program_state::ProgramState>,
    ) -> u64 {
        let at = self.slog.now();
        self.slog.observe_program(shown, at);
        let (state, kind) = shown.map_or(("-", "-"), |s| {
            (s.state.as_str(), s.kind.map_or("-", |k| k.as_str()))
        });
        let held = self
            .events
            .iter()
            .filter(|e| e.kind == PROGRAM_STATE)
            .count();
        if held >= PROGRAM_STATE_ROWS_MAX
            && let Some(oldest) = self.events.iter().position(|e| e.kind == PROGRAM_STATE)
        {
            self.events.remove(oldest);
        }
        self.record(PROGRAM_STATE, format!("state={state} kind={kind}"))
    }

    /// Append one event, evicting the oldest past the cap, and wake the
    /// session's watchers for a record ([`crate::session_watch::Watchers::ring_record`]:
    /// an `events` subscription reads it at once). Returns its id.
    ///
    /// Never a [`META_CHANGE`]: that kind has ONE writer,
    /// [`Self::record_meta_change`], so its session log mark and the
    /// `escalated` need it derives cannot be skipped (ruling S14). Asked for
    /// one, a debug build panics and a release build appends nothing.
    pub(crate) fn record(&mut self, kind: &'static str, payload: String) -> u64 {
        debug_assert_ne!(kind, META_CHANGE, "a meta-change is record_meta_change's");
        if kind == META_CHANGE {
            return self.next_id;
        }
        self.append(kind, payload)
    }

    /// THE ONE `meta-change` WRITER (session log phase 3B; ruling S14,
    /// addendum A16), under the meta → timeline hold its writer already
    /// takes (`meta` is the store as the write left it): the timeline row,
    /// the session log's `mark - meta field= value=` by its writer, and the
    /// `escalated` need derived from the effective attention owner — raised
    /// while it is the supervisor's, cleared once it is not, `by=aterm`
    /// whoever wrote. Returns the row's id.
    pub(crate) fn record_meta_change(
        &mut self,
        payload: String,
        change: MetaChange<'_>,
        meta: &SessionMeta,
    ) -> u64 {
        let id = self.append(META_CHANGE, payload);
        let escalated = meta.attention_owners.effective_owner()
            == Some(aterm_agent::supervise::run::ATTENTION_OWNER);
        let at = self.slog.now();
        self.slog.observe_meta(change, escalated, at);
        id
    }

    /// A raw `meta-change` row for a test that pins the timeline's own
    /// shapes (ids, the digest, the cap) and not a writer's.
    #[cfg(test)]
    pub(crate) fn record_raw_meta_change(&mut self, payload: String) -> u64 {
        self.append(META_CHANGE, payload)
    }

    fn append(&mut self, kind: &'static str, payload: String) -> u64 {
        self.next_id += 1;
        if self.events.len() == TIMELINE_CAP {
            self.events.pop_front();
        }
        self.events.push_back(TimelineEvent {
            id: self.next_id,
            t_ms: now_ms(),
            kind,
            payload: clamp_payload(payload),
        });
        self.watchers.ring_record();
        self.next_id
    }

    /// Record a `cwd-change` iff `cwd` differs from the last one recorded (the
    /// GUI observes cwd on every output wake, so the dedup lives here). A
    /// `None`/empty cwd (OSC 7 cleared) records `cwd=-`.
    pub(crate) fn record_cwd_change(&mut self, cwd: Option<&str>) {
        let cwd = cwd.filter(|c| !c.is_empty());
        // The session log's `cwd` mark, deduped on its own carried
        // fingerprint: a successor whose timeline starts empty does not
        // restate it.
        let at = self.slog.now();
        self.slog.observe_cwd(cwd, at);
        if self.last_cwd.as_deref() == cwd {
            return;
        }
        self.last_cwd = cwd.map(str::to_string);
        let payload = match cwd {
            Some(c) => format!("cwd={}", crate::control::pct_encode(c)),
            None => "cwd=-".to_string(),
        };
        self.record("cwd-change", payload);
    }

    /// The resize ledger, for its readers (`resizes`, the `events` digest).
    pub(crate) fn resizes(&self) -> &crate::resize_ledger::ResizeLedger {
        &self.resizes
    }

    /// Book a resize path's journal copy ([`crate::resize_ledger::ResizeLedger::ingest`])
    /// and record any verdict transition as a `render` event. Returns whether one
    /// was recorded. A booked row wakes the session's watchers even with no
    /// transition: `EVENT resize` is read off the ledger's rows.
    pub(crate) fn ingest_resizes(
        &mut self,
        copy: &crate::resize_ledger::JournalCopy,
        attribution: Option<crate::resize_ledger::Attribution>,
        now_ms: u64,
    ) -> bool {
        self.note_resize_class();
        let high = self.resizes.high_row();
        let transitions = self.resizes.ingest(copy, attribution, now_ms);
        let recorded = self.record_render(&transitions);
        if !recorded && self.resizes.high_row() != high {
            self.watchers.ring_record();
        }
        recorded
    }

    /// Evaluate the resize ledger against `probe` and record any verdict
    /// transition as a `render` event. Returns whether one was recorded.
    pub(crate) fn evaluate_resizes(
        &mut self,
        probe: crate::resize_ledger::EngineProbe,
        now_ms: u64,
    ) -> bool {
        self.note_resize_class();
        let transitions = self.resizes.evaluate(probe, now_ms);
        self.record_render(&transitions)
    }

    /// Tests: what the presence classifier's published reader says.
    #[cfg(test)]
    pub(crate) fn set_agent_reader_for_test(&mut self, claude: bool) {
        self.set_agent_reader(claude.then_some(aterm_phase::Program::Claude));
    }

    /// Whether this session's program reads as the agent class (Claude Code)
    /// now: the class the repaint nudge is limited to.
    pub(crate) fn resize_agent_class(&mut self) -> bool {
        self.note_resize_class();
        self.resizes.agent_class()
    }

    /// The open epoch when the repaint nudge may act on it (`desync-risk`, the
    /// agent class, untried).
    pub(crate) fn resize_nudge_view(&self) -> Option<crate::resize_epoch::NudgeView> {
        self.resizes.nudge_view()
    }

    /// Whether render epoch `id` is still the open one and at risk
    /// ([`crate::resize_epoch::EpochBook::still_at_risk`]).
    pub(crate) fn resize_epoch_at_risk(&self, id: u64) -> bool {
        self.resizes.still_at_risk(id)
    }

    /// The repaint nudge held the PTY (or gave up) for epoch `id`: tried for good.
    pub(crate) fn note_resize_tried(&mut self, id: u64) {
        self.resizes.note_tried(id);
    }

    /// The repaint nudge's attempt for epoch `id` was superseded by a real
    /// resize ([`crate::resize_epoch::EpochBook::note_superseded`]).
    pub(crate) fn note_resize_superseded(&mut self, id: u64) {
        self.resizes.note_superseded(id);
    }

    /// A superseded attempt's epoch the judge found back at risk, once
    /// ([`crate::resize_epoch::EpochBook::take_returned`]).
    pub(crate) fn take_resize_returned(&mut self) -> Option<u64> {
        self.resizes.take_returned()
    }

    /// The repaint nudge's phase and attempt count, for `resizes`.
    pub(crate) fn set_resize_nudge(&mut self, phase: &'static str, nudged: u32) {
        self.resizes.set_nudge(phase, nudged);
    }

    /// A render epoch's look found the terminal locked: ask again soon.
    pub(crate) fn defer_resize_look(&mut self, now_ms: u64) {
        self.resizes.defer_look(now_ms);
    }

    /// `render= resizes= flaps=` for `status`, evaluated against `probe` (`None`:
    /// the terminal lock was contended, and `render=-`).
    pub(crate) fn resize_status_fields(
        &mut self,
        probe: Option<crate::resize_ledger::EngineProbe>,
        now_ms: u64,
    ) -> String {
        self.note_resize_class();
        let (fields, transitions) = self.resizes.status_fields(probe, now_ms);
        self.record_render(&transitions);
        fields
    }

    /// Tell the resize ledger whether this session's program is Claude Code, the
    /// one class its epoch judge accuses (`crate::resize_epoch`): the presence
    /// classifier's published reader.
    fn note_resize_class(&mut self) {
        self.resizes.set_agent(match self.agent.reader {
            Some(aterm_phase::Program::Claude) => true,
            Some(
                aterm_phase::Program::Codex
                | aterm_phase::Program::Agy
                | aterm_phase::Program::Muse
                | aterm_phase::Program::Gemini
                | aterm_phase::Program::Generic,
            )
            | None => false,
        });
    }

    /// Only entry into and exit from `desync-risk` reach the lifecycle ring:
    /// a healthy flap records nothing, so at one a minute it cannot evict the
    /// rows other watchers read.
    pub(crate) fn record_render(
        &mut self,
        transitions: &[crate::resize_ledger::Transition],
    ) -> bool {
        // One `aterm.log` line per render epoch that reached `desync-risk` or
        // `unjudged`: the field rate is then known whatever the repair does.
        for line in self.resizes.take_logs() {
            aterm_log::info!(
                "{line} program={}",
                self.agent.program.as_deref().unwrap_or("-")
            );
        }
        for transition in transitions {
            self.record("render", transition.payload());
            // THE SESSION LOG's `render` mark, in the same hold (phase 3B).
            let at = self.slog.now();
            self.slog.observe_render(transition, at);
        }
        !transitions.is_empty()
    }

    /// Events with `id > after`, oldest-first (a suffix, since ids only
    /// increase). `None` = all retained events. Double-ended, so a caller
    /// wanting the newest few can walk backwards.
    ///
    /// Seeks rather than filters: ids are monotone across the deque, so
    /// `partition_point` lands on the first event past the watermark, making
    /// the `events` digest's 250 ms liveness tick O(log n + matched). A
    /// watermark below the retained low-water yields everything; the digest
    /// first reports how many records the hole cost (`GAP … events-dropped=`,
    /// [`Self::low_id`]).
    ///
    /// A watermark ABOVE every id this timeline has handed out yields
    /// everything as well: it is no id of this count, but another process's —
    /// the window the PTY keeper recovered this shell from after a crash
    /// (`keeper_link`, which carries no count, so the recovery starts at 1),
    /// one seamless update's `HANDOFF_ID_RESERVE` above it. Filtered, a
    /// driver tailing `timeline since=<last id>` read `OK 0` for some 65,540
    /// events after the crash (round eight of the update audit, U13); read
    /// from the recovery's first row, its `handoff` row, it reads on at once.
    pub(crate) fn since(
        &self,
        after: Option<u64>,
    ) -> impl DoubleEndedIterator<Item = &TimelineEvent> + ExactSizeIterator {
        let start = match after {
            Some(a) if a <= self.next_id => self.events.partition_point(|e| {
                #[cfg(test)]
                crate::work_counts::retained_record_touched();
                e.id <= a
            }),
            Some(_) | None => 0,
        };
        self.events.range(start..)
    }

    /// The id a seamless update's manifest carries (`SessionRecord::timeline_id`): the last id
    /// handed out plus [`HANDOFF_ID_RESERVE`], since this process keeps recording after the
    /// park draws the manifest. Remembered as this process's ceiling; clamped to what TOML can
    /// write.
    #[cfg(any(unix, test))]
    pub(crate) fn handoff_id(&mut self) -> u64 {
        let carried = self
            .next_id
            .saturating_add(HANDOFF_ID_RESERVE)
            .min(i64::MAX as u64);
        self.handoff_ceiling = Some(carried);
        carried
    }

    /// Whether this process has handed out an id past the one the last manifest carried
    /// ([`Self::handoff_id`]), or a session-log id past its carry ([`Self::handoff_log_ids`]);
    /// the successor would hand it out again, so the Commit stands down.
    #[cfg(any(unix, test))]
    pub(crate) fn past_handoff_ceiling(&self) -> bool {
        self.handoff_ceiling
            .is_some_and(|ceiling| self.next_id > ceiling)
            || self.slog.past_handoff_ceiling()
    }

    /// The session log's handoff carry (`SessionRecord::log_ids`): its epoch
    /// and node-id counters plus their overlap reserve, remembered as this
    /// process's ceiling ([`Self::past_handoff_ceiling`]).
    #[cfg(any(unix, test))]
    pub(crate) fn handoff_log_ids(&mut self) -> String {
        self.slog.handoff_carry().row()
    }

    /// The session log's seq count for the handoff (`SessionRecord::log_seq`):
    /// its last seq plus its overlap reserve, remembered as this process's
    /// ceiling ([`Self::past_handoff_ceiling`]).
    #[cfg(any(unix, test))]
    pub(crate) fn handoff_log_seq(&mut self) -> u64 {
        self.slog.handoff_seq()
    }

    /// The adopted session's engine, hydrated: its next block id and C count,
    /// which a carried block cursor must fit (`SessionLog::fit_engine`).
    pub(crate) fn slog_fit_engine(&mut self, next_block: u64, started: u64) {
        let now = self.slog.now();
        self.slog.fit_engine(next_block, started, now);
    }

    /// THE HANDOFF WORKER'S TAKE of the session log (phase 2A): its carry
    /// snapshot and producers' lines, its readers fenced from now on.
    #[cfg(any(unix, test))]
    pub(crate) fn slog_carry_source(&mut self) -> crate::session_log::SlogCarrySource {
        self.slog.carry_source()
    }

    /// The session log ([`crate::session_log`]).
    pub(crate) fn slog(&self) -> &crate::session_log::SessionLog {
        &self.slog
    }

    /// A timeline whose session log is the test constructor's
    /// ([`crate::session_log::SessionLog::for_test`]: `ep=1`, an allocator
    /// of its own), for a unit test that reads epochs by hand (ruling U46).
    #[cfg(test)]
    pub(crate) fn for_test() -> SessionTimeline {
        SessionTimeline {
            slog: crate::session_log::SessionLog::for_test(),
            ..SessionTimeline::default()
        }
    }

    /// The session log, to write.
    pub(crate) fn slog_mut(&mut self) -> &mut crate::session_log::SessionLog {
        &mut self.slog
    }

    /// When the session log's turn tracker wants its settle tick (monotonic
    /// ms), if one is pending: what the event loop's [`crate::session_log::Wakes`]
    /// holds for this session.
    pub(crate) fn slog_wake(&self) -> Option<u64> {
        self.slog.wake()
    }

    /// The event loop's settle tick: a resting turn that has held closes.
    /// Returns the next wake.
    pub(crate) fn slog_tick(&mut self) -> Option<u64> {
        let at = self.slog.now();
        self.slog.tick(at)
    }

    /// Whether the session log's readers were owed a wake
    /// ([`crate::session_log::SessionLog::take_log_wake`]). An owed wake rings
    /// the session's watchers here, under this lock, as [`Self::record`]
    /// does: the caller wakes nobody itself.
    pub(crate) fn slog_take_wake(&mut self) -> bool {
        let owed = self.slog.take_log_wake();
        if owed {
            self.watchers.ring_watchers();
        }
        owed
    }

    /// Count on from a seamless update: the adopted session's timeline starts
    /// empty in the successor, but its ids continue above the last one the old
    /// process handed out, so a `timeline since=<id>` anchor taken before the
    /// update sees the `handoff` row and everything after. Called before the
    /// first record; never lowers the count, and never past what a manifest can
    /// carry.
    pub(crate) fn continue_ids(&mut self, carried: u64) {
        self.next_id = self.next_id.max(carried.min(i64::MAX as u64));
    }

    /// The highest recorded event id, or `None` when empty — the events digest
    /// seeds its watermark here so only post-subscription events push.
    pub(crate) fn high_id(&self) -> Option<u64> {
        self.events.back().map(|e| e.id)
    }

    /// The lowest retained event id, or `None` when empty — what a watermark is
    /// measured against to report a drop-oldest eviction as a `GAP`.
    pub(crate) fn low_id(&self) -> Option<u64> {
        self.events.front().map(|e| e.id)
    }

    /// Retained event count.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }

    /// The server's published program and agent verdict for this session.
    pub(crate) fn agent(&self) -> &AgentPublication {
        &self.agent
    }

    /// The identity already published for input to this foreground group.
    /// No kernel read: an unobserved process change is not vouched for here.
    pub(crate) fn foreground_stamp(&self) -> Option<ForegroundStamp> {
        let agent = crate::harness_host::identified_agent(&self.agent)?;
        (self.agent.program_pgid > 0 && self.foreground_generation < u64::MAX).then(|| {
            ForegroundStamp {
                generation: self.foreground_generation,
                pgid: self.agent.program_pgid,
                holder: self.current_holder_image().and_then(|image| image.token),
                agent,
            }
        })
    }

    fn advance_foreground_generation(&mut self) {
        // Exhaustion disables new stamps instead of aliasing an older one.
        self.foreground_generation = self.foreground_generation.saturating_add(1);
    }

    fn set_agent_reader(&mut self, reader: Option<aterm_phase::Program>) {
        let before = crate::harness_host::identified_agent(&self.agent);
        self.agent.reader = reader;
        if crate::harness_host::identified_agent(&self.agent) != before {
            self.advance_foreground_generation();
        }
    }

    /// The session's task record, `None` for a session with no task.
    pub(crate) fn task(&self) -> Option<&crate::task_record::TaskRecord> {
        self.task.as_ref()
    }

    /// Note what the worker's launcher did with its line ([`Self::worker_launch`]).
    pub(crate) fn note_worker_launch(&mut self, words: String) {
        self.worker_launch = Some((std::time::Instant::now(), words));
    }

    /// What the worker's launcher did with its line, and when; `None` where
    /// no launcher of this process got that far.
    pub(crate) fn worker_launch(&self) -> Option<&(std::time::Instant, String)> {
        self.worker_launch.as_ref()
    }

    /// Publish the session's need verdict ([`Self::need`]). Returns whether
    /// it moved.
    pub(crate) fn set_need(&mut self, verdict: crate::needs::Verdict) -> bool {
        let moved = self.need != verdict;
        self.need = verdict;
        moved
    }

    /// The published need verdict ([`crate::needs`]); the default (no need,
    /// no wait, no hold) until the first publish.
    pub(crate) fn need(&self) -> &crate::needs::Verdict {
        &self.need
    }

    /// Apply `mark` to the task record ([`crate::task_record::TaskMark::apply`])
    /// at `now`, the supervisor's attention as the caller read it under the
    /// meta guard; a move records one `task-change` row (the `EVENT <local>
    /// task …` push, [`crate::task_record::TaskRecord::event_payload`]) and,
    /// in the same hold, the session log's `task` mark (plan 5C-3,
    /// [`crate::session_log::SessionLog::observe_task`]), whose wake the
    /// caller takes before it releases the lock.
    /// Returns the record after the mark and whether it moved. The callers
    /// are [`crate::task_record::mark_session_task`]; nothing else writes the record.
    pub(crate) fn mark_task(
        &mut self,
        mark: &crate::task_record::TaskMark,
        attention: bool,
        now: std::time::Instant,
        now_unix_ms: u64,
    ) -> (crate::task_record::TaskRecord, bool) {
        let (next, changed) = mark.apply(self.task.as_ref(), attention, now, now_unix_ms);
        if changed {
            self.record("task-change", next.event_payload());
            let at = self.slog.now();
            self.slog.observe_task(&next, at);
            self.task = Some(next.clone());
        }
        (next, changed)
    }

    /// The supervisor's attention entry was raised (`true`) or cleared: on a
    /// session with a task record whose flag differs, flip it, bump the rev
    /// and record the `task-change` row and the log's `task` mark, as
    /// [`Self::mark_task`] does. Returns whether the record moved. A session
    /// with no task keeps none.
    pub(crate) fn note_task_attention(&mut self, raised: bool) -> bool {
        let Some(task) = self.task.as_mut().filter(|task| task.attention != raised) else {
            return false;
        };
        task.attention = raised;
        task.rev = task.rev.saturating_add(1);
        let payload = task.event_payload();
        let at = self.slog.now();
        self.slog.observe_task(task, at);
        self.record("task-change", payload);
        true
    }

    /// Seed the record a seamless update carried, before the adopted session
    /// is registered ([`crate::task_record::seed_carried`]). No row: nothing
    /// moved, the record only crossed.
    pub(crate) fn seed_task(&mut self, record: crate::task_record::TaskRecord) {
        self.task = Some(record);
    }

    /// Name the agent a worker's spawn started on its task record, and the
    /// one folder its starter trusted for it
    /// ([`crate::task_record::TaskRecord::agent`],
    /// [`crate::task_record::TaskRecord::trusted_cwd`]): the spawn's own
    /// write, on the main thread right after the record's first mark, where
    /// no update's Commit comes between them. A session with no task keeps
    /// none. Not news: the rev stays.
    pub(crate) fn set_task_worker(
        &mut self,
        agent: aterm_phase::Program,
        trusted_cwd: Option<String>,
    ) {
        if let Some(task) = self.task.as_mut() {
            task.agent = Some(agent);
            task.trusted_cwd = trusted_cwd;
        }
    }

    /// The row a seamless update carries for the record, `None` with no task.
    #[cfg(any(unix, test))]
    pub(crate) fn task_carry_row(&self) -> Option<String> {
        self.task
            .as_ref()
            .map(crate::task_record::TaskRecord::carry_row)
    }

    /// The published program name for foreground group `pgid`, or `None` when
    /// the publication names another group (or none). The PTY reader's
    /// `modes-restored` event names the program that lost the terminal with it,
    /// best effort: the sweep may already have moved on to the group that
    /// reclaimed it.
    pub(crate) fn program_of(&self, pgid: i32) -> Option<&str> {
        if pgid > 0 && self.agent.program_pgid == pgid {
            self.agent.program.as_deref()
        } else {
            None
        }
    }

    /// Publish an agent verdict read from the screen `stamp` names. A move of `word` or
    /// `detail` bumps `rev` and records one `agent-change` (`<word> rev=<n> gen=<g>
    /// fp=<hex16>`); an unchanged verdict only moves the stamp. While an input stall is
    /// published ([`Self::set_input`]) the effective word stays `wall:unresponsive`. Returns
    /// whether the effective verdict moved.
    pub(crate) fn publish_agent(
        &mut self,
        word: &'static str,
        detail: Option<String>,
        subject: Option<String>,
        reader: Option<aterm_phase::Program>,
        stamp: AgentStamp,
    ) -> bool {
        self.agent.subject = subject;
        let regrid = self.regrid_settling();
        self.note_text(stamp, regrid);
        let reader_changed = self.agent.reader != reader;
        let old_word = self.agent.word;
        if word == "busy" && self.agent.screen_word != "busy" && !regrid {
            self.agent.busy_entries = self.agent.busy_entries.wrapping_add(1);
        }
        self.set_agent_reader(reader);
        self.agent.screen_word = word;
        self.agent.screen_detail = detail;
        let changed = self.settle_agent_word(regrid);
        self.settle_regrid(regrid, changed);
        if reader_changed || self.agent.word != old_word {
            // One wake after publication, even if both reader and phase moved.
            crate::harness_host::ring_phase();
        } else if self.note_screen_move(std::time::Instant::now()) {
            crate::harness_host::ring_phase();
        }
        changed
    }

    /// The live zone moved under a verdict that stood. The status sweep publishes only when
    /// the zone moved, and an in-app `/clear` moves it without moving the verdict while Claude
    /// Code rewrites its record of the agent to name the new conversation. Counted
    /// ([`AgentPublication::screen_moves`]) while an agent is published and not busy (a busy
    /// one's spinner moves the zone at every read and changes no conversation). Returns
    /// whether to wake the supervisor host to follow the record
    /// (`harness_host::follow_entries`): at most once per `harness_host::STATUS_AFTER_SCREEN`,
    /// since its own settle looks again after the last move.
    fn note_screen_move(&mut self, now: std::time::Instant) -> bool {
        if matches!(self.agent.word, "busy" | "-") {
            return false;
        }
        self.agent.screen_moves += 1;
        let due = self.screen_rung_at.is_none_or(|at| {
            now.saturating_duration_since(at) >= crate::harness_host::STATUS_AFTER_SCREEN
        });
        if due {
            self.screen_rung_at = Some(now);
        }
        due
    }

    /// Publish (`Some`) or clear (`None`) the input stall ([`AgentPublication::input`]) and
    /// re-settle the effective word. Returns whether it moved, as [`Self::publish_agent`] does.
    pub(crate) fn set_input(&mut self, input: Option<crate::input_stall::InputStallFact>) -> bool {
        let old_word = self.agent.word;
        self.agent.input = input;
        let changed = self.settle_agent_word(false);
        if self.agent.word != old_word {
            crate::harness_host::ring_phase();
        }
        changed
    }

    /// Make [`AgentPublication::word`]/`detail` the effective verdict; a move
    /// bumps the rev and records one `agent-change`
    /// (`<word> rev=<n> gen=<g> fp=<hex16>`), and counts in
    /// [`AgentPublication::verdict_moves`] unless it is a `regrid`'s (a
    /// screen read within a re-grid's settle). Returns whether it moved.
    fn settle_agent_word(&mut self, regrid: bool) -> bool {
        let (word, detail) = self.agent.effective();
        if self.agent.word == word && self.agent.detail == detail {
            return false;
        }
        self.agent.word = word;
        self.agent.detail = detail;
        self.agent.rev += 1;
        if !regrid {
            self.agent.verdict_moves = self.agent.verdict_moves.wrapping_add(1);
        }
        self.agent.changed_ms = now_ms();
        self.agent.changed_continuous_ms = aterm_uds::process::continuous_ms();
        let (generation, fp) = self.agent.stamp_fields();
        let payload = format!("{word} rev={} gen={generation} fp={fp}", self.agent.rev);
        self.record("agent-change", payload);
        // The session log: `mark agent word= rev= gen= fp=` and the turn tracker's
        // transitions; a settle it arms is read back by the caller (`Self::slog_wake`).
        let at = self.slog.now();
        let moved = crate::session_log::AgentMove {
            word,
            detail: self.agent.detail.as_deref(),
            rev: self.agent.rev,
            stamp: self.agent.stamp,
            reader: self.agent.reader,
            pgid: self.agent.program_pgid,
        };
        let _ = self.slog.agent_moved(moved, at);
        true
    }

    /// The live zone was read again and was unchanged: the published verdict
    /// stands on this newer screen, so its stamp moves (no rev, no event).
    pub(crate) fn note_agent_stamp(&mut self, stamp: AgentStamp) {
        let regrid = self.regrid_settling();
        self.note_text(stamp, regrid);
        self.settle_regrid(regrid, false);
    }

    /// Whether this read of the screen is a re-grid's: within the settle of
    /// the first read that found the grid resized ([`GridRead::settling`]).
    fn regrid_settling(&self) -> bool {
        self.agent.grid.as_ref().is_some_and(GridRead::settling)
    }

    /// The first read past a re-grid's settle closes the burst: a verdict
    /// unlike the one that stood before the re-grid ([`GridRead::before`])
    /// is one move of the program's, unless this read's own move (`counted`)
    /// already counted it ([`AgentPublication::verdict_moves`]).
    fn settle_regrid(&mut self, regrid: bool, counted: bool) {
        if regrid {
            return;
        }
        let Some(before) = self.agent.grid.as_mut().and_then(|g| g.before.take()) else {
            return;
        };
        if !counted
            && (before.0, before.1.as_ref()) != (self.agent.word, self.agent.detail.as_ref())
        {
            self.agent.verdict_moves = self.agent.verdict_moves.wrapping_add(1);
        }
    }

    /// The verdict stands on the screen `stamp` names: its stamp moves, and
    /// [`AgentPublication::text_moves`] counts a text unlike the last read's
    /// — unless this read is a re-grid's (`regrid`, [`GridRead::settling`]).
    fn note_text(&mut self, stamp: AgentStamp, regrid: bool) {
        if !regrid && self.agent.stamp.is_some_and(|last| last.fp != stamp.fp) {
            self.agent.text_moves = self.agent.text_moves.wrapping_add(1);
        }
        self.agent.stamp = Some(stamp);
    }

    /// THE GRID AT THIS READ OF THE SCREEN, noted by the sweep before the
    /// stamp the same read gives ([`Self::publish_agent`],
    /// [`Self::note_agent_stamp`]): `resize` is the engine's
    /// `(resize_lifetime, resize_ordinal)` under the same guard as the read,
    /// `at` the sweep's instant. A `resize` unlike the last read's starts a
    /// re-grid's settle ([`GridRead::since`]), which keeps the text the resize
    /// and the program's repaint change out of
    /// [`AgentPublication::text_moves`], and the verdict they move out of
    /// [`AgentPublication::verdict_moves`]: the verdict that stood before the
    /// burst's first re-grid is kept to compare at its end
    /// ([`GridRead::before`]).
    pub(crate) fn note_grid(&mut self, resize: (u64, u64), at: std::time::Instant) {
        let (since, before) = match self.agent.grid.take() {
            Some(last) if last.resize != resize => (
                Some(at),
                last.before
                    .or_else(|| Some((self.agent.word, self.agent.detail.clone()))),
            ),
            Some(last) => (last.since, last.before),
            None => (None, None),
        };
        self.agent.grid = Some(GridRead {
            resize,
            since,
            at,
            before,
        });
    }

    /// The foreground process group moved to `pgid`: forget the program named
    /// for the old one (it is no longer what runs) and return whether a
    /// resolution is owed. The same group again owes nothing.
    pub(crate) fn note_foreground_group(&mut self, pgid: i32) -> bool {
        if self.agent.program_pgid == pgid {
            return false;
        }
        self.advance_foreground_generation();
        self.agent.program_pgid = pgid;
        let had_reader = self.agent.reader.take().is_some();
        self.leader_copy = None;
        if self.agent.program.take().is_some() || had_reader {
            // A runtime identified by its screen can have no named program;
            // its worker must still see the foreground group leave.
            crate::harness_host::ring();
        }
        // The session log: the runs of the group that left close.
        let at = self.slog.now();
        self.slog.foreground_moved(pgid, at);
        true
    }

    /// The status sweep saw the engine's cwd report `seq`
    /// (`Terminal::reported_cwd_seq`) with group `pgid` in front: the first
    /// sight of a report names the group it is held to, a later sight of the
    /// same report changes nothing. No group (`pgid <= 0`) or a carried cwd
    /// (`None`) witnesses nothing.
    ///
    /// The group is named only when it is SETTLED: in front at the sweep before
    /// this one (the group this timeline last noted) and at `pgid`, the group
    /// the sweep read under the same terminal hold as `seq`. The PTY names no
    /// writer, so a report first seen after the front moved (an ssh that exited
    /// between its remote shell's OSC 7 and this look, a sweep a contended lock
    /// skipped) may be the group's that left: it is held to no group (`0`), and
    /// stands under none ([`crate::path_link::report_stands`]), so neither its
    /// host nor its directory is read until the next report.
    pub(crate) fn witness_cwd_report(&mut self, seq: Option<u64>, pgid: i32) {
        let Some(seq) = seq.filter(|_| pgid > 0) else {
            return;
        };
        if self.cwd_witness.is_none_or(|(seen, _)| seen != seq) {
            let settled = self.agent.program_pgid == pgid;
            self.cwd_witness = Some((seq, if settled { pgid } else { 0 }));
        }
    }

    /// [`Self::witness_cwd_report`]'s record: the report and the group it was
    /// first seen under.
    pub(crate) fn cwd_witness(&self) -> Option<(u64, i32)> {
        self.cwd_witness
    }

    /// The session's own process group (its shell's pid), for the session
    /// log: the shell in front is the session at its prompt, never a run.
    pub(crate) fn note_root_group(&mut self, pgid: i32) {
        self.slog.note_root(pgid);
    }

    /// The status sweep's session-log step, in one order under one hold of this lock. First the
    /// shell's `blocks` (read under the sweep's terminal guard): a command the shell reported
    /// ended closes at its own D, and with it the program and agent runs under it (the D
    /// proves they had ended), and the names the resolver gave since the last sweep
    /// ([`Self::set_program`]) open under the command each ran in, which only the blocks tell.
    /// Then a failure the person was looking at as it published (`seen`, the status observer's
    /// `FailureMark`), once its run has closed, as `mark - seen`. Then the foreground group
    /// `pgid` under the session's own group `root`, whose move closes only what the blocks left
    /// open. The blocks are told `pgid` (read after them), so a command still running never
    /// hangs under a run that move closes, and a name given before a command began is placed
    /// by whether its group still holds the front. Returns the group that left when the group
    /// moved (`pgid > 0` and new), for which [`Self::note_foreground_group`] owes a resolution.
    pub(crate) fn sweep_slog(
        &mut self,
        blocks: &crate::session_log::BlockRead,
        seen: bool,
        root: i32,
        pgid: i32,
    ) -> Option<i32> {
        let at = self.slog.now();
        self.slog
            .shell_blocks(blocks, (pgid > 0).then_some(pgid), at);
        if seen {
            self.slog.failure_seen(at);
        }
        if pgid <= 0 {
            return None;
        }
        let previous = self.agent.program_pgid;
        self.note_root_group(root);
        self.note_foreground_group(pgid).then_some(previous)
    }

    /// THE HOLDER IMAGE'S ONE WRITER: the PTY reader's parse stage offers the
    /// image it read at a foreground edge (or the byte arm's re-read) before
    /// the hold of the slice that starts there. Published when it or the image
    /// it replaces is card-relevant and it is not that same image
    /// (`holder_image::decide`); the dev rows `holder-edge` (stamped when the
    /// image was read) and `holder-image` (stamped now) record each
    /// publication.
    pub(crate) fn offer_holder_image(
        &mut self,
        image: std::sync::Arc<crate::holder_image::HolderImage>,
        kind: crate::holder_image::EdgeKind,
    ) -> crate::holder_image::Offer {
        let offer = crate::holder_image::decide(self.holder_image.as_deref(), &image);
        if offer == crate::holder_image::Offer::Published {
            self.advance_foreground_generation();
            if crate::holder_image::DEV_ROWS {
                self.record(
                    "holder-edge",
                    format!(
                        "kind={} token={} pgid={} present={} at_ns={}",
                        kind.word(),
                        crate::holder_image::token_word(image.token),
                        image.pgid,
                        crate::metrics::frames_presented(),
                        image.read_ns,
                    ),
                );
                self.record(
                    "holder-image",
                    format!("{} {}", image.payload(), crate::holder_image::dev_tail()),
                );
            }
            let pgid = image.pgid;
            let drawer = image.drawer.is_some().then_some(image.token).flatten();
            self.holder_image = Some(image);
            // The row standing for the drawer's group as its image is
            // published is a row from before it — the image an `exec`
            // replaced wrote it, or another image of the group came before —
            // unless it came while this very image was in front (it was
            // published before: `fg`, the group back in front), or before
            // any image of the group was, and this is the group's arrival
            // (a group edge: the byte arm's first read after the row).
            // Facts judged its own are this image's from here on.
            if let Some(token) = drawer {
                let standing = self
                    .claude_footer
                    .as_ref()
                    .filter(|(facts_pgid, _, _)| *facts_pgid == pgid)
                    .map(|(_, _, facts)| facts.owner.clone());
                let before = match self.claude_footer_image {
                    Some(image) => image != token,
                    None => kind == crate::holder_image::EdgeKind::Exec,
                };
                match standing {
                    Some(owner) if before => self.registry_drift.stale = Some((token, owner)),
                    Some(_) => {
                        self.registry_drift.stale = Some((token, None));
                        self.claude_footer_image = Some(token);
                    }
                    None => self.registry_drift.stale = Some((token, None)),
                }
            }
        }
        offer
    }

    /// The holder image last published ([`Self::offer_holder_image`]).
    pub(crate) fn holder_image(&self) -> Option<&std::sync::Arc<crate::holder_image::HolderImage>> {
        self.holder_image.as_ref()
    }

    /// The published holder image while it is CURRENT: its group is the one
    /// in front (`program_pgid`, the currency rule of [`Self::set_program`]
    /// and [`Self::claude_footer`]). A finished job's image left published
    /// is no one's.
    fn current_holder_image(&self) -> Option<&crate::holder_image::HolderImage> {
        self.holder_image
            .as_deref()
            .filter(|image| image.pgid > 0 && image.pgid == self.agent.program_pgid)
    }

    /// Whether the current holder image is a card drawer with a token: the
    /// pane takes the card's path with no footer facts (card engine §3.8).
    pub(crate) fn card_drawer_current(&self) -> bool {
        self.current_holder_image()
            .is_some_and(|image| image.drawer.is_some() && image.token.is_some())
    }

    /// THE CARD'S GATE (card engine §3.8), read under the splice's one
    /// timeline lock per pane: the current holder image's
    /// [`crate::holder_image::CardGate`], or `None` where the image is no
    /// drawer, has no token or is not current. An image of unknown build is
    /// held to the build ITS registry row reports ([`Self::registry_build`]);
    /// any other image never reads the registry. Integers only: no lock
    /// beyond the caller's, no clock, no allocation.
    pub(crate) fn card_gate(&self) -> Option<crate::holder_image::CardGate> {
        let image = self.current_holder_image()?;
        image.gate(|| self.registry_build(image))
    }

    /// The build `image`'s own registry row reports (`FooterFacts::version`,
    /// pushed by the footer's resolver, parsed when it was stored): facts
    /// for its group, with an owner, and not the owner whose facts stood
    /// when the image was first published (a row from before its `exec`,
    /// the replaced image's), whose version parses. `None` otherwise: the
    /// card waits for the row.
    fn registry_build(
        &self,
        image: &crate::holder_image::HolderImage,
    ) -> Option<atpkg::vendor_direct::Version> {
        let (pgid, _, facts) = self.claude_footer.as_ref()?;
        let owner = facts.owner.as_ref()?;
        if *pgid != image.pgid
            || self
                .registry_drift
                .stale
                .as_ref()
                .is_some_and(|(stale, before)| {
                    Some(*stale) == image.token && before.as_ref() == Some(owner)
                })
        {
            return None;
        }
        self.claude_footer_build
    }

    /// The token of the published image of group `pgid` when it is a Claude
    /// Code drawer from the managed store (a vendor build, or an ALab one
    /// named by aterm's record) or the vendor's installer (card engine
    /// §3.10): the image whose `exec` edge the status sweep asks the footer
    /// for again, since an ask before it may have fallen in a `/bin/sh`
    /// shim's window, where the Claude dir cannot be read. `None` for a
    /// launcher, another program or an unknown image, and for an image whose
    /// token could not be formed.
    pub(crate) fn claude_drawer_token(&self, pgid: i32) -> Option<u64> {
        self.holder_image
            .as_deref()
            .filter(|image| {
                image.pgid == pgid
                    && image.drawer == Some(crate::holder_image::CardProgram::Claude)
                    && matches!(
                        image.build.source,
                        atpkg::image::ImageSource::Store
                            | atpkg::image::ImageSource::StoreRecord
                            | atpkg::image::ImageSource::Native
                    )
            })
            .and_then(|image| image.token)
    }

    /// THE DRIFT (card engine §3.10): the build the group's registry row
    /// reports (`FooterFacts::version`) disagreeing with the build its Store
    /// or Native Claude Code drawer image names — said ONCE per image and
    /// reported build, and read by nothing that decides the card: the
    /// caller raises an exception entry (`App::note_claude_build_drift`).
    /// Judged only on facts with an owner other than the one standing when
    /// the image was published (a row from before an `exec` in place is the
    /// replaced image's), and only on a reported build that parses: an
    /// unknown spelling is no evidence of another build. An ALab build's
    /// image (`StoreRecord`) is not judged: its build is aterm's record, not
    /// the vendor's spelling.
    pub(crate) fn take_registry_drift(&mut self) -> Option<RegistryDrift> {
        let image = self.holder_image.as_deref()?;
        if !matches!(
            image.build.source,
            atpkg::image::ImageSource::Store | atpkg::image::ImageSource::Native
        ) {
            return None;
        }
        let token = self.claude_drawer_token(image.pgid)?;
        let expected = image.build.version()?;
        let (pgid, _, facts) = self
            .claude_footer
            .as_ref()
            .filter(|(pgid, _, _)| *pgid == image.pgid)?;
        let owner = facts.owner.as_ref()?;
        let reported = facts.version.as_deref()?;
        let stale = self
            .registry_drift
            .stale
            .as_ref()
            .is_some_and(|(stale, before)| *stale == token && before.as_ref() == Some(owner));
        if stale || atpkg::vendor_direct::Version::parse(reported)? == expected {
            return None;
        }
        if self
            .registry_drift
            .said
            .as_ref()
            .is_some_and(|(said, words)| *said == token && words == reported)
        {
            return None;
        }
        let drift = RegistryDrift {
            pgid: *pgid,
            image: expected.to_string(),
            source: image.build.source,
            reported: reported.to_owned(),
        };
        self.registry_drift.said = Some((token, drift.reported.clone()));
        Some(drift)
    }

    /// The `card-seen` dev row, once per snapshot token: the current drawer's
    /// family and known version were observed on screen, the banner at
    /// absolute row `row`. This is not writer attribution and can match
    /// stale same-build cells. `after=` is the last card pass's count that
    /// judged this session's card, so a judge counts the passes composed
    /// between the sight and the `card-decorated` row. Returns whether it
    /// recorded (`rows_kind` 3 Claude, 4 agy, 1 Muse).
    pub(crate) fn note_card_seen(
        &mut self,
        token: u64,
        rows_kind: usize,
        version: &str,
        row: i64,
    ) -> bool {
        if self.card_seen == Some(token)
            || !self.holder_image.as_ref().is_some_and(|image| {
                image.token == Some(token) && image.matches_card(rows_kind, version)
            })
        {
            return false;
        }
        self.card_seen = Some(token);
        self.card_frames.seen = Some((token, row, self.card_frames.composed));
        self.record(
            "card-seen",
            format!(
                "token={} rows_kind={rows_kind} row={row} after={} evidence=matching-screen {}",
                crate::holder_image::token_word(Some(token)),
                self.card_frames.composed,
                crate::holder_image::dev_tail()
            ),
        );
        true
    }

    /// The dev rows of one card pass's outcome for this session's card
    /// (`App::compose_claude_cards`, run only under the dev card probe;
    /// `composed` is the window's pass count): `card-decorated token= row=
    /// bare_frames=` the first frame a drawer's card is decorated, where
    /// `bare_frames` counts the frames composed before it that showed the
    /// drawer's card bare — read and refused by the gate at the row it is
    /// decorated at, or with no card read once its card was seen on screen
    /// (`card-seen`; [`BareFrames`]), and at least every pass composed after
    /// that sight of the same row; `card-armed token= cause=` each episode
    /// armed; `card-row token= row=` each time the same drawer's card is
    /// decorated at another banner row (a repaint after `fg`, say); and
    /// `card-bare reason= token= row=` each time the reason a card is bare,
    /// or the row of the card left bare, changes (card engine §8).
    pub(crate) fn note_card_frame(
        &mut self,
        outcome: crate::claude_card::CardOutcome,
        composed: u64,
    ) {
        use crate::claude_card::{Bare, CardOutcome};
        let token_word = |t: u64| crate::holder_image::token_word((t != 0).then_some(t));
        self.card_frames.composed = composed;
        match outcome {
            CardOutcome::Decorated { token, row, armed } => {
                self.card_frames.said = None;
                let moved = self.card_frames.decorated == Some(token)
                    && self.card_frames.decorated_row != Some(row);
                self.card_frames.decorated_row = Some(row);
                if moved {
                    self.record(
                        "card-row",
                        format!(
                            "token={} row={row} composed={composed} {}",
                            token_word(token),
                            crate::holder_image::dev_tail()
                        ),
                    );
                }
                if self.card_frames.decorated != Some(token) {
                    self.card_frames.decorated = Some(token);
                    // The frames counted bare, or every pass composed since
                    // this card was seen on screen, whichever is more.
                    let late = match self.card_frames.seen {
                        Some((t, r, after)) if t == token && r == row => {
                            u32::try_from(composed.saturating_sub(after).saturating_sub(1))
                                .unwrap_or(u32::MAX)
                        }
                        _ => 0,
                    };
                    let bare = self.card_frames.bare.of(token, row).max(late);
                    self.card_frames.bare = BareFrames::default();
                    self.record(
                        "card-decorated",
                        format!(
                            "token={} row={row} bare_frames={bare} seen={} composed={composed} {}",
                            token_word(token),
                            u8::from(self.card_seen == Some(token)),
                            crate::holder_image::dev_tail()
                        ),
                    );
                }
                if let Some(cause) = armed {
                    self.record(
                        "card-armed",
                        format!(
                            "token={} cause={cause} composed={composed} {}",
                            token_word(token),
                            crate::holder_image::dev_tail()
                        ),
                    );
                }
            }
            CardOutcome::Bare(token, why, row) => {
                let refused = matches!(
                    why,
                    Bare::Token
                        | Bare::Kind
                        | Bare::Fence
                        | Bare::Tainted
                        | Bare::Build
                        | Bare::Deferred
                );
                // A refusal counts where a card was read (its row): a fence
                // of another holder's reads none, and counts only once the
                // drawer's card was seen on screen, as no card read does.
                if token != 0
                    && self.card_frames.decorated != Some(token)
                    && ((refused && row.is_some()) || self.card_seen == Some(token))
                {
                    self.card_frames.bare.count(token, row);
                }
                if self.card_frames.said != Some((token, why, row)) {
                    self.card_frames.said = Some((token, why, row));
                    let row = row.map_or_else(|| "-".to_string(), |r| r.to_string());
                    self.record(
                        "card-bare",
                        format!(
                            "reason={} token={} row={row} composed={composed} {}",
                            why.word(),
                            token_word(token),
                            crate::holder_image::dev_tail()
                        ),
                    );
                }
            }
        }
    }

    /// The token of the current holder image when it is a drawer whose card
    /// has not been seen yet: the parse stage's dev probe looks only then.
    pub(crate) fn card_unseen(&self) -> Option<u64> {
        self.holder_image
            .as_ref()
            .filter(|i| i.drawer.is_some() && i.build.version().is_some())
            .and_then(|i| i.token)
            .filter(|t| self.card_seen != Some(*t))
    }

    /// The resolver's answer for `pgid`, applied only while that group is
    /// still the foreground one (a late answer for a finished job is dropped).
    /// Returns whether the current name changed, which is the worker's signal
    /// to wake the GUI status observer.
    pub(crate) fn set_program(&mut self, pgid: i32, program: Option<String>) -> bool {
        if self.agent.program_pgid == pgid && self.agent.program != program {
            self.advance_foreground_generation();
            if crate::holder_image::DEV_ROWS
                && let Some(card) = program
                    .as_deref()
                    .and_then(crate::holder_image::CardProgram::of)
            {
                self.record(
                    "holder-named",
                    format!(
                        "program={} pgid={pgid} {}",
                        card.word(),
                        crate::holder_image::dev_tail()
                    ),
                );
            }
            self.agent.program = program;
            // The in-GUI supervisor host attaches by program (`harness_host`).
            crate::harness_host::ring();
            // The session log: the name waits for the sweep's next block read, which hangs
            // its `prog` (or `agent`) run under the command it ran in; its tracker starts
            // from the verdict published so far.
            let at = self.slog.now();
            self.slog.program_named(
                pgid,
                self.agent.program.as_deref(),
                self.agent.word,
                self.agent.detail.as_deref(),
                at,
            );
            return true;
        }
        false
    }

    /// The footer resolver's answer for `pgid` and the process's kernel start time (when
    /// read), applied only while that group is in front. Returns whether the painted facts
    /// moved.
    pub(crate) fn set_claude_footer(
        &mut self,
        pgid: i32,
        started: Option<u64>,
        facts: Option<aterm_agent::harness::footer::FooterFacts>,
    ) -> bool {
        if self.agent.program_pgid != pgid {
            return false;
        }
        let mut next = facts.map(|facts| (pgid, started, facts));
        let is_muse = match self
            .agent
            .program
            .as_deref()
            .and_then(aterm_phase::program_of)
        {
            Some(aterm_phase::Program::Muse) => true,
            Some(
                aterm_phase::Program::Claude
                | aterm_phase::Program::Codex
                | aterm_phase::Program::Agy
                | aterm_phase::Program::Gemini
                | aterm_phase::Program::Generic,
            )
            | None => false,
        };
        let mut switched = false;
        if is_muse
            && let Some(email) = next
                .as_ref()
                .and_then(|(_, _, facts)| facts.subscription.as_ref()?.email.as_deref())
            && self.muse_card_account.as_deref() != Some(email)
        {
            switched = self.muse_card_account.is_some();
            self.muse_cards_need_account |= switched;
            self.muse_card_account = Some(email.to_owned());
        }
        if is_muse
            && self.muse_cards_need_account
            && let Some(sub) = next
                .as_mut()
                .and_then(|(_, _, facts)| facts.subscription.as_mut())
        {
            // Publication itself enforces the boundary, including a frame
            // that first discovers B while carrying A's anonymous quota.
            sub.weekly = None;
            sub.weekly_reset = None;
            if switched {
                sub.plan = None;
            }
        }
        if self.claude_footer == next {
            return false;
        }
        // A card belongs to one process and one owner: new facts for
        // anything else drop it. A bootstrap reading (`s == None`, `owner.floor == None`)
        // promotes in place when the resolver thread publishes the same process's
        // kernel start time (`ns`) and floor.
        if let Some((p, s, owner, card)) = self.claude_card.clone() {
            if let Some((np, ns, facts)) = &next
                && *np == p
                && (s.is_none() || *ns == s)
                && let Some(no) = &facts.owner
                && no.session_id == owner.session_id
                && (owner.floor.is_none() || no.floor == owner.floor)
            {
                self.claude_card = Some((*np, *ns, no.clone(), card));
            } else {
                self.claude_card = None;
            }
        }
        self.claude_footer = next;
        self.claude_footer_image = self
            .holder_image
            .as_deref()
            .filter(|image| image.pgid == pgid && image.drawer.is_some())
            .and_then(|image| image.token);
        self.claude_footer_build = self
            .claude_footer
            .as_ref()
            .and_then(|(_, _, facts)| facts.version.as_deref())
            .and_then(atpkg::vendor_direct::Version::parse);
        true
    }

    /// Keep the launch `card` the frame compose read for the process whose
    /// facts are stored (only while they are this group's and theirs is
    /// `owner`), replacing any kept before. Returns whether it changed what
    /// is kept. The caller hands only a reading the harness card's gate
    /// admitted — the current drawer's own card, by its fence and its
    /// build, and its own status row (`claude_card::GateCheck::admits_reading`,
    /// card engine §3.8) — so no predecessor's card, and no card quoted
    /// under the drawer's, fills the footer.
    pub(crate) fn note_claude_card(
        &mut self,
        pgid: i32,
        owner: &aterm_agent::harness::footer::FactsOwner,
        card: aterm_phase::LaunchCard,
    ) -> bool {
        let Some((p, started, facts)) = self.claude_footer.as_ref() else {
            return false;
        };
        if *p != pgid || self.agent.program_pgid != pgid || facts.owner.as_ref() != Some(owner) {
            return false;
        }
        let next = Some((pgid, *started, owner.clone(), card));
        if self.claude_card == next {
            return false;
        }
        self.claude_card = next;
        // THE SESSION LOG: the card's names amend the agent run (phase 3B).
        if let Some((.., card)) = self.claude_card.as_ref() {
            let at = self.slog.now();
            self.slog.observe_card(card, at);
        }
        true
    }

    /// The process the stored footer facts were read for — its group and its
    /// kernel start time — if any.
    pub(crate) fn claude_footer_identity(&self) -> Option<(i32, Option<u64>)> {
        self.claude_footer
            .as_ref()
            .map(|(pgid, started, _)| (*pgid, *started))
    }

    /// A known switch or carried glass cannot attribute an anonymous Muse
    /// card to the current login. Only an account-bearing card may fill its
    /// own fields; a separate usage card supplies no account proof.
    pub(crate) fn muse_cards_need_account(&self) -> bool {
        self.muse_cards_need_account
    }

    /// An adopted terminal may still show another account's cards, and its
    /// predecessor need not have carried an account observation. Revoke the
    /// bootstrap allowance without adding a version-dependent wire field.
    pub(crate) fn note_carried_glass(&mut self) {
        self.muse_cards_need_account = true;
    }

    /// Forget the footer facts; whether there were any. For when the status
    /// sweep that keeps them current stops (`tab_status` off), so stale facts
    /// are not painted over the next Claude Code in this tab. The survey's
    /// excerpts go with them.
    pub(crate) fn clear_claude_footer(&mut self) -> bool {
        self.claude_card = None;
        self.claude_excerpts = None;
        self.claude_side = None;
        self.claude_footer_read = None;
        self.claude_footer_build = None;
        self.claude_footer_image = None;
        self.claude_footer.take().is_some()
    }

    /// The footer resolver read `pgid`'s facts at `at` (a fresh read, its
    /// usage folded anew), kept only while that group is in front.
    pub(crate) fn note_claude_footer_read(&mut self, pgid: i32, at: std::time::Instant) {
        if self.agent.program_pgid == pgid {
            self.claude_footer_read = Some((pgid, at));
        }
    }

    /// When the facts [`Self::claude_footer`] gives were last read, while
    /// they are the group in front's.
    pub(crate) fn claude_footer_read_at(&self) -> Option<std::time::Instant> {
        self.claude_footer_read
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, at)| at)
    }

    /// The footer resolver's excerpts for `pgid`, applied only while that
    /// group is in front, as [`Self::set_claude_footer`] applies its facts;
    /// `None` forgets them.
    pub(crate) fn set_claude_excerpts(
        &mut self,
        pgid: i32,
        excerpts: Option<crate::survey::ClaudeExcerpts>,
    ) {
        if self.agent.program_pgid != pgid {
            return;
        }
        self.claude_excerpts = excerpts.map(|e| (pgid, e));
    }

    /// The survey's excerpts for the CURRENT foreground group, or `None` when
    /// the program in front is not the one they were read for.
    pub(crate) fn claude_excerpts(&self) -> Option<&crate::survey::ClaudeExcerpts> {
        self.claude_excerpts
            .as_ref()
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, excerpts)| excerpts)
    }

    /// The footer resolver's side for `pgid`, applied only while that group
    /// is in front, as [`Self::set_claude_excerpts`] applies its excerpts;
    /// `None` forgets it.
    pub(crate) fn set_claude_side(
        &mut self,
        pgid: i32,
        side: Option<std::sync::Arc<crate::claude_footer::FactSide>>,
    ) {
        if self.agent.program_pgid != pgid {
            return;
        }
        self.claude_side = side.map(|side| (pgid, side));
    }

    /// The footer's side for the CURRENT foreground group, or `None` when the
    /// program in front is not the one it was read for.
    pub(crate) fn claude_side(&self) -> Option<&std::sync::Arc<crate::claude_footer::FactSide>> {
        self.claude_side
            .as_ref()
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, side)| side)
    }

    /// The status sweep's latest [`AgentGauges`] for the agent in front.
    pub(crate) fn set_gauges(&mut self, gauges: AgentGauges) {
        self.gauges = gauges;
    }

    /// The gauges as last read; none while no identified agent is in front.
    pub(crate) fn gauges(&self) -> AgentGauges {
        if self.agent.reader.is_some() {
            self.gauges
        } else {
            AgentGauges::default()
        }
    }

    /// The footer facts for the CURRENT foreground group, or `None` when the
    /// program in front is not the one they were read for.
    pub(crate) fn claude_footer(&self) -> Option<&aterm_agent::harness::footer::FooterFacts> {
        self.claude_footer
            .as_ref()
            .filter(|(pgid, _, _)| *pgid == self.agent.program_pgid)
            .map(|(_, _, facts)| facts)
    }

    /// The launch card kept for the process whose facts are in front — `None`
    /// once they are another process's or another owner's.
    pub(crate) fn claude_card(&self) -> Option<&aterm_phase::LaunchCard> {
        let (pgid, started, facts) = self
            .claude_footer
            .as_ref()
            .filter(|(pgid, _, _)| *pgid == self.agent.program_pgid)?;
        self.claude_card
            .as_ref()
            .filter(|(p, s, owner, _)| {
                (p, s) == (pgid, started) && facts.owner.as_ref() == Some(owner)
            })
            .map(|(_, _, _, card)| card)
    }

    /// What the footer shows for the group in front: its facts, filled from the
    /// kept launch card where nothing newer decided them
    /// (`FooterFacts::filled_from`). The survey publishes it (`survey.rs`).
    pub(crate) fn claude_footer_shown(&self) -> Option<aterm_agent::harness::footer::FooterFacts> {
        self.claude_footer()
            .map(|facts| facts.filled_from(self.claude_card()))
    }

    /// The leader read's other two answers for `pgid`
    /// (`session_status::program::leader_facts`), applied only while that group
    /// is in front: the agent copy it runs and, when it is the shell's direct
    /// child, a PATH measurement that replaces the last one (agreed when the
    /// previous job read the same verdict, [`PathReading`]; owed to the
    /// registry's mark when live).
    pub(crate) fn set_leader(
        &mut self,
        pgid: i32,
        copy: Option<&'static str>,
        path: Option<crate::session_status::program::PathVerdict>,
    ) {
        if self.agent.program_pgid != pgid {
            return;
        }
        self.leader_copy = copy.map(|c| (pgid, c));
        if let Some(verdict) = path {
            let same = self.path.filter(|p| p.verdict.word() == verdict.word());
            self.path = Some(PathReading {
                verdict,
                at_ms: now_ms(),
                pgid,
                agreed: same.is_some_and(|p| p.agreed || p.pgid != pgid),
                reported: same.is_some_and(|p| p.reported),
            });
        }
    }

    /// `(path word, age in ms, settled)` of the last PATH measurement
    /// ([`PathReading::settled`]), or `None`.
    pub(crate) fn path_reading(&self) -> Option<(&'static str, u64, bool)> {
        self.path.map(|p| {
            (
                p.verdict.word(),
                now_ms().saturating_sub(p.at_ms),
                p.settled(),
            )
        })
    }

    /// Whether the shell's PATH is measured frozen and settled: two jobs in a row put a foreign
    /// `claude`/`codex` first ([`PathReading`]).
    pub(crate) fn path_settled_frozen(&self) -> bool {
        self.path
            .is_some_and(|p| p.settled() && p.verdict.word() == "frozen")
    }

    /// `true` once for a live PATH reading, then `false` until a live reading follows a frozen
    /// one; the status sweep lowers the registry's frozen mark on it. A frozen reading never
    /// raises the mark.
    pub(crate) fn take_path_lowered(&mut self) -> bool {
        let Some(p) = self
            .path
            .as_mut()
            .filter(|p| !p.reported && p.verdict.word() == "live")
        else {
            return false;
        };
        p.reported = true;
        true
    }

    /// `copy=` of the current foreground leader, or `None`.
    pub(crate) fn copy(&self) -> Option<&'static str> {
        self.leader_copy
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, c)| c)
    }

    /// Replace this tab's live-upgrade row; whether it moved.
    pub(crate) fn set_upgrade(
        &mut self,
        row: Option<aterm_agent::harness::upgrade_drive::Row>,
    ) -> bool {
        if self.upgrade == row {
            return false;
        }
        self.upgrade = row;
        true
    }

    /// This tab's live-upgrade row, or `None`.
    pub(crate) fn upgrade(&self) -> Option<&aterm_agent::harness::upgrade_drive::Row> {
        self.upgrade.as_ref()
    }

    /// Replace the upgrade's note for the person here; whether it moved (the one change of the
    /// row a frame must show).
    pub(crate) fn set_upgrade_note(
        &mut self,
        note: Option<aterm_agent::harness::footer::UpgradeNote>,
    ) -> bool {
        if self.upgrade_note == note {
            return false;
        }
        self.upgrade_note = note;
        true
    }

    /// The live upgrade's note for the person here, while the move waits
    /// only on them (`Row::pause_note`), or `None`.
    pub(crate) fn upgrade_note(&self) -> Option<&aterm_agent::harness::footer::UpgradeNote> {
        self.upgrade_note.as_ref()
    }

    /// Replace the task's face ([`Self::task_face()`]): `face` and the wall
    /// time of its move (unix ms), or `None` for "unknown". Whether the face
    /// moved — a move to the face already held keeps its first time, so a
    /// face told again is no change. A face told after "unknown" that is the
    /// one held before it (a landed session's next loop, which starts landed)
    /// takes back that face's first time: `landed 17:42` stays 17:42 across a
    /// restart.
    pub(crate) fn set_task_face(
        &mut self,
        face: Option<aterm_agent::harness::acts::TaskFace>,
        at_ms: i64,
    ) -> bool {
        if self.task_face.map(|(f, _)| f) == face {
            return false;
        }
        let held = self.task_face_held.take();
        self.task_face = match face {
            None => {
                self.task_face_held = self.task_face;
                None
            }
            Some(f) => Some(held.filter(|&(h, _)| h == f).unwrap_or((f, at_ms))),
        };
        true
    }

    /// The task's face as this window's host last heard it, and the wall
    /// time (unix ms) it moved there; `None` while unknown.
    pub(crate) fn task_face(&self) -> Option<(aterm_agent::harness::acts::TaskFace, i64)> {
        self.task_face
    }

    /// A SEAMLESS UPDATE'S CARRIED END (`docs/DESIGN-harness-autopilot-
    /// 2026-10-15.md` §11.2.3, item 8; `spawn::seed_adopted_end`): `face` and
    /// the wall time (unix ms) it moved there in the outgoing process, HELD
    /// as a face is held over "unknown" — where this timeline holds no face
    /// and none is held (a successor's, before registration) — so the loop's
    /// restated face takes that time back ([`Self::set_task_face`]'s rule),
    /// and another face its own. Whether it was seeded.
    pub(crate) fn seed_task_face_held(
        &mut self,
        face: aterm_agent::harness::acts::TaskFace,
        at_ms: i64,
    ) -> bool {
        if self.task_face.is_some() || self.task_face_held.is_some() {
            return false;
        }
        self.task_face_held = Some((face, at_ms));
        true
    }

    /// The wall time (unix ms) the task's face moved to `face`, where that is
    /// the face this timeline holds — or held when its loop ended and no face
    /// was told since ("unknown" between loop runs) — else `None`: the
    /// instant a task's end standing on the glass was told at
    /// (`harness_glass::EndGlass`), which the menu bar's Done for you row
    /// ages from (`App::status_session_row`) and a seamless update's park
    /// carries of a standing end (`App::end_carry_rows`).
    pub(crate) fn task_face_at(&self, face: aterm_agent::harness::acts::TaskFace) -> Option<i64> {
        self.task_face
            .or(self.task_face_held)
            .filter(|(held, _)| *held == face)
            .map(|(_, at)| at)
    }

    /// Replace the loop's pause ([`Self::pause()`]): whether it moved.
    pub(crate) fn set_pause(&mut self, pause: Option<crate::PausePlan>) -> bool {
        std::mem::replace(&mut self.pause, pause) != pause
    }

    /// The loop's pause after short turns as this window's host last heard
    /// it; `None` while none stands.
    pub(crate) fn pause(&self) -> Option<crate::PausePlan> {
        self.pause
    }

    /// The worker's NEEDS YOU set as its loop told it, `told` (`None`, the
    /// take-down), with `standing`, the needs owner's entry as the server
    /// holds it now ([`crate::needs::ToldNeeds::tell`]): whether what a
    /// reader reads beside that entry moved, so the window must be woken.
    /// The set is built and the entry copied before the caller takes this
    /// lock, so the write under it is a few text comparisons and two swaps.
    pub(crate) fn set_harness_needs(
        &mut self,
        told: Option<Arc<crate::needs::HarnessNeeds>>,
        standing: Option<&str>,
    ) -> bool {
        self.harness_needs.tell(told, standing)
    }

    /// The worker's NEEDS YOU sets as its loop told this window's host,
    /// whole, each with the entry's text it was told with — what slice 2d's
    /// readers read, a set only beside its own entry
    /// ([`crate::needs::NeedFacts::told_needs`]). Two `Arc`s.
    pub(crate) fn harness_needs(&self) -> crate::needs::ToldNeeds {
        self.harness_needs.clone()
    }

    /// Replace Land's hold as the session's loop told it ([`Self::land_held()`]):
    /// whether it moved. A `Copy` compare and a store, under this leaf lock
    /// alone.
    pub(crate) fn set_land_held(
        &mut self,
        held: Option<aterm_agent::harness::acts::LandHeld>,
    ) -> bool {
        std::mem::replace(&mut self.land_held, held) != held
    }

    /// Land's hold as the session's loop last told this window's host
    /// (§12.3.3 item 3); `None` while none stands.
    pub(crate) fn land_held(&self) -> Option<aterm_agent::harness::acts::LandHeld> {
        self.land_held
    }

    /// THE OWNER'S COLUMNS every roster row and `status` carry after
    /// `supervisor=` (gap audit 2026-09-24): `path_evidence=` (how `path=`
    /// was learned: `measured:<age_ms>`, `unconfirmed:<age_ms>`, `carried`,
    /// `-` — [`path_columns`]), `copy=`
    /// (`managed|foreign|-`) and `upgrade=` (the live upgrade,
    /// `Row::column`, `-` for none). `mark` is the registry's frozen mark;
    /// the `path=` word it goes with is the first of the pair returned.
    ///
    /// `upgrade=` is judged with the awake clock as of this read: the row is
    /// the one the upgrade host handed over at its last look, and it reads
    /// `unwatched` past a wake grace
    /// ([`aterm_agent::harness::upgrade_drive::Row::unwatched`]), so a Mac woken
    /// since that look is inside its grace and a host that died since is past
    /// it. The later of that and the row's own clock (which carries the host's
    /// watch start, `View::note_look`) is used, so a host that just began
    /// watching is inside its grace too (owner, 2026-09-29).
    pub(crate) fn owner_columns(&self, mark: bool, now_s: u64) -> (&'static str, String) {
        let (path, evidence) = path_columns(mark, self.path_reading());
        let upgrade = self.upgrade.as_ref().map_or_else(
            || "-".to_string(),
            |row| {
                let mut row = row.clone();
                row.reread(now_s);
                row.column(now_s)
            },
        );
        (
            path,
            format!(
                "path_evidence={evidence} copy={} upgrade={upgrade}",
                self.copy().unwrap_or("-")
            ),
        )
    }
}

/// Clamp a payload to [`MAX_PAYLOAD`] bytes without bisecting a UTF-8 char or a
/// `%XX` percent-escape.
///
/// Recorded values are pct-encoded before recording, so the payload is ASCII
/// and a char-boundary cut can still land mid-escape; both the `timeline` verb
/// and the `EVENT <sid> meta …` push emit the payload verbatim, and a strict
/// pct-decoder must never see a truncated escape. So if a `%` in the last two
/// bytes before the cut starts a genuine escape (two hex digits follow in the
/// full string), the cut backs off to that `%` (still a char boundary).
pub(crate) fn clamp_payload(s: String) -> Arc<str> {
    if s.len() <= MAX_PAYLOAD {
        return s.into();
    }
    let mut end = MAX_PAYLOAD;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let bytes = s.as_bytes();
    let escape_starts_at = |i: usize| {
        bytes[i] == b'%'
            && bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
            && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
    };
    // A triple bisected by the cut has its `%` at end-1 (`…%|XX`) or end-2
    // (`…%X|X`); a `%` at end-3 or earlier fits whole and needs no back-off.
    for back in 1..=2 {
        if end >= back && escape_starts_at(end - back) {
            end -= back;
            break;
        }
    }
    Arc::from(&s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_sanitizer_is_single_line_bidi_safe_and_grapheme_bounded() {
        let hostile = "  alpha\nbe\u{0085}ta\u{202e}spoof\u{2066}\u{feff}  ";
        assert!(metadata_has_forbidden_formatting(hostile));
        assert_eq!(
            sanitize_metadata_value("title", hostile).as_deref(),
            Some("alphabetaspoof")
        );
        for invisible in [
            '\u{00ad}',
            '\u{034f}',
            '\u{180b}',
            '\u{115f}',
            '\u{3164}',
            '\u{ffa0}',
            '\u{1bca0}',
            '\u{1d173}',
            '\u{e0001}',
            // The engine's table also hides noncharacters and the interlinear
            // annotation anchors.
            '\u{fdd0}',
            '\u{fffe}',
            '\u{1ffff}',
            '\u{fff9}',
            '\u{fffb}',
        ] {
            assert!(
                metadata_has_forbidden_formatting(&format!("a{invisible}b")),
                "spoof-relevant default-ignorable U+{:04X} is rejected",
                u32::from(invisible)
            );
        }

        // ZWJ is presentation data, not a bidi/control primitive: keep the
        // family cluster intact for icon metadata.
        let family = "👨‍👩‍👧‍👦";
        assert!(!metadata_has_forbidden_formatting(family));
        assert_eq!(
            sanitize_metadata_value("icon", family).as_deref(),
            Some(family)
        );
        for legitimate in ["a\u{200c}b", "✈\u{fe0f}", "漢\u{e0100}"] {
            assert!(
                !metadata_has_forbidden_formatting(legitimate),
                "joiners/standard variation selectors remain available: {legitimate:?}"
            );
            assert_eq!(
                sanitize_metadata_value("title", legitimate).as_deref(),
                Some(legitimate)
            );
        }

        // `e + COMBINING ACUTE` is three bytes but one grapheme. Forty fit the
        // 120-byte title cap exactly; the trailing grapheme is dropped whole.
        let cluster = "e\u{301}";
        let expected = cluster.repeat(META_TITLE_MAX / cluster.len());
        let over_cap = format!("{expected}z");
        let sanitized = sanitize_metadata_value("title", &over_cap).expect("non-empty");
        assert_eq!(sanitized, expected);
        assert_eq!(sanitized.len(), META_TITLE_MAX);
    }

    #[test]
    fn session_meta_defensively_canonicalizes_internal_callers() {
        let mut meta = SessionMeta::default();
        assert_eq!(
            meta.set("title", Some(" safe\n\u{202e}title ".to_string())),
            Some(true)
        );
        assert_eq!(meta.user_title.as_deref(), Some("safetitle"));

        // A raw legacy/restore assignment is still sanitized at the chrome /
        // persistence boundary even if it bypassed `set`.
        meta.description = Some("one\u{2029}two".to_string());
        meta.icon = Some("\u{2066}🚀\u{2069}".to_string());
        let canonical = meta.sanitized();
        assert_eq!(canonical.description.as_deref(), Some("onetwo"));
        assert_eq!(canonical.icon.as_deref(), Some("🚀"));
    }

    #[test]
    fn timeline_ids_are_monotonic_and_ring_drops_oldest() {
        let mut tl = SessionTimeline::default();
        for _ in 0..(TIMELINE_CAP + 3) {
            tl.record("state-change", "state=alive".to_string());
        }
        assert_eq!(tl.len(), TIMELINE_CAP, "capped");
        assert_eq!(tl.high_id(), Some(TIMELINE_CAP as u64 + 3));
        // The three oldest were evicted; ids stay strictly increasing and
        // `since` is a suffix keyed on them.
        let ids: Vec<u64> = tl.since(None).map(|e| e.id).collect();
        assert_eq!(ids[0], 4, "oldest three evicted");
        assert!(ids.windows(2).all(|w| w[1] == w[0] + 1), "monotonic ids");
        let tail: Vec<u64> = tl
            .since(Some(TIMELINE_CAP as u64 + 1))
            .map(|e| e.id)
            .collect();
        assert_eq!(tail, vec![TIMELINE_CAP as u64 + 2, TIMELINE_CAP as u64 + 3]);
        // Timestamps never move backward (monotonic clock).
        let ts: Vec<u64> = tl.since(None).map(|e| e.t_ms).collect();
        assert!(ts.windows(2).all(|w| w[1] >= w[0]));
    }

    /// A `program-state` row spells the shown report as two closed-set words
    /// (`-` for none), and the rows hold at most [`PROGRAM_STATE_ROWS_MAX`] of
    /// the ring: a program flapping its report a thousand times keeps its
    /// newest moves, in order, and the session's other rows (its `spawned`
    /// row here) stay. In a ring already FULL of other rows, the flood ages
    /// out exactly the share of them and the newest rest stay. NEGATIVE
    /// CONTROL: the same flood written as `program-state` rows with no share
    /// (straight through `record`) evicts the `spawned` row and, in the full
    /// ring, every other row, so the share is what kept them.
    #[test]
    fn program_state_rows_name_the_report_and_hold_at_most_their_share() {
        use aterm_core::terminal::program_state::{
            ProgramBlockedKind, ProgramState, ProgramStateWord as W,
        };
        let blocked = ProgramState {
            state: W::Blocked,
            kind: Some(ProgramBlockedKind::Question),
        };
        let working = ProgramState {
            state: W::Working,
            kind: None,
        };
        let mut tl = SessionTimeline::default();
        let spawned = tl.record("spawned", "pid=1".to_string());
        for i in 0..1000 {
            let shown = match i % 3 {
                0 => Some(working),
                1 => Some(blocked),
                _ => None,
            };
            tl.note_program_state(shown);
        }
        let rows: Vec<&TimelineEvent> =
            tl.since(None).filter(|e| e.kind == PROGRAM_STATE).collect();
        assert_eq!(rows.len(), PROGRAM_STATE_ROWS_MAX, "held to the share");
        assert!(tl.len() <= TIMELINE_CAP);
        assert_eq!(
            tl.since(None).next().map(|e| (e.id, e.kind)),
            Some((spawned, "spawned")),
            "the session's own rows stay"
        );
        // The newest moves are the ones kept, ids increasing.
        assert!(rows.windows(2).all(|w| w[1].id > w[0].id));
        assert_eq!(rows.last().map(|e| e.id), tl.high_id());
        let tail: Vec<&str> = rows[rows.len() - 3..].iter().map(|e| &*e.payload).collect();
        // 997 % 3 == 1, 998 % 3 == 2, 999 % 3 == 0.
        assert_eq!(
            tail,
            [
                "state=blocked kind=question",
                "state=- kind=-",
                "state=working kind=-"
            ]
        );
        // A ring already full of the session's other rows: the flood ages out
        // the share of them, oldest first, and the newest rest stay.
        let others = |tl: &SessionTimeline| -> Vec<u64> {
            tl.since(None)
                .filter(|e| e.kind == "state-change")
                .map(|e| e.id)
                .collect()
        };
        let mut full = SessionTimeline::default();
        let mut ids: Vec<u64> = (0..TIMELINE_CAP)
            .map(|_| full.record("state-change", "state=alive".to_string()))
            .collect();
        for i in 0..1000 {
            full.note_program_state((i % 2 == 0).then_some(working));
        }
        let kept = ids.split_off(PROGRAM_STATE_ROWS_MAX);
        assert_eq!(
            others(&full),
            kept,
            "the newest {} other rows stay",
            TIMELINE_CAP - PROGRAM_STATE_ROWS_MAX
        );
        assert_eq!(full.len(), TIMELINE_CAP);
        // NEGATIVE CONTROL: the same rows with no share, straight through
        // `record`, flood the spawned row out, and in a full ring every other row.
        let mut flood = SessionTimeline::default();
        flood.record("spawned", "pid=1".to_string());
        for _ in 0..1000 {
            flood.record(PROGRAM_STATE, "state=working kind=-".to_string());
        }
        assert!(
            flood.since(None).all(|e| e.kind != "spawned"),
            "control: without the share the spawned row is evicted"
        );
        let mut unshared = SessionTimeline::default();
        for _ in 0..TIMELINE_CAP {
            unshared.record("state-change", "state=alive".to_string());
        }
        for _ in 0..1000 {
            unshared.record(PROGRAM_STATE, "state=working kind=-".to_string());
        }
        assert!(
            others(&unshared).is_empty(),
            "control: without the share a full ring loses every other row"
        );
    }

    /// The `partition_point` seek agrees with a linear filter at every
    /// watermark, including below the retained low-water (everything), at the
    /// high (nothing), above it (everything: another count's anchor), and
    /// either side of each retained id.
    #[test]
    fn since_seek_matches_the_linear_filter_for_every_watermark() {
        let mut tl = SessionTimeline::default();
        for _ in 0..(TIMELINE_CAP + 5) {
            tl.record("state-change", "state=alive".to_string());
        }
        let low = tl.since(None).next().expect("non-empty").id;
        let high = tl.high_id().expect("non-empty");
        assert!(
            low > 1,
            "the fixture must have evicted, or the below-low arm is vacuous"
        );

        // An anchor above every id handed out is another count's: everything.
        let reference = |after: Option<u64>| -> Vec<u64> {
            tl.events
                .iter()
                .filter(|e| after.is_none_or(|a| a > high || e.id > a))
                .map(|e| e.id)
                .collect()
        };
        let observed = |after: Option<u64>| -> Vec<u64> { tl.since(after).map(|e| e.id).collect() };

        let mut probes: Vec<Option<u64>> = vec![None, Some(0), Some(1)];
        for id in [low - 1, low, low + 1, high - 1, high, high + 1, high + 100] {
            probes.push(Some(id));
        }
        for after in probes {
            assert_eq!(
                observed(after),
                reference(after),
                "since({after:?}) diverged"
            );
        }
        assert_eq!(
            observed(Some(low - 1)).len(),
            TIMELINE_CAP,
            "below low-water = all retained"
        );
        assert!(
            observed(Some(high)).is_empty(),
            "at the high-water = nothing new"
        );
        assert_eq!(
            observed(Some(high + 1)).len(),
            TIMELINE_CAP,
            "above every id handed out = all retained"
        );
        // Still double-ended: the reverse walk the newest-few readers use.
        let newest: Vec<u64> = tl.since(None).rev().take(3).map(|e| e.id).collect();
        assert_eq!(newest, vec![high, high - 1, high - 2]);
    }

    #[test]
    fn cwd_change_records_only_actual_changes_and_clears_to_dash() {
        let mut tl = SessionTimeline::default();
        tl.record_cwd_change(Some("/a"));
        tl.record_cwd_change(Some("/a")); // reprompt: no new event
        tl.record_cwd_change(Some("/b b")); // space -> pct-encoded
        tl.record_cwd_change(None); // OSC 7 cleared
        tl.record_cwd_change(None); // still cleared: no new event
        let got: Vec<(&str, &str)> = tl.since(None).map(|e| (e.kind, &*e.payload)).collect();
        assert_eq!(
            got,
            vec![
                ("cwd-change", "cwd=/a"),
                ("cwd-change", "cwd=/b%20b"),
                ("cwd-change", "cwd=-"),
            ]
        );
    }

    #[test]
    fn payload_clamps_on_a_char_boundary() {
        let mut tl = SessionTimeline::default();
        let long = "é".repeat(400); // 800 bytes
        tl.record_raw_meta_change(long);
        let e = tl.since(None).next().unwrap();
        assert!(e.payload.len() <= MAX_PAYLOAD);
        assert!(e.payload.chars().all(|c| c == 'é'), "boundary-safe clamp");
    }

    /// The clamp never bisects a `%XX` escape: slide the cut across all three
    /// in-triple offsets (via a 0/1/2-byte ASCII prefix) and require every
    /// stored `%` to head a complete, decodable triple.
    #[test]
    fn payload_clamp_never_bisects_a_pct_escape() {
        for pad in 0..3usize {
            let encoded = crate::control::pct_encode(&"中".repeat(MAX_PAYLOAD)); // %E4%B8%AD…
            let payload = format!("{}{encoded}", "x".repeat(pad));
            let mut tl = SessionTimeline::default();
            tl.record_raw_meta_change(payload);
            let stored = &tl.since(None).next().unwrap().payload;
            assert!(stored.len() <= MAX_PAYLOAD, "cap holds (pad {pad})");
            let b = stored.as_bytes();
            for (i, &c) in b.iter().enumerate() {
                if c == b'%' {
                    assert!(
                        i + 2 < b.len()
                            && b[i + 1].is_ascii_hexdigit()
                            && b[i + 2].is_ascii_hexdigit(),
                        "dangling escape at byte {i} of {stored:?} (pad {pad})"
                    );
                }
            }
        }
    }

    #[test]
    fn meta_set_reports_change_caps_and_unknown_fields() {
        let mut m = SessionMeta::default();
        assert!(!m.any_set());
        assert_eq!(m.set("title", Some("build agent".into())), Some(true));
        assert_eq!(m.get("title"), Some("build agent"));
        assert!(m.any_set());
        // Same value again: known field, not a change.
        assert_eq!(m.set("title", Some("build agent".into())), Some(false));
        // Unset flips back; a second unset is a known-field no-change.
        assert_eq!(m.set("title", None), Some(true));
        assert_eq!(m.set("title", None), Some(false));
        assert!(!m.any_set());
        // Unknown fields are rejected (None), not silently stored.
        assert_eq!(m.set("colour", Some("red".into())), None);
        assert_eq!(m.get("colour"), None);
        // The typed keys round-trip like the original three and flip any_set.
        assert_eq!(m.set("role", Some("reviewer".into())), Some(true));
        assert_eq!(m.get("role"), Some("reviewer"));
        assert!(m.any_set());
        assert_eq!(
            m.set("attention", Some("needs human: approval".into())),
            Some(true)
        );
        assert_eq!(m.get("attention"), Some("needs human: approval"));
        assert_eq!(m.set("role", None), Some(true));
        assert_eq!(m.set("attention", None), Some(true));
        assert!(!m.any_set());
    }

    /// `upgrade=` reads the stored row against the awake clock now: it says `unwatched`
    /// exactly when the machine is past its wake grace. Negative control: the row as stored.
    #[test]
    fn the_upgrade_column_reads_the_awake_clock_as_it_stands_now() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::{Row, WAKE_GRACE_S, system_awake_since};
        let now = crate::upgrade_host::now_s();
        let row = Row {
            tab: "s-a".into(),
            from: "2.1.281".into(),
            to: "2.1.282".into(),
            phase: Phase::Pending,
            behind_since: now - 3_600,
            watch_at: now - 2 * 3_600,
            looked_at: now - 3 * 3_600,
            awake_since: 0,
            ..Row::default()
        };
        assert_eq!(row.column(now), "pending/2.1.282/-/1h", "as stored");
        let mut tl = SessionTimeline::default();
        assert!(tl.set_upgrade(Some(row)));
        let (_, cols) = tl.owner_columns(false, now);
        let awake = system_awake_since();
        let past_grace = awake != 0 && now >= awake + WAKE_GRACE_S;
        assert_eq!(
            cols.ends_with(" upgrade=stalled/2.1.282/unwatched:3h/1h"),
            past_grace,
            "{cols} (awake since {awake}, now {now})"
        );
        if !past_grace {
            assert!(cols.ends_with(" upgrade=pending/2.1.282/-/1h"), "{cols}");
        }
    }
}

/// The typed metadata write API that `meta set`/`meta unset` and the GUI rename both take.
#[cfg(test)]
mod write_api_tests {
    use super::{MetaEdit, MetaField, MetaWriteError, apply_meta_value, validated_meta_value};

    /// The validation ladder refuses rather than repairs: an empty `Set`, a
    /// value with forbidden formatting and an over-cap value are refusals, and
    /// a legal value comes back canonicalized. Only `Clear` reaches `Ok(None)`.
    #[test]
    fn the_ladder_refuses_rather_than_repairs() {
        use MetaField::{Icon, Title};
        assert_eq!(
            validated_meta_value(Title, MetaEdit::Set("   ")),
            Err(MetaWriteError::Empty),
            "an empty Set is a refusal, not a clear"
        );
        assert_eq!(
            validated_meta_value(Title, MetaEdit::Set("build\u{202e}agent")),
            Err(MetaWriteError::ForbiddenFormatting)
        );
        let over = "x".repeat(super::META_TITLE_MAX + 1);
        assert_eq!(
            validated_meta_value(Title, MetaEdit::Set(&over)),
            Err(MetaWriteError::TooLong {
                cap: super::META_TITLE_MAX
            }),
            "over-cap is refused, never truncated"
        );
        assert_eq!(
            validated_meta_value(Title, MetaEdit::Set("  build agent  ")),
            Ok(Some("build agent".to_string())),
            "interior whitespace survives; the edges are trimmed"
        );
        assert_eq!(validated_meta_value(Title, MetaEdit::Clear), Ok(None));
        assert_eq!(validated_meta_value(Icon, MetaEdit::Clear), Ok(None));
        assert_eq!(Icon.cap(), super::META_ICON_MAX);
        assert_eq!(MetaField::parse("colour"), None);
    }

    /// A clear records the cleared marker (`value=-`), distinguishing it in the
    /// event stream from a `Set("")`, which would also store `None`.
    #[test]
    fn a_clear_records_the_cleared_marker_and_only_on_a_real_change() {
        let ctx = crate::stub_session(0).ctx.clone();
        assert!(apply_meta_value(
            &ctx,
            MetaField::Title,
            Some("agent".into()),
            aterm_slog::Actor::Ctl
        ));
        assert!(
            !apply_meta_value(
                &ctx,
                MetaField::Title,
                Some("agent".into()),
                aterm_slog::Actor::Ctl
            ),
            "a no-op re-set reports unchanged so the caller stays silent"
        );
        assert!(apply_meta_value(
            &ctx,
            MetaField::Title,
            None,
            aterm_slog::Actor::Ctl
        ));
        assert!(
            !apply_meta_value(&ctx, MetaField::Title, None, aterm_slog::Actor::Ctl),
            "clearing an unset field is a no-op"
        );
        let tl = ctx.timeline.lock().unwrap();
        let payloads: Vec<&str> = tl
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| &*e.payload)
            .collect();
        assert_eq!(
            payloads,
            vec!["field=title value=agent", "field=title value=-"],
            "exactly one record per REAL change, and a clear is `-`"
        );
    }

    /// Every write the door accepts marks its field as the driver's (a no-op
    /// re-set and a clear of an unset field included), a restore seed through
    /// [`super::SessionMeta::set`] marks nothing, and the marks never enter
    /// equality.
    #[test]
    fn every_accepted_write_marks_its_field_and_a_seed_marks_none() {
        use super::SessionMeta;
        let ctx = crate::stub_session(0).ctx.clone();
        assert!(
            !apply_meta_value(&ctx, MetaField::Attention, None, aterm_slog::Actor::Ctl),
            "clearing an unset field moves nothing"
        );
        assert!(apply_meta_value(
            &ctx,
            MetaField::Role,
            Some("lead".into()),
            aterm_slog::Actor::Ctl
        ));
        assert!(!apply_meta_value(
            &ctx,
            MetaField::Role,
            Some("lead".into()),
            aterm_slog::Actor::Ctl
        ));
        let meta = ctx.meta.lock().unwrap().clone();
        let wrote = MetaField::ALL.map(|field| meta.driver_wrote(field));
        assert_eq!(
            wrote,
            [false, false, false, true, true, false, false],
            "title, description, icon, role, attention, questions, autopilot"
        );

        let mut seeded = SessionMeta::default();
        assert_eq!(seeded.set("role", Some("lead".into())), Some(true));
        assert!(
            MetaField::ALL
                .iter()
                .all(|field| !seeded.driver_wrote(*field))
        );
        assert_eq!(seeded, meta, "equality is over the values alone");
    }
}

/// Tests for the `questions` field: a closed set of four policy words,
/// case-folded on the way in, refused whole otherwise, and carried like every
/// other user field.
#[cfg(test)]
mod questions_field_tests {
    use super::{
        META_QUESTIONS_MAX, MetaEdit, MetaField, MetaWriteError, QUESTIONS_POLICY_WORDS,
        SessionMeta, normalize_questions_value, restore_carried_meta, sanitize_metadata_value,
        validated_meta_value,
    };

    /// Every spelling the contract admits, and the one it stores; the cap
    /// holds the longest.
    #[test]
    fn the_policy_words_are_admitted_and_case_folded() {
        for word in QUESTIONS_POLICY_WORDS {
            assert_eq!(normalize_questions_value(word), Some(word));
            assert!(word.len() <= META_QUESTIONS_MAX, "{word}");
        }
        for (typed, stored) in [
            ("Recommended", "recommended"),
            ("  RECOMMENDED ", "recommended"),
            ("Ask", "ask"),
            ("ASK", "ask"),
            ("Long-Term", "long-term"),
            (" SHORT-TERM ", "short-term"),
        ] {
            assert_eq!(normalize_questions_value(typed), Some(stored), "{typed:?}");
        }
    }

    /// Anything else is refused whole — never trimmed into a different value.
    /// No controller form: a session principal is not a policy word, and a
    /// priority word spelled otherwise (`long_term`, `long term`) is no word.
    #[test]
    fn anything_outside_the_words_is_refused() {
        for bad in [
            "",
            "long_term",
            "short_term",
            "recommend",
            "long term",
            "longterm",
            "long-term-ish",
            "askx",
            "ask ask",
            "@s-abc123",
            "s-abc123@n-lab",
            "controller",
            "ask\u{202e}",
            "rec\u{200b}ommended",
            "whatever claude thinks",
        ] {
            assert_eq!(normalize_questions_value(bad), None, "{bad:?}");
        }
    }

    /// The write ladder: a legal word stores its normalized spelling; an
    /// illegal one is the typed refusal the wire renders as its usage line;
    /// an empty `Set` stays `Empty`; a `Clear` clears.
    #[test]
    fn the_write_ladder_normalizes_or_refuses() {
        use MetaField::Questions;
        assert_eq!(
            validated_meta_value(Questions, MetaEdit::Set(" Recommended ")),
            Ok(Some("recommended".to_string()))
        );
        assert_eq!(
            validated_meta_value(Questions, MetaEdit::Set("whatever you think")),
            Err(MetaWriteError::NotAQuestionsPolicy)
        );
        let long = "x".repeat(200);
        assert_eq!(
            validated_meta_value(Questions, MetaEdit::Set(&long)),
            Err(MetaWriteError::NotAQuestionsPolicy),
            "an over-cap value is outside the set first"
        );
        assert_eq!(
            validated_meta_value(Questions, MetaEdit::Set("  ")),
            Err(MetaWriteError::Empty)
        );
        assert_eq!(validated_meta_value(Questions, MetaEdit::Clear), Ok(None));
        assert_eq!(MetaField::parse("questions"), Some(Questions));
        assert_eq!(Questions.wire_name(), "questions");
        assert_eq!(Questions.cap(), META_QUESTIONS_MAX);
    }

    /// The store's second gate: a value that bypassed the wire (a hand-edited
    /// restore manifest) is normalized or dropped, `questions` counts toward
    /// `any_set`, and it takes part in equality.
    #[test]
    fn the_store_normalizes_counts_and_compares_questions() {
        let mut m = SessionMeta::default();
        assert_eq!(m.set("questions", Some("Recommended".into())), Some(true));
        assert_eq!(m.get("questions"), Some("recommended"));
        assert!(m.any_set(), "a user field: `ls meta=1`");
        assert_ne!(m, SessionMeta::default(), "a value, part of equality");
        assert_eq!(
            m.set("questions", Some("not a policy".into())),
            Some(true),
            "an illegal seed clears rather than stores"
        );
        assert_eq!(m.get("questions"), None);
        assert!(!m.any_set());
        assert_eq!(
            sanitize_metadata_value("questions", "Ask").as_deref(),
            Some("ask")
        );
        assert_eq!(sanitize_metadata_value("questions", "@s-x"), None);
    }

    /// Restore and handoff carry it: a sanitized copy keeps it, and the carried
    /// identity puts it back on the live session as an ordinary `meta-change`,
    /// for each of the four words.
    #[test]
    fn a_carried_identity_restores_the_policy() {
        for word in QUESTIONS_POLICY_WORDS {
            let ctx = crate::stub_session(0).ctx.clone();
            let mut live = SessionMeta::default();
            let _ = live.set("questions", Some(word.into()));
            let carried = live.sanitized();
            assert_eq!(carried.questions.as_deref(), Some(word));
            assert!(restore_carried_meta(&ctx, &carried));
            assert_eq!(ctx.meta.lock().unwrap().questions.as_deref(), Some(word));
            let events: Vec<String> = ctx
                .timeline
                .lock()
                .unwrap()
                .since(None)
                .filter(|e| e.kind == "meta-change")
                .map(|e| e.payload.to_string())
                .collect();
            assert_eq!(events, vec![format!("field=questions value={word}")]);
        }
    }
}

/// The `autopilot` field: the supervisor's three words and their aliases store the mode's
/// word, anything else is refused whole, and the word is counted, compared and carried like
/// every other user field.
#[cfg(test)]
mod autopilot_field_tests {
    use super::{
        AUTOPILOT_USAGE, META_AUTOPILOT_MAX, MetaEdit, MetaField, MetaWriteError, SessionMeta,
        apply_meta_value, normalize_autopilot_value, restore_carried_meta, sanitize_metadata_value,
        validated_meta_value,
    };
    use aterm_agent::supervise::Autopilot;

    /// Every mode's word is admitted as itself and fits the cap; aliases and any case store
    /// the mode's word; the usage line spells the three words in the chip's order.
    #[test]
    fn the_modes_and_the_owners_words_store_the_modes_word() {
        for mode in Autopilot::ALL {
            assert_eq!(normalize_autopilot_value(mode.word()), Some(mode.word()));
            assert!(mode.word().len() <= META_AUTOPILOT_MAX, "{mode:?}");
        }
        for (typed, stored) in [
            ("Cruise", "cruise"),
            ("  LAND ", "land"),
            ("Off", "off"),
            ("forever", "cruise"),
            ("Infinity", "cruise"),
            ("wrap-up", "land"),
            ("WrapUp", "land"),
            ("wrap", "land"),
        ] {
            assert_eq!(normalize_autopilot_value(typed), Some(stored), "{typed:?}");
        }
        // The three words in the chip's cycle order, Cruise's `until=`
        // (§9.3.5) beside it.
        let words: Vec<String> = Autopilot::ALL
            .iter()
            .map(|m| match m {
                Autopilot::Cruise => format!("{} [until=HH:MM|+DURATION]", m.word()),
                _ => m.word().to_string(),
            })
            .collect();
        assert_eq!(
            AUTOPILOT_USAGE,
            format!("usage: meta set autopilot {}", words.join("|"))
        );
    }

    /// NEGATIVE CONTROLS: a switch word other than `off`, a near miss and an
    /// invisible character are no mode, and are refused whole.
    #[test]
    fn anything_that_names_no_mode_is_refused() {
        for bad in [
            "",
            "on",
            "true",
            "false",
            "no",
            "cruising",
            "land it",
            "of\u{200b}f",
            "off\u{202e}",
            "@s-abc123",
        ] {
            assert_eq!(normalize_autopilot_value(bad), None, "{bad:?}");
        }
    }

    /// The write ladder: a mode stores its word, anything else is the typed
    /// refusal the wire renders as [`AUTOPILOT_USAGE`], an empty `Set` stays
    /// `Empty`, and a `Clear` clears.
    #[test]
    fn the_write_ladder_normalizes_or_refuses() {
        use MetaField::Autopilot as Field;
        assert_eq!(
            validated_meta_value(Field, MetaEdit::Set(" Wrap-Up ")),
            Ok(Some("land".to_string()))
        );
        assert_eq!(
            validated_meta_value(Field, MetaEdit::Set("sometimes")),
            Err(MetaWriteError::NotAnAutopilot)
        );
        assert_eq!(
            validated_meta_value(Field, MetaEdit::Set(&"x".repeat(200))),
            Err(MetaWriteError::NotAnAutopilot)
        );
        assert_eq!(
            validated_meta_value(Field, MetaEdit::Set("  ")),
            Err(MetaWriteError::Empty)
        );
        assert_eq!(validated_meta_value(Field, MetaEdit::Clear), Ok(None));
        assert_eq!(MetaField::parse("autopilot"), Some(Field));
        assert_eq!(Field.wire_name(), "autopilot");
        assert_eq!(Field.cap(), META_AUTOPILOT_MAX);
        assert!(MetaField::NAMES.ends_with("|questions|autopilot"));
    }

    /// The store's second gate normalizes or drops a value that bypassed the
    /// wire; the word counts toward `any_set`, takes part in equality and
    /// reads back typed.
    #[test]
    fn the_store_normalizes_counts_compares_and_types_the_word() {
        let mut m = SessionMeta::default();
        assert_eq!(m.autopilot_word(), None);
        assert_eq!(m.set("autopilot", Some("Infinity".into())), Some(true));
        assert_eq!(m.get("autopilot"), Some("cruise"));
        assert_eq!(m.autopilot_word(), Some(Autopilot::Cruise));
        assert!(m.any_set(), "a user field: `ls meta=1`");
        assert_ne!(m, SessionMeta::default(), "a value, part of equality");
        assert_eq!(
            m.set("autopilot", Some("hover".into())),
            Some(true),
            "an illegal seed clears rather than stores"
        );
        assert_eq!(m.get("autopilot"), None);
        assert!(!m.any_set());
        assert_eq!(
            sanitize_metadata_value("autopilot", "OFF").as_deref(),
            Some("off")
        );
        assert_eq!(sanitize_metadata_value("autopilot", "on"), None);
        // Each stored word reads as its mode, and a spelling no writer here
        // stores (set past both gates) still reads by the supervisor's reader.
        for mode in Autopilot::ALL {
            m.autopilot = Some(mode.word().into());
            assert_eq!(m.autopilot_word(), Some(mode));
        }
        m.autopilot = Some(" Wrap-Up ".into());
        assert_eq!(m.autopilot_word(), Some(Autopilot::Land));
    }

    /// A write that moves the word records the ordinary `meta-change` and
    /// rings the window's host; a write that moves nothing rings nothing
    /// (the ring count is this thread's own).
    #[test]
    fn a_word_that_moves_is_recorded_and_rings_the_host() {
        let ctx = crate::stub_session(0).ctx.clone();
        let rings = crate::harness_host::rings_here();
        assert!(apply_meta_value(
            &ctx,
            MetaField::Autopilot,
            Some("off".into()),
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(crate::harness_host::rings_here(), rings + 1, "moved: rung");
        assert!(!apply_meta_value(
            &ctx,
            MetaField::Autopilot,
            Some("off".into()),
            aterm_slog::Actor::Ctl
        ));
        assert!(apply_meta_value(
            &ctx,
            MetaField::Role,
            Some("lead".into()),
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(
            crate::harness_host::rings_here(),
            rings + 1,
            "NEGATIVE CONTROLS: a re-set and another field ring nothing"
        );
        let events: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.to_string())
            .collect();
        assert_eq!(events[0], "field=autopilot value=off");
    }

    /// Restore and handoff carry it: a sanitized copy keeps it, and the carried
    /// identity puts it back on the live session as an ordinary `meta-change`,
    /// ringing the host, for each of the three words.
    #[test]
    fn a_carried_identity_restores_the_word() {
        for mode in Autopilot::ALL {
            let ctx = crate::stub_session(0).ctx.clone();
            let mut live = SessionMeta::default();
            let _ = live.set("autopilot", Some(mode.word().into()));
            let carried = live.sanitized();
            assert_eq!(carried.autopilot.as_deref(), Some(mode.word()));
            let rings = crate::harness_host::rings_here();
            assert!(restore_carried_meta(&ctx, &carried));
            assert_eq!(crate::harness_host::rings_here(), rings + 1);
            assert_eq!(ctx.meta.lock().unwrap().autopilot_word(), Some(mode));
        }
    }
}

/// A WORKER AN OLDER BUILD STARTED KEEPS LAND (FOREVER LAND): a session that
/// holds a task record — it was started as a worker — and no Autopilot word
/// of its own gets [`crate::task_launch::WORKER_AUTOPILOT`], the word every
/// worker's spawn writes now, written as aterm's (never as a driver's: a
/// carried identity grafted after it still puts its own word back). A build
/// before that word was written left its workers on the table's default,
/// and under FOREVER CRUISE a worker on Cruise never publishes `done`: its
/// manager's `await task` never woke after a seamless update (the review of
/// 2026-10-20). Called where a carried record or identity lands on a session
/// ([`crate::task_record::seed_carried`], the restore's identity seed and
/// graft). A word the session has — carried, restored or written here — is
/// kept, and an exited worker's task is over. Whether the word was written.
pub(crate) fn keep_worker_autopilot(ctx: &crate::SessionCtx) -> bool {
    let tasked = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task()
        .is_some_and(|t| t.state != crate::task_record::TaskState::Exited);
    if !tasked {
        return false;
    }
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if meta.autopilot.is_some() || meta.driver_wrote(MetaField::Autopilot) {
        return false;
    }
    let word = crate::task_launch::WORKER_AUTOPILOT.word().to_string();
    let moved = meta.set_autopilot(Some(word), None);
    if moved {
        // aterm's write, stored as its setter (§11.7.3, item 1).
        meta.note_autopilot_set_by(AutopilotSetBy::now(aterm_slog::Actor::Aterm, None));
        record_autopilot(ctx, &meta, aterm_slog::Actor::Aterm);
    }
    drop(meta);
    if moved {
        crate::harness_host::ring();
    }
    moved
}

/// Proofs for [`keep_worker_autopilot`].
#[cfg(test)]
mod worker_autopilot_tests {
    use super::{SessionMeta, keep_worker_autopilot, restore_carried_meta};
    use aterm_agent::supervise::Autopilot;

    /// A carried worker's record on `ctx`, in `state`.
    fn carried(ctx: &crate::SessionCtx, state: &str) {
        let row = format!(
            "rev=3 state={state} from=s-0123456789abcdef0123 since=1789000000000 \
             result_bytes=0 attention=0"
        );
        crate::task_record::seed_carried(ctx, Some(&row));
        assert!(ctx.timeline.lock().unwrap().task().is_some(), "{row}");
    }

    /// A WORKER WITH NO WORD, ITS RECORD CARRIED, IS LAND: the record's seed
    /// writes it, and a graft of a carried identity that names no word —
    /// which clears the word — is followed by the same keep. NEGATIVE
    /// CONTROLS: a carried word (Cruise) stands; a session with no task
    /// record keeps the table's default; an exited worker gets nothing.
    #[test]
    fn a_worker_with_no_word_keeps_land() {
        let word = |ctx: &crate::SessionCtx| ctx.meta.lock().unwrap().autopilot_word();
        let ctx = crate::stub_session(0).ctx.clone();
        carried(&ctx, "working");
        assert_eq!(word(&ctx), Some(Autopilot::Land), "the seed keeps it");
        restore_carried_meta(&ctx, &SessionMeta::default());
        assert_eq!(word(&ctx), None, "the graft put the carried none back");
        assert!(keep_worker_autopilot(&ctx));
        assert_eq!(word(&ctx), Some(Autopilot::Land));
        // NEGATIVE CONTROLS.
        let cruise = SessionMeta {
            autopilot: Some("cruise".into()),
            ..SessionMeta::default()
        };
        restore_carried_meta(&ctx, &cruise);
        assert!(!keep_worker_autopilot(&ctx));
        assert_eq!(word(&ctx), Some(Autopilot::Cruise), "a carried word stands");
        let plain = crate::stub_session(1).ctx.clone();
        assert!(!keep_worker_autopilot(&plain));
        assert_eq!(word(&plain), None, "no task: the table's default");
        let exited = crate::stub_session(2).ctx.clone();
        carried(&exited, "exited");
        assert_eq!(word(&exited), None, "an exited worker");
    }
}

/// Proofs for [`super::restore_carried_meta`]: the rules that put an identity
/// carried in from the previous process back on a session that is already
/// live here.
#[cfg(test)]
mod carried_meta_tests {
    use super::{MetaField, SessionMeta, apply_meta_value, restore_carried_meta};

    fn meta_events_after(ctx: &crate::SessionCtx, watermark: Option<u64>) -> Vec<String> {
        ctx.timeline
            .lock()
            .unwrap()
            .since(watermark)
            .filter(|event| event.kind == "meta-change")
            .map(|event| event.payload.to_string())
            .collect()
    }

    /// A field a driver wrote here keeps the driver's value, set or cleared; every other field
    /// takes the carried value, and each move is one `meta-change` naming the stored value.
    #[test]
    fn a_driver_write_outranks_the_carried_value_and_every_move_is_announced() {
        let ctx = crate::stub_session(0).ctx.clone();
        // A value from an earlier restore — no driver wrote it here — that
        // the carried identity does not name.
        let _ = ctx.meta.lock().unwrap().set("icon", Some("🧟".into()));
        // What a driver wrote here before the carried identity arrived.
        assert!(apply_meta_value(
            &ctx,
            MetaField::Role,
            Some("worker:new".into()),
            aterm_slog::Actor::Ctl
        ));
        assert!(apply_meta_value(
            &ctx,
            MetaField::Attention,
            Some("fresh escalation".into()),
            aterm_slog::Actor::Ctl
        ));
        assert!(!apply_meta_value(
            &ctx,
            MetaField::Description,
            None,
            aterm_slog::Actor::Ctl
        ));
        let watermark = ctx.timeline.lock().unwrap().high_id();

        let carried = SessionMeta {
            user_title: Some("fable driver".into()),
            description: Some("carried notes".into()),
            role: Some("worker:old".into()),
            ..SessionMeta::default()
        };
        assert!(restore_carried_meta(&ctx, &carried));

        let meta = ctx.meta.lock().unwrap().clone();
        assert_eq!(
            meta,
            SessionMeta {
                user_title: Some("fable driver".into()),
                role: Some("worker:new".into()),
                attention: Some("fresh escalation".into()),
                ..SessionMeta::default()
            },
            "the driver's role and attention and its clear of description \
             stand; the title is carried; the stale icon is cleared"
        );
        assert_eq!(
            meta_events_after(&ctx, watermark),
            vec![
                "field=title value=fable%20driver".to_string(),
                "field=icon value=-".to_string(),
            ],
            "exactly the fields that moved, each naming what it now stores"
        );

        // Nothing left to move: silent, and says so.
        let watermark = ctx.timeline.lock().unwrap().high_id();
        assert!(!restore_carried_meta(&ctx, &carried));
        assert!(meta_events_after(&ctx, watermark).is_empty());
    }
}

/// [`apply_meta_value`] records under the meta guard, so racing `meta set`s never leave the
/// event stream ending on a value other than the one stored.
#[cfg(test)]
mod meta_atomicity_tests {
    use super::{MetaField, apply_meta_value};

    #[test]
    fn racing_meta_sets_keep_the_last_event_matching_final_state() {
        let ctx = crate::stub_session(0).ctx.clone();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    for j in 0..50 {
                        apply_meta_value(
                            &ctx,
                            MetaField::Title,
                            Some(format!("t{i}-{j}")),
                            aterm_slog::Actor::Ctl,
                        );
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let final_title = ctx
            .meta
            .lock()
            .unwrap()
            .get("title")
            .expect("some writer won")
            .to_string();
        let tl = ctx.timeline.lock().unwrap();
        let last = tl
            .since(None)
            .rfind(|e| e.kind == "meta-change")
            .expect("changes were recorded");
        assert_eq!(
            last.payload.to_string(),
            format!(
                "field=title value={}",
                crate::control::pct_encode(&final_title)
            ),
            "the final meta-change event names the finally-stored value"
        );
    }
}

#[cfg(test)]
mod keyed_attention_tests {
    use super::{
        ATTENTION_OWNERS_MAX, AttentionWriteError, BARE_ATTENTION_OWNER, META_ATTENTION_KEYED_MAX,
        MetaEdit, MetaField, MetaWriteError, SessionMeta, apply_meta_value, claim_supervisor,
        release_supervisor, release_supervisor_held_by, write_attention_owned,
    };

    fn meta_events(ctx: &crate::SessionCtx) -> Vec<String> {
        ctx.timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|event| event.kind == "meta-change")
            .map(|event| event.payload.to_string())
            .collect()
    }

    fn effective(ctx: &crate::SessionCtx) -> Option<String> {
        ctx.meta.lock().unwrap().attention.clone()
    }

    /// Two owners raise and clear their own escalations independently: each
    /// clear leaves the other's standing, readers see the most recently set
    /// one, and only a move of what readers see is recorded as an event.
    #[test]
    fn two_owners_set_and_clear_independently() {
        let ctx = crate::stub_session(0).ctx.clone();
        let set = |owner: &str, text: &str| {
            write_attention_owned(&ctx, owner, MetaEdit::Set(text), aterm_slog::Actor::Ctl)
                .expect("accepted")
        };
        assert!(set("sup", "claude needs approval: rm -rf tmp"));
        assert!(set("human", "check the deploy"));
        assert_eq!(effective(&ctx).as_deref(), Some("check the deploy"));
        // The earlier owner's entry is still there, underneath.
        assert_eq!(
            ctx.meta.lock().unwrap().attention_owners.get("sup"),
            Some("claude needs approval: rm -rf tmp")
        );

        // Clearing the SHOWN owner falls back to the other, not to nothing.
        assert!(
            write_attention_owned(&ctx, "human", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap()
        );
        assert_eq!(
            effective(&ctx).as_deref(),
            Some("claude needs approval: rm -rf tmp")
        );
        // Clearing an owner that holds nothing changes nothing.
        assert!(
            !write_attention_owned(&ctx, "human", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap()
        );
        // Re-setting the shown text is a no-op, not an event.
        assert!(!set("sup", "claude needs approval: rm -rf tmp"));
        assert!(
            write_attention_owned(&ctx, "sup", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap()
        );
        assert_eq!(effective(&ctx), None);
        assert!(ctx.meta.lock().unwrap().attention_owners.is_empty());

        assert_eq!(
            meta_events(&ctx),
            [
                "field=attention value=claude%20needs%20approval:%20rm%20-rf%20tmp",
                "field=attention value=check%20the%20deploy",
                "field=attention value=claude%20needs%20approval:%20rm%20-rf%20tmp",
                "field=attention value=-",
            ]
        );

        // Clearing a non-shown owner moves nothing `attention=` shows, so it
        // records nothing; but an entry went, which the need reads, so the
        // write answers a move (what posts the wake that republishes it).
        set("a", "first");
        set("b", "second");
        let before = meta_events(&ctx).len();
        assert!(write_attention_owned(&ctx, "a", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap());
        assert_eq!(effective(&ctx).as_deref(), Some("second"));
        assert_eq!(meta_events(&ctx).len(), before);
        // …and so does the bare clear under a newer keyed entry (the wire's
        // `meta unset attention`); clearing what is not there moves nothing.
        apply_meta_value(
            &ctx,
            MetaField::Attention,
            Some("lunch".into()),
            aterm_slog::Actor::Ctl,
        );
        set("c", "third");
        let before = meta_events(&ctx).len();
        assert!(apply_meta_value(
            &ctx,
            MetaField::Attention,
            None,
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(effective(&ctx).as_deref(), Some("third"));
        assert_eq!(meta_events(&ctx).len(), before);
        assert!(!apply_meta_value(
            &ctx,
            MetaField::Attention,
            None,
            aterm_slog::Actor::Ctl
        ));
        assert!(
            !write_attention_owned(&ctx, "a", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap()
        );
    }

    /// The bare form is owner `-`: `meta set attention` / `meta unset
    /// attention` (the `apply_meta_value` path every pre-keyed caller takes)
    /// set and clear only the bare entry, and a keyed escalation outlives it.
    #[test]
    fn the_bare_form_is_the_bare_owner_and_keeps_its_contract() {
        let ctx = crate::stub_session(0).ctx.clone();
        assert!(apply_meta_value(
            &ctx,
            MetaField::Attention,
            Some("legacy badge".to_string()),
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(effective(&ctx).as_deref(), Some("legacy badge"));
        assert_eq!(
            ctx.meta
                .lock()
                .unwrap()
                .attention_owners
                .get(BARE_ATTENTION_OWNER),
            Some("legacy badge")
        );
        write_attention_owned(&ctx, "sup", MetaEdit::Set("keyed"), aterm_slog::Actor::Ctl).unwrap();
        // A bare clear leaves the keyed owner's escalation up.
        apply_meta_value(&ctx, MetaField::Attention, None, aterm_slog::Actor::Ctl);
        assert_eq!(effective(&ctx).as_deref(), Some("keyed"));
        // With no keyed owner, the bare set/clear round-trips exactly as before.
        write_attention_owned(&ctx, "sup", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap();
        assert_eq!(effective(&ctx), None);
        assert!(apply_meta_value(
            &ctx,
            MetaField::Attention,
            Some("x".into()),
            aterm_slog::Actor::Ctl
        ));
        assert!(apply_meta_value(
            &ctx,
            MetaField::Attention,
            None,
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(effective(&ctx), None);
    }

    /// Bounded: a ninth owner is refused without touching anyone, the bare
    /// owner always has its slot, and a keyed text is capped at 200 bytes
    /// while the bare field keeps its 256.
    #[test]
    fn the_map_is_bounded_and_the_keyed_text_capped() {
        let ctx = crate::stub_session(0).ctx.clone();
        for i in 0..ATTENTION_OWNERS_MAX - 1 {
            write_attention_owned(
                &ctx,
                &format!("o{i}"),
                MetaEdit::Set("x"),
                aterm_slog::Actor::Ctl,
            )
            .unwrap();
        }
        // The bare owner is the eighth.
        assert!(apply_meta_value(
            &ctx,
            MetaField::Attention,
            Some("bare".into()),
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(
            write_attention_owned(
                &ctx,
                "one-too-many",
                MetaEdit::Set("y"),
                aterm_slog::Actor::Ctl
            ),
            Err(AttentionWriteError::OwnersFull)
        );
        assert_eq!(effective(&ctx).as_deref(), Some("bare"), "nothing moved");
        // An EXISTING owner still updates at capacity.
        assert!(
            write_attention_owned(&ctx, "o0", MetaEdit::Set("updated"), aterm_slog::Actor::Ctl)
                .unwrap()
        );
        // …and a slot freed is a slot a new owner can take.
        write_attention_owned(&ctx, "o1", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap();
        write_attention_owned(
            &ctx,
            "one-too-many",
            MetaEdit::Set("y"),
            aterm_slog::Actor::Ctl,
        )
        .unwrap();

        let long = "a".repeat(META_ATTENTION_KEYED_MAX + 1);
        assert_eq!(
            write_attention_owned(&ctx, "o0", MetaEdit::Set(&long), aterm_slog::Actor::Ctl),
            Err(AttentionWriteError::Meta(MetaWriteError::TooLong {
                cap: META_ATTENTION_KEYED_MAX
            }))
        );
        write_attention_owned(
            &ctx,
            BARE_ATTENTION_OWNER,
            MetaEdit::Set(&long),
            aterm_slog::Actor::Ctl,
        )
        .expect("the bare owner keeps the bare field's 256-byte cap");
    }

    /// A restore carries the bare owner's entry only; a meta seeded as a
    /// struct literal adopts its value as the bare entry on the first keyed
    /// write, so a keyed set-and-clear on top of it does not lose it.
    #[test]
    fn a_restore_carries_the_bare_entry_and_a_literal_seed_is_adopted() {
        let mut meta = SessionMeta::default();
        meta.set("attention", Some("bare".into()));
        meta.set_attention_owned("sup", Some("keyed".into()))
            .unwrap();
        assert_eq!(meta.attention.as_deref(), Some("keyed"));
        assert_eq!(meta.sanitized().attention.as_deref(), Some("bare"));
        meta.set_attention_owned(BARE_ATTENTION_OWNER, None)
            .unwrap();
        assert_eq!(
            meta.sanitized().attention,
            None,
            "a keyed entry never travels"
        );

        let mut seeded = SessionMeta {
            attention: Some("seeded".into()),
            ..SessionMeta::default()
        };
        assert!(
            seeded
                .set_attention_owned("sup", Some("keyed".into()))
                .unwrap()
        );
        assert!(seeded.set_attention_owned("sup", None).unwrap());
        assert_eq!(seeded.attention.as_deref(), Some("seeded"));
    }

    /// The retired hooks' bare badge stays live where it stands but does not
    /// travel, as the bare owner's entry, a literal seed, or under a keyed
    /// entry. Negative control: a bare attention without the prefix travels.
    #[test]
    fn a_retired_hook_badge_does_not_travel() {
        let legacy = "claude needs approval: Claude needs your permission - permission_prompt";
        let mut meta = SessionMeta::default();
        meta.set("attention", Some(legacy.into()));
        assert_eq!(
            meta.attention.as_deref(),
            Some(legacy),
            "a live write stands"
        );
        assert_eq!(meta.sanitized().attention, None);
        meta.set_attention_owned("sup", Some("claude bash: ls (x)".into()))
            .unwrap();
        assert_eq!(meta.sanitized().attention, None);

        let seeded = SessionMeta {
            attention: Some(legacy.into()),
            ..SessionMeta::default()
        };
        assert_eq!(seeded.sanitized().attention, None);

        let mut person = SessionMeta::default();
        person.set("attention", Some("needs approval soon".into()));
        assert_eq!(
            person.sanitized().attention.as_deref(),
            Some("needs approval soon")
        );
    }

    /// `meta unset supervisor holder=<h>` clears the claim only while `<h>` holds it.
    /// Negative control: the holder's own release clears it.
    #[test]
    fn a_holder_conditional_release_never_clears_another_holders_claim() {
        let ctx = crate::stub_session(0).ctx.clone();
        let shown = || {
            ctx.meta
                .lock()
                .unwrap()
                .live_supervisor(100)
                .map(str::to_owned)
        };
        assert_eq!(
            claim_supervisor(&ctx, "sup-b", Some(8), None, 100),
            Ok(true)
        );
        assert!(!release_supervisor_held_by(
            &ctx,
            "sup-a",
            100,
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(shown().as_deref(), Some("sup-b"));
        assert!(release_supervisor_held_by(
            &ctx,
            "sup-b",
            100,
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(shown(), None);
        assert!(
            !release_supervisor_held_by(&ctx, "sup-b", 100, aterm_slog::Actor::Ctl),
            "nothing left"
        );
    }

    #[test]
    fn a_supervisor_claim_is_exclusive_lapses_and_releases_by_connection() {
        let ctx = crate::stub_session(0).ctx.clone();
        let shown = |now: u64| {
            ctx.meta
                .lock()
                .unwrap()
                .live_supervisor(now)
                .map(str::to_owned)
        };
        assert_eq!(
            claim_supervisor(&ctx, "sup-a", Some(7), None, 100),
            Ok(true)
        );
        assert_eq!(shown(100).as_deref(), Some("sup-a"));
        assert_eq!(
            claim_supervisor(&ctx, "sup-b", Some(8), None, 100),
            Err("sup-a".to_string())
        );
        assert_eq!(
            claim_supervisor(&ctx, "sup-a", Some(9), None, 100),
            Ok(false)
        );
        // A release by a connection the claim is NOT bound to is a no-op.
        assert!(!release_supervisor(
            &ctx,
            Some(7),
            100,
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(shown(100).as_deref(), Some("sup-a"));
        assert!(release_supervisor(
            &ctx,
            Some(9),
            100,
            aterm_slog::Actor::Ctl
        ));
        assert_eq!(shown(100), None);

        // A TTL claim is shown until it lapses, then another holder may claim.
        assert_eq!(
            claim_supervisor(&ctx, "sup-a", None, Some(500), 100),
            Ok(true)
        );
        assert_eq!(shown(499).as_deref(), Some("sup-a"));
        assert_eq!(shown(500), None);
        assert_eq!(claim_supervisor(&ctx, "sup-b", None, None, 600), Ok(true));

        assert_eq!(
            meta_events(&ctx),
            [
                "field=supervisor value=sup-a",
                "field=supervisor value=-",
                "field=supervisor value=sup-a",
                "field=supervisor value=sup-b",
            ]
        );
    }
}

#[cfg(test)]
mod input_stall_tests {
    //! The server's publication of an input stall: the effective agent word and
    //! the server's own attention entry.
    use std::time::Instant;

    use aterm_session::input_backlog::InputWord;

    use super::{
        AgentStamp, META_ATTENTION_KEYED_MAX, MetaEdit, MetaField, SessionTimeline,
        write_attention_owned, write_server_attention,
    };
    use crate::input_stall::{InputStallFact, SERVER_ATTENTION_OWNER};

    fn stamp() -> AgentStamp {
        AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 7 },
            fp: 0x5eed,
        }
    }

    fn stall(stopped: bool) -> InputStallFact {
        InputStallFact {
            word: if stopped {
                InputWord::Stopped
            } else {
                InputWord::Stalled
            },
            since: Instant::now(),
            since_unix: None,
            bytes: 1,
            stopped,
            rss_mb: Some(39_731),
            restart: crate::input_stall::Restart::default(),
            act: None,
        }
    }

    fn agent_changes(tl: &SessionTimeline) -> Vec<String> {
        tl.since(None)
            .filter(|event| event.kind == "agent-change")
            .map(|event| event.payload.to_string())
            .collect()
    }

    fn publish(tl: &mut SessionTimeline, word: &'static str, claude: bool) -> bool {
        let reader = claude.then_some(aterm_phase::Program::Claude);
        tl.publish_agent(word, None, None, reader, stamp())
    }

    /// A stall on a Claude Code session turns its word `wall:unresponsive` (one rev) and holds
    /// it while the screen verdict moves underneath; the clear hands back the screen's word.
    #[test]
    fn a_stall_on_an_agent_is_wall_unresponsive_and_its_clear_hands_back_the_screen() {
        let mut tl = SessionTimeline::default();
        assert!(publish(&mut tl, "prompt", true));
        assert_eq!((tl.agent().word, tl.agent().rev), ("prompt", 1));

        assert!(tl.set_input(Some(stall(false))));
        assert_eq!(tl.agent().word, "wall:unresponsive");
        assert_eq!(tl.agent().detail, None, "a stall's detail is `-`");
        assert_eq!(tl.agent().rev, 2);
        assert!(
            tl.agent()
                .wire_fields()
                .contains(" agent=wall:unresponsive agent_detail=- agent_rev=2 "),
            "{}",
            tl.agent().wire_fields()
        );

        // The screen moves under the frozen program: kept, not shown.
        assert!(!publish(&mut tl, "busy", true));
        assert_eq!((tl.agent().word, tl.agent().rev), ("wall:unresponsive", 2));
        assert_eq!(tl.agent().screen_word, "busy");
        // Re-publishing the same stall moves nothing.
        assert!(!tl.set_input(Some(stall(false))));
        assert_eq!(tl.agent().rev, 2);

        assert!(tl.set_input(None));
        assert_eq!((tl.agent().word, tl.agent().rev), ("busy", 3));
        assert_eq!(
            agent_changes(&tl)
                .iter()
                .map(|p| p.split_once(' ').map_or(p.as_str(), |(w, _)| w))
                .collect::<Vec<_>>(),
            ["prompt", "wall:unresponsive", "busy"],
            "exactly one agent-change per effective move"
        );
    }

    /// A stopped job reads `agent_detail=stopped`; the move from a stall to a
    /// stop is its own rev.
    #[test]
    fn a_stopped_job_is_wall_unresponsive_with_detail_stopped() {
        let mut tl = SessionTimeline::default();
        publish(&mut tl, "busy", true);
        assert!(tl.set_input(Some(stall(false))));
        assert!(tl.set_input(Some(stall(true))));
        assert_eq!(tl.agent().word, "wall:unresponsive");
        assert_eq!(tl.agent().detail.as_deref(), Some("stopped"));
        assert_eq!(tl.agent().rev, 3);
    }

    /// Negative control: a session that is no identified agent keeps `agent=-`
    /// (its stall is the server's attention entry) and stays `-` when the stall
    /// clears. The stall counts once the session is read as an agent.
    #[test]
    fn a_stall_on_no_agent_leaves_the_agent_word_alone() {
        let mut tl = SessionTimeline::default();
        publish(&mut tl, "-", false);
        assert!(!tl.set_input(Some(stall(false))));
        assert_eq!((tl.agent().word, tl.agent().rev), ("-", 0));
        assert!(
            tl.agent().input.is_some(),
            "kept for when a reader names it"
        );
        // The program is identified while the stall stands.
        assert!(publish(&mut tl, "prompt", true));
        assert_eq!(tl.agent().word, "wall:unresponsive");
        assert!(tl.set_input(None));
        assert_eq!(tl.agent().word, "prompt");
    }

    /// The server's entry outranks an older supervisor badge while the stall stands, and its
    /// clear uncovers the supervisor's entry. The server's write marks nothing driver-written.
    #[test]
    fn the_server_entry_outranks_the_supervisor_and_its_clear_uncovers_it() {
        let ctx = crate::stub_session(0).ctx.clone();
        let effective = || {
            let meta = ctx.meta.lock().unwrap();
            (
                meta.attention.clone(),
                meta.attention_owners.effective_owner().map(str::to_owned),
            )
        };
        let frozen = "claude is frozen: not reading input since 14:02 (1 B queued)";
        assert!(write_server_attention(
            &ctx,
            SERVER_ATTENTION_OWNER,
            Some(frozen.to_string())
        ));
        assert!(
            !ctx.meta.lock().unwrap().driver_wrote(MetaField::Attention),
            "no driver wrote it"
        );
        assert!(write_server_attention(&ctx, SERVER_ATTENTION_OWNER, None));
        assert_eq!(effective(), (None, None));

        // The supervisor's badge first, the stall later.
        write_attention_owned(
            &ctx,
            "supervisor",
            MetaEdit::Set("claude other: answer this box"),
            aterm_slog::Actor::Ctl,
        )
        .expect("accepted");
        assert!(write_server_attention(
            &ctx,
            SERVER_ATTENTION_OWNER,
            Some(frozen.to_string())
        ));
        assert_eq!(
            effective(),
            (Some(frozen.to_string()), Some("aterm".to_string()))
        );
        // Re-writing the same line is no move.
        assert!(!write_server_attention(
            &ctx,
            SERVER_ATTENTION_OWNER,
            Some(frozen.to_string())
        ));
        assert!(write_server_attention(&ctx, SERVER_ATTENTION_OWNER, None));
        assert_eq!(
            effective(),
            (
                Some("claude other: answer this box".to_string()),
                Some("supervisor".to_string())
            )
        );
        let events: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|event| event.kind == "meta-change")
            .map(|event| event.payload.to_string())
            .collect();
        assert_eq!(events.len(), 5, "{events:?}");
        assert_eq!(
            events.last().map(String::as_str),
            Some("field=attention value=claude%20other:%20answer%20this%20box")
        );
    }

    /// The stored line is held to the keyed cap and the presentation
    /// sanitizer: it quotes a program name, which is whatever argv[0] said.
    #[test]
    fn the_server_entry_is_capped_and_sanitized() {
        let ctx = crate::stub_session(0).ctx.clone();
        let long = format!("evil\u{202e}prog {}", "x".repeat(400));
        assert!(write_server_attention(
            &ctx,
            SERVER_ATTENTION_OWNER,
            Some(long)
        ));
        let shown = ctx.meta.lock().unwrap().attention.clone().expect("set");
        assert!(shown.len() <= META_ATTENTION_KEYED_MAX, "{}", shown.len());
        assert!(shown.starts_with("evilprog "), "{shown}");
        // A line that sanitizes to nothing is a clear, never an empty badge.
        assert!(write_server_attention(
            &ctx,
            SERVER_ATTENTION_OWNER,
            Some("\u{202e}".to_string())
        ));
        assert_eq!(ctx.meta.lock().unwrap().attention, None);
    }
}

#[cfg(test)]
mod screen_move_tests {
    //! A move of the screen under a verdict that stands, counted for the supervisor host
    //! while the agent is not busy.
    use std::time::{Duration, Instant};

    use super::{AgentStamp, SessionTimeline};
    use crate::harness_host::STATUS_AFTER_SCREEN;

    fn read(tl: &mut SessionTimeline, word: &'static str, seq: u64) {
        tl.publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq },
                fp: seq,
            },
        );
    }

    /// A `/clear` at an idle composer moves no verdict, and its screen's move is counted
    /// ([`super::AgentPublication::screen_moves`]); the host is woken for the first move and
    /// then at most once per [`STATUS_AFTER_SCREEN`] ([`SessionTimeline::note_screen_move`]).
    /// NEGATIVE CONTROLS: a busy agent's moving screen counts nothing and wakes nothing; a
    /// verdict's own move is no screen move; no agent, no count.
    #[test]
    fn an_idle_screens_move_is_counted_and_wakes_the_host_once_per_settle() {
        let mut tl = SessionTimeline::default();
        read(&mut tl, "idle", 1);
        assert_eq!(
            (tl.agent().rev, tl.agent().screen_moves),
            (1, 0),
            "a verdict's move"
        );
        read(&mut tl, "idle", 2);
        read(&mut tl, "idle", 3);
        assert_eq!(
            (tl.agent().rev, tl.agent().screen_moves),
            (1, 2),
            "/clear: no rev"
        );
        let t0 = Instant::now();
        let mut fresh = SessionTimeline::default();
        read(&mut fresh, "idle", 1);
        assert!(fresh.note_screen_move(t0), "the first move wakes the host");
        assert!(!fresh.note_screen_move(t0 + Duration::from_millis(500)));
        assert!(!fresh.note_screen_move(t0 + STATUS_AFTER_SCREEN - Duration::from_millis(1)));
        assert!(
            fresh.note_screen_move(t0 + STATUS_AFTER_SCREEN),
            "once per settle"
        );
        // NEGATIVE CONTROLS.
        let mut busy = SessionTimeline::default();
        read(&mut busy, "busy", 1);
        assert!(!busy.note_screen_move(t0), "a busy agent's spinner");
        read(&mut busy, "busy", 2);
        assert_eq!(busy.agent().screen_moves, 0);
        let mut shell = SessionTimeline::default();
        assert!(!shell.note_screen_move(t0), "no agent");
        assert_eq!(shell.agent().screen_moves, 0);
    }
}

#[cfg(test)]
mod registry_drift_tests {
    //! THE DRIFT (card engine §3.10): the build a Claude Code's registry row
    //! reports, against the build its Store or Native drawer image names.
    use std::path::PathBuf;
    use std::sync::Arc;

    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};
    use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

    use super::{RegistryDrift, SessionTimeline};
    use crate::holder_image::{EdgeKind, HolderImage};

    /// The store build id of `version`.
    fn build(version: &str) -> u64 {
        atpkg::vendor_direct::Version::parse(version)
            .expect("a vendor build")
            .build_id()
    }

    /// Publish group `pgid`'s image: `claude` at `exe`, of `source`, naming
    /// `version` (`None`: no build).
    fn image(
        tl: &mut SessionTimeline,
        pgid: i32,
        exe: &str,
        source: ImageSource,
        version: Option<&str>,
    ) {
        tl.offer_holder_image(
            Arc::new(HolderImage::of(
                pgid,
                Some(5),
                Some(PathBuf::from(exe)),
                ImageBuild {
                    program: ProgramTag::new("claude"),
                    build: version.map_or(0, build),
                    source,
                },
                1,
            )),
            EdgeKind::Exec,
        );
    }

    /// The registry row of conversation `id` reporting `version`, read for
    /// group `pgid`.
    fn row(tl: &mut SessionTimeline, pgid: i32, id: &str, version: &str) {
        tl.set_claude_footer(
            pgid,
            None,
            Some(FooterFacts {
                version: Some(version.into()),
                owner: Some(FactsOwner {
                    floor: None,
                    session_id: id.into(),
                }),
                ..FooterFacts::default()
            }),
        );
    }

    const STORE_304: &str = "/p/pkg/store/claude/1000002000001000304/bin/claude";

    /// A row that reports another build than its Store drawer image names is
    /// said once per reported build; one that agrees is not.
    #[test]
    fn a_row_that_disagrees_with_its_drawer_image_is_said_once() {
        let mut tl = SessionTimeline::default();
        tl.note_foreground_group(42);
        image(&mut tl, 42, STORE_304, ImageSource::Store, Some("2.1.304"));
        assert_eq!(tl.take_registry_drift(), None, "no row yet");
        row(&mut tl, 42, "a", "2.1.303");
        assert_eq!(
            tl.take_registry_drift(),
            Some(RegistryDrift {
                pgid: 42,
                image: "2.1.304".into(),
                source: ImageSource::Store,
                reported: "2.1.303".into(),
            })
        );
        assert_eq!(tl.take_registry_drift(), None, "said once");
        row(&mut tl, 42, "a", "2.1.302");
        assert_eq!(
            tl.take_registry_drift().map(|drift| drift.reported),
            Some("2.1.302".into()),
            "another reported build is said"
        );
        row(&mut tl, 42, "a", "2.1.304");
        assert_eq!(tl.take_registry_drift(), None, "agreeing");
        let mut native = SessionTimeline::default();
        native.note_foreground_group(43);
        image(
            &mut native,
            43,
            "/h/.local/share/claude/versions/2.1.305",
            ImageSource::Native,
            Some("2.1.305"),
        );
        row(&mut native, 43, "b", "2.1.303");
        assert_eq!(
            native.take_registry_drift().map(|drift| drift.source),
            Some(ImageSource::Native)
        );
    }

    /// CONTROLS: the row that stood when the image was published (the
    /// replaced image's, until the new one writes its own), a build spelled
    /// in no form a vendor build parses from, and an image that is no Store
    /// or Native Claude Code drawer are no drift.
    #[test]
    fn a_stale_row_an_unknown_spelling_or_no_drawer_is_no_drift() {
        let mut tl = SessionTimeline::default();
        tl.note_foreground_group(42);
        row(&mut tl, 42, "a", "2.1.303");
        image(&mut tl, 42, STORE_304, ImageSource::Store, Some("2.1.304"));
        assert_eq!(tl.take_registry_drift(), None, "the replaced image's row");
        row(&mut tl, 42, "b", "2.1.303");
        assert!(tl.take_registry_drift().is_some(), "the new image's row");
        let mut odd = SessionTimeline::default();
        odd.note_foreground_group(42);
        image(&mut odd, 42, STORE_304, ImageSource::Store, Some("2.1.304"));
        row(&mut odd, 42, "a", "nightly");
        assert_eq!(odd.take_registry_drift(), None, "an unknown spelling");
        for (exe, source, build) in [
            ("/p/agents/claude", ImageSource::Launcher, None),
            ("/opt/elsewhere/claude", ImageSource::Unknown, None),
            (STORE_304, ImageSource::StoreRecord, Some("2.1.304")),
        ] {
            let mut tl = SessionTimeline::default();
            tl.note_foreground_group(42);
            image(&mut tl, 42, exe, source, build);
            row(&mut tl, 42, "a", "2.1.303");
            assert_eq!(tl.take_registry_drift(), None, "{source:?}");
        }
    }
}

#[cfg(all(test, any(debug_assertions, feature = "dev-seams")))]
mod card_engine_dev_row_tests {
    //! The card engine's dev rows a lead measurement reads (design §8):
    //! `holder-named` when the sweep names a card program, `card-seen` once
    //! per drawer token for its own family and known version, `holder-claim`
    //! for a claim taken. Each with its negative control: another program, a
    //! mismatched card or a seen token, and a renewal record nothing.
    use std::path::PathBuf;
    use std::sync::Arc;

    use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

    use super::{SessionTimeline, claim_supervisor_as};
    use crate::holder_image::{EdgeKind, HolderImage};

    fn rows(tl: &SessionTimeline, kind: &str) -> usize {
        tl.since(None).filter(|e| e.kind == kind).count()
    }

    fn claude_304(pgid: i32) -> HolderImage {
        HolderImage::of(
            pgid,
            Some(41),
            Some(PathBuf::from(
                "/p/store/claude/1000002000001000304/bin/claude",
            )),
            ImageBuild {
                program: ProgramTag::new("claude"),
                build: 1_000_002_000_001_000_304,
                source: ImageSource::Store,
            },
            1,
        )
    }

    #[test]
    fn holder_named_is_recorded_for_a_card_program_only() {
        let mut tl = SessionTimeline::default();
        tl.note_foreground_group(70);
        assert!(tl.set_program(70, Some("zsh".into())));
        assert_eq!(rows(&tl, "holder-named"), 0, "a shell is no card program");
        tl.note_foreground_group(71);
        assert!(tl.set_program(71, Some("claude".into())));
        assert_eq!(rows(&tl, "holder-named"), 1);
        assert!(!tl.set_program(72, Some("claude".into())), "not in front");
        assert_eq!(rows(&tl, "holder-named"), 1);
    }

    #[test]
    fn card_seen_is_recorded_once_for_the_current_drawers_own_card() {
        let mut tl = SessionTimeline::default();
        let image = claude_304(80);
        let token = image.token.expect("tracked");
        assert_eq!(tl.card_unseen(), None, "no drawer yet");
        assert!(!tl.note_card_seen(token, 3, "2.1.304", 0), "not published");
        tl.offer_holder_image(Arc::new(image), EdgeKind::Group);
        assert_eq!(tl.card_unseen(), Some(token));
        assert!(!tl.note_card_seen(token, 4, "2.1.304", 0), "agy's card");
        assert!(!tl.note_card_seen(token, 3, "2.1.303", 0), "another build");
        assert!(
            !tl.note_card_seen(token ^ 1, 3, "2.1.304", 0),
            "another token"
        );
        assert_eq!(rows(&tl, "card-seen"), 0);
        assert!(tl.note_card_seen(token, 3, "2.1.304", 0));
        assert!(!tl.note_card_seen(token, 3, "2.1.304", 0), "once per token");
        assert_eq!(rows(&tl, "card-seen"), 1);
        assert_eq!(tl.card_unseen(), None, "the probe stops looking");
    }

    /// §8's `bare_frames`: a predecessor's card refused above the drawer's
    /// own is not its bare time, but every pass composed after the drawer's
    /// card was seen on screen at the row it is decorated at is, whatever the
    /// gate read meanwhile. Mutation-checked: dropping the passes since the
    /// sight (`.max(late)`) reports `bare_frames=0` for the second drawer.
    #[test]
    fn bare_frames_count_the_passes_since_the_drawers_card_was_seen() {
        use crate::claude_card::{Bare, CardOutcome};
        fn decorated(tl: &SessionTimeline) -> Vec<String> {
            tl.since(None)
                .filter(|e| e.kind == "card-decorated")
                .map(|e| e.payload.to_string())
                .collect()
        }
        let mut tl = SessionTimeline::default();
        let image = claude_304(80);
        let token = image.token.expect("tracked");
        tl.offer_holder_image(Arc::new(image), EdgeKind::Group);
        // A predecessor's card at row 0 refused for two passes; the drawer's
        // own is decorated at row 9 with none of them counted.
        tl.note_card_frame(CardOutcome::Bare(token, Bare::Fence, Some(0)), 1);
        tl.note_card_frame(CardOutcome::Bare(token, Bare::Fence, Some(0)), 2);
        tl.note_card_frame(
            CardOutcome::Decorated {
                token,
                row: 9,
                armed: Some("new"),
            },
            3,
        );
        let rows = decorated(&tl);
        assert!(rows[0].contains("bare_frames=0"), "{rows:?}");
        // The next drawer: the old card refused at pass 4, the new card seen
        // at row 20 after it, and two passes still read the old one.
        let image = claude_304(81);
        let next = image.token.expect("tracked");
        tl.offer_holder_image(Arc::new(image), EdgeKind::Group);
        tl.note_card_frame(CardOutcome::Bare(next, Bare::Fence, Some(9)), 4);
        assert!(tl.note_card_seen(next, 3, "2.1.304", 20));
        tl.note_card_frame(CardOutcome::Bare(next, Bare::Fence, Some(9)), 5);
        tl.note_card_frame(CardOutcome::Bare(next, Bare::Fence, Some(9)), 6);
        tl.note_card_frame(
            CardOutcome::Decorated {
                token: next,
                row: 20,
                armed: Some("new"),
            },
            7,
        );
        let rows = decorated(&tl);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows[1].contains("bare_frames=2"),
            "the passes since the sight: {rows:?}"
        );
        let seen: Vec<String> = tl
            .since(None)
            .filter(|e| e.kind == "card-seen")
            .map(|e| e.payload.to_string())
            .collect();
        assert!(seen[0].contains("row=20 after=4"), "{seen:?}");
    }

    #[test]
    fn holder_claim_is_recorded_for_a_claim_taken_not_renewed() {
        let session = crate::stub_session(9_401);
        let now = crate::metrics::now_us();
        let claim = |at: u64| {
            claim_supervisor_as(
                &session.ctx,
                "harness",
                None,
                Some(at + 60_000_000),
                None,
                at,
                aterm_slog::Actor::Ctl,
            )
        };
        let count = || rows(&session.ctx.timeline.lock().unwrap(), "holder-claim");
        assert!(claim(now).is_ok(), "taken");
        assert_eq!(count(), 1);
        assert!(claim(now + 1).is_ok(), "renewed by its holder");
        assert_eq!(count(), 1, "a renewal is no new claim");
    }
}
