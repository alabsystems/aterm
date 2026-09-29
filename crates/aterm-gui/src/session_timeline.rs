// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session USER METADATA + EVENT TIMELINE (session-metadata stage 1).
//!
//! Two small, session-scoped stores that live on [`crate::SessionCtx`] (the one
//! per-session context every holder — the owning `Session`, the registry
//! [`crate::session_store::SessionHandle`], and each control connection — already
//! shares by `Arc`):
//!
//! * [`SessionMeta`] — the USER-settable identity a driver (human or agent)
//!   stamps on a session over the control socket (`meta set title|description|
//!   icon|role|attention …`): a display title that OUTRANKS the OSC 0/2 title
//!   in tab labels, a free-text description, an icon token, a typed `role`
//!   (`operator` designates the fleet operator), and a typed `attention`
//!   escalation message (non-empty ⇒ the menu-bar status item badges), and
//!   a typed `questions` policy word — how the in-window supervisor answers
//!   Claude Code's question dialog for THIS session (`ask` or `recommended`;
//!   see [`normalize_questions_value`]).
//!   Orthogonal to the engine's OSC title (which programs keep rewriting):
//!   this is what the OPERATOR calls the session, not what the running
//!   program does.
//! * [`SessionTimeline`] — a bounded, drop-oldest ring of lifecycle events
//!   (`spawned`, `state-change`, `title-change`, `cwd-change`, `meta-change`,
//!   `agent-change`, `status-change`, `human`), plus the server's published program and agent verdict
//!   ([`AgentPublication`]),
//!   modeled on [`crate::turn_ledger::TurnLedger`] (same cap, same monotonic-ms
//!   clock, same clamp discipline). Read back by the `timeline` verb and scanned
//!   by the `subscribe … events` digest for `EVENT <sid> meta …` pushes.
//!
//! ## Why `SessionCtx`, not `SessionHandle`
//!
//! A `SessionHandle` is a CLONE living in the registry — cloned out per control
//! request under the store lock. State stored as plain handle fields would fork
//! on every clone and would force every hot-path reader (the per-frame tab-label
//! refill) through a `Store` read lock. On `SessionCtx` there is exactly ONE
//! copy per session, reachable lock-disjointly from the pool session (tab
//! labels), the registry handle (`sessions`/handoff projection), and the control
//! thread (`meta`/`timeline` verbs) — no store lock on any hot path, and the
//! recorders in `SessionStore` mutators reach it through the handle's `ctx` Arc
//! they already hold. Both locks are LEAVES: taken briefly, never across a
//! `Terminal` or `Store` lock.

use std::collections::VecDeque;

use aterm_grapheme::GraphemeClusters;

use crate::turn_ledger::now_ms;

/// Byte cap for `meta set title` (after trim). Small: it is a tab label.
pub(crate) const META_TITLE_MAX: usize = 120;
/// Byte cap for `meta set description` (after trim) — a paragraph, not a doc.
pub(crate) const META_DESCRIPTION_MAX: usize = 1024;
/// Byte cap for `meta set icon` (after trim) — an emoji / short token.
pub(crate) const META_ICON_MAX: usize = 64;
/// Byte cap for `meta set role` (after trim) — a short role token. The one
/// recognized value today is `operator` (the menu-bar status item keys on it);
/// other values are stored verbatim for future roles.
pub(crate) const META_ROLE_MAX: usize = 64;
/// Byte cap for `meta set attention` (after trim) — a one-line needs-human
/// message. NON-EMPTY means the session is escalating: the status item badges
/// the menu bar and lists the message. Unset it once the human has acted.
pub(crate) const META_ATTENTION_MAX: usize = 256;
/// Byte cap for one KEYED attention entry (`meta set attention owner=<k>`),
/// after trim. Tighter than the bare field's: several owners share one badge.
pub(crate) const META_ATTENTION_KEYED_MAX: usize =
    aterm_types::control_verbs::META_ATTENTION_KEYED_MAX;
/// How many attention owners one session holds at once, the bare `-` owner
/// included — and its slot is always kept, so at most one fewer KEYED owners.
/// A write from a NEW keyed owner past that is refused, never evicting another
/// owner's escalation.
pub(crate) const ATTENTION_OWNERS_MAX: usize = 8;
/// Byte cap for an attention owner key or a supervisor holder name.
pub(crate) const META_OWNER_MAX: usize = 64;
/// The owner a bare `meta set attention <text>` writes as.
pub(crate) const BARE_ATTENTION_OWNER: &str = "-";
/// Byte cap for `meta set questions` (the stored, NORMALIZED value). A storage
/// bound only: the field is a CLOSED set ([`QUESTIONS_POLICY_WORDS`]) whose
/// longest word is 11 bytes, so the grammar refuses first and this cap is
/// never the reason a write fails.
pub(crate) const META_QUESTIONS_MAX: usize = 16;

/// The POLICY words `meta set questions` accepts: this session's own word for
/// how the in-window supervisor treats Claude Code's question dialog
/// (AskUserQuestion), over the global `[harness] answer_questions` switch.
/// What each MEANS belongs to the supervisor (`aterm_agent::supervise`):
/// `ask` leaves the dialog to a person (escalated like any box — the way a
/// controlling session takes this worker's questions), `recommended` answers
/// it with its recommended option(s), as `answer_questions = true` does. This
/// store holds the word and nothing else — the metadata layer never answers a
/// box. Unset (`questions=-`), the global switch decides.
pub(crate) const QUESTIONS_POLICY_WORDS: [&str; 2] = ["ask", "recommended"];

/// The one usage line every refusal of a `meta set questions` value prints
/// (after the `ERR ` prefix), so the wire and its tests cannot spell it twice.
pub(crate) const QUESTIONS_USAGE: &str = "usage: meta set questions ask|recommended";

/// NORMALIZE a `questions` value to its policy word, or `None` when it is not
/// one: surrounding whitespace is trimmed and the ASCII case folded
/// (`Recommended` stores `recommended`), so every reader — the `meta` reply,
/// the supervisor that parses it by key, a restore — compares ONE spelling.
/// Anything else (a misspelling, an interior space, a principal, a formatting
/// character) is `None`, and the wire answers [`QUESTIONS_USAGE`]. Pure: the
/// control verb, the restore sanitizer and [`SessionMeta::set`]'s second gate
/// all run this one door.
#[must_use]
pub(crate) fn normalize_questions_value(value: &str) -> Option<&'static str> {
    let value = value.trim();
    QUESTIONS_POLICY_WORDS
        .into_iter()
        .find(|word| word.eq_ignore_ascii_case(value))
}

/// True for characters that must never reach native/window chrome from USER
/// metadata. `char::is_control` covers C0/C1 (including every ASCII line break
/// and tab); the explicit format characters cover Unicode line separators,
/// bidi overrides/isolation, tags/fillers, and spoof-relevant default-ignorables.
/// ZWJ/ZWNJ and the standardized variation-selector blocks U+FE00..FE0F and
/// U+E0100..E01EF are deliberately allowed so ordinary emoji, joining scripts,
/// and ideographic variants survive.
pub(crate) fn is_forbidden_metadata_char(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{00ad}' // SOFT HYPHEN
                | '\u{034f}' // COMBINING GRAPHEME JOINER
                | '\u{061c}' // ARABIC LETTER MARK
                | '\u{115f}'..='\u{1160}' // Hangul fillers
                | '\u{17b4}'..='\u{17b5}' // Khmer inherent-vowel controls
                | '\u{180b}'..='\u{180f}' // Mongolian selectors / vowel separator
                | '\u{200b}' // ZERO WIDTH SPACE
                | '\u{200e}'..='\u{200f}' // directional marks
                | '\u{2028}'..='\u{202e}' // line/paragraph separators + bidi embedding/override
                | '\u{2060}'..='\u{206f}' // invisible operators + bidi isolates/deprecated controls
                | '\u{3164}' // HANGUL FILLER
                | '\u{feff}' // BOM / ZERO WIDTH NO-BREAK SPACE
                | '\u{ffa0}' // HALFWIDTH HANGUL FILLER
                | '\u{fff0}'..='\u{fff8}' // reserved specials
                | '\u{13430}'..='\u{1343f}' // Egyptian hieroglyph format controls
                | '\u{1bca0}'..='\u{1bca3}' // shorthand format controls
                | '\u{1d173}'..='\u{1d17a}' // musical symbol controls
                | '\u{e0000}'..='\u{e007f}' // language/tag characters
        )
}

#[must_use]
pub(crate) fn metadata_has_forbidden_formatting(value: &str) -> bool {
    value.chars().any(is_forbidden_metadata_char)
}

/// Canonical single-line presentation sanitizer shared by restored metadata
/// and every USER-metadata chrome boundary. Unsafe controls are removed and
/// the byte cap is applied only between grapheme clusters, so a combining mark
/// or ZWJ emoji is never bisected into malformed-looking chrome.
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

/// Sanitize one named USER metadata field for presentation/persistence.
/// Unknown fields and values that become empty are represented as unset.
#[must_use]
pub(crate) fn sanitize_metadata_value(field: &str, value: &str) -> Option<String> {
    let cap = SessionMeta::cap(field)?;
    // `questions` is a closed set, not a presentation line: a value from a
    // restore manifest or an older caller is NORMALIZED or dropped — never cut
    // to the cap, which could turn one word into another.
    if field == MetaField::Questions.wire_name() {
        return normalize_questions_value(value).map(str::to_owned);
    }
    let value = sanitize_presentation_line(value, cap);
    (!value.is_empty()).then_some(value)
}

/// The badge the retired Claude Code hooks left: aterm-link's Notification and
/// PermissionRequest hooks set the BARE attention to `claude needs approval:
/// <message>` (their `permission::ATTENTION_PREFIX`), and only the hook's own
/// clear could unset it. The hooks went in 59c3bee59 and `aterm link hook` is a
/// tombstone that writes no meta, so nothing clears such a badge any more — and
/// nothing writes one. Carried as the bare owner's entry, it crossed every
/// update and restore and came back whenever the supervisor cleared its own:
/// measured 2026-09-27 on v0.95.0, a busy agent's tab reading `claude needs
/// approval: Claude needs your permission - permission_prompt` two days after
/// the box it was raised for was answered.
pub(crate) const RETIRED_HOOK_BADGE: &str = "claude needs approval:";

/// [`sanitize_metadata_value`] for an attention CARRIED past this process — a
/// handoff, a restore, the crash journal — or past its shell, when a closed
/// tab is reopened on a fresh one: the retired hooks' badge
/// ([`RETIRED_HOOK_BADGE`]) does not travel. Dropping it on the carry is what
/// clears the last one standing, since no writer produces it. A live `meta set
/// attention` is not filtered: a person may still write any text, and loses
/// only this one prefix across a restart.
#[must_use]
pub(crate) fn carried_attention(value: &str) -> Option<String> {
    sanitize_metadata_value("attention", value)
        .filter(|value| !value.starts_with(RETIRED_HOOK_BADGE))
}

/// How many timeline events a session retains (drop-oldest past this), sized
/// like the turn ledger: a long session stays readable, the ring never grows.
pub(crate) const TIMELINE_CAP: usize = 512;

/// Byte cap for one event's stored payload — the payload is a short `k=v` wire
/// token string (values already pct-encoded), so 256B holds every real event;
/// the clamp only guards a pathological title/cwd from bloating the ring.
const MAX_PAYLOAD: usize = 256;

/// The user-settable per-session metadata (`meta set`/`meta unset`). All
/// fields are `None` until a driver sets them; `user_title` (when set +
/// non-empty) outranks the live OSC title in tab labels and stays until unset.
#[derive(Default, Clone, Debug)]
pub(crate) struct SessionMeta {
    /// Operator-chosen display title — the TOP rung of the tab-label chain.
    pub user_title: Option<String>,
    /// Free-text purpose/notes for the session (agents leave context here).
    pub description: Option<String>,
    /// Icon token (emoji or short name); reserved for the strip/UI stage.
    pub icon: Option<String>,
    /// TYPED role token. `operator` designates the fleet operator to the
    /// menu-bar status item (which falls back to the legacy `operator: …`
    /// title convention only when no session carries the typed role).
    pub role: Option<String>,
    /// TYPED needs-human escalation: non-empty ⇒ this session wants a human,
    /// and the value is the one-line reason shown in the status-item menu.
    /// Replaces the legacy `⚠`-title convention (still honored as fallback).
    ///
    /// This is the EFFECTIVE value — the most recently set entry of
    /// [`Self::attention_owners`] — so every reader (the status item, the tab
    /// chrome, `meta`, `sessions meta=1`, the `meta-change` event) sees one
    /// line without knowing that several owners may be escalating. Written only
    /// through [`Self::set`] and [`Self::set_attention_owned`], which keep it in
    /// step with the map.
    pub attention: Option<String>,
    /// KEYED attention (`meta set attention owner=<k> <text>`): one entry per
    /// owner, bounded at [`ATTENTION_OWNERS_MAX`], so a supervisor and a human
    /// (or two supervisors) can raise and clear their own escalations without
    /// clearing each other's. The bare form is owner [`BARE_ATTENTION_OWNER`].
    /// Values only in the sense [`Self::attention`] projects: not part of
    /// equality, and only the bare owner's entry is carried across a restore
    /// (the restore leaf has one attention field; see [`Self::sanitized`]).
    pub(crate) attention_owners: AttentionOwners,
    /// Who is SUPERVISING this session (`meta set supervisor <holder>`), so a
    /// running supervisor is visible in `status`, `sessions` and `meta`. Not
    /// user identity: never restored, not part of equality, not `meta=1`.
    pub(crate) supervisor: Option<SupervisorClaim>,
    /// TYPED question policy (2026-09-25): how the in-window supervisor
    /// treats Claude Code's question dialog for this session — one of
    /// [`QUESTIONS_POLICY_WORDS`], stored lowercase. Unset ⇒ the global
    /// `[harness] answer_questions` switch decides. A VALUE like
    /// `role`: part of equality, counted by `sessions meta=1`, carried across a
    /// restore and an update's handoff. Only ever holds a word
    /// [`normalize_questions_value`] produced.
    pub questions: Option<String>,
    /// PROVENANCE, not value: one bit per [`MetaField`] a DRIVER has written on
    /// this process — every write [`apply_meta_value`] accepted (`meta set`,
    /// `meta unset`, the GUI rename, the operator row's stamp), including a
    /// re-set to the same value and a clear of a field that was already unset,
    /// since each of those answered `OK` too. A restore seed ([`Self::set`])
    /// marks nothing. [`restore_carried_meta`] reads it so that an identity
    /// carried in from the previous process never overwrites a write a driver
    /// made here after that identity was captured. Not persisted and not part
    /// of equality (see the `PartialEq` impl). Write it only through
    /// [`apply_meta_value`]; read it through [`Self::driver_wrote`].
    pub(crate) driver_writes: u8,
}

/// Equality is over the six VALUES — what a tab label, a `meta` reader and a
/// restore capture see. `driver_writes` records how a value got there, and two
/// metas that read the same are the same identity. Destructured, so another
/// field cannot be added without deciding which side of that line it is on.
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
            questions,
            driver_writes: _,
        } = self;
        *user_title == other.user_title
            && *description == other.description
            && *icon == other.icon
            && *role == other.role
            && *attention == other.attention
            && *questions == other.questions
    }
}

impl Eq for SessionMeta {}

impl SessionMeta {
    /// Whether ANY field is set — the `sessions` listing's `meta=<1|0>` bit.
    /// `questions` counts like every other USER field: a fleet driver reading
    /// `meta=1` learns there is something to `@<sid> meta` for, and a session
    /// with a question policy of its own is exactly that.
    #[must_use]
    pub(crate) fn any_set(&self) -> bool {
        self.user_title.is_some()
            || self.description.is_some()
            || self.icon.is_some()
            || self.role.is_some()
            || self.attention.is_some()
            || self.questions.is_some()
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
            _ => None,
        }
    }

    /// The named field after the canonical single-line presentation policy.
    /// This is defensive even though the control setter validates input: a
    /// restore manifest or an older in-process caller may predate that gate.
    #[must_use]
    pub(crate) fn presentation_value(&self, field: &str) -> Option<String> {
        self.get(field)
            .and_then(|value| sanitize_metadata_value(field, value))
    }

    /// [`Self::presentation_value`] without the allocation: writes the canonical
    /// value into `out` and returns whether the field is set (and non-empty
    /// after the policy). `out` is left UNTOUCHED when the answer is `false`, so
    /// a caller may pass a resident slot holding a stale value it wants kept —
    /// which is exactly what the tab-strip refill's try-lock fallback needs.
    ///
    /// This exists for the render path: `presentation_value` allocates TWO
    /// `String`s (the char filter's collect plus the trim's `to_string`) per set
    /// field per tab, on a function that runs before the redraw early-out.
    pub(crate) fn presentation_value_into(&self, field: &str, out: &mut String) -> bool {
        let Some(cap) = Self::cap(field) else {
            return false;
        };
        let Some(value) = self.get(field) else {
            return false;
        };
        // Fixpoint of `sanitize_presentation_line`: nothing to filter (so the
        // `collect` would reproduce `value`), nothing to trim, and within the
        // cap (so the early `to_string` arm returns `value` verbatim). Every
        // production writer goes through `set`, which sanitizes, so this is the
        // steady-state path — one scan, zero allocations.
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
        // the exact sanitizer as before, byte-for-byte. This arm is the
        // bidi-override / invisible-character guard and must not be dropped.
        match sanitize_metadata_value(field, value) {
            Some(sanitized) => {
                out.clear();
                out.push_str(&sanitized);
                true
            }
            None => false,
        }
    }

    /// A canonical bounded copy suitable for restore/handoff persistence. The
    /// copy carries values only: which of them a driver wrote here is a fact
    /// about this process, and it does not travel.
    ///
    /// Attention travels as the BARE owner's entry only. A keyed entry belongs
    /// to a live driver that reached this process's socket; after a handoff
    /// that driver reconnects to the new instance and re-asserts it, and one
    /// carried under the bare owner could never be cleared by its real owner.
    /// The retired hooks' badge does not travel either ([`carried_attention`]).
    #[must_use]
    pub(crate) fn sanitized(&self) -> Self {
        let attention = if self.attention_owners.is_empty() {
            self.attention.as_deref()
        } else {
            self.attention_owners.get(BARE_ATTENTION_OWNER)
        }
        .and_then(carried_attention);
        Self {
            user_title: self.presentation_value("title"),
            description: self.presentation_value("description"),
            icon: self.presentation_value("icon"),
            role: self.presentation_value("role"),
            attention,
            attention_owners: AttentionOwners::default(),
            supervisor: None,
            questions: self.presentation_value("questions"),
            driver_writes: 0,
        }
    }

    /// Set (or with `None`, clear) `owner`'s attention entry and re-project
    /// [`Self::attention`]. `value` is stored as given — the caller validated
    /// it ([`validated_attention_value`]). Returns whether the EFFECTIVE value
    /// moved, or [`AttentionOwnersFull`] when `owner` is new and the map is at
    /// [`ATTENTION_OWNERS_MAX`] (nothing changed).
    pub(crate) fn set_attention_owned(
        &mut self,
        owner: &str,
        value: Option<String>,
    ) -> Result<bool, AttentionOwnersFull> {
        self.adopt_literal_attention();
        self.attention_owners.put(owner, value)?;
        let effective = self.attention_owners.effective().map(str::to_owned);
        let changed = self.attention != effective;
        self.attention = effective;
        Ok(changed)
    }

    /// A meta built as a struct literal (a restore seed, a test) carries its
    /// attention in [`Self::attention`] with an empty map. Before the first
    /// keyed write, that value becomes the bare owner's entry, so a keyed
    /// write on top of it — and the clear after — cannot lose it.
    fn adopt_literal_attention(&mut self) {
        if self.attention_owners.is_empty()
            && let Some(value) = self.attention.clone()
        {
            let _ = self.attention_owners.put(BARE_ATTENTION_OWNER, Some(value));
        }
    }

    /// The supervisor's holder name while its claim is live at `now_us`
    /// ([`crate::metrics::now_us`]); a lapsed `ttl=` claim reads as none.
    #[must_use]
    pub(crate) fn live_supervisor(&self, now_us: u64) -> Option<&str> {
        self.supervisor
            .as_ref()
            .filter(|claim| claim.expires_us.is_none_or(|at| at > now_us))
            .map(|claim| claim.holder.as_str())
    }

    /// When the `ttl=` claim lapses ([`crate::metrics::now_us`]), live or not;
    /// `None` without a claim or for a connection-bound one. The presence
    /// timer wakes at it ([`lapse_supervisor`]) — nothing else would notice.
    #[must_use]
    pub(crate) fn supervisor_expiry(&self) -> Option<u64> {
        self.supervisor.as_ref().and_then(|claim| claim.expires_us)
    }

    /// THE CLAIM A SEAMLESS UPDATE CARRIES (the round-four plan, item 9), as
    /// `(row, claim_known)` for the handoff record
    /// (`SessionRecord::supervisor` / `claim_known`) at `now_us`:
    ///
    /// * no live claim, or one held by `own_holder` — this process's in-GUI
    ///   host, whose claim dies with it and whose successor host takes the
    ///   session at Commit — is `(None, true)`: nobody else holds it, and the
    ///   successor may supervise at once;
    /// * a live `ttl=` lease is `(Some("<holder> <remaining_ms>"), true)`,
    ///   put back on the successor's session before it is registered, so its
    ///   holder renews it there as if nothing happened and the successor's
    ///   host parks behind it;
    /// * a claim bound to a connection is `(None, false)`: that connection
    ///   ends with this process, so the claim cannot cross — its holder
    ///   claims again from the successor, and `false` tells the successor's
    ///   host to give it the time to ([`crate::harness_host`]'s adopted-claim
    ///   grace).
    ///
    /// Carrying our OWN host's lease would park the successor's host behind
    /// a dead process's claim for up to its whole lease, after every update.
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
        match claim.expires_us {
            // Rounded UP to a whole ms, so a live lease never carries as 0.
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

    /// THE KEYED ESCALATIONS A SEAMLESS UPDATE CARRIES (the round-four plan,
    /// item 9): every owner's attention entry, `"<owner> <pct-text>"`, in
    /// write order (oldest first, so the successor shows the same one), the
    /// bare owner's included — but never [`crate::input_stall::SERVER_ATTENTION_OWNER`]'s,
    /// which the successor's own input watch derives again from the program.
    ///
    /// Why carried: an escalation is a request for a person. Before this, an
    /// update erased every keyed one — a supervisor's "needs you" went from
    /// the menu bar with nobody having answered it, and nothing re-raised it
    /// until that supervisor looked at the same point again. A map built as a
    /// literal (its bare value in [`Self::attention`] alone) carries nothing
    /// here: that bare value crosses in the restore leaf, as it always has.
    #[must_use]
    pub(crate) fn handoff_attention_owners(&self) -> Vec<String> {
        self.attention_owners
            .in_write_order()
            .into_iter()
            .filter(|(owner, _)| *owner != crate::input_stall::SERVER_ATTENTION_OWNER)
            .map(|(owner, text)| format!("{owner} {}", crate::control::pct_encode(text)))
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
            _ => None,
        }
    }

    /// Set (or with `None`, unset) a named field. Returns `Some(changed)` for a
    /// known field (`changed` = the stored value actually moved, so callers only
    /// record/notify/repaint on a REAL change), `None` for an unknown name.
    pub(crate) fn set(&mut self, field: &str, value: Option<String>) -> Option<bool> {
        if field == "attention" {
            // The bare field is the bare OWNER: every existing caller (the
            // wire's bare form, a restore seed) keeps its meaning, and a
            // keyed owner's entry is untouched by it. The bare owner is never
            // refused for capacity — the map always has its slot.
            let value = value.and_then(|value| sanitize_metadata_value(field, &value));
            return Some(
                self.set_attention_owned(BARE_ATTENTION_OWNER, value)
                    .unwrap_or(false),
            );
        }
        let slot = match field {
            "title" => &mut self.user_title,
            "description" => &mut self.description,
            "icon" => &mut self.icon,
            "role" => &mut self.role,
            "questions" => &mut self.questions,
            _ => return None,
        };
        // Callers exposed to the user reject unsafe/over-cap values so the
        // rejection is visible. This second gate protects older/internal
        // callers by storing only the same canonical representation chrome
        // consumes.
        let value = value.and_then(|value| sanitize_metadata_value(field, &value));
        let changed = *slot != value;
        *slot = value;
        Some(changed)
    }
}

/// One owner's attention entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AttentionEntry {
    owner: String,
    text: String,
    /// Write order within this map: the highest is the most recent, and the
    /// most recent entry is the effective one.
    stamp: u64,
}

/// A new owner found the map at [`ATTENTION_OWNERS_MAX`]: nothing was stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AttentionOwnersFull;

/// The keyed attention map ([`SessionMeta::attention_owners`]): at most
/// [`ATTENTION_OWNERS_MAX`] owners, each with one line.
///
/// WHICH ENTRY READERS SEE: the most recently WRITTEN one. The texts are free
/// form, so there is no severity to rank them by that aterm did not invent;
/// recency is the order a human reading the badge can predict, and a clear
/// falls back to the next most recent, so no owner's escalation is hidden by
/// another's clear. A re-set of an entry to the text it already holds is a
/// no-op and does not move it to the front.
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

    /// Every `(owner, text)` in WRITE order, oldest first: the order that,
    /// written again into an empty map, gives back the same effective entry
    /// (the most recent is written last) — what a handoff carries
    /// ([`SessionMeta::handoff_attention_owners`]).
    fn in_write_order(&self) -> Vec<(&str, &str)> {
        let mut entries: Vec<&AttentionEntry> = self.entries.iter().collect();
        entries.sort_by_key(|entry| entry.stamp);
        entries
            .into_iter()
            .map(|entry| (entry.owner.as_str(), entry.text.as_str()))
            .collect()
    }

    /// Store (`Some`) or clear (`None`) `owner`'s entry.
    fn put(&mut self, owner: &str, value: Option<String>) -> Result<(), AttentionOwnersFull> {
        let at = self.entries.iter().position(|entry| entry.owner == owner);
        match (at, value) {
            (Some(at), None) => {
                self.entries.remove(at);
            }
            (None, None) => {}
            (Some(at), Some(text)) => {
                if self.entries[at].text != text {
                    self.next_stamp += 1;
                    self.entries[at].text = text;
                    self.entries[at].stamp = self.next_stamp;
                }
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
                    stamp: self.next_stamp,
                });
            }
        }
        Ok(())
    }
}

/// A live supervisor's claim on a session ([`SessionMeta::supervisor`]).
///
/// TWO LIFETIMES, mirroring the two the protocol already has. With `ttl=<ms>`
/// the claim is a lease: it lapses at `expires_us` unless the holder re-sets
/// it, which is how a driver that opens one connection per request (`aterm
/// ctl`) keeps it. Without `ttl=` it is bound to the CONNECTION that set it
/// (`conn`), and is cleared when that connection stops serving requests — it
/// closes, however it closes, or turns into a `subscribe` stream — the
/// fabric bridge's drop-guard shape (`BridgeLostGuard`), so a supervisor that
/// dies stops being shown as running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorClaim {
    /// The holder name the claim was made under.
    pub(crate) holder: String,
    /// The control connection it is bound to, when it has no `ttl=`.
    pub(crate) conn: Option<u64>,
    /// When a `ttl=` claim lapses ([`crate::metrics::now_us`]).
    pub(crate) expires_us: Option<u64>,
}

/// Whether `token` is a well-formed attention owner key or supervisor holder
/// name: one wire token of 1..=[`META_OWNER_MAX`] bytes from
/// `[A-Za-z0-9._:@/-]` — the lease holder's shape, so a name prints verbatim
/// and can never be mistaken for a field, an option or the `-` sentinel of a
/// reply (a lone `-` is the bare attention owner and no holder's name).
#[must_use]
pub(crate) fn valid_owner_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= META_OWNER_MAX
        && token.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'/' | b'-')
        })
}

/// The USER-metadata fields as a CLOSED type. Everything past the wire
/// PARSE boundary carries this instead of a `&str` name, which removes two
/// hazards by construction: the unknown-field case stops being reachable, and
/// the `meta-change` record no longer has to re-map a borrowed name onto a
/// `'static` token before printing it into a payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaField {
    /// `title` — the operator's display title, TOP rung of the label chain.
    Title,
    /// `description` — free-text purpose/notes an agent leaves behind.
    Description,
    /// `icon` — an emoji / short token for the strip.
    Icon,
    /// `role` — typed role token (`operator` is the recognized value).
    Role,
    /// `attention` — typed needs-human escalation message (non-empty ⇒ badge).
    Attention,
    /// `questions` — the typed question policy the in-window supervisor reads
    /// for this session (a CLOSED set, [`QUESTIONS_POLICY_WORDS`]).
    Questions,
}

impl MetaField {
    /// Every field, in the order `meta` prints them.
    pub(crate) const ALL: [Self; 6] = [
        Self::Title,
        Self::Description,
        Self::Icon,
        Self::Role,
        Self::Attention,
        Self::Questions,
    ];

    /// This field's bit in [`SessionMeta::driver_writes`].
    const fn bit(self) -> u8 {
        1 << self as u8
    }

    /// Parse a wire field token, or `None` for an unknown name — the ONE door
    /// from the string vocabulary into the typed one.
    #[must_use]
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "title" => Self::Title,
            "description" => Self::Description,
            "icon" => Self::Icon,
            "role" => Self::Role,
            "attention" => Self::Attention,
            "questions" => Self::Questions,
            _ => return None,
        })
    }

    /// The stable token [`SessionMeta::set`]/[`SessionMeta::get`] key on and the
    /// `meta-change` payload prints. Safe to print verbatim: a closed
    /// vocabulary, never free text (only the VALUE is user-supplied).
    #[must_use]
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Description => "description",
            Self::Icon => "icon",
            Self::Role => "role",
            Self::Attention => "attention",
            Self::Questions => "questions",
        }
    }

    /// This field's byte cap — applied to the TRIMMED value, measured in bytes.
    #[must_use]
    pub(crate) const fn cap(self) -> usize {
        match self {
            Self::Title => META_TITLE_MAX,
            Self::Description => META_DESCRIPTION_MAX,
            Self::Icon => META_ICON_MAX,
            Self::Role => META_ROLE_MAX,
            Self::Attention => META_ATTENTION_MAX,
            Self::Questions => META_QUESTIONS_MAX,
        }
    }
}

/// What a metadata write INTENDS. `Clear` is first-class rather than a `Set("")`
/// spelling: both end with a stored `None`, but they record DIFFERENT timeline
/// payloads (`value=-` is the documented cleared marker; `value=` is not), so an
/// `events` consumer must be able to tell them apart. Typing the intent stops
/// `Set("")` from silently masquerading as a clear at the mutation boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaEdit<'a> {
    /// Store this value, after the full validation ladder.
    Set(&'a str),
    /// Unset the field — labels fall back down the chain.
    Clear,
}

/// Why a metadata write was REFUSED. Every variant is user-visible: the control
/// arm renders it as its existing `ERR` line, a GUI caller as an inline
/// rejection. Refusal is deliberate — a value is never silently truncated or
/// stripped, because the caller must know its label did not land.
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
    /// ([`normalize_questions_value`]); the wire answers [`QUESTIONS_USAGE`].
    NotAQuestionsPolicy,
}

/// PURE validation: the whole `meta set` ladder minus the store — trim, empty
/// rejection, forbidden-formatting rejection, byte cap, then canonicalization to
/// the STORED representation. No locks and no ctx, so any caller can run it
/// before it owns anything.
///
/// `Ok(None)` is produced ONLY by [`MetaEdit::Clear`]: a `Set` that survives the
/// ladder always has a canonical value, because rejection already removed every
/// input that could sanitize away to nothing.
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
    // The one TYPED-VOCABULARY field: its closed set is the whole ladder (a
    // word it accepts has no formatting character and is far under the cap),
    // and what it stores is the normalized spelling, not the typed one.
    if field == MetaField::Questions {
        return normalize_questions_value(value)
            .map(|word| Some(word.to_string()))
            .ok_or(MetaWriteError::NotAQuestionsPolicy);
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

/// Apply one already-validated metadata mutation and — on an ACTUAL change —
/// record the `meta-change` timeline event (`field=<f> value=<pct|->`). Returns
/// whether the stored value moved, which is the caller's gate for the wake +
/// subscriber fan-out (see the `meta` dispatch arm and the GUI rename commit).
///
/// Every call, a no-op one included, marks the field in
/// [`SessionMeta::driver_writes`]: this is the one door a driver's write comes
/// through, and each such write was answered `OK`.
///
/// ATOMICITY: the timeline record happens WHILE the meta guard is still held —
/// the one sanctioned meta→timeline nesting (documented on `SessionCtx`). The
/// control socket runs concurrent worker threads, so two authorized `meta set`s
/// racing on one session must not interleave between the store and the record:
/// dropping the meta guard first lets the pair invert (store A,B — record B,A),
/// leaving every `subscribe … events` watcher and the `timeline` verb with a
/// LAST event that names the LOSING value while the stored meta, the bare
/// `meta` readout, and the tab label all show the winner. Holding the guard
/// across the record makes event-stream order match store order by
/// construction. Deadlock-free: the one other site that takes these two nested
/// ([`restore_carried_meta`]) takes them in the same order, and timeline is a
/// leaf everywhere (nothing locks meta under timeline).
///
/// GUARDS ARE RELEASED ON RETURN, and that is load-bearing rather than tidy: a
/// SAME-THREAD GUI caller refreshes the tab chrome immediately after this
/// returns, and that refresh re-takes `ctx.meta` once per tab inside
/// `App::tab_titles`. `std::sync::Mutex` is not reentrant, so a refresh run
/// from inside the mutation would self-deadlock the event loop.
pub(crate) fn apply_meta_value(
    ctx: &crate::SessionCtx,
    field: MetaField,
    value: Option<String>,
) -> bool {
    let payload = meta_change_payload(field, value.as_deref());
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.driver_writes |= field.bit();
    let changed = meta.set(field.wire_name(), value).unwrap_or(false);
    if changed {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record("meta-change", payload);
    }
    drop(meta);
    changed
}

/// Why a keyed attention write was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttentionWriteError {
    /// The value failed the ordinary metadata ladder.
    Meta(MetaWriteError),
    /// `owner` is new and [`ATTENTION_OWNERS_MAX`] owners already hold one.
    OwnersFull,
}

/// PURE validation for one owner's attention text: [`validated_meta_value`]'s
/// ladder, with the keyed cap ([`META_ATTENTION_KEYED_MAX`]) for a keyed owner
/// and the bare field's own cap for the bare one, so the bare form's contract
/// does not move.
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

/// `meta set|unset attention owner=<k> …`: validate, then store `owner`'s
/// entry and — when the EFFECTIVE value moved — record the ordinary
/// `meta-change field=attention` event under the meta guard, exactly as
/// [`apply_meta_value`] does (the same atomicity argument holds). Marks the
/// attention field driver-written, like every accepted write. Returns whether
/// the effective value moved.
pub(crate) fn write_attention_owned(
    ctx: &crate::SessionCtx,
    owner: &str,
    edit: MetaEdit<'_>,
) -> Result<bool, AttentionWriteError> {
    let value = validated_attention_value(owner, edit).map_err(AttentionWriteError::Meta)?;
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let changed = meta
        .set_attention_owned(owner, value)
        .map_err(|AttentionOwnersFull| AttentionWriteError::OwnersFull)?;
    meta.driver_writes |= MetaField::Attention.bit();
    if changed {
        let payload = meta_change_payload(MetaField::Attention, meta.attention.as_deref());
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record("meta-change", payload);
    }
    drop(meta);
    Ok(changed)
}

/// THE SERVER'S OWN attention entry (2026-09-24): store (`Some`) or clear
/// (`None`) `owner`'s line — aterm's input watch writing
/// [`crate::input_stall::SERVER_ATTENTION_OWNER`] — and, when the EFFECTIVE
/// value moved, record `meta-change field=attention` under the meta guard
/// exactly as [`write_attention_owned`] does. Unlike that door it marks
/// nothing in [`SessionMeta::driver_writes`]: no driver wrote it, and a carried
/// identity is no older than a stall. The text is held to the keyed cap
/// ([`META_ATTENTION_KEYED_MAX`]) and the presentation sanitizer, since it
/// quotes a program name. Returns whether the effective value moved; a map
/// already holding [`ATTENTION_OWNERS_MAX`] owners refuses a new one, and then
/// nothing moves (the stall still reads on `status input=` and `agent=`).
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
        let payload = meta_change_payload(MetaField::Attention, meta.attention.as_deref());
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record("meta-change", payload);
    }
    drop(meta);
    changed
}

/// `meta set supervisor <holder> [ttl=<ms>]`: claim the session for `holder`.
/// `conn` binds a claim with no TTL to the setting connection
/// ([`SupervisorClaim`]); `expires_us` is a TTL claim's lapse instant.
///
/// A live claim by a DIFFERENT holder is refused with its name (`Err`), the
/// `lease acquire` rule — two supervisors answering one session's prompts is
/// the double press this key exists to make visible. The same holder renews:
/// its binding and expiry are replaced. Returns whether the shown holder moved
/// (the caller's wake/notify gate); a move records `meta-change
/// field=supervisor value=<holder>` under the meta guard.
pub(crate) fn claim_supervisor(
    ctx: &crate::SessionCtx,
    holder: &str,
    conn: Option<u64>,
    expires_us: Option<u64>,
    now_us: u64,
) -> Result<bool, String> {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let shown = meta.live_supervisor(now_us).map(str::to_owned);
    if let Some(other) = shown.as_deref()
        && other != holder
    {
        return Err(other.to_string());
    }
    meta.supervisor = Some(SupervisorClaim {
        holder: holder.to_string(),
        conn,
        expires_us,
    });
    let changed = shown.as_deref() != Some(holder);
    if changed {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record("meta-change", supervisor_change_payload(Some(holder)));
    }
    drop(meta);
    Ok(changed)
}

/// Clear the session's supervisor claim — every claim when `conn` is `None`
/// (`meta unset supervisor`), or only one bound to connection `conn` (that
/// connection stopped serving). Returns whether a LIVE holder stopped being
/// shown; that move records `meta-change field=supervisor value=-`.
pub(crate) fn release_supervisor(ctx: &crate::SessionCtx, conn: Option<u64>, now_us: u64) -> bool {
    release_supervisor_if(ctx, now_us, |claim| conn.is_none() || claim.conn == conn)
}

/// `meta unset supervisor holder=<h>`: clear the claim only while `holder`
/// holds it, so a supervisor giving its OWN claim back — after its
/// connection was lost and another holder claimed the session — never clears
/// the other's. Returns what [`release_supervisor`] returns.
pub(crate) fn release_supervisor_held_by(
    ctx: &crate::SessionCtx,
    holder: &str,
    now_us: u64,
) -> bool {
    release_supervisor_if(ctx, now_us, |claim| claim.holder == holder)
}

/// Clear the claim when `ours` says so, under the one meta guard that also
/// read it (no gap for another claim between the test and the clear).
fn release_supervisor_if(
    ctx: &crate::SessionCtx,
    now_us: u64,
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
    meta.supervisor = None;
    if was_live {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record("meta-change", supervisor_change_payload(None));
    }
    drop(meta);
    // A session the in-GUI host found held by another supervisor is claimed
    // once a claim goes (`harness_host`); a lapsed TTL claim read as `-`
    // before this, so the host is told of every release, shown or not.
    crate::harness_host::note_claim_released();
    was_live
}

/// A `ttl=` claim that has LAPSED at `now_us` is removed and says so:
/// `meta-change field=supervisor value=-`, the record a release makes, so the
/// `EVENT meta` push and the human channel learn that nothing answers this
/// session any more. A claim still live, a connection-bound one, or none is
/// left alone (`false`). Called by the presence timer at the expiry; a lapse
/// is otherwise silent, because nothing writes when a supervisor dies.
pub(crate) fn lapse_supervisor(ctx: &crate::SessionCtx, now_us: u64) -> bool {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if meta.supervisor_expiry().is_none_or(|at| at > now_us) {
        return false;
    }
    meta.supervisor = None;
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record("meta-change", supervisor_change_payload(None));
    drop(meta);
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
/// <pct-text>"`): the owner one owner token (the bare `-` included) and
/// never the server's own; the text decoded and put through the chrome's
/// presentation sanitizer at its owner's cap (the keyed cap, or the bare
/// field's). `None` when nothing presentable is left.
#[must_use]
pub(crate) fn parse_carried_attention(row: &str) -> Option<(&str, String)> {
    let (owner, text) = row.split_once(' ')?;
    if !valid_owner_token(owner) || owner == crate::input_stall::SERVER_ATTENTION_OWNER {
        return None;
    }
    let cap = if owner == BARE_ATTENTION_OWNER {
        META_ATTENTION_MAX
    } else {
        META_ATTENTION_KEYED_MAX
    };
    let text = sanitize_presentation_line(&aterm_control::wire::pct_decode(text), cap);
    (!text.is_empty()).then_some((owner, text))
}

/// PUT BACK THE CLAIM AND THE ESCALATIONS A SEAMLESS UPDATE CARRIED (the
/// round-four plan, item 9) onto an ADOPTED session's fresh meta — before
/// `App::register_session`, so no reader ever sees the session without them
/// and no second supervisor can claim it in between.
///
/// * `supervisor` (a carried `ttl=` lease) becomes the same lease with its
///   remaining time counted from `now_us`: its holder's next renewal renews
///   it here, and the successor's own host is refused as it would have been
///   in the parent. A row that does not read ([`parse_carried_supervisor`])
///   seeds nothing — the successor's host still waits its grace on that
///   session (`SessionRecord::claim_grace`).
/// * each `attention_owners` row is written again in order, so the same
///   entry is the effective one; a row that does not read
///   ([`parse_carried_attention`]) is dropped, and past
///   [`ATTENTION_OWNERS_MAX`] a new owner is refused exactly as a live write
///   would be.
///
/// Nothing here marks [`SessionMeta::driver_writes`]: a seed is no driver's
/// write, so a restore leaf's identity may still land on top of it. What moved
/// is recorded as the ordinary `meta-change` events, under the meta guard as
/// every other recorder takes them.
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
    for (owner, text) in attention_owners
        .iter()
        .filter_map(|row| parse_carried_attention(row))
    {
        attention_moved |= meta.set_attention_owned(owner, Some(text)).unwrap_or(false);
    }
    if let Some((holder, ms)) = claim {
        meta.supervisor = Some(SupervisorClaim {
            holder: holder.to_string(),
            conn: None,
            expires_us: Some(now_us.saturating_add(ms.saturating_mul(1000))),
        });
    }
    let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
    if attention_moved {
        timeline.record(
            "meta-change",
            meta_change_payload(MetaField::Attention, meta.attention.as_deref()),
        );
    }
    if let Some((holder, _)) = claim {
        timeline.record("meta-change", supervisor_change_payload(Some(holder)));
    }
    drop(timeline);
    drop(meta);
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

/// Put a CARRIED identity — the six USER fields a restore leaf captured in
/// the previous process — back onto a session that is ALREADY LIVE here:
/// registered and served under its sid, so a reader may have seen it without
/// them and a driver may have written it since. That is what separates this
/// from a seed onto a session nobody can address yet, and it sets two rules.
///
/// * A DRIVER'S WRITE WINS. A field a driver wrote on this process
///   ([`SessionMeta::driver_writes`]) keeps the driver's value, set or
///   cleared. That write was answered `OK` here after `carried` was captured
///   there, so it is the newer intent.
/// * Every other field takes the carried value EXACTLY — absent means unset —
///   and a field that moves records the ordinary `meta-change` event, under
///   the meta guard exactly as [`apply_meta_value`] records it. A `timeline`
///   reader or an `events` watcher that saw the session bare sees each field
///   arrive the way a `meta set` would arrive, and the chrome cache's
///   `high_id` key moves with it.
///
/// Returns whether any stored value moved: the caller's gate for waking the
/// session's `events` watchers.
pub(crate) fn restore_carried_meta(ctx: &crate::SessionCtx, carried: &SessionMeta) -> bool {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut moved = false;
    for field in MetaField::ALL {
        if meta.driver_wrote(field) {
            continue;
        }
        let name = field.wire_name();
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
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record("meta-change", payload);
    }
    drop(meta);
    moved
}

/// Validate THEN apply — the entry point a NON-wire caller uses so it cannot
/// skip a rung. The wire handler splits the two only because it must render
/// each refusal as its own byte-exact `ERR` line.
pub(crate) fn write_session_meta(
    ctx: &crate::SessionCtx,
    field: MetaField,
    edit: MetaEdit<'_>,
) -> Result<bool, MetaWriteError> {
    let value = validated_meta_value(field, edit)?;
    Ok(apply_meta_value(ctx, field, value))
}

/// THE HANDOFF GAP (round five, item 17): what an ADOPTED session's
/// recorders say about the time before the seamless update that adopted it.
/// Nothing of the timeline, the asciicast or the temporal spine crosses an
/// update — each starts empty in the successor — so each opens with this gap
/// instead of reading as a session that began at the update: the timeline's
/// first row is `handoff carried=0 from_build=<n>`, the cast header carries
/// `aterm_handoff` beside `aterm_truncated`/`aterm_dropped`, and `temporal
/// status` appends ` carried=0 from_build=<n>` after `dropped_events=`.
/// `from_build` is the manifest's `outgoing_build`; a producer that wrote
/// none reads `-` (`null` in the cast's JSON).
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

/// One recorded lifecycle event. `kind` is a closed vocabulary, in three groups:
/// the LIFECYCLE kinds this module's own recorders write (`spawned`,
/// `state-change`, `title-change`, `cwd-change`, `meta-change`,
/// `agent-change`, `closing`, and an adopted session's opening `handoff`
/// row, [`HandoffGap`]) and the status sweep's `status-change`
/// (`crate::session_status::STATUS_CHANGE`); the
/// FABRIC kinds (`inbox`, `inbox-seen`, `post`, `fetch`, `post-landed`, `hold`, `topic` —
/// `crate::fabric::FABRIC_EVENT_KINDS`, which is also their wire spelling on the
/// digest); `render`, the resize ledger's entry into and exit from
/// `desync-risk` (`desync-risk run= displaced= net= at=` / `healed run=
/// after_ms=`, pushed as `EVENT <local> render …`; [`crate::resize_ledger`]);
/// and `in-doubt`, which `crate::pty_idem` writes when an input verb
/// carrying an `id=` key failed in a way that says nothing about whether its
/// bytes reached the PTY. That last one has NO wire form on purpose: it is a
/// per-session record of one driver's unresolved write, not a fabric message,
/// and the fabric `EVENT` names are pinned. `payload` is a short
/// space-separated `k=v` token string whose free-text values are ALREADY
/// pct-encoded at record time, so the `timeline` verb and the events digest can
/// print it verbatim as the line tail (one line per event, always).
///
/// WHO CAN READ WHICH. The `timeline` verb lists every kind a LIVE session has
/// recorded. The last two rows a session ever records — `closing reason= by=`
/// (only when the close path said why) and `state-change state=closed` — are
/// written by the store as it deregisters the session, and the sid stops
/// resolving in that same write, so no request can reach them through the verb.
/// The `subscribe … events` digest pushes these lifecycle kinds from this ring:
/// `meta-change` as `EVENT <local> meta …`, `agent-change` (the server's agent
/// verdict moved, [`SessionTimeline::publish_agent`]) as `EVENT <local> agent
/// <word> rev=<n>`, `status-change` (the classified status moved phase or
/// outcome) as `EVENT <local> status …`, and `closing` as `EVENT <local> closing …` (the dying
/// watch's final pass, ahead of its `exited` frame); the
/// final `state-change` has no wire form of its own — `exited` is that fact.
/// Afterwards the same reason and actor stay answerable on the instance's
/// `exits` ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TimelineEvent {
    /// Per-session monotonic id (1-based), the `since=<id>` resume key.
    pub id: u64,
    /// Milliseconds since the process epoch ([`now_ms`] — monotonic, ordering
    /// + alignment against the turn ledger's clock, not wall time).
    pub t_ms: u64,
    /// The event kind token.
    pub kind: &'static str,
    /// Pre-encoded `k=v` tail, clamped to [`MAX_PAYLOAD`] bytes.
    pub payload: String,
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
    /// What the SERVER reads this session to be running, and its agent
    /// verdict ([`AgentPublication`]). Kept here — beside the ring the events
    /// digest already scans — because this is the one per-session store every
    /// reader reaches lock-disjointly: the status sweep writes it on the main
    /// thread, the program resolver thread writes `program`, and `sessions`,
    /// `await agent` and the push loop read it from the control threads.
    agent: AgentPublication,
    /// THE CLAUDE CODE FOOTER's facts (`aterm_agent::harness::footer`) and the
    /// foreground group they were read for. HOST-SIDE ONLY: the window writes
    /// them into the rule under Claude Code's input box, on the glass
    /// (`crate::claude_footer`); no verb carries them. Written by the footer
    /// resolver thread, read by the frame compose.
    claude_footer: Option<(i32, Option<u64>, aterm_agent::harness::footer::FooterFacts)>,
    /// The NEWEST launch card the frame compose read for the process those
    /// facts are — its group, kernel start and owner (floor and session id):
    /// the card names the model and effort it started on until the
    /// transcript or its `--model` decides them (`FooterFacts::filled_from`).
    /// A newer reading replaces it (a predecessor's card read before the new
    /// process painted its own); a frame with no card to read does not (the
    /// card scrolls away, and a message under it hides it, long before the
    /// transcript decides).
    claude_card: Option<(
        i32,
        Option<u64>,
        aterm_agent::harness::footer::FactsOwner,
        aterm_phase::LaunchCard,
    )>,
    /// THE SHELL'S PATH, MEASURED (gap audit 2026-09-24) from the environment
    /// of a foreground leader whose parent is the session's shell
    /// (`session_status::program::leader_facts`) — a fact about the SHELL, so
    /// it outlives the job it was read from. `None` until such a leader ran.
    path: Option<PathReading>,
    /// `copy=` of the current foreground leader and the group it was read
    /// for (`managed`/`foreign`, an agent only).
    leader_copy: Option<(i32, &'static str)>,
    /// The live agent upgrade of the conversation in this tab, as the window's
    /// upgrade host last reported it (`upgrade=`), or `None`.
    upgrade: Option<aterm_agent::harness::upgrade_drive::Row>,
    /// THE RESIZE LEDGER ([`crate::resize_ledger`]): every grid resize this
    /// session took and whether the app drew over content one displaced. Here,
    /// beside the ring its verdict transitions are recorded on (`kind=render`),
    /// for the same reason `agent` is: the one per-session leaf every reader
    /// reaches lock-disjointly.
    resizes: crate::resize_ledger::ResizeLedger,
}

/// One measurement of the shell's PATH ([`SessionTimeline::set_leader`]).
///
/// THE EVIDENCE IS A CHILD'S EXEC ENVIRONMENT, NOT THE SHELL'S (review of
/// 2026-09-25): a one-off `PATH=/usr/bin:/bin tool` reads frozen from a shell
/// whose own PATH is live. So a FROZEN reading is believed only once it is
/// SETTLED — a second job the shell started agrees, or it agrees with the
/// carried adoption mark ([`path_columns`]) — and it never raises the mark
/// (the mark means "adopted from a build before this update", and a
/// measurement cannot make that true). A LIVE reading settles at once: an
/// override that puts aterm's own `agents/` first is not a thing a person
/// types by accident, and it LOWERS the mark ([`SessionTimeline::take_path_lowered`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PathReading {
    /// What the PATH does with `claude`/`codex`.
    pub(crate) verdict: crate::session_status::program::PathVerdict,
    /// When it was read, on the ledger clock ([`now_ms`]).
    pub(crate) at_ms: u64,
    /// The foreground group it was read from: a second reading from ANOTHER
    /// group with the same verdict is another job agreeing.
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

/// `path=` and `path_evidence=` for a session: a SETTLED reading wins
/// (`measured:<age_ms>`), as does a frozen one the carried adoption mark
/// agrees with; a single job's frozen reading on an unmarked shell is not
/// taken as the shell's (`live`, `unconfirmed:<age_ms>` — a per-command
/// `PATH=` override reads exactly so); else the registry's carried adoption
/// mark (`frozen`, `carried`), else `live` with no evidence (`-`: a shell a
/// self-healing build spawned, whose integration keeps `agents/` in front).
/// `reading` is `(word, age_ms, settled)` ([`SessionTimeline::path_reading`]).
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
    /// The agent the last verdict's reader read — identified by `program`'s
    /// name or, for a runtime it runs under (`node`), by its screen — or
    /// `None` when the session is no identified agent. The in-GUI
    /// supervisor host attaches by it (`harness_host::agent_of`), so a
    /// Claude Code started as `node` is supervised like one started as
    /// `claude`.
    pub(crate) reader: Option<aterm_phase::Program>,
    /// The foreground process group `program` belongs to (`0` = none seen
    /// yet). A resolution answered for an older group is dropped.
    pub(crate) program_pgid: i32,
    /// `busy|prompt|question|wall:<kind>|idle|survey|unknown`, or `-` when
    /// the session is not an identified agent. The EFFECTIVE word: the
    /// screen's verdict ([`Self::screen_word`]), or `wall:unresponsive` while
    /// the program has stopped reading its input ([`Self::input`]).
    pub(crate) word: &'static str,
    /// The verdict's detail (`bash:not-read-only`, a limit's reset), wire-safe
    /// (pct-encoded at the edge), or `None`.
    pub(crate) detail: Option<String>,
    /// Bumps each time `word` or `detail` moves; `0` = never published.
    pub(crate) rev: u64,
    /// The approval box's command or path while `word` is `prompt`, folded
    /// to one clipped line — HOST-SIDE ONLY: the menu-bar row and the
    /// notification name it, the wire never carries it (`agent_detail` is
    /// the kind and verdict). Moving it bumps no `rev`.
    pub(crate) subject: Option<String>,
    /// When `word`/`detail` last moved, on the ledger clock ([`now_ms`]).
    pub(crate) changed_ms: u64,
    /// The screen the verdict stands on: the stamp of the LAST read of the
    /// live zone (a re-read whose zone was unchanged re-confirms the verdict
    /// and moves the stamp). `None` until the first read.
    pub(crate) stamp: Option<AgentStamp>,
    /// The SCREEN's verdict as last published, kept apart from the effective
    /// [`Self::word`] so a stall that ends hands the word back to it.
    pub(crate) screen_word: &'static str,
    /// The screen verdict's detail, likewise.
    pub(crate) screen_detail: Option<String>,
    /// The published input stall (2026-09-24,
    /// [`crate::input_stall::InputStallFact`]): while set on an identified
    /// agent the word is `wall:unresponsive` whatever the screen says — a
    /// frozen program's screen keeps its last verdict for ever, which is how
    /// the incident's approval box read `prompt` for 2h41m.
    pub(crate) input: Option<crate::input_stall::InputStallFact>,
}

/// Which screen an agent verdict was read from: its generation and its
/// FNV-1a-64 hash, the values `status gen=`/`hash=` print for that screen. A
/// press decided from the verdict fences on THESE (`key if-gen=<agent_gen>
/// if-fp=<agent_fp>`), never on a fresh `status gen=`: the verdict can be up to
/// one sweep older than the screen `status` reads, and a fence on the fresh
/// stamp would hold over a box nothing classified.
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
            subject: None,
            changed_ms: now_ms(),
            stamp: None,
            screen_word: "-",
            screen_detail: None,
            input: None,
        }
    }
}

impl AgentPublication {
    /// Milliseconds since the verdict last moved (`agent_since_ms=`).
    pub(crate) fn since_ms(&self) -> u64 {
        now_ms().saturating_sub(self.changed_ms)
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

/// How long a person's hands must have been off a session before their next
/// input opens a new burst: `EVENT <local> human` is pushed, and a `human` row
/// recorded, once per burst. The row shares the [`TIMELINE_CAP`] ring with
/// every other watcher's rows, so the gap is what bounds it: a person at work
/// is one row however long they stay, and the worst case — one input just
/// past each gap — is 120 rows an hour. (At 1 s, a slow typist wrote a row a
/// keystroke and evicted the agent, fabric and lifecycle rows others read.)
/// The supervisor needs no finer edge: it reads `human_ms=` itself at every
/// act, and its grace (`[harness] human_grace_s`, 120 s by default) is longer.
/// The stamp and its bursts are [`crate::human_input::HumanInputStamp`]'s.
pub(crate) const HUMAN_BURST_GAP_MS: u64 = 30_000;

impl SessionTimeline {
    /// Append one event, evicting the oldest past the cap. Returns its id.
    pub(crate) fn record(&mut self, kind: &'static str, payload: String) -> u64 {
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
        self.next_id
    }

    /// Record a `cwd-change` IFF `cwd` differs from the last one recorded (the
    /// GUI observes cwd drift on every output wake via the title-epoch path, so
    /// the dedup lives here, not at the observer). A `None`/empty cwd (OSC 7
    /// cleared) records `cwd=-`.
    pub(crate) fn record_cwd_change(&mut self, cwd: Option<&str>) {
        let cwd = cwd.filter(|c| !c.is_empty());
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
    /// was recorded (a reason to wake this session's subscribers).
    pub(crate) fn ingest_resizes(
        &mut self,
        copy: &crate::resize_ledger::JournalCopy,
        attribution: Option<crate::resize_ledger::Attribution>,
        now_ms: u64,
    ) -> bool {
        let transitions = self.resizes.ingest(copy, attribution, now_ms);
        self.record_render(&transitions)
    }

    /// Evaluate the resize ledger against `probe` and record any verdict
    /// transition as a `render` event. Returns whether one was recorded.
    pub(crate) fn evaluate_resizes(
        &mut self,
        probe: crate::resize_ledger::EngineProbe,
        now_ms: u64,
    ) -> bool {
        let transitions = self.resizes.evaluate(probe, now_ms);
        self.record_render(&transitions)
    }

    /// `render= resizes= flaps=` for `status`, evaluated against `probe` (`None`:
    /// the terminal lock was contended, and `render=-`).
    pub(crate) fn resize_status_fields(
        &mut self,
        probe: Option<crate::resize_ledger::EngineProbe>,
        now_ms: u64,
    ) -> String {
        let (fields, transitions) = self.resizes.status_fields(probe, now_ms);
        self.record_render(&transitions);
        fields
    }

    /// Only entry into and exit from `desync-risk` reach the lifecycle ring:
    /// a healthy flap records nothing, so at one a minute it cannot evict the
    /// rows other watchers read.
    fn record_render(&mut self, transitions: &[crate::resize_ledger::Transition]) -> bool {
        for transition in transitions {
            self.record("render", transition.payload());
        }
        !transitions.is_empty()
    }

    /// Events with `id > after`, oldest-first (ids only ever append increasing,
    /// so this is a suffix). `None` = all retained events.
    ///
    /// Double-ended so a caller that only wants the newest few can walk backwards
    /// instead of materializing the whole retained deque; the concrete iterator
    /// (a `VecDeque::range`) is, and so was the filtered `iter` before it.
    /// `DoubleEndedIterator: Iterator`, so every existing caller is unaffected.
    ///
    /// SEEK, DON'T FILTER — the turn-ledger twin. `next_id` is bumped per record
    /// and records are pushed to the BACK, so `id <= after` is monotone across
    /// the deque and `partition_point` lands on the first event past the
    /// watermark. The subscribe `events` digest calls this on every 250 ms
    /// liveness tick per watched target purely to learn that nothing changed;
    /// filtering made that O([`TIMELINE_CAP`]), seeking makes it
    /// O(log n + matched). A watermark below the retained low-water still yields
    /// everything (`partition_point` returns 0); the events digest says first how
    /// many records the hole cost (`GAP … events-dropped=`, [`Self::low_id`]).
    pub(crate) fn since(
        &self,
        after: Option<u64>,
    ) -> impl DoubleEndedIterator<Item = &TimelineEvent> + ExactSizeIterator {
        let start = match after {
            None => 0,
            Some(a) => self.events.partition_point(|e| {
                #[cfg(test)]
                crate::work_counts::retained_record_touched();
                e.id <= a
            }),
        };
        self.events.range(start..)
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

    /// Retained event count (the tests' and the bench fixture's measure; the
    /// verb frames via `since(None)`).
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }

    /// The server's published program and agent verdict for this session.
    pub(crate) fn agent(&self) -> &AgentPublication {
        &self.agent
    }

    /// The published program name for foreground group `pgid`, or `None` when
    /// the publication names another group (or none). The PTY reader's
    /// `modes-restored` event (2026-09-25, the foreground handback) names the
    /// program that lost the terminal with it — best effort: the status sweep
    /// may already have moved the publication to the group that reclaimed it.
    pub(crate) fn program_of(&self, pgid: i32) -> Option<&str> {
        if pgid > 0 && self.agent.program_pgid == pgid {
            self.agent.program.as_deref()
        } else {
            None
        }
    }

    /// Publish an agent verdict read from the screen `stamp` names. A move of
    /// `word` or `detail` bumps the rev and records one `agent-change` event
    /// (`<word> rev=<n> gen=<g> fp=<hex16>`, the `EVENT <local> agent …` push);
    /// an unchanged verdict records nothing but still moves the stamp (the
    /// verdict now stands on this read). Returns whether it moved. While an
    /// input stall is published ([`Self::set_input`]) the screen's verdict is
    /// kept but the effective word stays `wall:unresponsive`, so a screen that
    /// moves under a frozen program moves no rev.
    pub(crate) fn publish_agent(
        &mut self,
        word: &'static str,
        detail: Option<String>,
        subject: Option<String>,
        reader: Option<aterm_phase::Program>,
        stamp: AgentStamp,
    ) -> bool {
        self.agent.subject = subject;
        self.agent.stamp = Some(stamp);
        let reader_changed = self.agent.reader != reader;
        let old_word = self.agent.word;
        self.agent.reader = reader;
        self.agent.screen_word = word;
        self.agent.screen_detail = detail;
        let changed = self.settle_agent_word();
        if reader_changed || self.agent.word != old_word {
            // One wake after publication, even if both reader and phase moved.
            crate::harness_host::ring_phase();
        }
        changed
    }

    /// Publish (`Some`) or clear (`None`) the session's input stall
    /// ([`AgentPublication::input`]) and re-settle the effective word. Returns
    /// whether the word or detail moved — then the rev bumped and one
    /// `agent-change` was recorded, exactly as a screen verdict's move does.
    pub(crate) fn set_input(&mut self, input: Option<crate::input_stall::InputStallFact>) -> bool {
        let old_word = self.agent.word;
        self.agent.input = input;
        let changed = self.settle_agent_word();
        if self.agent.word != old_word {
            crate::harness_host::ring_phase();
        }
        changed
    }

    /// Make [`AgentPublication::word`]/`detail` the effective verdict; a move
    /// bumps the rev and records one `agent-change`
    /// (`<word> rev=<n> gen=<g> fp=<hex16>`). Returns whether it moved.
    fn settle_agent_word(&mut self) -> bool {
        let (word, detail) = self.agent.effective();
        if self.agent.word == word && self.agent.detail == detail {
            return false;
        }
        self.agent.word = word;
        self.agent.detail = detail;
        self.agent.rev += 1;
        self.agent.changed_ms = now_ms();
        let (generation, fp) = self.agent.stamp_fields();
        let payload = format!("{word} rev={} gen={generation} fp={fp}", self.agent.rev);
        self.record("agent-change", payload);
        true
    }

    /// The live zone was read again and was unchanged: the published verdict
    /// stands on this newer screen, so its stamp moves (no rev, no event).
    pub(crate) fn note_agent_stamp(&mut self, stamp: AgentStamp) {
        self.agent.stamp = Some(stamp);
    }

    /// The foreground process group moved to `pgid`: forget the program named
    /// for the old one (it is no longer what runs) and return whether a
    /// resolution is owed. The same group again owes nothing.
    pub(crate) fn note_foreground_group(&mut self, pgid: i32) -> bool {
        if self.agent.program_pgid == pgid {
            return false;
        }
        self.agent.program_pgid = pgid;
        let had_reader = self.agent.reader.take().is_some();
        self.leader_copy = None;
        if self.agent.program.take().is_some() || had_reader {
            // A runtime identified by its screen can have no named program;
            // its worker must still see the foreground group leave.
            crate::harness_host::ring();
        }
        true
    }

    /// The resolver's answer for `pgid`, applied only while that group is
    /// still the foreground one (a late answer for a finished job is dropped).
    /// Returns whether the current name changed, which is the worker's signal
    /// to wake the GUI status observer.
    pub(crate) fn set_program(&mut self, pgid: i32, program: Option<String>) -> bool {
        if self.agent.program_pgid == pgid && self.agent.program != program {
            self.agent.program = program;
            // The in-GUI supervisor host attaches by program (`harness_host`).
            crate::harness_host::ring();
            return true;
        }
        false
    }

    /// The footer resolver's answer for `pgid`, applied only while that group
    /// is still the foreground one. Returns whether the painted facts MOVED, so
    /// the caller repaints only for a real change.
    ///
    /// `started` is the process's kernel start time where it could be read:
    /// with the group it names the PROCESS the facts belong to
    /// ([`Self::claude_footer_identity`]).
    pub(crate) fn set_claude_footer(
        &mut self,
        pgid: i32,
        started: Option<u64>,
        facts: Option<aterm_agent::harness::footer::FooterFacts>,
    ) -> bool {
        if self.agent.program_pgid != pgid {
            return false;
        }
        let next = facts.map(|facts| (pgid, started, facts));
        if self.claude_footer == next {
            return false;
        }
        // A card belongs to one process and one owner: new facts for
        // anything else drop it.
        if self.claude_card.as_ref().is_some_and(|(p, s, owner, _)| {
            next.as_ref().is_none_or(|(np, ns, facts)| {
                (np, ns) != (p, s) || facts.owner.as_ref() != Some(owner)
            })
        }) {
            self.claude_card = None;
        }
        self.claude_footer = next;
        true
    }

    /// Keep the launch `card` the frame compose read for the process whose
    /// facts are stored — only while they are this group's and theirs is
    /// `owner`, and only a card of the build they name — in place of any it
    /// kept before: the newest reading wins. Whether it changed what is kept.
    pub(crate) fn note_claude_card(
        &mut self,
        pgid: i32,
        owner: &aterm_agent::harness::footer::FactsOwner,
        card: aterm_phase::LaunchCard,
    ) -> bool {
        let Some((p, started, facts)) = self.claude_footer.as_ref() else {
            return false;
        };
        if *p != pgid
            || self.agent.program_pgid != pgid
            || facts.owner.as_ref() != Some(owner)
            || facts.version.as_deref() != Some(card.version.as_str())
        {
            return false;
        }
        let next = Some((pgid, *started, owner.clone(), card));
        if self.claude_card == next {
            return false;
        }
        self.claude_card = next;
        true
    }

    /// The process the stored footer facts were read for — its group and its
    /// kernel start time — if any.
    pub(crate) fn claude_footer_identity(&self) -> Option<(i32, Option<u64>)> {
        self.claude_footer
            .as_ref()
            .map(|(pgid, started, _)| (*pgid, *started))
    }

    /// Forget the footer facts; whether there were any. For when the status
    /// sweep that keeps them honest stops (`tab_status` off): its foreground
    /// group goes stale with it, and stale facts would be painted over the
    /// next Claude Code in this tab.
    pub(crate) fn clear_claude_footer(&mut self) -> bool {
        self.claude_card = None;
        self.claude_footer.take().is_some()
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

    /// What the footer SHOWS for the group in front: its facts, filled from
    /// the kept launch card where nothing newer decided them
    /// (`FooterFacts::filled_from`).
    pub(crate) fn claude_footer_shown(&self) -> Option<aterm_agent::harness::footer::FooterFacts> {
        self.claude_footer()
            .map(|facts| facts.filled_from(self.claude_card()))
    }

    /// The leader read's other two answers for `pgid`
    /// (`session_status::program::leader_facts`), applied only while that
    /// group is the foreground one: the agent copy it runs, and — when it is
    /// the shell's direct child — a PATH measurement, which replaces the last
    /// one: agreed when another job read the same verdict last ([`PathReading`]),
    /// and owed to the registry's mark when it is live.
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

    /// Whether the shell's PATH is SETTLED frozen by measurement — two jobs
    /// in a row put a foreign `claude`/`codex` first ([`PathReading`]). The
    /// managed record counts such a tab when it carries no adoption mark: it
    /// is not "from before this update", but "used in every tab" is not true
    /// of it either.
    pub(crate) fn path_settled_frozen(&self) -> bool {
        self.path
            .is_some_and(|p| p.settled() && p.verdict.word() == "frozen")
    }

    /// A LIVE measurement the registry's mark has not been lowered for yet:
    /// `true` once, then `false` until another live reading follows a frozen
    /// one. The status sweep lowers the mark on it, so the handoff carries the
    /// measured fact and the managed-current count drops the tab. A frozen
    /// reading never raises the mark (review of 2026-09-25: one job's
    /// `PATH=` override raised it, and the band then counted that tab as
    /// "from before this update").
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

    /// THE OWNER'S COLUMNS every roster row and `status` carry after
    /// `supervisor=` (gap audit 2026-09-24): `path_evidence=` (how `path=`
    /// was learned: `measured:<age_ms>`, `unconfirmed:<age_ms>`, `carried`,
    /// `-` — [`path_columns`]), `copy=`
    /// (`managed|foreign|-`) and `upgrade=` (the live upgrade,
    /// `Row::column`, `-` for none). `mark` is the registry's frozen mark;
    /// the `path=` word it goes with is the first of the pair returned.
    pub(crate) fn owner_columns(&self, mark: bool, now_s: u64) -> (&'static str, String) {
        let (path, evidence) = path_columns(mark, self.path_reading());
        let upgrade = self
            .upgrade
            .as_ref()
            .map_or_else(|| "-".to_string(), |row| row.column(now_s));
        (
            path,
            format!(
                "path_evidence={evidence} copy={} upgrade={upgrade}",
                self.copy().unwrap_or("-")
            ),
        )
    }
}

/// Clamp a payload to [`MAX_PAYLOAD`] bytes without bisecting a UTF-8 char OR a
/// `%XX` percent-escape triple.
///
/// The char-boundary walk alone is NOT enough: recorded values are pct-encoded
/// BEFORE recording (`meta-change`/`cwd-change` payloads), and a pct-encoded
/// string is pure ASCII — every byte is a char boundary, so a naive cut can
/// land mid-triple and leave a dangling `%` or `%X` tail. A legitimately
/// accepted value overruns the cap easily (the meta caps are on RAW bytes and
/// encoding expands up to 3x), and both the `timeline` verb and the
/// `EVENT <sid> meta …` subscribe push emit the stored payload verbatim — a
/// strict client pct-decoder must never receive a truncated escape. So after
/// the char-boundary walk, if a `%` in the last two bytes before the cut starts
/// a GENUINE escape (two hex digits follow in the full string), back the cut
/// off to that `%`. A literal `%` that happens to precede hex loses ≤2 extra
/// bytes of an already-lossy clamp — harmless; correctness of the escape stream
/// wins. `%` is ASCII, so the backed-off cut is still a char boundary.
fn clamp_payload(s: String) -> String {
    if s.len() <= MAX_PAYLOAD {
        return s;
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
    s[..end].to_string()
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

    /// DIFFERENTIAL: the `partition_point` seek agrees with the linear filter it
    /// replaced at EVERY watermark, including the two the events digest stands
    /// on — below the retained low-water (yield everything) and at the high
    /// (yield nothing). The timeline's ids ARE contiguous, so this fixture also
    /// probes the boundaries either side of each retained id rather than only
    /// the gaps a sparse ledger would have.
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

        let reference = |after: Option<u64>| -> Vec<u64> {
            tl.events
                .iter()
                .filter(|e| after.is_none_or(|a| e.id > a))
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
        let got: Vec<(&str, &str)> = tl
            .since(None)
            .map(|e| (e.kind, e.payload.as_str()))
            .collect();
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
        tl.record("meta-change", long);
        let e = tl.since(None).next().unwrap();
        assert!(e.payload.len() <= MAX_PAYLOAD);
        assert!(e.payload.chars().all(|c| c == 'é'), "boundary-safe clamp");
    }

    /// The clamp must never bisect a `%XX` escape: recorded values are
    /// pct-encoded BEFORE recording, so the payload is pure ASCII and every
    /// byte is a char boundary — the char walk alone would happily cut `%E4`
    /// into `%E`. Slide the cut across all three in-triple offsets (via a
    /// 0/1/2-byte ASCII prefix) and require every stored `%` to still head a
    /// complete, decodable triple.
    #[test]
    fn payload_clamp_never_bisects_a_pct_escape() {
        for pad in 0..3usize {
            let encoded = crate::control::pct_encode(&"中".repeat(MAX_PAYLOAD)); // %E4%B8%AD…
            let payload = format!("{}{encoded}", "x".repeat(pad));
            let mut tl = SessionTimeline::default();
            tl.record("meta-change", payload);
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
        // Same value again: known field, NOT a change.
        assert_eq!(m.set("title", Some("build agent".into())), Some(false));
        // Unset flips back; a second unset is a known-field no-change.
        assert_eq!(m.set("title", None), Some(true));
        assert_eq!(m.set("title", None), Some(false));
        assert!(!m.any_set());
        // Unknown fields are rejected (None), not silently stored.
        assert_eq!(m.set("colour", Some("red".into())), None);
        assert_eq!(m.get("colour"), None);
        assert_eq!(SessionMeta::cap("title"), Some(META_TITLE_MAX));
        assert_eq!(SessionMeta::cap("description"), Some(META_DESCRIPTION_MAX));
        assert_eq!(SessionMeta::cap("icon"), Some(META_ICON_MAX));
        assert_eq!(SessionMeta::cap("role"), Some(META_ROLE_MAX));
        assert_eq!(SessionMeta::cap("attention"), Some(META_ATTENTION_MAX));
        assert_eq!(SessionMeta::cap("colour"), None);
        // The typed keys round-trip like the original three and flip any_set.
        assert_eq!(m.set("role", Some("operator".into())), Some(true));
        assert_eq!(m.get("role"), Some("operator"));
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
}

/// Proofs for the TYPED metadata write API — the one path both the control
/// socket (`meta set`/`meta unset`) and the GUI rename affordance take.
#[cfg(test)]
mod write_api_tests {
    use super::{MetaEdit, MetaField, MetaWriteError, apply_meta_value, validated_meta_value};

    /// The validation ladder refuses rather than repairs: an empty `Set` is a
    /// refusal (never a clear), forbidden formatting and an over-cap value are
    /// refusals (never a strip or a truncation), and a legal value comes back
    /// canonicalized. `Clear` is the ONLY way to reach `Ok(None)`.
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

    /// A CLEAR records the documented cleared marker (`value=-`), which is what
    /// distinguishes it from the unrepresentable `Set("")` — both would store
    /// `None`, but only one of them says so in the event stream.
    #[test]
    fn a_clear_records_the_cleared_marker_and_only_on_a_real_change() {
        let ctx = crate::stub_session(0).ctx.clone();
        assert!(apply_meta_value(
            &ctx,
            MetaField::Title,
            Some("agent".into())
        ));
        assert!(
            !apply_meta_value(&ctx, MetaField::Title, Some("agent".into())),
            "a no-op re-set reports unchanged so the caller stays silent"
        );
        assert!(apply_meta_value(&ctx, MetaField::Title, None));
        assert!(
            !apply_meta_value(&ctx, MetaField::Title, None),
            "clearing an unset field is a no-op"
        );
        let tl = ctx.timeline.lock().unwrap();
        let payloads: Vec<&str> = tl
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.as_str())
            .collect();
        assert_eq!(
            payloads,
            vec!["field=title value=agent", "field=title value=-"],
            "exactly one record per REAL change, and a clear is `-`"
        );
    }

    /// Every write the door accepts marks its field as the DRIVER's — a no-op
    /// re-set and a clear of an unset field included, since both answered `OK`
    /// — while a restore seed through [`super::SessionMeta::set`] marks nothing,
    /// and the marks never enter equality.
    #[test]
    fn every_accepted_write_marks_its_field_and_a_seed_marks_none() {
        use super::SessionMeta;
        let ctx = crate::stub_session(0).ctx.clone();
        assert!(
            !apply_meta_value(&ctx, MetaField::Attention, None),
            "clearing an unset field moves nothing"
        );
        assert!(apply_meta_value(&ctx, MetaField::Role, Some("lead".into())));
        assert!(!apply_meta_value(
            &ctx,
            MetaField::Role,
            Some("lead".into())
        ));
        let meta = ctx.meta.lock().unwrap().clone();
        let wrote = MetaField::ALL.map(|field| meta.driver_wrote(field));
        assert_eq!(
            wrote,
            [false, false, false, true, true, false],
            "title, description, icon, role, attention, questions"
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

/// Proofs for the `questions` field (2026-09-25): a CLOSED set of two policy
/// words, case-folded on the way in,
/// refused as a whole otherwise, and carried like every other USER field.
#[cfg(test)]
mod questions_field_tests {
    use super::{
        META_QUESTIONS_MAX, MetaEdit, MetaField, MetaWriteError, QUESTIONS_POLICY_WORDS,
        QUESTIONS_USAGE, SessionMeta, normalize_questions_value, restore_carried_meta,
        sanitize_metadata_value, validated_meta_value,
    };

    /// Every spelling the contract admits, and the one it stores; the usage
    /// line spells exactly the words, and the cap holds the longest.
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
        ] {
            assert_eq!(normalize_questions_value(typed), Some(stored), "{typed:?}");
        }
        assert_eq!(
            QUESTIONS_USAGE,
            format!(
                "usage: meta set questions {}",
                QUESTIONS_POLICY_WORDS.join("|")
            ),
            "the usage line spells the closed set"
        );
    }

    /// Anything else is refused whole — never trimmed into a different value.
    /// No controller form: a session principal is not a policy word, and the
    /// retired free-text leanings are no longer words.
    #[test]
    fn anything_outside_the_words_is_refused() {
        for bad in [
            "",
            "long-term",
            "short-term",
            "recommend",
            "long term",
            "longterm",
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

    /// Restore and handoff carry it: a sanitized copy keeps it, and the
    /// carried identity puts it back on the live session like any field,
    /// announced as the ordinary `meta-change`.
    #[test]
    fn a_carried_identity_restores_the_policy() {
        let ctx = crate::stub_session(0).ctx.clone();
        let mut live = SessionMeta::default();
        let _ = live.set("questions", Some("recommended".into()));
        let carried = live.sanitized();
        assert_eq!(carried.questions.as_deref(), Some("recommended"));
        assert!(restore_carried_meta(&ctx, &carried));
        assert_eq!(
            ctx.meta.lock().unwrap().questions.as_deref(),
            Some("recommended")
        );
        let events: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.clone())
            .collect();
        assert_eq!(
            events,
            vec!["field=questions value=recommended".to_string()]
        );
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
            .map(|event| event.payload.clone())
            .collect()
    }

    /// A field a driver wrote here keeps the driver's value, set or cleared;
    /// every other field takes the carried value exactly; and each field that
    /// moves is recorded as the ordinary `meta-change`, naming the value it
    /// now stores, so a watcher that saw the session bare sees it arrive.
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
            Some("worker:new".into())
        ));
        assert!(apply_meta_value(
            &ctx,
            MetaField::Attention,
            Some("fresh escalation".into())
        ));
        assert!(!apply_meta_value(&ctx, MetaField::Description, None));
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

/// Concurrency proof for [`apply_meta_value`]: the meta store and the timeline
/// record are ATOMIC with respect to racing `meta set`s (the record is taken
/// while the meta guard is held), so the event stream can never invert against
/// the finally-stored value. The control socket runs a pool of worker threads;
/// before the fix, two writers could interleave store(A) store(B) record(B)
/// record(A) — every `subscribe … events` watcher and the `timeline` verb then
/// ended on an event naming A while the store, the bare `meta` readout, and
/// the tab label all showed B.
#[cfg(test)]
mod meta_atomicity_tests {
    use super::{MetaField, apply_meta_value};

    /// Hammer one session's title from several threads, then require the LAST
    /// recorded `meta-change` event to name exactly the value the store ended
    /// on. Deterministically true with the guard-held record; reliably flaky
    /// without it (the race window was the whole guard-drop → record gap).
    #[test]
    fn racing_meta_sets_keep_the_last_event_matching_final_state() {
        let ctx = crate::stub_session(0).ctx.clone();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    for j in 0..50 {
                        apply_meta_value(&ctx, MetaField::Title, Some(format!("t{i}-{j}")));
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
            last.payload,
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
            .map(|event| event.payload.clone())
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
            write_attention_owned(&ctx, owner, MetaEdit::Set(text)).expect("accepted")
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
        assert!(write_attention_owned(&ctx, "human", MetaEdit::Clear).unwrap());
        assert_eq!(
            effective(&ctx).as_deref(),
            Some("claude needs approval: rm -rf tmp")
        );
        // Clearing an owner that holds nothing changes nothing.
        assert!(!write_attention_owned(&ctx, "human", MetaEdit::Clear).unwrap());
        // Re-setting the shown text is a no-op, not an event.
        assert!(!set("sup", "claude needs approval: rm -rf tmp"));
        assert!(write_attention_owned(&ctx, "sup", MetaEdit::Clear).unwrap());
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

        // NEGATIVE CONTROL: clearing a NON-shown owner moves nothing readers
        // see, so it records nothing — the event log above is not just "every
        // write".
        set("a", "first");
        set("b", "second");
        let before = meta_events(&ctx).len();
        assert!(!write_attention_owned(&ctx, "a", MetaEdit::Clear).unwrap());
        assert_eq!(effective(&ctx).as_deref(), Some("second"));
        assert_eq!(meta_events(&ctx).len(), before);
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
            Some("legacy badge".to_string())
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
        write_attention_owned(&ctx, "sup", MetaEdit::Set("keyed")).unwrap();
        // A bare clear leaves the keyed owner's escalation up.
        apply_meta_value(&ctx, MetaField::Attention, None);
        assert_eq!(effective(&ctx).as_deref(), Some("keyed"));
        // With no keyed owner, the bare set/clear round-trips exactly as before.
        write_attention_owned(&ctx, "sup", MetaEdit::Clear).unwrap();
        assert_eq!(effective(&ctx), None);
        assert!(apply_meta_value(
            &ctx,
            MetaField::Attention,
            Some("x".into())
        ));
        assert!(apply_meta_value(&ctx, MetaField::Attention, None));
        assert_eq!(effective(&ctx), None);
    }

    /// Bounded: a ninth owner is refused without touching anyone, the bare
    /// owner always has its slot, and a keyed text is capped at 200 bytes
    /// while the bare field keeps its 256.
    #[test]
    fn the_map_is_bounded_and_the_keyed_text_capped() {
        let ctx = crate::stub_session(0).ctx.clone();
        for i in 0..ATTENTION_OWNERS_MAX - 1 {
            write_attention_owned(&ctx, &format!("o{i}"), MetaEdit::Set("x")).unwrap();
        }
        // The bare owner is the eighth.
        assert!(apply_meta_value(
            &ctx,
            MetaField::Attention,
            Some("bare".into())
        ));
        assert_eq!(
            write_attention_owned(&ctx, "one-too-many", MetaEdit::Set("y")),
            Err(AttentionWriteError::OwnersFull)
        );
        assert_eq!(effective(&ctx).as_deref(), Some("bare"), "nothing moved");
        // An EXISTING owner still updates at capacity.
        assert!(write_attention_owned(&ctx, "o0", MetaEdit::Set("updated")).unwrap());
        // …and a slot freed is a slot a new owner can take.
        write_attention_owned(&ctx, "o1", MetaEdit::Clear).unwrap();
        write_attention_owned(&ctx, "one-too-many", MetaEdit::Set("y")).unwrap();

        let long = "a".repeat(META_ATTENTION_KEYED_MAX + 1);
        assert_eq!(
            write_attention_owned(&ctx, "o0", MetaEdit::Set(&long)),
            Err(AttentionWriteError::Meta(MetaWriteError::TooLong {
                cap: META_ATTENTION_KEYED_MAX
            }))
        );
        write_attention_owned(&ctx, BARE_ATTENTION_OWNER, MetaEdit::Set(&long))
            .expect("the bare owner keeps the bare field's 256-byte cap");
    }

    /// A restore carries the BARE owner's entry only; a meta seeded as a
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
    /// travel — as the bare owner's entry, as a literal seed, or under a keyed
    /// entry that the carry leaves behind anyway. NEGATIVE CONTROL: a bare
    /// attention without the prefix travels.
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

    /// A supervisor claim: shown while live, refused to a second holder,
    /// renewed by its own, a `ttl=` claim lapses, and a connection-bound
    /// release clears only the claim bound to THAT connection.
    /// `meta unset supervisor holder=<h>` clears the claim only while `<h>`
    /// holds it: a supervisor whose connection was lost, and whose session
    /// another holder then claimed, never clears the other's claim by giving
    /// back its own. NEGATIVE CONTROL: the holder's own release clears it.
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
        assert!(!release_supervisor_held_by(&ctx, "sup-a", 100));
        assert_eq!(shown().as_deref(), Some("sup-b"));
        assert!(release_supervisor_held_by(&ctx, "sup-b", 100));
        assert_eq!(shown(), None);
        assert!(
            !release_supervisor_held_by(&ctx, "sup-b", 100),
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
        assert!(!release_supervisor(&ctx, Some(7), 100));
        assert_eq!(shown(100).as_deref(), Some("sup-a"));
        assert!(release_supervisor(&ctx, Some(9), 100));
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
    //! The server's publication of an input stall (2026-09-24): the effective
    //! agent word and the server's own attention entry.
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
            bytes: 1,
            stopped,
            rss_mb: Some(39_731),
            restart: crate::input_stall::Restart::default(),
        }
    }

    fn agent_changes(tl: &SessionTimeline) -> Vec<String> {
        tl.since(None)
            .filter(|event| event.kind == "agent-change")
            .map(|event| event.payload.clone())
            .collect()
    }

    fn publish(tl: &mut SessionTimeline, word: &'static str, claude: bool) -> bool {
        let reader = claude.then_some(aterm_phase::Program::Claude);
        tl.publish_agent(word, None, None, reader, stamp())
    }

    /// THE INCIDENT'S WORD. A Claude Code session whose screen reads `prompt`
    /// (the approval box it froze under) turns `wall:unresponsive` when a
    /// stall is published — one rev, one `agent-change` — and stays there
    /// while its screen verdict moves underneath (no rev: the word did not
    /// move). The clear hands the word back to the screen's CURRENT verdict,
    /// with its own rev.
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

    /// NEGATIVE CONTROL: a session that is no identified agent keeps
    /// `agent=-` — its stall is the server's attention entry, not an agent
    /// word — and it stays `-` when the stall clears. The stall still counts
    /// once the session is read as an agent.
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

    /// The SERVER'S entry outranks an OLDER supervisor badge while the stall
    /// stands — the incident's "answer this box" was the one line that was
    /// wrong — and its clear uncovers the supervisor's entry again, untouched.
    /// Each move of the effective line is one `meta-change`, and the server's
    /// write marks nothing driver-written.
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

        // The incident's order: the supervisor's badge first, the stall later.
        write_attention_owned(
            &ctx,
            "supervisor",
            MetaEdit::Set("claude other: answer this box"),
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
            .map(|event| event.payload.clone())
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
