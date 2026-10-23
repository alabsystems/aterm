// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session timeline, metadata, attention state, and lease claim tracking.
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
/// Byte cap for `meta set role` (after trim).
pub(crate) const META_ROLE_MAX: usize = 64;
/// Byte cap for `meta set attention` (after trim): a one-line needs-human message.
pub(crate) const META_ATTENTION_MAX: usize = 256;
/// Byte cap for one keyed attention entry (`meta set attention owner=<k>`), after trim.
pub(crate) const META_ATTENTION_KEYED_MAX: usize =
    aterm_types::control_verbs::META_ATTENTION_KEYED_MAX;
/// How many attention owners one session holds at once, the bare `-` owner included (its slot is always kept).
pub(crate) const ATTENTION_OWNERS_MAX: usize = 8;
/// Byte cap for an attention owner key or a supervisor holder name.
pub(crate) const META_OWNER_MAX: usize = 64;
/// The owner a bare `meta set attention <text>` writes as.
pub(crate) const BARE_ATTENTION_OWNER: &str = "-";
/// Byte cap for `meta set questions` (the stored, normalized value).
pub(crate) const META_QUESTIONS_MAX: usize = 16;

/// The policy words `meta set questions` accepts.
pub(crate) const QUESTIONS_POLICY_WORDS: [&str;
    1 + aterm_agent::supervise::QuestionPriority::WORDS.len()] = {
    let w = aterm_agent::supervise::QuestionPriority::WORDS;
    ["ask", w[0], w[1], w[2]]
};

/// The one usage line every refusal of a `meta set questions` value prints, built from [`QUESTIONS_POLICY_WORDS`].
pub(crate) fn questions_usage() -> String {
    format!(
        "usage: meta set questions {}",
        QUESTIONS_POLICY_WORDS.join("|")
    )
}

/// Normalize a `questions` value to its policy word, or `None` when it is not one.
#[must_use]
pub(crate) fn normalize_questions_value(value: &str) -> Option<&'static str> {
    let value = value.trim();
    QUESTIONS_POLICY_WORDS
        .into_iter()
        .find(|word| word.eq_ignore_ascii_case(value))
}

/// Byte cap for `meta set autopilot` (the stored word).
pub(crate) const META_AUTOPILOT_MAX: usize = 16;

/// Usage message returned when `meta set autopilot` receives an invalid value.
pub(crate) const AUTOPILOT_USAGE: &str =
    "usage: meta set autopilot cruise [until=HH:MM|+DURATION]|land|off";

/// Normalize an `autopilot` value to its stored word, or `None` when it names no mode.
#[must_use]
pub(crate) fn normalize_autopilot_value(value: &str) -> Option<&'static str> {
    aterm_agent::supervise::Autopilot::parse(value).map(aterm_agent::supervise::Autopilot::word)
}

/// True for characters that must never reach chrome from user metadata.
pub(crate) fn is_forbidden_metadata_char(ch: char) -> bool {
    aterm_messages::text::is_hidden_char(ch)
}

#[must_use]
pub(crate) fn metadata_has_forbidden_formatting(value: &str) -> bool {
    value.chars().any(is_forbidden_metadata_char)
}

/// Canonical single-line presentation sanitizer shared by restored metadata and every user-metadata chrome boundary.
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

/// Sanitize one named user metadata field for presentation and persistence.
#[must_use]
pub(crate) fn sanitize_metadata_value(field: &str, value: &str) -> Option<String> {
    let cap = SessionMeta::cap(field)?;
    if field == MetaField::Questions.wire_name() {
        return normalize_questions_value(value).map(str::to_owned);
    }
    if field == MetaField::Autopilot.wire_name() {
        return normalize_autopilot_value(value).map(str::to_owned);
    }
    let value = sanitize_presentation_line(value, cap);
    (!value.is_empty()).then_some(value)
}

/// The bare-attention prefix the retired aterm-link Claude Code hooks wrote (`claude needs approval: <message>`).
pub(crate) const RETIRED_HOOK_BADGE: &str = "claude needs approval:";

/// Sanitize an attention value carried across seamless update handoff.
#[must_use]
pub(crate) fn carried_attention(value: &str) -> Option<String> {
    sanitize_metadata_value("attention", value)
        .filter(|value| !value.starts_with(RETIRED_HOOK_BADGE))
}

/// How many timeline events a session retains (drop-oldest past this), sized like the turn ledger.
pub(crate) const TIMELINE_CAP: usize = 512;

const MAX_PAYLOAD: usize = 256;

/// The user-settable per-session metadata (`meta set`/`meta unset`).
#[derive(Default, Clone, Debug)]
pub(crate) struct SessionMeta {
    pub user_title: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub role: Option<String>,
    pub attention: Option<String>,
    pub(crate) attention_owners: AttentionOwners,
    pub(crate) supervisor: Option<SupervisorClaim>,
    pub(crate) supervisor_revision: u64,
    pub questions: Option<String>,
    pub autopilot: Option<String>,
    pub(crate) land_at: Option<i64>,
    pub(crate) autopilot_set_by: Option<AutopilotSetBy>,
    pub(crate) autopilot_revision: u64,
    pub(crate) driver_writes: u8,
}

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
    /// Whether any field is set.
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

    /// The session's Autopilot word, typed ([`Self::autopilot`]).
    #[must_use]
    pub(crate) fn autopilot_word(&self) -> Option<aterm_agent::supervise::Autopilot> {
        use aterm_agent::supervise::Autopilot;
        let stored = self.autopilot.as_deref()?;
        Autopilot::ALL
            .into_iter()
            .find(|mode| mode.word() == stored)
            .or_else(|| Autopilot::parse(stored))
    }

    /// Whether a driver wrote `field` on this process.
    #[must_use]
    pub(crate) const fn driver_wrote(&self, field: MetaField) -> bool {
        self.driver_writes & field.bit() != 0
    }

    /// The named field's current value.
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

    /// The named field after the canonical single-line presentation policy, which also covers a value that bypassed the setter.
    #[must_use]
    pub(crate) fn presentation_value(&self, field: &str) -> Option<String> {
        self.get(field)
            .and_then(|value| sanitize_metadata_value(field, value))
    }

    /// [`Self::presentation_value`] without allocating, for the render path.
    pub(crate) fn presentation_value_into(&self, field: &str, out: &mut String) -> bool {
        let Some(cap) = Self::cap(field) else {
            return false;
        };
        let Some(value) = self.get(field) else {
            return false;
        };
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
        match sanitize_metadata_value(field, value) {
            Some(sanitized) => {
                out.clear();
                out.push_str(&sanitized);
                true
            }
            None => false,
        }
    }

    /// A canonical bounded copy for restore/handoff persistence, values only.
    #[must_use]
    pub(crate) fn sanitized(&self) -> Self {
        let attention = if self.attention_owners.is_empty() {
            self.attention.as_deref()
        } else {
            self.attention_owners.get(BARE_ATTENTION_OWNER)
        }
        .and_then(carried_attention);
        let (autopilot, land_at) = self.carried_autopilot();
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

    /// THE AUTOPILOT PAIR AS A RESTORE LAYOUT CARRIES IT.
    #[must_use]
    pub(crate) fn carried_autopilot(&self) -> (Option<String>, Option<i64>) {
        (
            self.presentation_value("autopilot"),
            self.land_at.filter(|_| {
                self.autopilot_word() == Some(aterm_agent::supervise::Autopilot::Cruise)
            }),
        )
    }

    /// Set (or with `None`, clear) `owner`'s attention entry and re-project [`Self::attention`].
    pub(crate) fn set_attention_owned(
        &mut self,
        owner: &str,
        value: Option<String>,
    ) -> Result<bool, AttentionOwnersFull> {
        self.set_attention_kind(owner, value, None)
            .map(|moved| moved.effective)
    }

    /// Set an owned attention entry with an explicit attention kind.
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

    /// The bare field (`meta set|unset attention` with no `owner=`).
    pub(crate) fn set_bare_attention(&mut self, value: Option<String>) -> AttentionMove {
        let value = value.and_then(|value| sanitize_metadata_value("attention", &value));
        self.set_attention_kind(BARE_ATTENTION_OWNER, value, None)
            .unwrap_or_default()
    }

    fn adopt_literal_attention(&mut self) {
        if self.attention_owners.is_empty()
            && let Some(value) = self.attention.clone()
        {
            let _ = self
                .attention_owners
                .put(BARE_ATTENTION_OWNER, Some(value), None);
        }
    }

    /// The supervisor's holder name while its claim is live at `now_us` ([`crate::metrics::now_us`]).
    #[must_use]
    pub(crate) fn live_supervisor(&self, now_us: u64) -> Option<&str> {
        self.live_claim(now_us).map(|claim| claim.holder.as_str())
    }

    /// The live claim itself at `now_us` (a lapsed `ttl=` claim reads as none).
    #[must_use]
    pub(crate) fn live_claim(&self, now_us: u64) -> Option<&SupervisorClaim> {
        self.supervisor
            .as_ref()
            .filter(|claim| claim.expires_us.is_none_or(|at| at > now_us))
    }

    fn set_supervisor(&mut self, claim: Option<SupervisorClaim>) -> u64 {
        self.supervisor = claim;
        self.supervisor_revision = self.supervisor_revision.wrapping_add(1);
        self.supervisor_revision
    }

    /// Whether the live claim at `now_us` is a proven self-claim ([`crate::self_claim`]).
    #[must_use]
    pub(crate) fn supervisor_proven(&self, now_us: u64) -> bool {
        self.live_claim(now_us)
            .is_some_and(|claim| claim.proven.is_some())
    }

    /// When the `ttl=` claim lapses ([`crate::metrics::now_us`]), live or not.
    #[must_use]
    pub(crate) fn supervisor_expiry(&self) -> Option<u64> {
        self.supervisor.as_ref().and_then(|claim| claim.expires_us)
    }

    /// The claim a seamless update carries at `now_us`, as `` for the handoff record.
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
        if claim.proven.is_some() {
            return (None, false);
        }
        match claim.expires_us {
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

    /// The keyed escalations a seamless update carries.
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) fn handoff_attention_owners(&self) -> Vec<String> {
        self.attention_owners
            .in_write_order()
            .into_iter()
            .filter(|(owner, _, _)| *owner != crate::input_stall::SERVER_ATTENTION_OWNER)
            .map(|(owner, text, kind)| {
                let text = crate::control::pct_encode(text);
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

    /// Set (or with `None`, unset) a named field.
    pub(crate) fn set(&mut self, field: &str, value: Option<String>) -> Option<bool> {
        if field == "attention" {
            // untouched.
            return Some(self.set_bare_attention(value).effective);
        }
        if field == "autopilot" {
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
        let value = value.and_then(|value| sanitize_metadata_value(field, &value));
        let changed = *slot != value;
        *slot = value;
        Some(changed)
    }
}

impl SessionMeta {
    /// THE ONE WRITER OF THE AUTOPILOT PAIR (§9.3.6, item 1).
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

    /// Store `set_by` as the Autopilot pair's setter ([`Self::autopilot_set_by`]) and step [`Self::autopilot_revision`].
    pub(crate) fn note_autopilot_set_by(&mut self, set_by: AutopilotSetBy) {
        self.autopilot_set_by = Some(set_by);
        self.autopilot_revision = self.autopilot_revision.wrapping_add(1);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AttentionEntry {
    owner: String,
    text: String,
    kind: Option<aterm_types::needs::AttentionKind>,
    stamp: u64,
}

/// A new owner found the map at [`ATTENTION_OWNERS_MAX`]: nothing was stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AttentionOwnersFull;

/// What one attention write moved ([`SessionMeta::set_attention_kind`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AttentionMove {
    pub(crate) effective: bool,
    pub(crate) entry: bool,
}

impl AttentionMove {
    /// Whether anything a reader of the entries reads moved.
    #[must_use]
    pub(crate) const fn any(self) -> bool {
        self.effective || self.entry
    }
}

/// The keyed attention map ([`SessionMeta::attention_owners`]).
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

    /// The most recently written entry's text.
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

    /// Every entry as the need classifier reads it, most recent first.
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

    /// `meta`'s `attention_kinds=`.
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

    #[cfg(any(unix, test))]
    fn in_write_order(&self) -> Vec<(&str, &str, Option<aterm_types::needs::AttentionKind>)> {
        let mut entries: Vec<&AttentionEntry> = self.entries.iter().collect();
        entries.sort_by_key(|entry| entry.stamp);
        entries
            .into_iter()
            .map(|entry| (entry.owner.as_str(), entry.text.as_str(), entry.kind))
            .collect()
    }

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

/// A live supervisor's claim on a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorClaim {
    pub(crate) holder: String,
    pub(crate) conn: Option<u64>,
    pub(crate) expires_us: Option<u64>,
    pub(crate) proven: Option<crate::self_claim::Proven>,
}

/// Whether `token` is a well-formed attention owner key or supervisor holder name.
#[must_use]
pub(crate) fn valid_owner_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= META_OWNER_MAX
        && token.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'/' | b'-')
        })
}

/// The user-metadata fields as a closed type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaField {
    Title,
    Description,
    Icon,
    Role,
    Attention,
    Questions,
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

    /// The field list an unknown-field refusal names (`meta set|unset <field>`), in [`Self::ALL`]'s order.
    pub(crate) const NAMES: &'static str =
        "title|description|icon|role|attention|questions|autopilot";

    const fn bit(self) -> u8 {
        1 << self as u8
    }

    /// Parse a wire field token, or `None` for an unknown name.
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

    /// The stable token [`SessionMeta::set`]/[`SessionMeta::get`] key on and the `meta-change` payload prints.
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

/// What a metadata write intends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaEdit<'a> {
    Set(&'a str),
    Clear,
}

/// Why a metadata write was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaWriteError {
    Empty,
    ForbiddenFormatting,
    TooLong { cap: usize },
    NotAQuestionsPolicy,
    NotAnAutopilot,
}

/// Pure validation.
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

/// Apply one validated metadata mutation and, on an actual change, record the `meta-change` timeline event (`field=<f> value=<pct|->`).
pub(crate) fn apply_meta_value(
    ctx: &crate::SessionCtx,
    field: MetaField,
    value: Option<String>,
    by: aterm_slog::Actor,
) -> bool {
    if field == MetaField::Autopilot {
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
    pub(crate) moved: bool,
    pub(crate) word_moved: bool,
    pub(crate) revision: u64,
}

/// WHO SET A SESSION'S AUTOPILOT PAIR, AND WHEN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AutopilotSetBy {
    pub(crate) actor: aterm_slog::Actor,
    pub(crate) surface: Option<crate::autopilot::Surface>,
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

    /// The setter's word, as `meta`'s `autopilot_by=` and the restore leaf write it (§11.7.2).
    #[must_use]
    pub(crate) fn word(&self) -> String {
        match (self.actor, self.surface) {
            (aterm_slog::Actor::Human, Some(surface)) => format!("human:{}", surface.word()),
            (actor, _) => actor.word().to_string(),
        }
    }

    /// As the restore leaf carries it (§11.7.3, item 5).
    #[must_use]
    pub(crate) fn carried(&self) -> String {
        format!("{} {}", self.word(), self.since_ms)
    }

    /// A leaf's `autopilot_by` read back ([`Self::carried`]'s form).
    #[must_use]
    pub(crate) fn parse_carried(text: &str) -> Option<Self> {
        let (setter, since) = text.trim().split_once(' ')?;
        let since_ms = since.trim().parse::<i64>().ok()?;
        let (actor, surface) = match setter.strip_prefix("human:") {
            Some(surface) => (
                aterm_slog::Actor::Human,
                Some(crate::autopilot::Surface::from_word(surface)?),
            ),
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

/// Write session `ctx`'s AUTOPILOT PAIR (§9.3.6, item 1).
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
    Landed { revision: u64 },
    NotYet(i64),
    Moved(Option<i64>),
}

/// THE FIRE IS A COMPARE-AND-SET (§9.3.6, item 5).
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

/// THE CANCEL IS A COMPARE-AND-SET TOO (§9.3.3, §9.3.4: "Cancel landing" is offered only while a timer is armed).
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
        meta.note_autopilot_set_by(AutopilotSetBy::now(aterm_slog::Actor::Human, Some(surface)));
        record_autopilot(ctx, &meta, aterm_slog::Actor::Human);
    }
    AutopilotWrite {
        moved,
        word_moved: false,
        revision: meta.autopilot_revision,
    }
}

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
    Meta(MetaWriteError),
    OwnersFull,
}

/// Pure validation for one owner's attention text.
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

/// `meta set|unset attention owner=<k> …`.
pub(crate) fn write_attention_owned(
    ctx: &crate::SessionCtx,
    owner: &str,
    edit: MetaEdit<'_>,
    by: aterm_slog::Actor,
) -> Result<bool, AttentionWriteError> {
    write_attention_kind(ctx, owner, edit, None, by)
}

/// [`write_attention_owned`] with the entry's kind (`kind=`, `None` when the writer named none).
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
        log_owed = timeline.slog_take_wake();
    }
    drop(meta);
    Ok(moved.any() || task_moved || log_owed)
}

/// THE SUPERVISOR'S ATTENTION ENTRY WRITTEN INSIDE A TASK MARK (`task_record::mark_session_task_with`).
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
        let change = MetaChange::written_by(
            MetaField::Attention.wire_name(),
            meta.attention.as_deref(),
            aterm_slog::Actor::Aterm,
        );
        timeline.record_meta_change(payload, change, meta);
    }
    true
}

/// Store or clear the control server's own attention entry for a session.
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

/// [`claim_supervisor_as`] without a proof, for tests.
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
    pub(crate) changed: bool,
    pub(crate) replaced: Option<String>,
    pub(crate) revision: u64,
}

/// How a claim request meets the claim standing ([`claim_rule`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaimRule {
    Take,
    Renew,
    Replace,
    Busy,
}

/// The claim rule (pure).
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

/// `meta set supervisor <holder> [ttl=<ms>]`.
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
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .forget_holder_acts();
    }
    if crate::holder_image::DEV_ROWS && matches!(rule, ClaimRule::Take | ClaimRule::Replace) {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record_row(
                "holder-claim",
                format!(
                    "holder={} {}",
                    crate::control::pct_encode(holder),
                    crate::holder_image::dev_tail()
                ),
            );
    }
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

/// The `if-supervisor=<holder>` fence, read under the meta lock.
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

/// The session's fenced writes in flight.
#[derive(Debug, Default)]
pub(crate) struct FencedWrites {
    inflight: std::sync::atomic::AtomicU32,
    settled: std::sync::Condvar,
}

/// One admitted fenced write, counted in [`FencedWrites`] from the fence's read until it drops.
pub(crate) struct FencedWrite<'a> {
    ctx: &'a crate::SessionCtx,
}

impl<'a> FencedWrite<'a> {
    /// Admit one write under `holder`'s fence.
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
        let meta = self.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        writes.settled.notify_all();
        drop(meta);
    }
}

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

/// Release a proven claim when its holder process has exited or left the foreground.
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
    release_supervisor_if(ctx, now_us, aterm_slog::Actor::Aterm, |claim| {
        claim.proven.as_ref() == Some(&proven)
    })
}

/// Clear the session's supervisor claim.
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

/// `meta unset supervisor holder=<h>`.
pub(crate) fn release_supervisor_held_by(
    ctx: &crate::SessionCtx,
    holder: &str,
    now_us: u64,
    by: aterm_slog::Actor,
) -> bool {
    release_supervisor_if(ctx, now_us, by, |claim| claim.holder == holder)
}

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
    if let Some(proven) = proven {
        crate::self_claim::released(ctx, &proven, revision);
    }
    crate::harness_host::note_claim_released();
    was_live
}

/// Remove a `ttl=` claim lapsed at `now_us` and record `meta-change field=supervisor value=-`, as a release does.
pub(crate) fn lapse_supervisor(ctx: &crate::SessionCtx, now_us: u64) -> bool {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if meta.supervisor_expiry().is_none_or(|at| at > now_us) {
        return false;
    }
    let proven = meta.supervisor.take().and_then(|claim| claim.proven);
    let revision = meta.set_supervisor(None);
    record_supervisor(ctx, &meta, None, aterm_slog::Actor::Aterm);
    drop(meta);
    if let Some(proven) = proven {
        crate::self_claim::released(ctx, &proven, revision);
    }
    crate::harness_host::note_claim_released();
    true
}

/// The longest `ttl=` a `meta set supervisor` accepts (ms).
pub(crate) const SUPERVISOR_TTL_MAX_MS: u64 = 600_000;

/// A carried supervisor row (`SessionRecord::supervisor`, `"<holder> <remaining_ms>"`).
#[must_use]
pub(crate) fn parse_carried_supervisor(row: &str) -> Option<(&str, u64)> {
    let (holder, ms) = row.split_once(' ')?;
    if holder == "-" || !valid_owner_token(holder) {
        return None;
    }
    let ms = ms.parse::<u64>().ok().filter(|ms| *ms > 0)?;
    Some((holder, ms.min(SUPERVISOR_TTL_MAX_MS)))
}

/// A carried attention row (`SessionRecord::attention_owners`, `"<owner> [kind=<ask|wait>] <pct-text>"`).
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

/// Put back the claim and escalations a seamless update carried onto an adopted session's fresh meta.
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
            proven: None,
        }));
    }
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

/// The `meta-change` kind.
pub(crate) const META_CHANGE: &str = "meta-change";

/// The `human` kind.
pub(crate) const HUMAN: &str = "human";

/// The `title-change` kind.
pub(crate) const TITLE_CHANGE: &str = "title-change";

/// The `cwd-change` kind.
pub(crate) const CWD_CHANGE: &str = "cwd-change";

/// The supervisor key's wire name, as its `meta-change` names it.
pub(crate) const SUPERVISOR_FIELD: &str = "supervisor";

/// One `meta-change`'s typed facts as its writer knows them, for the session log's mark ([`SessionTimeline::record_meta_change`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct MetaChange<'a> {
    pub(crate) field: &'static str,
    pub(crate) value: Option<&'a str>,
    pub(crate) by: aterm_slog::Actor,
    pub(crate) land_at: Option<i64>,
}

impl<'a> MetaChange<'a> {
    /// `field` now storing `value`, written by `by`.
    pub(crate) fn written_by(
        field: &'static str,
        value: Option<&'a str>,
        by: aterm_slog::Actor,
    ) -> Self {
        MetaChange {
            field,
            value,
            by,
            land_at: None,
        }
    }

    /// This change with the Autopilot pair's timer, as its row says it.
    pub(crate) fn with_land_at(self, land_at: Option<i64>) -> Self {
        MetaChange { land_at, ..self }
    }
}

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

fn record_autopilot(ctx: &crate::SessionCtx, meta: &SessionMeta, by: aterm_slog::Actor) {
    let change = MetaChange::written_by(
        MetaField::Autopilot.wire_name(),
        meta.autopilot.as_deref(),
        by,
    )
    .with_land_at(meta.land_at);
    record_meta(ctx, autopilot_change_payload(meta), change, meta);
}

fn record_attention(ctx: &crate::SessionCtx, meta: &SessionMeta, by: aterm_slog::Actor) {
    let payload = meta_change_payload(MetaField::Attention, meta.attention.as_deref());
    let change = MetaChange::written_by(
        MetaField::Attention.wire_name(),
        meta.attention.as_deref(),
        by,
    );
    record_meta(ctx, payload, change, meta);
}

/// THE LONGEST RE-EXEC GAP (µs, `SessionTimeline::self_reexec_since_us`).
pub(crate) const SELF_REEXEC_GAP_US: u64 = 60_000_000;

fn record_supervisor(
    ctx: &crate::SessionCtx,
    meta: &SessionMeta,
    holder: Option<&str>,
    by: aterm_slog::Actor,
) {
    let change = MetaChange::written_by(SUPERVISOR_FIELD, holder, by);
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.forget_holder_acts();
    timeline.note_supervisor_moved(holder.is_some(), crate::metrics::now_us());
    timeline.record_meta_change(supervisor_change_payload(holder), change, meta);
}

fn supervisor_change_payload(holder: Option<&str>) -> String {
    let value = holder.map_or_else(|| "-".to_string(), crate::control::pct_encode);
    format!("field=supervisor value={value}")
}

fn meta_change_payload(field: MetaField, value: Option<&str>) -> String {
    let value = value.map_or_else(|| "-".to_string(), crate::control::pct_encode);
    format!("field={} value={value}", field.wire_name())
}

/// Put a carried identity back onto a session already live here, which a reader may have seen bare and a driver may have written since.
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
            let before = meta.autopilot.clone();
            let pair_moved = meta.set_autopilot(carried.autopilot.clone(), carried.land_at);
            meta.autopilot_set_by = carried
                .autopilot_set_by
                .filter(|_| meta.autopilot.is_some());
            if !pair_moved {
                continue;
            }
            moved = true;
            autopilot_moved |= meta.autopilot != before;
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

/// Validate then apply.
pub(crate) fn write_session_meta(
    ctx: &crate::SessionCtx,
    field: MetaField,
    edit: MetaEdit<'_>,
    by: aterm_slog::Actor,
) -> Result<bool, MetaWriteError> {
    let value = validated_meta_value(field, edit)?;
    Ok(apply_meta_value(ctx, field, value, by))
}

/// What an adopted session's recorders say about the time before the seamless update that adopted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HandoffGap {
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

/// One recorded lifecycle event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TimelineEvent {
    pub id: u64,
    pub t_ms: u64,
    pub kind: &'static str,
    pub payload: Arc<str>,
}

/// The timeline kind of an act a self-supervising agent recorded.
pub(crate) const HARNESS_ACT: &str = "harness-act";

/// The timeline kind of a move of the program's own state report as the status sweep honoured it.
pub(crate) const PROGRAM_STATE: &str = "program-state";

/// How many [`PROGRAM_STATE`] rows the ring holds at once.
pub(crate) const PROGRAM_STATE_ROWS_MAX: usize = TIMELINE_CAP / 2;

/// Published foreground custody for an input decision, independent of ordinary phase or screen revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundStamp {
    generation: u64,
    pgid: i32,
    holder: Option<u64>,
    agent: aterm_phase::Program,
}

/// A session's bounded event timeline, newest-last, drop-oldest at [`TIMELINE_CAP`], with per-session monotonic ids minted here.
#[derive(Default)]
pub(crate) struct SessionTimeline {
    events: VecDeque<TimelineEvent>,
    next_id: u64,
    agent: AgentPublication,
    foreground_generation: u64,
    claude_footer: Option<(i32, Option<u64>, aterm_agent::harness::footer::FooterFacts)>,
    muse_card_account: Option<String>,
    muse_cards_need_account: bool,
    claude_footer_build: Option<atpkg::vendor_direct::Version>,
    claude_footer_image: Option<u64>,
    claude_card: Option<(
        i32,
        Option<u64>,
        aterm_agent::harness::footer::FactsOwner,
        aterm_phase::LaunchCard,
    )>,
    path: Option<PathReading>,
    leader_copy: Option<(i32, &'static str)>,
    leader_no_harness: Option<i32>,
    upgrade: Option<aterm_agent::harness::upgrade_drive::Row>,
    upgrade_note: Option<aterm_agent::harness::footer::UpgradeNote>,
    resizes: crate::resize_ledger::ResizeLedger,
    #[cfg(any(unix, test))]
    handoff_ceiling: Option<u64>,
    task: Option<crate::task_record::TaskRecord>,
    worker_launch: Option<(std::time::Instant, String)>,
    claude_excerpts: Option<(i32, crate::survey::ClaudeExcerpts)>,
    claude_side: Option<(i32, std::sync::Arc<crate::claude_footer::FactSide>)>,
    claude_footer_read: Option<(i32, std::time::Instant)>,
    gauges: AgentGauges,
    screen_rung_at: Option<std::time::Instant>,
    slog: crate::session_log::SessionLog,
    /// Whether the debug cross-check ([`Self::cross_check`]) saw this session's ring and view apart: a dev instance logs only the first.
    #[cfg(all(debug_assertions, not(test)))]
    view_disagreed: std::sync::atomic::AtomicBool,
    need: crate::needs::Verdict,
    last_act: Option<String>,
    pending_self_tasks: VecDeque<aterm_agent::harness::acts::TaskFace>,
    upgrade_recorded: bool,
    self_reexec_since_us: Option<u64>,
    landing_mark: Option<LandingMark>,
    typed_turn: Option<u64>,
    task_face: Option<(aterm_agent::harness::acts::TaskFace, i64)>,
    watchers: Arc<crate::session_watch::Watchers>,
    task_face_held: Option<(aterm_agent::harness::acts::TaskFace, i64)>,
    pause: Option<crate::PausePlan>,
    harness_needs: crate::needs::ToldNeeds,
    land_held: Option<aterm_agent::harness::acts::LandHeld>,
    cwd_witness: Option<(u64, i32)>,
    holder_image: Option<std::sync::Arc<crate::holder_image::HolderImage>>,
    card_seen: Option<u64>,
    card_frames: CardFrameMemo,
    registry_drift: RegistryDriftMemo,
}

#[derive(Default)]
struct CardFrameMemo {
    decorated: Option<u64>,
    decorated_row: Option<i64>,
    bare: BareFrames,
    said: Option<(u64, crate::claude_card::Bare, Option<i64>)>,
    composed: u64,
    seen: Option<(u64, i64, u64)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BareFrames {
    token: u64,
    row: Option<i64>,
    at_row: u32,
    unplaced: u32,
}

impl BareFrames {
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

#[derive(Default)]
struct RegistryDriftMemo {
    stale: Option<(u64, Option<aterm_agent::harness::footer::FactsOwner>)>,
    said: Option<(u64, String)>,
}

/// A Claude Code process whose registry row reports a build its Store or Native drawer image does not name (card engine §3.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistryDrift {
    pub(crate) pgid: i32,
    pub(crate) image: String,
    pub(crate) source: atpkg::image::ImageSource,
    pub(crate) reported: String,
}

/// What an identified agent's screen counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentGauges {
    pub(crate) ctx_pct: Option<u8>,
    pub(crate) shells: Option<u32>,
}

/// How many ids a seamless update leaves this process between the park's draw and its `_exit` ([`SessionTimeline::handoff_id`]).
#[cfg(any(unix, test))]
pub(crate) const HANDOFF_ID_RESERVE: u64 = 1 << 16;

/// One measurement of the shell's PATH.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PathReading {
    pub(crate) verdict: crate::session_status::program::PathVerdict,
    pub(crate) at_ms: u64,
    pgid: i32,
    agreed: bool,
    reported: bool,
}

impl PathReading {
    /// Whether the reading is the shell's, not one job's.
    pub(crate) fn settled(&self) -> bool {
        self.verdict.word() == "live" || self.agreed
    }
}

/// Format `(path=, path_evidence=)` status columns from the adoption mark and live cwd.
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

/// The server-published identity and agent verdict of one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentPublication {
    pub(crate) program: Option<String>,
    pub(crate) reader: Option<aterm_phase::Program>,
    pub(crate) program_pgid: i32,
    pub(crate) word: &'static str,
    pub(crate) detail: Option<String>,
    pub(crate) rev: u64,
    pub(crate) verdict_moves: u64,
    pub(crate) subject: Option<String>,
    pub(crate) changed_ms: u64,
    pub(crate) changed_continuous_ms: Option<u64>,
    pub(crate) stamp: Option<AgentStamp>,
    pub(crate) screen_word: &'static str,
    pub(crate) screen_detail: Option<String>,
    pub(crate) input: Option<crate::input_stall::InputStallFact>,
    pub(crate) text_moves: u64,
    pub(crate) grid: Option<GridRead>,
    pub(crate) screen_moves: u64,
    pub(crate) busy_entries: u64,
}

/// WHAT A PROVEN SELF-CLAIM'S LANDING WAS RECORDED OVER (Autopilot Land).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LandingMark {
    pub(crate) program_turns: u64,
    pub(crate) busy_entries: u64,
}

/// How long after the first read that finds a session's grid resized a text change is the re-grid's.
pub(crate) const REGRID_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

/// THE GRID AT A READ OF THE SCREEN ([`SessionTimeline::note_grid`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GridRead {
    pub(crate) resize: (u64, u64),
    pub(crate) since: Option<std::time::Instant>,
    pub(crate) at: std::time::Instant,
    pub(crate) before: Option<(&'static str, Option<String>)>,
}

impl GridRead {
    /// Whether a text change at this read is the re-grid's.
    #[must_use]
    pub(crate) fn settling(&self) -> bool {
        self.since
            .is_some_and(|since| self.at.saturating_duration_since(since) < REGRID_SETTLE)
    }
}

/// Which screen an agent verdict was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AgentStamp {
    pub(crate) generation: crate::control::ScreenGen,
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

    /// [`Self::since_ms`] counted THROUGH SLEEP, as the survey reports it.
    pub(crate) fn age_ms(&self) -> u64 {
        match (
            self.changed_continuous_ms,
            aterm_uds::process::continuous_ms(),
        ) {
            (Some(at), Some(now)) => now.saturating_sub(at),
            _ => self.since_ms(),
        }
    }

    fn effective(&self) -> (&'static str, Option<String>) {
        match &self.input {
            Some(stall) if self.reader.is_some() => (
                "wall:unresponsive",
                stall.stopped.then(|| "stopped".to_string()),
            ),
            _ => (self.screen_word, self.screen_detail.clone()),
        }
    }

    /// The seven wire fields, space-separated, in their fixed order.
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

    fn stamp_fields(&self) -> (String, String) {
        self.stamp.map_or_else(
            || ("-".to_string(), "-".to_string()),
            |s| (s.generation.to_string(), format!("{:016x}", s.fp)),
        )
    }
}

/// How long a person's hands must be off a session before their next input opens a new burst (one `EVENT <local> human` push, one `human` row).
pub(crate) const HUMAN_BURST_GAP_MS: u64 = 30_000;

impl SessionTimeline {
    /// An empty timeline that rings `watchers` on every record.
    pub(crate) fn watched_by(watchers: Arc<crate::session_watch::Watchers>) -> Self {
        SessionTimeline {
            watchers,
            ..SessionTimeline::default()
        }
    }

    /// The list this timeline rings.
    pub(crate) fn watchers(&self) -> &Arc<crate::session_watch::Watchers> {
        &self.watchers
    }

    /// An act the session's proven self-claim's holder took (`meta set harness_act <act>`, the native harness contract §5).
    pub(crate) fn note_harness_act(
        &mut self,
        act: &str,
        holder: &str,
        by: aterm_slog::Actor,
    ) -> Option<u64> {
        self.last_act = Some(act.to_string());
        if act == aterm_agent::harness::acts::RULE_UPGRADE {
            self.upgrade_recorded = true;
        }
        let at = self.slog.now();
        self.slog.self_claim_act(act, holder, by, at)?;
        Some(self.record(HARNESS_ACT, format!("act={act} holder={holder}")))
    }

    /// The last act [`Self::note_harness_act`] kept, if any.
    pub(crate) fn last_act(&self) -> Option<&str> {
        self.last_act.as_deref()
    }

    /// Queue the face of an admitted act in the hold that commits its row.
    pub(crate) fn queue_self_task(&mut self, face: aterm_agent::harness::acts::TaskFace) {
        self.pending_self_tasks.push_back(face);
    }

    /// Take the finite batch committed before this read.
    pub(crate) fn take_self_tasks(&mut self) -> VecDeque<aterm_agent::harness::acts::TaskFace> {
        std::mem::take(&mut self.pending_self_tasks)
    }

    /// A proven self-claim's holder recorded its landing over `mark`, replacing any mark before it.
    pub(crate) fn mark_landing(&mut self, mark: LandingMark) {
        self.landing_mark = Some(mark);
    }

    /// The latest landing's mark ([`Self::mark_landing`]), `None` before one.
    pub(crate) fn landing_mark(&self) -> Option<LandingMark> {
        self.landing_mark
    }

    /// Record a turn-starting act typed by a proven self-claim holder.
    pub(crate) fn note_typed_act(&mut self, at_rest_turns: Option<u64>) {
        self.typed_turn = at_rest_turns;
    }

    /// The count [`Self::note_typed_act`] kept, taken by the landing or the point left to a person that follows it.
    pub(crate) fn take_typed_turn(&mut self) -> Option<u64> {
        self.typed_turn.take()
    }

    /// THE CLAIM MOVED at `now_us` (taken, or given back: `taken`).
    pub(crate) fn note_supervisor_moved(&mut self, taken: bool, now_us: u64) {
        self.self_reexec_since_us = (!taken && self.upgrade_recorded).then_some(now_us);
        self.upgrade_recorded = false;
    }

    /// Whether `now_us` falls in the re-exec's gap.
    pub(crate) fn in_self_reexec(&self, now_us: u64) -> bool {
        self.self_reexec_since_us
            .is_some_and(|at| now_us.saturating_sub(at) < SELF_REEXEC_GAP_US)
    }

    /// A logical claim change.
    pub(crate) fn forget_holder_acts(&mut self) {
        self.pending_self_tasks.clear();
        self.typed_turn = None;
    }

    /// A move of what the program in front says about itself.
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

    /// Append one event, evicting the oldest past the cap, and wake the session's watchers for a record. Returns its id.
    pub(crate) fn record(&mut self, kind: &'static str, payload: String) -> u64 {
        debug_assert_ne!(kind, META_CHANGE, "a meta-change is record_meta_change's");
        debug_assert!(
            !crate::session_timeline_view::ROW_ONLY.contains(&kind),
            "a {kind} row is record_row's: the log holds it as its row alone"
        );
        if kind == META_CHANGE {
            return self.next_id;
        }
        self.append(kind, payload)
    }

    /// THE ONE WRITER OF A ROW-ONLY KIND ([`crate::session_timeline_view::ROW_ONLY`]): the session log's `row` mark, then the ring's row, in this one hold. Returns the ring row's id.
    pub(crate) fn record_row(&mut self, kind: &'static str, payload: String) -> u64 {
        debug_assert!(
            crate::session_timeline_view::ROW_ONLY.contains(&kind),
            "{kind} is no row-only kind: its writer writes its own mark"
        );
        let at = self.slog.now();
        self.slog.observe_row(kind, &payload, at);
        self.append(kind, payload)
    }

    /// A row a test writes by hand, of any kind, as [`Self::record_row`] writes it: a `row` mark and the ring's row.
    #[cfg(test)]
    pub(crate) fn record_hand_row(&mut self, kind: &'static str, payload: String) -> u64 {
        let at = self.slog.now();
        self.slog.observe_row(kind, &payload, at);
        self.append(kind, payload)
    }

    /// The view past `after`, its newest `keep` rows (0: every one), copied under this lock to format after it (the `timeline` verb's).
    pub(crate) fn view_copy(
        &self,
        after: Option<u64>,
        keep: usize,
    ) -> crate::session_timeline_view::ViewCopy {
        self.cross_check();
        crate::session_timeline_view::ViewCopy::of(self.slog.log(), after, keep)
    }

    /// The view past `after` (`None`: from the first row), with the row ids it no longer holds (the `events` digest's read).
    pub(crate) fn view_since(
        &self,
        after: Option<u64>,
    ) -> crate::session_timeline_view::TimelineView {
        self.cross_check();
        crate::session_timeline_view::timeline_rows_since(self.slog.log(), after)
    }

    /// The view's head: the last row id the log passed, `None` before any (the digest's early-out and seed, the chrome's cache key).
    pub(crate) fn view_head(&self) -> Option<u64> {
        Some(self.slog.row_head()).filter(|&h| h > 0)
    }

    /// The view's rows newest first, as `(kind, process-clock ms)`: the tab chrome's tail.
    pub(crate) fn view_newest_first(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        self.cross_check();
        crate::session_timeline_view::newest_first(self.slog.log())
    }

    /// The ring held to the view at a reader's read, in a debug build ([`crate::session_timeline_view::ring_against_view`]): a test panics on a disagreement, a dev instance logs the session's first and reads on.
    #[cfg(debug_assertions)]
    fn cross_check(&self) {
        let held = crate::session_timeline_view::ring_against_view(
            self.events.iter(),
            self.slog.burst_rows_owed(),
            self.slog.log(),
            self.slog.program_pending(),
        );
        if let Err(why) = held {
            #[cfg(test)]
            panic!("THE RING AND THE VIEW DISAGREE: {why}");
            #[cfg(not(test))]
            if !self
                .view_disagreed
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                aterm_log::error!("timeline: the ring and the view disagree: {why}");
            }
        }
    }

    #[cfg(not(debug_assertions))]
    #[inline]
    fn cross_check(&self) {}

    /// THE ONE `meta-change` WRITER, under the meta → timeline hold its writer already takes (`meta` is the store as the write left it).
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

    /// A raw `meta-change` row for a test that pins the timeline's own shapes and not a writer's.
    #[cfg(test)]
    pub(crate) fn record_raw_meta_change(&mut self, payload: String) -> u64 {
        self.record_hand_row(META_CHANGE, payload)
    }

    fn append(&mut self, kind: &'static str, payload: String) -> u64 {
        self.pay_burst_rows();
        self.push_row(kind, payload)
    }

    /// THE `human` ROWS THE SESSION LOG'S BURST MARKS OWE (the review of 6F-8).
    pub(crate) fn pay_burst_rows(&mut self) {
        for _ in 0..self.slog.take_burst_rows() {
            let _ = self.push_row(HUMAN, String::new());
        }
    }

    fn push_row(&mut self, kind: &'static str, payload: String) -> u64 {
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

    /// THE `cwd-change` WRITER: a row iff the session log's `cwd` mark moved; returns whether it recorded one.
    pub(crate) fn record_cwd_change(&mut self, cwd: Option<&str>) -> bool {
        let at = self.slog.now();
        let Some(held) = self.slog.observe_cwd(cwd, at) else {
            return false;
        };
        let payload = if held.is_empty() {
            "cwd=-".to_string()
        } else {
            format!("cwd={}", crate::control::pct_encode(&held))
        };
        self.record(CWD_CHANGE, payload);
        true
    }

    /// THE `title-change` WRITER (DESIGN §8: "the C-tier title keeps the `title-change` rows").
    pub(crate) fn record_title(&mut self, shown: &str) -> bool {
        let at = self.slog.now();
        let Some(held) = self.slog.observe_title(shown, at) else {
            return false;
        };
        self.record(
            TITLE_CHANGE,
            format!("title={}", crate::control::pct_encode(&held)),
        );
        true
    }

    /// The resize ledger, for its readers (`resizes`, the `events` digest).
    pub(crate) fn resizes(&self) -> &crate::resize_ledger::ResizeLedger {
        &self.resizes
    }

    /// Book a resize path's journal copy ([`crate::resize_ledger::ResizeLedger::ingest`]) and record any verdict transition as a `render` event.
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

    /// Evaluate the resize ledger against `probe` and record any verdict transition as a `render` event.
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

    /// Whether this session's program reads as the agent class (Claude Code) now.
    pub(crate) fn resize_agent_class(&mut self) -> bool {
        self.note_resize_class();
        self.resizes.agent_class()
    }

    /// The open epoch when the repaint nudge may act on it.
    pub(crate) fn resize_nudge_view(&self) -> Option<crate::resize_epoch::NudgeView> {
        self.resizes.nudge_view()
    }

    /// Whether render epoch `id` is still the open one and at risk.
    pub(crate) fn resize_epoch_at_risk(&self, id: u64) -> bool {
        self.resizes.still_at_risk(id)
    }

    /// The repaint nudge held the PTY (or gave up) for epoch `id`: tried for good.
    pub(crate) fn note_resize_tried(&mut self, id: u64) {
        self.resizes.note_tried(id);
    }

    /// The repaint nudge's attempt for epoch `id` was superseded by a real resize.
    pub(crate) fn note_resize_superseded(&mut self, id: u64) {
        self.resizes.note_superseded(id);
    }

    /// A superseded attempt's epoch the judge found back at risk, once.
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

    /// `render= resizes= flaps=` for `status`, evaluated against `probe`.
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

    /// Only entry into and exit from `desync-risk` reach the lifecycle ring.
    pub(crate) fn record_render(
        &mut self,
        transitions: &[crate::resize_ledger::Transition],
    ) -> bool {
        for line in self.resizes.take_logs() {
            aterm_log::info!(
                "{line} program={}",
                self.agent.program.as_deref().unwrap_or("-")
            );
        }
        for transition in transitions {
            self.record("render", transition.payload());
            let at = self.slog.now();
            self.slog.observe_render(transition, at);
        }
        !transitions.is_empty()
    }

    /// Events with `id > after` (all with `None`), oldest first: a test's, holding the view to the ring.
    #[cfg(test)]
    pub(crate) fn since(
        &self,
        after: Option<u64>,
    ) -> impl DoubleEndedIterator<Item = &TimelineEvent> + ExactSizeIterator {
        let start = match after {
            Some(a) if a <= self.next_id => self.events.partition_point(|e| {
                crate::work_counts::retained_record_touched();
                e.id <= a
            }),
            Some(_) | None => 0,
        };
        self.events.range(start..)
    }

    /// The id a seamless update's manifest carries (`SessionRecord::timeline_id`).
    #[cfg(any(unix, test))]
    pub(crate) fn handoff_id(&mut self) -> u64 {
        let carried = self
            .next_id
            .max(self.slog.row_head())
            .saturating_add(HANDOFF_ID_RESERVE)
            .min(i64::MAX as u64);
        self.handoff_ceiling = Some(carried);
        carried
    }

    /// Return whether this process has allocated a session id past the handoff ceiling.
    #[cfg(any(unix, test))]
    pub(crate) fn past_handoff_ceiling(&self) -> bool {
        self.handoff_ceiling
            .is_some_and(|ceiling| self.next_id > ceiling || self.slog.row_head() > ceiling)
            || self.slog.past_handoff_ceiling()
    }

    /// The session log's handoff carry (`SessionRecord::log_ids`).
    #[cfg(any(unix, test))]
    pub(crate) fn handoff_log_ids(&mut self) -> String {
        self.slog.handoff_carry().row()
    }

    /// The session log's seq count for the handoff (`SessionRecord::log_seq`).
    #[cfg(any(unix, test))]
    pub(crate) fn handoff_log_seq(&mut self) -> u64 {
        self.slog.handoff_seq()
    }

    /// The adopted session's engine, hydrated.
    pub(crate) fn slog_fit_engine(&mut self, next_block: u64, started: u64) {
        let now = self.slog.now();
        self.slog.fit_engine(next_block, started, now);
    }

    /// THE HANDOFF WORKER'S TAKE of the session log (phase 2A).
    #[cfg(any(unix, test))]
    pub(crate) fn slog_carry_source(&mut self) -> crate::session_log::SlogCarrySource {
        self.slog.carry_source()
    }

    /// The session log ([`crate::session_log`]).
    pub(crate) fn slog(&self) -> &crate::session_log::SessionLog {
        &self.slog
    }

    /// A timeline whose session log is the test constructor's, for a unit test that reads epochs by hand.
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

    /// When the session log's turn tracker wants its settle tick (monotonic ms), if one is pending.
    pub(crate) fn slog_wake(&self) -> Option<u64> {
        self.slog.wake()
    }

    /// The event loop's settle tick.
    pub(crate) fn slog_tick(&mut self) -> Option<u64> {
        let at = self.slog.now();
        self.slog.tick(at)
    }

    /// Whether the session log's readers were owed a wake ([`crate::session_log::SessionLog::take_log_wake`]).
    pub(crate) fn slog_take_wake(&mut self) -> bool {
        let owed = self.slog.take_log_wake();
        if owed {
            self.watchers.ring_watchers();
        }
        owed
    }

    /// Count on from a seamless update.
    pub(crate) fn continue_ids(&mut self, carried: u64) {
        let carried = carried.min(i64::MAX as u64);
        self.next_id = self.next_id.max(carried);
        self.slog.continue_rows(carried);
    }

    /// The highest recorded event id, or `None` when empty.
    #[cfg(test)]
    pub(crate) fn high_id(&self) -> Option<u64> {
        self.events.back().map(|e| e.id)
    }

    /// The lowest retained event id, or `None` when empty.
    #[cfg(test)]
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

    /// What the worker's launcher did with its line, and when.
    pub(crate) fn worker_launch(&self) -> Option<&(std::time::Instant, String)> {
        self.worker_launch.as_ref()
    }

    /// Publish the session's need verdict ([`Self::need`]).
    pub(crate) fn set_need(&mut self, verdict: crate::needs::Verdict) -> bool {
        let moved = self.need != verdict;
        let at = self.slog.now();
        self.slog
            .observe_shown(crate::session_log::observed::ShownTuple::of(&verdict), at);
        self.need = verdict;
        moved
    }

    /// The published need verdict ([`crate::needs`]).
    pub(crate) fn need(&self) -> &crate::needs::Verdict {
        &self.need
    }

    /// Apply `mark` to the task record ([`crate::task_record::TaskMark::apply`]) at `now`.
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

    /// The supervisor's attention entry was raised (`true`) or cleared.
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

    /// Seed the record a seamless update carried, before the adopted session is registered ([`crate::task_record::seed_carried`]).
    pub(crate) fn seed_task(&mut self, record: crate::task_record::TaskRecord) {
        self.task = Some(record);
    }

    /// Name the agent a worker's spawn started on its task record.
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

    /// The published program for foreground group `pgid`, or `None` when the publication names another group.
    pub(crate) fn program_of(&self, pgid: i32) -> Option<&str> {
        if pgid > 0 && self.agent.program_pgid == pgid {
            self.agent.program.as_deref()
        } else {
            None
        }
    }

    /// Publish an agent verdict read from the screen `stamp` names.
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
            // Only record screen movement when the phase word itself did not change.
            crate::harness_host::ring_phase();
        }
        changed
    }

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

    /// Publish (`Some`) or clear (`None`) the input stall ([`AgentPublication::input`]) and re-settle the effective word.
    pub(crate) fn set_input(&mut self, input: Option<crate::input_stall::InputStallFact>) -> bool {
        let old_word = self.agent.word;
        self.agent.input = input;
        let changed = self.settle_agent_word(false);
        if self.agent.word != old_word {
            crate::harness_host::ring_phase();
        }
        changed
    }

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
        self.record("agent-change", payload);
        true
    }

    /// The live zone was read again and was unchanged.
    pub(crate) fn note_agent_stamp(&mut self, stamp: AgentStamp) {
        let regrid = self.regrid_settling();
        self.note_text(stamp, regrid);
        self.settle_regrid(regrid, false);
    }

    fn regrid_settling(&self) -> bool {
        self.agent.grid.as_ref().is_some_and(GridRead::settling)
    }

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

    fn note_text(&mut self, stamp: AgentStamp, regrid: bool) {
        if !regrid && self.agent.stamp.is_some_and(|last| last.fp != stamp.fp) {
            self.agent.text_moves = self.agent.text_moves.wrapping_add(1);
        }
        self.agent.stamp = Some(stamp);
    }

    /// THE GRID AT THIS READ OF THE SCREEN.
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

    /// The foreground process group moved to `pgid`.
    pub(crate) fn note_foreground_group(&mut self, pgid: i32) -> bool {
        if self.agent.program_pgid == pgid {
            return false;
        }
        self.advance_foreground_generation();
        self.agent.program_pgid = pgid;
        let had_reader = self.agent.reader.take().is_some();
        self.leader_copy = None;
        self.leader_no_harness = None;
        if self.agent.program.take().is_some() || had_reader {
            // its worker must still see the foreground group leave.
            crate::harness_host::ring();
        }
        let at = self.slog.now();
        self.slog.foreground_moved(pgid, at);
        true
    }

    /// The status sweep saw the engine's cwd report `seq` (`Terminal::reported_cwd_seq`) with group `pgid` in front.
    pub(crate) fn witness_cwd_report(&mut self, seq: Option<u64>, pgid: i32) {
        let Some(seq) = seq.filter(|_| pgid > 0) else {
            return;
        };
        if self.cwd_witness.is_none_or(|(seen, _)| seen != seq) {
            let settled = self.agent.program_pgid == pgid;
            self.cwd_witness = Some((seq, if settled { pgid } else { 0 }));
        }
    }

    /// [`Self::witness_cwd_report`]'s record.
    pub(crate) fn cwd_witness(&self) -> Option<(u64, i32)> {
        self.cwd_witness
    }

    /// The session's own process group (its shell's pid), for the session log.
    pub(crate) fn note_root_group(&mut self, pgid: i32) {
        self.slog.note_root(pgid);
    }

    /// The status sweep's session-log step, in one order under one hold of this lock.
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

    /// THE HOLDER IMAGE'S ONE WRITER.
    pub(crate) fn offer_holder_image(
        &mut self,
        image: std::sync::Arc<crate::holder_image::HolderImage>,
        kind: crate::holder_image::EdgeKind,
    ) -> crate::holder_image::Offer {
        let offer = crate::holder_image::decide(self.holder_image.as_deref(), &image);
        if offer == crate::holder_image::Offer::Published {
            self.advance_foreground_generation();
            if crate::holder_image::DEV_ROWS {
                self.record_row(
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
                self.record_row(
                    "holder-image",
                    format!("{} {}", image.payload(), crate::holder_image::dev_tail()),
                );
            }
            let pgid = image.pgid;
            let drawer = image.drawer.is_some().then_some(image.token).flatten();
            self.holder_image = Some(image);
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

    fn current_holder_image(&self) -> Option<&crate::holder_image::HolderImage> {
        self.holder_image
            .as_deref()
            .filter(|image| image.pgid > 0 && image.pgid == self.agent.program_pgid)
    }

    /// Whether the current holder image is a card drawer with a token.
    pub(crate) fn card_drawer_current(&self) -> bool {
        self.current_holder_image()
            .is_some_and(|image| image.drawer.is_some() && image.token.is_some())
    }

    /// THE CARD'S GATE (card engine §3.8), read under the splice's one timeline lock per pane.
    pub(crate) fn card_gate(&self) -> Option<crate::holder_image::CardGate> {
        let image = self.current_holder_image()?;
        image.gate(|| self.registry_build(image))
    }

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

    /// Return the published Claude launch-card image token for process group `pgid`.
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

    /// THE DRIFT (card engine §3.10).
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

    /// The `card-seen` dev row, once per snapshot token.
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
        self.record_row(
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

    /// Record dev diagnostic rows for one launch-card compose pass.
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
                    self.record_row(
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
                    let late = match self.card_frames.seen {
                        Some((t, r, after)) if t == token && r == row => {
                            u32::try_from(composed.saturating_sub(after).saturating_sub(1))
                                .unwrap_or(u32::MAX)
                        }
                        _ => 0,
                    };
                    let bare = self.card_frames.bare.of(token, row).max(late);
                    self.card_frames.bare = BareFrames::default();
                    self.record_row(
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
                    self.record_row(
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
                if token != 0
                    && self.card_frames.decorated != Some(token)
                    && ((refused && row.is_some()) || self.card_seen == Some(token))
                {
                    self.card_frames.bare.count(token, row);
                }
                if self.card_frames.said != Some((token, why, row)) {
                    self.card_frames.said = Some((token, why, row));
                    let row = row.map_or_else(|| "-".to_string(), |r| r.to_string());
                    self.record_row(
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

    /// The token of the current holder image when it is a drawer whose card has not been seen yet.
    pub(crate) fn card_unseen(&self) -> Option<u64> {
        self.holder_image
            .as_ref()
            .filter(|i| i.drawer.is_some() && i.build.version().is_some())
            .and_then(|i| i.token)
            .filter(|t| self.card_seen != Some(*t))
    }

    /// The resolver's answer for `pgid`, applied only while that group is in front (a late answer for a finished job is dropped).
    pub(crate) fn set_program(&mut self, pgid: i32, program: Option<String>) -> bool {
        if self.agent.program_pgid == pgid && self.agent.program != program {
            self.advance_foreground_generation();
            if crate::holder_image::DEV_ROWS
                && let Some(card) = program
                    .as_deref()
                    .and_then(crate::holder_image::CardProgram::of)
            {
                self.record_row(
                    "holder-named",
                    format!(
                        "program={} pgid={pgid} {}",
                        card.word(),
                        crate::holder_image::dev_tail()
                    ),
                );
            }
            self.agent.program = program;
            crate::harness_host::ring();
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

    /// The footer resolver's answer for `pgid` and the process's kernel start time (when read), applied only while that group is in front.
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
            sub.weekly = None;
            sub.weekly_reset = None;
            if switched {
                sub.plan = None;
            }
        }
        if self.claude_footer == next {
            return false;
        }
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

    /// Store the composed Claude launch card for the foreground process group.
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
        if let Some((.., card)) = self.claude_card.as_ref() {
            let at = self.slog.now();
            self.slog.observe_card(card, at);
        }
        true
    }

    /// The process the stored footer facts were read for.
    pub(crate) fn claude_footer_identity(&self) -> Option<(i32, Option<u64>)> {
        self.claude_footer
            .as_ref()
            .map(|(pgid, started, _)| (*pgid, *started))
    }

    /// A known switch or carried glass cannot attribute an anonymous Muse card to the current login.
    pub(crate) fn muse_cards_need_account(&self) -> bool {
        self.muse_cards_need_account
    }

    /// An adopted terminal may still show another account's cards, and its predecessor need not have carried an account observation.
    pub(crate) fn note_carried_glass(&mut self) {
        self.muse_cards_need_account = true;
    }

    /// Forget the footer facts.
    pub(crate) fn clear_claude_footer(&mut self) -> bool {
        self.claude_card = None;
        self.claude_excerpts = None;
        self.claude_side = None;
        self.claude_footer_read = None;
        self.claude_footer_build = None;
        self.claude_footer_image = None;
        self.claude_footer.take().is_some()
    }

    /// The footer resolver read `pgid`'s facts at `at`, kept only while that group is in front.
    pub(crate) fn note_claude_footer_read(&mut self, pgid: i32, at: std::time::Instant) {
        if self.agent.program_pgid == pgid {
            self.claude_footer_read = Some((pgid, at));
        }
    }

    /// When the facts [`Self::claude_footer`] gives were last read, while they are the group in front's.
    pub(crate) fn claude_footer_read_at(&self) -> Option<std::time::Instant> {
        self.claude_footer_read
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, at)| at)
    }

    /// The footer resolver's excerpts for `pgid`, applied only while that group is in front, as [`Self::set_claude_footer`] applies its facts.
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

    /// The survey's excerpts for the CURRENT foreground group, or `None` when the program in front is not the one they were read for.
    pub(crate) fn claude_excerpts(&self) -> Option<&crate::survey::ClaudeExcerpts> {
        self.claude_excerpts
            .as_ref()
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, excerpts)| excerpts)
    }

    /// The footer resolver's side for `pgid`, applied only while that group is in front, as [`Self::set_claude_excerpts`] applies its excerpts.
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

    /// The footer's side for the CURRENT foreground group, or `None` when the program in front is not the one it was read for.
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

    /// The footer facts for the current foreground group, or `None` when the program in front is not the one they were read for.
    pub(crate) fn claude_footer(&self) -> Option<&aterm_agent::harness::footer::FooterFacts> {
        self.claude_footer
            .as_ref()
            .filter(|(pgid, _, _)| *pgid == self.agent.program_pgid)
            .map(|(_, _, facts)| facts)
    }

    /// The launch card kept for the process whose facts are in front.
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

    /// What the footer shows for the group in front.
    pub(crate) fn claude_footer_shown(&self) -> Option<aterm_agent::harness::footer::FooterFacts> {
        self.claude_footer()
            .map(|facts| facts.filled_from(self.claude_card()))
    }

    /// The leader read's copy and PATH answers for `pgid` (`session_status::program::leader_facts`), applied only while that group is in front.
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

    /// Record whether `pgid`'s leader was started with `--no-harness` / `--no-alab-harness`
    /// (`ATERM_NO_HARNESS=1`).
    pub(crate) fn set_leader_no_harness(&mut self, pgid: i32, no_harness: bool) {
        if self.agent.program_pgid != pgid {
            return;
        }
        self.leader_no_harness = no_harness.then_some(pgid);
    }

    /// Whether the current foreground leader was started with `--no-harness` / `--no-alab-harness`.
    pub(crate) fn leader_no_harness(&self) -> bool {
        self.leader_no_harness
            .is_some_and(|pgid| pgid > 0 && pgid == self.agent.program_pgid)
    }

    /// `` of the last PATH measurement, or `None`.
    pub(crate) fn path_reading(&self) -> Option<(&'static str, u64, bool)> {
        self.path.map(|p| {
            (
                p.verdict.word(),
                now_ms().saturating_sub(p.at_ms),
                p.settled(),
            )
        })
    }

    /// Whether the shell's PATH is measured frozen and settled.
    pub(crate) fn path_settled_frozen(&self) -> bool {
        self.path
            .is_some_and(|p| p.settled() && p.verdict.word() == "frozen")
    }

    /// `true` once for a live PATH reading, then `false` until a live reading follows a frozen one.
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

    /// Replace the upgrade's note for the person here.
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

    /// The live upgrade's note for the person here, while the move waits only on them, or `None`.
    pub(crate) fn upgrade_note(&self) -> Option<&aterm_agent::harness::footer::UpgradeNote> {
        self.upgrade_note.as_ref()
    }

    /// Replace the task's face ([`Self::task_face()`]).
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

    /// The task's face as this window's host last heard it, and the wall time (unix ms) it moved there.
    pub(crate) fn task_face(&self) -> Option<(aterm_agent::harness::acts::TaskFace, i64)> {
        self.task_face
    }

    /// A SEAMLESS UPDATE'S CARRIED END.
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

    /// The wall time (unix ms) the task's face moved to `face`, where that is the face this timeline holds.
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

    /// The loop's pause after short turns as this window's host last heard it.
    pub(crate) fn pause(&self) -> Option<crate::PausePlan> {
        self.pause
    }

    /// The worker's NEEDS YOU set as its loop told it.
    pub(crate) fn set_harness_needs(
        &mut self,
        told: Option<Arc<crate::needs::HarnessNeeds>>,
        standing: Option<&str>,
    ) -> bool {
        self.harness_needs.tell(told, standing)
    }

    /// The worker's NEEDS YOU sets as its loop told this window's host, whole, each with the entry's text it was told with.
    pub(crate) fn harness_needs(&self) -> crate::needs::ToldNeeds {
        self.harness_needs.clone()
    }

    /// Replace Land's hold as the session's loop told it ([`Self::land_held()`]).
    pub(crate) fn set_land_held(
        &mut self,
        held: Option<aterm_agent::harness::acts::LandHeld>,
    ) -> bool {
        std::mem::replace(&mut self.land_held, held) != held
    }

    /// Land's hold as the session's loop last told this window's host (§12.3.3 item 3).
    pub(crate) fn land_held(&self) -> Option<aterm_agent::harness::acts::LandHeld> {
        self.land_held
    }

    /// `(path word, "path_evidence= copy= upgrade=")` for `status` and roster rows, after `supervisor=`.
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

/// Clamp a payload to [`MAX_PAYLOAD`] bytes without bisecting a UTF-8 char or a `%XX` escape.
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
        let ids: Vec<u64> = tl.since(None).map(|e| e.id).collect();
        assert_eq!(ids[0], 4, "oldest three evicted");
        assert!(ids.windows(2).all(|w| w[1] == w[0] + 1), "monotonic ids");
        let tail: Vec<u64> = tl
            .since(Some(TIMELINE_CAP as u64 + 1))
            .map(|e| e.id)
            .collect();
        assert_eq!(tail, vec![TIMELINE_CAP as u64 + 2, TIMELINE_CAP as u64 + 3]);
        let ts: Vec<u64> = tl.since(None).map(|e| e.t_ms).collect();
        assert!(ts.windows(2).all(|w| w[1] >= w[0]));
    }

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
        assert!(rows.windows(2).all(|w| w[1].id > w[0].id));
        assert_eq!(rows.last().map(|e| e.id), tl.high_id());
        let tail: Vec<&str> = rows[rows.len() - 3..].iter().map(|e| &*e.payload).collect();
        assert_eq!(
            tail,
            [
                "state=blocked kind=question",
                "state=- kind=-",
                "state=working kind=-"
            ]
        );
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
        assert_eq!(m.set("title", Some("build agent".into())), Some(false));
        assert_eq!(m.set("title", None), Some(true));
        assert_eq!(m.set("title", None), Some(false));
        assert!(!m.any_set());
        assert_eq!(m.set("colour", Some("red".into())), None);
        assert_eq!(m.get("colour"), None);
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

#[cfg(test)]
mod write_api_tests {
    use super::{MetaEdit, MetaField, MetaWriteError, apply_meta_value, validated_meta_value};

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

#[cfg(test)]
mod questions_field_tests {
    use super::{
        META_QUESTIONS_MAX, MetaEdit, MetaField, MetaWriteError, QUESTIONS_POLICY_WORDS,
        SessionMeta, normalize_questions_value, restore_carried_meta, sanitize_metadata_value,
        validated_meta_value,
    };

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

#[cfg(test)]
mod autopilot_field_tests {
    use super::{
        AUTOPILOT_USAGE, META_AUTOPILOT_MAX, MetaEdit, MetaField, MetaWriteError, SessionMeta,
        apply_meta_value, normalize_autopilot_value, restore_carried_meta, sanitize_metadata_value,
        validated_meta_value,
    };
    use aterm_agent::supervise::Autopilot;

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
        for mode in Autopilot::ALL {
            m.autopilot = Some(mode.word().into());
            assert_eq!(m.autopilot_word(), Some(mode));
        }
        m.autopilot = Some(" Wrap-Up ".into());
        assert_eq!(m.autopilot_word(), Some(Autopilot::Land));
    }

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

/// A WORKER AN OLDER BUILD STARTED KEEPS LAND (Autopilot Land).
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
        meta.note_autopilot_set_by(AutopilotSetBy::now(aterm_slog::Actor::Aterm, None));
        record_autopilot(ctx, &meta, aterm_slog::Actor::Aterm);
    }
    drop(meta);
    if moved {
        crate::harness_host::ring();
    }
    moved
}

#[cfg(test)]
mod worker_autopilot_tests {
    use super::{SessionMeta, keep_worker_autopilot, restore_carried_meta};
    use aterm_agent::supervise::Autopilot;

    fn carried(ctx: &crate::SessionCtx, state: &str) {
        let row = format!(
            "rev=3 state={state} from=s-0123456789abcdef0123 since=1789000000000 \
             result_bytes=0 attention=0"
        );
        crate::task_record::seed_carried(ctx, Some(&row));
        assert!(ctx.timeline.lock().unwrap().task().is_some(), "{row}");
    }

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

    #[test]
    fn a_driver_write_outranks_the_carried_value_and_every_move_is_announced() {
        let ctx = crate::stub_session(0).ctx.clone();
        let _ = ctx.meta.lock().unwrap().set("icon", Some("🧟".into()));
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

        let watermark = ctx.timeline.lock().unwrap().high_id();
        assert!(!restore_carried_meta(&ctx, &carried));
        assert!(meta_events_after(&ctx, watermark).is_empty());
    }
}

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
        assert_eq!(
            ctx.meta.lock().unwrap().attention_owners.get("sup"),
            Some("claude needs approval: rm -rf tmp")
        );

        assert!(
            write_attention_owned(&ctx, "human", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap()
        );
        assert_eq!(
            effective(&ctx).as_deref(),
            Some("claude needs approval: rm -rf tmp")
        );
        assert!(
            !write_attention_owned(&ctx, "human", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap()
        );
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

        set("a", "first");
        set("b", "second");
        let before = meta_events(&ctx).len();
        assert!(write_attention_owned(&ctx, "a", MetaEdit::Clear, aterm_slog::Actor::Ctl).unwrap());
        assert_eq!(effective(&ctx).as_deref(), Some("second"));
        assert_eq!(meta_events(&ctx).len(), before);
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
        apply_meta_value(&ctx, MetaField::Attention, None, aterm_slog::Actor::Ctl);
        assert_eq!(effective(&ctx).as_deref(), Some("keyed"));
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
        assert!(
            write_attention_owned(&ctx, "o0", MetaEdit::Set("updated"), aterm_slog::Actor::Ctl)
                .unwrap()
        );
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

        assert!(!publish(&mut tl, "busy", true));
        assert_eq!((tl.agent().word, tl.agent().rev), ("wall:unresponsive", 2));
        assert_eq!(tl.agent().screen_word, "busy");
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
        assert!(publish(&mut tl, "prompt", true));
        assert_eq!(tl.agent().word, "wall:unresponsive");
        assert!(tl.set_input(None));
        assert_eq!(tl.agent().word, "prompt");
    }

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
    use std::path::PathBuf;
    use std::sync::Arc;

    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};
    use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

    use super::{RegistryDrift, SessionTimeline};
    use crate::holder_image::{EdgeKind, HolderImage};

    fn build(version: &str) -> u64 {
        atpkg::vendor_direct::Version::parse(version)
            .expect("a vendor build")
            .build_id()
    }

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
