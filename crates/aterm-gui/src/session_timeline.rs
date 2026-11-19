// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session timeline, metadata, attention state, and lease claim tracking.
use std::collections::VecDeque;
use std::sync::Arc;

use aterm_grapheme::GraphemeClusters;

use crate::turn_ledger::now_ms;

/// Byte cap for `meta set title` (after trim).
pub(crate) const META_TITLE_MAX: usize = 120;
/// Byte cap for `meta set description` (after trim).
pub(crate) const META_DESCRIPTION_MAX: usize = 1024;
/// Byte cap for `meta set icon` (after trim).
pub(crate) const META_ICON_MAX: usize = 64;
/// Byte cap for `meta set role` (after trim).
pub(crate) const META_ROLE_MAX: usize = 64;
/// Byte cap for `meta set attention` (after trim).
pub(crate) const META_ATTENTION_MAX: usize = 256;
/// Byte cap for one keyed attention entry (`meta set attention owner=<k>`), after trim.
pub(crate) const META_ATTENTION_KEYED_MAX: usize =
    aterm_types::control_verbs::META_ATTENTION_KEYED_MAX;
/// Maximum attention owners one session holds at once, the bare `-` owner included.
pub(crate) const ATTENTION_OWNERS_MAX: usize = 8;
/// Byte cap for an attention owner key or a supervisor holder name.
pub(crate) const META_OWNER_MAX: usize = 64;
/// The owner a bare `meta set attention <text>` writes as.
pub(crate) const BARE_ATTENTION_OWNER: &str = "-";
/// Byte cap for `meta set questions` (the stored, normalized value).
pub(crate) const META_QUESTIONS_MAX: usize = 16;
/// The most result files a tab notes under other session IDs (`SessionTimeline::carried_results`).
/// Each name is a snapshot's (`task_launch::snapshot_sid`, at most 53 bytes), so ` carried=` stays
/// under 900 bytes, well inside the 8 KiB a stored layout row may hold
/// (`restore::MAX_DOCUMENT_URI_BYTES`), past which a restore drops the row and its record whole.
pub(crate) const CARRIED_RESULTS_MAX: usize = 16;

/// The policy words `meta set questions` accepts.
pub(crate) const QUESTIONS_POLICY_WORDS: [&str;
    1 + aterm_agent::supervise::PriorityWord::WORDS.len()] = {
    let w = aterm_agent::supervise::PriorityWord::WORDS;
    ["ask", w[0], w[1], w[2]]
};

/// The one usage line every refusal of a `meta set questions` value prints.
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
    alab_policy::config::Autopilot::parse(value).map(alab_policy::config::Autopilot::word)
}

/// True for characters that must never reach chrome from user metadata.
pub(crate) const fn is_forbidden_metadata_char(ch: char) -> bool {
    aterm_messages::text::is_hidden_char(ch)
}

#[must_use]
pub(crate) fn metadata_has_forbidden_formatting(value: &str) -> bool {
    value.chars().any(is_forbidden_metadata_char)
}

/// Canonical single-line presentation sanitizer shared by restored metadata and every
/// user-metadata chrome boundary.
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

/// The bare-attention prefix the retired aterm-link Claude Code hooks wrote
/// (`claude needs approval: <message>`).
pub(crate) const RETIRED_HOOK_BADGE: &str = "claude needs approval:";

/// Sanitize an attention value carried across seamless update handoff.
#[must_use]
pub(crate) fn carried_attention(value: &str) -> Option<String> {
    sanitize_metadata_value("attention", value)
        .filter(|value| !value.starts_with(RETIRED_HOOK_BADGE))
}

const MAX_PAYLOAD: usize = 256;

/// A finite continuity generation with an explicit, absorbing exhausted state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RelaunchEpoch(Option<u64>);

impl Default for RelaunchEpoch {
    fn default() -> Self {
        Self(Some(0))
    }
}

impl RelaunchEpoch {
    fn advance(&mut self) {
        self.0 = self.0.and_then(|epoch| epoch.checked_add(1));
    }

    fn same(self, other: Self) -> bool {
        self.0.is_some() && self == other
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) const fn for_test(epoch: u64) -> Self {
        Self(Some(epoch))
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) const fn test_value(self) -> Option<u64> {
        self.0
    }
}

/// A session's metadata mutex, ranked after the fabric's Inbox ([`crate::fabric::lock_rank`]).
pub(crate) type MetaLock = crate::fabric::lock_rank::MetaLock<SessionMeta>;
/// A held [`MetaLock`].
pub(crate) type MetaGuard<'a> = crate::fabric::lock_rank::MetaGuard<'a, SessionMeta>;

/// The user-settable per-session metadata (`meta set`/`meta unset`).
#[derive(Default, Clone, Debug)]
pub(crate) struct SessionMeta {
    pub(crate) continuation_activity: Option<(u64, crate::autopilot_activity::Activity)>,
    pub user_title: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub role: Option<String>,
    pub attention: Option<String>,
    pub(crate) attention_owners: AttentionOwners,
    pub(crate) supervisor: Option<SupervisorClaim>,
    pub(crate) supervisor_revision: u64,
    /// Every placement and release of the claim ([`Self::set_supervisor`]), a renewal in place
    /// included, which [`Self::supervisor_revision`] leaves alone. It orders the claim's off-lock
    /// record reads ([`crate::self_claim`]): one begun before the latest placement never
    /// publishes, so it cannot overwrite the copy that placement's own read took, as a renewal
    /// in place no longer moving the revision would otherwise let it.
    pub(crate) claim_placements: u64,
    /// The claimant's record stream (`stream @<sid> v=1`), kept beside the claim and outside
    /// [`SupervisorClaim`], which [`Self::set_supervisor`] rebuilds on every claim line: a renewal
    /// must not forget what was acknowledged. Keyed on the claimant's proof
    /// ([`Self::open_stream`]); never serialized.
    pub(crate) stream: Option<StreamState>,
    /// The version of the build the standing scoped claim's proof admitted, with the claimant
    /// it was read for ([`crate::harness_claim::ClaimProof`]): what `supervisor_version=`
    /// prints ([`Self::verified_version`]). Written at each scoped claim placement and dropped
    /// when the claim ends or another claimant holds it ([`Self::set_supervisor`]); never
    /// serialized, so a successor process reads it at the claimant's next claim.
    pub(crate) supervisor_verified: Option<(crate::harness_claim::Proven, String)>,
    /// The actual host runner carrying this session's task across a process replacement.
    /// A claim's release or a new claim does not end the runner's responsibility.
    pub(crate) task_exit_owner: crate::harness_host::task_exit::Slot,
    /// Exhaustion is permanent for this session; an absent epoch cannot prove continuity.
    pub(crate) builtin_relaunch_epoch: RelaunchEpoch,
    /// Authored only by the authenticated claim setter; never inferred from holder text.
    pub(crate) supervisor_builtin: bool,
    /// The connections proven claims were bound to, with each claim's proof
    /// ([`SupervisorClaim::identity`]), kept past the claim's end until the connection closes
    /// ([`Self::close_connection`]): a `kind=wait` written on one is its claimant's own.
    pub(crate) claimant_conn: Vec<(u64, crate::self_claim::Proven)>,
    /// Connections that ever bound a scoped claim, until EOF. Unlike `claimant_conn`, an
    /// ordinary claim cannot erase this typed-input restriction.
    pub(crate) scoped_input_connections: Vec<u64>,
    /// Proven scoped actors seen in this session. A later direct peer is recognized only by
    /// its fresh kernel pid AND birth, including after claim release and socket reconnect.
    pub(crate) scoped_input_actors: Vec<crate::self_claim::Proven>,
    /// The stop a proven claim told of its own typing (`meta set harness_held`), with that
    /// claim's act proof ([`SupervisorClaim::act_proof`]): its claimant's own, so it lapses where a wait written under the claim does
    /// ([`Self::set_supervisor`], [`Self::close_connection`]), and at the same proof's next claim
    /// once the claim that told it lapsed by its `ttl=`, and is read only while that claim stands
    /// ([`Self::self_held`]).
    pub(crate) claim_held: Option<(
        crate::self_claim::Proven,
        aterm_agent::harness::acts::HeldBy,
    )>,
    /// The Land hold a proven claim told of its own landing (`meta set harness_land`), on
    /// the glass with its `since` stamped once, with that claim's act proof: kept and lapsing exactly
    /// as [`Self::claim_held`] is, and read only while that claim stands ([`Self::self_land`]).
    pub(crate) claim_land: Option<(crate::self_claim::Proven, crate::harness_glass::HeldGlass)>,
    /// What a proven claim said of its own NEEDS YOU (its `owner=needs` text and its `meta set
    /// harness_needs` set) and the person's sets told of them as aterm's own loop's are
    /// ([`crate::needs::ClaimNeeds`]) but kept apart from them, with that claim's act proof: kept
    /// and lapsing exactly as [`Self::claim_held`] is, and read only while that claim stands
    /// ([`Self::self_needs`]).
    pub(crate) claim_needs: Option<(crate::self_claim::Proven, crate::needs::ClaimNeeds)>,
    /// THE FACTS A PROVEN SELF-CLAIM'S ACTS MAY CARRY (`harness fact`, gap 23): which of the
    /// session's measured facts its standing proven codex-alab claim was offered, to which of its
    /// readers, and which it was told. Session-scoped, so it lasts across claims, while each offer
    /// is bound to its own claim's proof; never serialized, so a successor process seeds it again
    /// from the session ledger.
    pub(crate) self_facts:
        aterm_agent::supervise::facts_offer::SelfClaimFacts<crate::self_claim::Proven>,
    pub questions: Option<String>,
    pub autopilot: Option<String>,
    pub(crate) land_at: Option<i64>,
    pub(crate) autopilot_set_by: Option<AutopilotSetBy>,
    pub(crate) autopilot_revision: u64,
    pub(crate) driver_writes: u8,
    /// The session's pin (`pin`, `unpin`): a person's or a lead agent's mark that keeps it first
    /// among its Mac's agents in the fleet band. A fresh session's is `false`, and a worker's
    /// stays so whoever spawned it; a restore and an update carry it.
    pub(crate) pinned: bool,
    /// Whether `pin` or `unpin` wrote [`Self::pinned`] on this process, so a carried pin grafted
    /// later does not undo it ([`restore_carried_meta`]).
    pub(crate) pin_written: bool,
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
            claim_placements: _,
            stream: _,
            supervisor_verified: _,
            task_exit_owner: _,
            builtin_relaunch_epoch: _,
            supervisor_builtin: _,
            claimant_conn: _,
            scoped_input_connections: _,
            scoped_input_actors: _,
            claim_held: _,
            claim_land: _,
            claim_needs: _,
            self_facts: _,
            questions,
            autopilot,
            land_at,
            autopilot_set_by: _,
            autopilot_revision: _,
            driver_writes: _,
            continuation_activity: _,
            pinned,
            pin_written: _,
        } = self;
        *user_title == other.user_title
            && *description == other.description
            && *icon == other.icon
            && *role == other.role
            && *attention == other.attention
            && *questions == other.questions
            && *autopilot == other.autopilot
            && *land_at == other.land_at
            && *pinned == other.pinned
    }
}

impl Eq for SessionMeta {}

impl SessionMeta {
    /// Whether any field is set.
    #[must_use]
    pub(crate) const fn any_set(&self) -> bool {
        self.user_title.is_some()
            || self.description.is_some()
            || self.icon.is_some()
            || self.role.is_some()
            || self.attention.is_some()
            || self.questions.is_some()
            || self.autopilot.is_some()
            || self.pinned
    }

    /// The session's Autopilot word, typed ([`Self::autopilot`]).
    #[must_use]
    pub(crate) fn autopilot_word(&self) -> Option<alab_policy::config::Autopilot> {
        use alab_policy::config::Autopilot;
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

    /// The named field after the canonical single-line presentation policy, which also covers a
    /// value that bypassed the setter.
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
            continuation_activity: None,
            description: self.presentation_value("description"),
            icon: self.presentation_value("icon"),
            role: self.presentation_value("role"),
            attention,
            attention_owners: AttentionOwners::default(),
            supervisor: None,
            supervisor_revision: 0,
            claim_placements: 0,
            stream: None,
            supervisor_verified: None,
            task_exit_owner: Default::default(),
            builtin_relaunch_epoch: RelaunchEpoch::default(),
            supervisor_builtin: false,
            claimant_conn: Vec::new(),
            scoped_input_connections: Vec::new(),
            scoped_input_actors: Vec::new(),
            claim_held: None,
            claim_land: None,
            claim_needs: None,
            self_facts: aterm_agent::supervise::facts_offer::SelfClaimFacts::default(),
            questions: self.presentation_value("questions"),
            autopilot,
            land_at,
            autopilot_set_by,
            autopilot_revision: 0,
            driver_writes: 0,
            pinned: self.pinned,
            pin_written: false,
        }
    }

    /// The Autopilot pair as a restore layout carries it: `land_at` only under Cruise.
    #[must_use]
    pub(crate) fn carried_autopilot(&self) -> (Option<String>, Option<i64>) {
        (
            self.presentation_value("autopilot"),
            self.land_at
                .filter(|_| self.autopilot_word() == Some(alab_policy::config::Autopilot::Cruise)),
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
        self.set_attention_under(owner, value, kind, None)
    }

    /// [`Self::set_attention_kind`], the entry written under `under`, the proven claim a wait
    /// lapses with ([`AttentionOwners::lapse_under`]); `None`, or an entry that is no wait, outlives
    /// every claim. A write of the `owner=needs` entry settles a self-claim's outgoing NEEDS YOU
    /// set against it ([`crate::needs::ToldNeeds::settle`]).
    pub(crate) fn set_attention_under(
        &mut self,
        owner: &str,
        value: Option<String>,
        kind: Option<aterm_types::needs::AttentionKind>,
        under: Option<crate::self_claim::Proven>,
    ) -> Result<AttentionMove, AttentionOwnersFull> {
        self.adopt_literal_attention();
        let entry = self.attention_owners.put(owner, value, kind, under)?;
        if owner == crate::harness_host::NEEDS_ATTENTION_OWNER {
            self.settle_self_needs();
        }
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
                .put(BARE_ATTENTION_OWNER, Some(value), None, None);
        }
    }

    /// The attention a `sessions`, `task` or `await task` row shows: the newest entry that asks,
    /// else [`Self::attention`] (the newest entry, a wait). A wait's words never hide an ask raised
    /// before it.
    #[must_use]
    pub(crate) fn row_attention(&self) -> Option<&str> {
        self.attention_owners
            .newest_ask()
            .or(self.attention.as_deref())
    }

    /// The supervisor's holder name while its claim is live at `now_us`
    /// ([`crate::metrics::now_us`]).
    #[must_use]
    pub(crate) fn live_supervisor(&self, now_us: u64) -> Option<&str> {
        self.live_claim(now_us).map(|claim| claim.holder.as_str())
    }

    /// Open the record stream of the claimant `proven` on the aterm instance `epoch`
    /// ([`crate::control_auth::process_instance_id`]), served on connection `conn`, and return
    /// its state. The same claimant keeps what was acknowledged (`acked`) and its counters
    /// across connections, and its applied kinds while its claim stands
    /// ([`Self::settle_stream_run`]), so a reopened stream resends only what follows; another
    /// claimant starts from nothing. `in` counts aterm's records within one epoch: a new epoch
    /// starts it at 0. Every open starts owed nothing: a `restart` or `policy` owed to an
    /// earlier stream whose lane has not closed yet (busy serving a `claim` record, or blocked
    /// writing to a peer that stopped reading) is dropped here, as its close would have dropped
    /// it (D8), and the `policy` every open sends covers one that was due.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn open_stream(
        &mut self,
        proven: &crate::harness_claim::Proven,
        epoch: &str,
        conn: u64,
    ) -> &mut StreamState {
        let mut state = match self.stream.take() {
            Some(mut kept) if kept.proven == *proven => {
                if kept.epoch != epoch {
                    kept.epoch = epoch.to_string();
                    kept.sent = 0;
                }
                kept
            }
            _ => StreamState::new(proven.clone(), epoch),
        };
        state.open = Some(conn);
        state.restarts.clear();
        state.policy_due = false;
        self.stream.insert(state)
    }

    /// The stream of the claimant `proven`, served on connection `conn`, closed: nothing it was
    /// owed and did not send is sent on a later stream (D8). A stream another connection opened
    /// since is left as it is.
    #[cfg(unix)]
    pub(crate) fn close_stream(&mut self, proven: &crate::harness_claim::Proven, conn: u64) {
        if let Some(state) = self
            .stream
            .as_mut()
            .filter(|state| state.proven == *proven && state.open == Some(conn))
        {
            state.open = None;
            state.restarts.clear();
            state.policy_due = false;
        }
    }

    /// The state of the record stream open now for the claim live at `now_us`: the claimant's
    /// own stream, served on the connection the claim is bound to. `None` with no stream open,
    /// and for a claim that left the stream's connection or is another claimant's: aterm owes
    /// its records only to an open stream.
    fn open_stream_state(&mut self, now_us: u64) -> Option<&mut StreamState> {
        let open = match (self.live_claim(now_us), self.stream.as_ref()) {
            (Some(claim), Some(stream)) => {
                stream.open.is_some()
                    && claim.conn == stream.open
                    && claim
                        .scoped
                        .as_ref()
                        .is_some_and(|scoped| scoped.proven == stream.proven)
            }
            _ => false,
        };
        if open { self.stream.as_mut() } else { None }
    }

    /// OWE THE OPEN STREAM A `restart` RECORD for the `restart` row `payload` (the row's text,
    /// `cause=<cause>[ to=<label> home=<pct|->][ model=<word>]`), written under this same hold:
    /// whether one is owed. A row written while no stream is open for the live claim owes none,
    /// then or later (D8, D32).
    pub(crate) fn owe_stream_restart(&mut self, payload: &str, now_us: u64) -> bool {
        self.open_stream_state(now_us)
            .map(|state| state.restarts.push_back(payload.to_string()))
            .is_some()
    }

    /// OWE THE OPEN STREAM A `policy` RECORD: the person's policy moved (`harness policy`).
    /// Whether one is owed; a stream that opens later hears a `policy` as it opens.
    pub(crate) fn owe_stream_policy(&mut self, now_us: u64) -> bool {
        self.open_stream_state(now_us)
            .map(|state| state.policy_due = true)
            .is_some()
    }

    /// The live claim itself at `now_us` (a lapsed `ttl=` claim reads as none).
    #[must_use]
    pub(crate) fn live_claim(&self, now_us: u64) -> Option<&SupervisorClaim> {
        self.supervisor
            .as_ref()
            .filter(|claim| claim.expires_us.is_none_or(|at| at > now_us))
    }

    /// A contract 4 claim bound to this open connection. Its only expiry is the grace
    /// installed after the connection closes, when it no longer admits a stream.
    #[cfg(unix)]
    pub(crate) fn stream_claim(
        &self,
        conn: u64,
    ) -> Option<(&SupervisorClaim, &crate::harness_claim::Scoped)> {
        let claim = self
            .supervisor
            .as_ref()
            .filter(|claim| claim.conn == Some(conn) && claim.expires_us.is_none())?;
        let scoped = claim
            .scoped
            .as_ref()
            .filter(|scoped| scoped.claim.contract == aterm_agent::harness::contract::UNIVERSAL)?;
        Some((claim, scoped))
    }

    /// Whether the live scoped claim's own record stream, opened in `epoch`, has applied a
    /// record of `kind` (its wire word) and the claim still holds `scope`, the scope that kind
    /// is admitted under: the stream feeds that kind, and its built-in reader stands down for
    /// it. The stream's state outlives the claim, and its applied kinds outlive the claim's
    /// grant until the claim ends ([`Self::settle_stream_run`]), so the claim is read too: a
    /// released or lapsed claim (one past its `ttl=` not yet removed included), another
    /// claimant's, and one renewed without `scope` (whose records of the kind are dropped from
    /// then on) feed nothing.
    #[must_use]
    pub(crate) fn stream_applied(
        &self,
        kind: &str,
        scope: aterm_agent::harness::contract::Scope,
        epoch: &str,
        now_us: u64,
    ) -> bool {
        self.stream_applied_under(kind, &[scope], epoch, now_us)
    }

    /// [`Self::stream_applied`] for a kind admitted under any one of `scopes` (`need` and
    /// `clear`: `approvals`, `questions` or `walls`), or under none beyond the open stream
    /// (`model`, `upgrade`: `scopes` empty).
    #[must_use]
    pub(crate) fn stream_applied_under(
        &self,
        kind: &str,
        scopes: &[aterm_agent::harness::contract::Scope],
        epoch: &str,
        now_us: u64,
    ) -> bool {
        match (self.live_scoped(now_us), &self.stream) {
            (Some(scoped), Some(stream)) => {
                stream.proven == scoped.proven
                    && (scopes.is_empty()
                        || scopes
                            .iter()
                            .any(|scope| scoped.claim.scopes.contains(*scope)))
                    && stream.epoch == epoch
                    && stream.applied.contains(kind)
            }
            _ => false,
        }
    }

    /// D29 FOR THE `meta set harness_*` WRITERS: whether the live claim's own record stream,
    /// opened on this aterm ([`crate::control_auth::process_instance_id`]), applied a record of
    /// one of `kinds` that the claim still holds a scope of `scopes` for
    /// ([`Self::stream_applied_under`]). From then that stream is the kind's one writer, so the
    /// same claimant's `meta set` for it is answered `ERR usage: stream`; another claimant's,
    /// and the same one's once its claim stops feeding the kind, are written as before.
    #[must_use]
    pub(crate) fn stream_writes(
        &self,
        kinds: &[&str],
        scopes: &[aterm_agent::harness::contract::Scope],
        now_us: u64,
    ) -> bool {
        let epoch = crate::control_auth::process_instance_id();
        kinds
            .iter()
            .any(|kind| self.stream_applied_under(kind, scopes, epoch, now_us))
    }

    /// KEEP `activity` AS THE CLAIM'S CONTINUATION OBSERVATION (`meta set harness_activity`, an
    /// `act` record of act `wait`), under the supervisor revision that stands: refused (`false`,
    /// nothing kept) where the revision's kept one, reported by the same source, has a
    /// `sequence` at or past it, so a resent or older report never replaces a newer one. A
    /// report of the other source is not ordered by it: a line's `seq=` and the stream's record
    /// `seq` count apart ([`crate::autopilot_activity::Activity::from_stream`]), and a stream's
    /// first `wait` replaces what a line kept before it, whatever sequence that line chose.
    /// Whether what a reader words moved: another revision's, another reason or another end.
    pub(crate) fn keep_continuation_activity(
        &mut self,
        activity: crate::autopilot_activity::Activity,
    ) -> bool {
        let revision = self.supervisor_revision;
        if self
            .continuation_activity
            .as_ref()
            .is_some_and(|(rev, old)| {
                *rev == revision
                    && old.from_stream == activity.from_stream
                    && old.sequence >= activity.sequence
            })
        {
            return false;
        }
        let changed = self
            .continuation_activity
            .as_ref()
            .is_none_or(|(rev, old)| {
                *rev != revision
                    || old.reason != activity.reason
                    || old.until_ms != activity.until_ms
            });
        self.continuation_activity = Some((revision, activity));
        changed
    }

    /// The version of the build the live scoped claim's proof admitted at `now_us`: its
    /// manifest's, or `link` for a development link's checkout. `None` under any other claim,
    /// and where the proof read no version. It is never the `version=` the claim states.
    #[must_use]
    pub(crate) fn verified_version(&self, now_us: u64) -> Option<&str> {
        let scoped = self.live_scoped(now_us)?;
        self.supervisor_verified
            .as_ref()
            .filter(|(proven, _)| *proven == scoped.proven)
            .map(|(_, version)| version.as_str())
    }

    /// The live claim's proven scoped claim at `now_us` ([`SupervisorClaim::scoped`]): a
    /// registered harness's contract 3 or 4 claim, with the versions it announced and the child
    /// aterm kept for it.
    #[must_use]
    pub(crate) fn live_scoped(&self, now_us: u64) -> Option<&crate::harness_claim::Scoped> {
        self.live_claim(now_us)
            .and_then(|claim| claim.scoped.as_ref())
    }

    /// Capture the live claim and its revision together. This snapshot describes duty ownership;
    /// it grants no input or act admission and must be checked again before either is admitted.
    #[must_use]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "Capture",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeObservationContinuity",
            action = "ForeignExpiry",
            reason = "The fixture advances the external clock past the foreign lease; this reader observes expiry without resetting the retained revocation epoch."
        )
    )]
    pub(crate) fn supervisor_authority(&self, now_us: u64) -> SupervisorAuthority {
        let claim = self.live_claim(now_us);
        SupervisorAuthority {
            holder: claim.map(|c| c.holder.clone()),
            contract: claim.and_then(|c| match &c.scoped {
                Some(scoped) => u8::try_from(scoped.claim.contract).ok(),
                None => c.proven.as_ref().map(|_| 2),
            }),
            identity: claim.and_then(|c| c.identity().cloned()),
            conn: claim.and_then(|c| c.conn),
            scopes: claim.and_then(|c| c.scoped.as_ref().map(|s| s.claim.scopes.clone())),
            revision: self.supervisor_revision,
            builtin_relaunch_epoch: self.builtin_relaunch_epoch,
            builtin_relaunch_kept: claim.is_none_or(|claim| {
                self.supervisor_builtin
                    || !claim.takes(aterm_agent::harness::contract::Scope::Relaunch)
            }),
        }
    }

    /// Transfer only the connection of this exact live v2 claim while its
    /// native exec operation still owns input custody. No scope or identity is
    /// reconstructed from the wire, and a changed owner/revision refuses.
    #[cfg(any(unix, test))]
    pub(crate) fn transfer_native_exec_claim(
        &mut self,
        expected: &SupervisorAuthority,
        connection: u64,
        detached_until: Option<u64>,
        now_us: u64,
    ) -> Option<SupervisorAuthority> {
        if expected.contract() != Some(2)
            || expected.identity().is_none()
            || self.supervisor_authority(now_us) != *expected
        {
            return None;
        }
        let mut claim = self.supervisor.clone()?;
        if claim.expires_us != detached_until {
            return None;
        }
        let previous = claim.conn;
        claim.conn = Some(connection);
        claim.expires_us = None;
        self.set_supervisor(Some(claim), now_us);
        if let Some(previous) = previous.filter(|old| *old != connection) {
            self.close_connection(previous, now_us);
        }
        Some(self.supervisor_authority(now_us))
    }

    /// Keep the exact v2 SDK claim through a bounded first-adoption window
    /// after its connection closes. Expiry changes no authority or revision.
    #[cfg(unix)]
    pub(crate) fn retain_closed_native_exec_claim(
        &mut self,
        expected: &SupervisorAuthority,
        until_us: u64,
        now_us: u64,
    ) -> bool {
        let Some(connection) = expected.connection() else {
            return false;
        };
        if expected.contract() != Some(2)
            || expected.identity().is_none()
            || until_us <= now_us
            || self.supervisor_authority(now_us) != *expected
        {
            return false;
        }
        let Some(claim) = self.supervisor.as_mut() else {
            return false;
        };
        if claim.expires_us.is_some_and(|expiry| expiry != until_us) {
            return false;
        }
        claim.expires_us = Some(until_us);
        // The live authority survives this EOF, but its socket bindings do not.
        self.close_connection(connection, now_us);
        true
    }

    /// A scoped claimant or the private built-in harness must name its input duty. Ordinary
    /// operator/drive credentials retain their independent authority; attribution text is no
    /// substitute for this connection history or the direct socket's kernel peer.
    pub(crate) fn scoped_input_required(
        &self,
        principal: crate::control::Principal,
        conn: Option<u64>,
        peer: Option<SupervisorInputPeer<'_>>,
        now_us: u64,
    ) -> bool {
        let live = self.live_scoped(now_us);
        (principal == crate::control::Principal::Harness
            && (live.is_some() || !self.scoped_input_actors.is_empty()))
            || conn.is_some_and(|conn| self.scoped_input_connections.contains(&conn))
            || peer.is_some_and(|peer| {
                let matches = |known: &crate::self_claim::Proven| {
                    known.pid == peer.pid && peer.start.is_none_or(|start| known.start == start)
                };
                live.is_some_and(|scoped| matches(&scoped.proven))
                    || self.scoped_input_actors.iter().any(matches)
            })
    }

    /// The proof of the proven claim `conn` was bound to last ([`Self::claimant_conn`]), whether
    /// that claim stands or ended while `conn` stayed open.
    fn claimant_on(&self, conn: Option<u64>) -> Option<&crate::self_claim::Proven> {
        let conn = conn?;
        self.claimant_conn
            .iter()
            .find(|(bound, _)| *bound == conn)
            .map(|(_, proven)| proven)
    }

    /// Put `claim` in the supervisor slot at `now_us` and move [`Self::supervisor_revision`],
    /// which it returns beside what the claim's end moved in the attention map: every wait written
    /// under a proven claim other than `claim`'s ([`AttentionOwners::lapse_except`]) goes; a stop,
    /// a Land hold or a NEEDS YOU set told under one goes too ([`Self::self_held`],
    /// [`Self::self_land`], [`Self::self_needs`]), and so does one `claim` does not renew, where
    /// the claim live at `now_us` is not of the proof that told it (its `ttl=` lapsed, the same
    /// process claiming again included, so a stop, a hold or a set its claimant could not lift
    /// during the lapse never comes back), each judged by the act proof that told it
    /// ([`SupervisorClaim::act_proof`]), so a scope that drops `turn-end` lets it go as well; a
    /// proven claim bound to a connection is recorded in
    /// [`Self::claimant_conn`] until that connection closes, and an unproven claim bound to a
    /// connection clears any earlier proof recorded for that connection. The revision stays where
    /// it was for a renewal in place: `claim` retains the live claim's authority at `now_us`,
    /// while its lease or requested words may move ([`SupervisorClaim::renews_in_place`]), and
    /// the authority the session shows ([`Self::supervisor_authority`]) is unchanged, so a seal,
    /// a native operation or a typed input bound to the revision outlives its own holder's
    /// reconfirmations. Every call moves [`Self::claim_placements`].
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "OwnClaim",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "OwnRenew",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "OwnRelease",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "OwnRetake",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "ForeignRelaunch",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "ForeignReturn",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "ForeignUpgradeOnly",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "OverflowForeign",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "OverflowReturn",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeObservationContinuity",
            action = "NearExhaustion",
            reason = "The fixture sets the epoch beside its last values, a state no run reaches by counting; no production step takes it there."
        )
    )]
    fn set_supervisor(
        &mut self,
        claim: Option<SupervisorClaim>,
        now_us: u64,
    ) -> (u64, AttentionMove) {
        let before = self.supervisor_authority(now_us);
        let in_place = claim
            .as_ref()
            .zip(self.live_claim(now_us))
            .is_some_and(|(next, live)| live.renews_in_place(next));
        let builtin = crate::control::serving_principal() == crate::control::Principal::Harness
            && crate::control::serving_peer_pid() == Some(std::process::id())
            && crate::control::serving_connection().is_some()
            && claim.as_ref().is_some_and(|claim| {
                claim.holder == crate::harness_host::holder()
                    && match claim.conn {
                        Some(conn) => Some(conn) == crate::control::serving_connection(),
                        None => claim.expires_us.is_some(),
                    }
            });
        let old_kept = self.supervisor.as_ref().is_none_or(|claim| {
            self.supervisor_builtin || !claim.takes(aterm_agent::harness::contract::Scope::Relaunch)
        });
        let new_kept = claim.as_ref().is_none_or(|claim| {
            builtin || !claim.takes(aterm_agent::harness::contract::Scope::Relaunch)
        });
        if old_kept != new_kept {
            self.builtin_relaunch_epoch.advance();
        }
        self.supervisor_builtin = builtin;
        if let Some(scoped) = claim.as_ref().and_then(|claim| claim.scoped.as_ref()) {
            if !self.scoped_input_actors.contains(&scoped.proven) {
                self.scoped_input_actors.push(scoped.proven.clone());
            }
            if let Some(conn) = claim.as_ref().and_then(|claim| claim.conn)
                && !self.scoped_input_connections.contains(&conn)
            {
                self.scoped_input_connections.push(conn);
            }
        }
        if let Some(claim) = claim.as_ref()
            && let Some(conn) = claim.conn
        {
            match claim.identity() {
                Some(proven) => match self
                    .claimant_conn
                    .iter_mut()
                    .find(|(bound, _)| *bound == conn)
                {
                    Some((_, slot)) => *slot = proven.clone(),
                    None => self.claimant_conn.push((conn, proven.clone())),
                },
                None => self.claimant_conn.retain(|(bound, _)| *bound != conn),
            }
        }
        let keep = claim.as_ref().and_then(SupervisorClaim::identity);
        let lapsed = self.lapse_except(keep);
        // A stop, a Land hold and a NEEDS YOU set were told under the act proof, so they are kept
        // only by a claim that writes under that proof again: one whose scope drops `turn-end`
        // lets them go, and they never come back if it takes `turn-end` again.
        let keep_acts = claim.as_ref().and_then(SupervisorClaim::act_proof);
        let renewed = self.live_claim(now_us).and_then(SupervisorClaim::act_proof);
        let lapses =
            |told: &crate::self_claim::Proven| Some(told) != keep_acts || Some(told) != renewed;
        let held = self
            .claim_held
            .as_ref()
            .is_some_and(|(told, _)| lapses(told));
        let land = self
            .claim_land
            .as_ref()
            .is_some_and(|(told, _)| lapses(told));
        let needs = self
            .claim_needs
            .as_ref()
            .is_some_and(|(told, _)| lapses(told));
        if held {
            self.claim_held = None;
        }
        if land {
            self.claim_land = None;
        }
        if needs {
            self.claim_needs = None;
        }
        self.settle_stream_run(claim.as_ref(), now_us);
        self.supervisor = claim;
        let claimant = self
            .supervisor
            .as_ref()
            .and_then(|claim| claim.scoped.as_ref())
            .map(|scoped| &scoped.proven);
        if self.supervisor_verified.as_ref().map(|(proven, _)| proven) != claimant {
            self.supervisor_verified = None;
        }
        if !in_place || self.supervisor_authority(now_us) != before {
            self.supervisor_revision = self.supervisor_revision.wrapping_add(1);
            // The claim's grant or revision moved under its own open stream (a `claim` record,
            // D7): the harness is owed a `policy` record. A claim placed from another
            // connection is no longer the stream's, which closes.
            if let Some(stream) = self.open_stream_state(now_us) {
                stream.policy_due = true;
            }
        }
        self.claim_placements = self.claim_placements.wrapping_add(1);
        (self.supervisor_revision, lapsed)
    }

    /// THE STREAM'S RUN ENDS WITH ITS CLAIM, at the placement `next` (`None`: a release or a
    /// lapse) of [`Self::set_supervisor`], before it stands. What the claimant's stream applied
    /// (its applied kinds) and the needs it holds open are its claim's, kept while each placement
    /// is the same claimant's contract 4 claim over its own claim still live: a renewal, a
    /// `claim` record, a claim placed again inside the re-exec grace. A release, a lapse,
    /// another claimant's claim, a placement over a claim that had already lapsed and the same
    /// process's contract 3 claim end it: the applied kinds and the open needs are emptied, so
    /// no `meta set harness_*` line of the same claimant is stood down for a kind no stream of
    /// its claim applied, and the needs the stream raised are lifted
    /// ([`SessionTimeline::settle_stream_claim`]). What was acknowledged (`acked`) and the
    /// counters stay, so a stream the claimant opens again resends only what follows. A
    /// placement whose grant names none of [`NEED_SCOPES`] ends the open needs alone: the
    /// scope that admitted them is gone, and they do not come back with it.
    fn settle_stream_run(&mut self, next: Option<&SupervisorClaim>, now_us: u64) {
        let standing = self.live_scoped(now_us).map(|scoped| scoped.proven.clone());
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        let next = next
            .and_then(|claim| claim.scoped.as_ref())
            .filter(|scoped| {
                scoped.proven == stream.proven
                    && scoped.claim.contract == aterm_agent::harness::contract::UNIVERSAL
            });
        match next {
            Some(scoped) if standing.as_ref() == Some(&stream.proven) => {
                if !NEED_SCOPES
                    .iter()
                    .any(|scope| scoped.claim.scopes.contains(*scope))
                {
                    stream.needs.clear();
                }
            }
            _ => {
                stream.applied.clear();
                stream.needs.clear();
            }
        }
    }

    /// Connection `conn` closed at `now_us`: the waits written on it under the proven claim it was
    /// bound to ([`Self::claimant_conn`]) go unless that same proven claim still stands live on
    /// the session (for instance after a `ttl=` renewal) or on another open connection (a
    /// reconnected claim socket whose new claim arrived before `conn`'s EOF), though that claim
    /// ended before it (a person's release, the image leaving the foreground), since a closed
    /// claim connection with no remaining live hold is a claimant gone. A stop, a Land hold and a
    /// NEEDS YOU set told under that claim ([`Self::self_held`], [`Self::self_land`],
    /// [`Self::self_needs`]) go at the same point; the second value says whether any did.
    pub(crate) fn close_connection(&mut self, conn: u64, now_us: u64) -> (AttentionMove, bool) {
        self.scoped_input_connections.retain(|bound| *bound != conn);
        let Some(at) = self
            .claimant_conn
            .iter()
            .position(|(bound, _)| *bound == conn)
        else {
            return (AttentionMove::default(), false);
        };
        let (_, proven) = self.claimant_conn.remove(at);
        if self.live_claim(now_us).and_then(SupervisorClaim::identity) == Some(&proven)
            || self.claimant_conn.iter().any(|(_, other)| *other == proven)
        {
            return (AttentionMove::default(), false);
        }
        let held = self
            .claim_held
            .take_if(|(told, _)| *told == proven)
            .is_some();
        let land = self
            .claim_land
            .take_if(|(told, _)| *told == proven)
            .is_some();
        let needs = self
            .claim_needs
            .take_if(|(told, _)| *told == proven)
            .is_some();
        (self.lapse_under(&proven), held || land || needs)
    }

    /// The stop the claim live at `now_us` told of its own typing under its act proof (`meta set
    /// harness_held`, [`Self::tell_self_held`], [`SupervisorClaim::act_proof`]): `None` with no
    /// such claim, and where the claim that told it is not the one standing, whatever is kept.
    #[must_use]
    pub(crate) fn self_held(&self, now_us: u64) -> Option<aterm_agent::harness::acts::HeldBy> {
        let (told, arm) = self.claim_held.as_ref()?;
        (self.live_claim(now_us)?.identity() == Some(told)).then_some(*arm)
    }

    /// Keep `held` as the stop the proven claim `proven` told (`None`: its typing goes on):
    /// whether the kept stop moved.
    pub(crate) fn tell_self_held(
        &mut self,
        proven: &crate::self_claim::Proven,
        held: Option<aterm_agent::harness::acts::HeldBy>,
    ) -> bool {
        let told = held.map(|arm| (proven.clone(), arm));
        if self.claim_held == told {
            return false;
        }
        self.claim_held = told;
        true
    }

    /// The Land hold the claim live at `now_us` told of its own landing under its act proof (`meta
    /// set harness_land`, [`Self::tell_self_land`], [`SupervisorClaim::act_proof`]): `None` with
    /// no such claim, and where the claim that told it is not the one standing, whatever is kept.
    #[must_use]
    pub(crate) fn self_land(&self, now_us: u64) -> Option<crate::harness_glass::HeldGlass> {
        let (told, hold) = self.claim_land.as_ref()?;
        (self.live_claim(now_us)?.identity() == Some(told)).then_some(*hold)
    }

    /// Keep `held` as the Land hold the proven claim `proven` told (`None`: it holds no landing),
    /// its `since` stamped once ([`crate::harness_glass::HeldGlass::of`] over the hold that proof
    /// told last): whether the kept hold moved.
    pub(crate) fn tell_self_land(
        &mut self,
        proven: &crate::self_claim::Proven,
        held: Option<aterm_agent::harness::acts::LandHeld>,
    ) -> bool {
        let kept = self
            .claim_land
            .as_ref()
            .map(|(told, hold)| (told, hold.held));
        if kept == held.map(|held| (proven, held)) {
            return false;
        }
        let was = self
            .claim_land
            .as_ref()
            .filter(|(told, _)| told == proven)
            .map(|(_, hold)| *hold);
        self.claim_land = held.map(|held| {
            (
                proven.clone(),
                crate::harness_glass::HeldGlass::of(held, was),
            )
        });
        true
    }

    /// THE NEEDS YOU SETS A PROVEN CLAIM TOLD OF ITS OWN ENTRY, read only while it stands (`meta
    /// set harness_needs`, [`Self::tell_self_needs`]): where the claim live at `now_us` has an act
    /// proof ([`SupervisorClaim::act_proof`]: a proven codex-alab self-claim, which aterm's own
    /// harness never holds, or a rostered harness's proven claim whose scope names `turn-end`,
    /// the duty NEEDS YOU belongs to), what that claim told, empty where it told none, so a set
    /// aterm's own loop left on the timeline is never read in its place; `None` where no such
    /// claim stands (none, an unproven claim, a rostered harness's claim without `turn-end`, one
    /// past its `ttl=`), and a reader then reads aterm's own ([`crate::needs::told_of`]).
    #[must_use]
    pub(crate) fn self_needs(&self, now_us: u64) -> Option<crate::needs::ToldNeeds> {
        let standing = self.live_claim(now_us)?.act_proof()?;
        Some(
            self.claim_needs
                .as_ref()
                .filter(|(told, _)| told == standing)
                .map(|(_, needs)| needs.told.clone())
                .unwrap_or_default(),
        )
    }

    /// The claim `proven`'s needs record, a new one where it kept none or another proof's.
    fn claim_needs_of(&mut self, proven: &crate::self_claim::Proven) -> crate::needs::ClaimNeeds {
        match self.claim_needs.take() {
            Some((by, needs)) if by == *proven => needs,
            _ => crate::needs::ClaimNeeds::default(),
        }
    }

    /// Keep `needs` as the claim `proven`'s, or nothing where it holds nothing.
    fn keep_claim_needs(
        &mut self,
        proven: &crate::self_claim::Proven,
        needs: crate::needs::ClaimNeeds,
    ) {
        self.claim_needs =
            (needs != crate::needs::ClaimNeeds::default()).then(|| (proven.clone(), needs));
    }

    /// Tell `told` as the NEEDS YOU set the proven claim `proven` told of its own `owner=needs`
    /// entry (`None`: it took the entry down), its credential lines off and the person's set told
    /// as aterm's own loop's is ([`crate::needs::ClaimNeeds::tell`]: [`crate::needs::ToldNeeds::tell`]
    /// beside that entry as stored now, so a set told before its entry stands waits for it, and
    /// the outgoing one keeps describing the entry it was told with while that entry stands,
    /// [`Self::settle_self_needs`]): whether what a reader reads beside the standing entry moved,
    /// the credential lines taken off, and the `owner=needs` entry aterm stands in place of the
    /// claimant's where it moves (`Some(None)`: taken down).
    pub(crate) fn tell_self_needs(
        &mut self,
        proven: &crate::self_claim::Proven,
        told: Option<crate::needs::HarnessNeeds>,
    ) -> (bool, Vec<String>, Option<Option<String>>) {
        let mut kept = self.claim_needs_of(proven);
        let standing = self
            .attention_owners
            .get(crate::harness_host::NEEDS_ATTENTION_OWNER);
        let told = kept.tell(told, standing);
        self.keep_claim_needs(proven, kept);
        told
    }

    /// THE `owner=needs` TEXT `said` (`None`: taken down), written on connection `conn`: the entry
    /// that stands for it ([`crate::needs::entry_for`]). Where `conn` is bound to the claim live at
    /// `now_us` under its act proof ([`SupervisorClaim::act_proof`]), it is that claimant's own words, kept with its set
    /// ([`crate::needs::ClaimNeeds::say`]) and read against it; any other writer tells no set.
    pub(crate) fn say_needs_entry(
        &mut self,
        conn: Option<u64>,
        now_us: u64,
        said: Option<String>,
    ) -> Option<String> {
        let live = self.live_claim(now_us).and_then(SupervisorClaim::act_proof);
        let Some(proven) = self
            .claimant_on(conn)
            .filter(|on| Some(*on) == live)
            .cloned()
        else {
            return crate::needs::entry_for(said.as_deref(), None);
        };
        let mut kept = self.claim_needs_of(&proven);
        let shown = kept.say(said);
        self.keep_claim_needs(&proven, kept);
        shown
    }

    /// THE SELF-CLAIM'S OUTGOING SET GOES WITH THE ENTRY IT DESCRIBED: settle [`Self::claim_needs`]
    /// against the `owner=needs` entry as stored now ([`crate::needs::ToldNeeds::settle`]), so an
    /// entry taken down, or replaced by one the outgoing set does not describe, takes that set,
    /// and the same text set again before the claimant's next tell reads its entry's own words,
    /// never a set it took down; a record kept for nothing is dropped.
    fn settle_self_needs(&mut self) {
        let standing = self
            .attention_owners
            .get(crate::harness_host::NEEDS_ATTENTION_OWNER);
        if let Some((_, needs)) = self.claim_needs.as_mut() {
            needs.told.settle(standing);
        }
        if self
            .claim_needs
            .as_ref()
            .is_some_and(|(_, needs)| *needs == crate::needs::ClaimNeeds::default())
        {
            self.claim_needs = None;
        }
    }

    /// Remove the waits written under any proven claim other than `keep`
    /// ([`AttentionOwners::lapse_except`]) and re-project [`Self::attention`].
    fn lapse_except(&mut self, keep: Option<&crate::self_claim::Proven>) -> AttentionMove {
        if !self.attention_owners.lapse_except(keep) {
            return AttentionMove::default();
        }
        let effective = self.attention_owners.effective().map(str::to_owned);
        let changed = self.attention != effective;
        self.attention = effective;
        AttentionMove {
            effective: changed,
            entry: true,
        }
    }

    /// Remove the waits written under `ended` ([`AttentionOwners::lapse_under`]) and re-project
    /// [`Self::attention`].
    fn lapse_under(&mut self, ended: &crate::self_claim::Proven) -> AttentionMove {
        if !self.attention_owners.lapse_under(ended) {
            return AttentionMove::default();
        }
        let effective = self.attention_owners.effective().map(str::to_owned);
        let changed = self.attention != effective;
        self.attention = effective;
        AttentionMove {
            effective: changed,
            entry: true,
        }
    }

    /// Whether the live claim at `now_us` is a proven codex-alab self-claim (contract v2,
    /// [`crate::self_claim`]): the agent supervises itself, so what acts for that self-claim acts.
    /// A proven scoped claim (contract 3 or 4) is not one ([`Self::live_scoped`]).
    #[must_use]
    pub(crate) fn supervisor_proven(&self, now_us: u64) -> bool {
        self.live_claim(now_us)
            .is_some_and(|claim| claim.proven.is_some())
    }

    /// The wire's `supervisor_proven=1`: the live claim at `now_us` is proven, a codex-alab
    /// self-claim ([`Self::supervisor_proven`]) or a rostered harness's scoped claim
    /// ([`SupervisorClaim::scoped`], [`crate::harness_claim`]).
    #[must_use]
    pub(crate) fn claim_proven(&self, now_us: u64) -> bool {
        self.live_claim(now_us)
            .is_some_and(SupervisorClaim::is_proven)
    }

    /// When the claim lapses ([`crate::metrics::now_us`]), live or not: a `ttl=` claim's lease,
    /// the end of re-exec grace for a closed contract 4 connection, or the end of
    /// native first-adoption grace for a closed v2 SDK connection. A claim bound
    /// to an open connection has none.
    #[must_use]
    pub(crate) fn supervisor_expiry(&self) -> Option<u64> {
        self.supervisor.as_ref().and_then(|claim| claim.expires_us)
    }

    /// THE RE-EXEC GRACE (`docs/HARNESS-CONTRACT.md`, "The re-exec grace"): connection `conn`
    /// closed at `now_us`, and where the live claim is a proven contract 4 claim bound to it,
    /// the claim is kept for `grace_us` more, with that deadline as its expiry
    /// ([`Self::supervisor_expiry`]), so the window's lapse timer releases it
    /// ([`publish_lapse_supervisor`]) unless the same claimant claims again first. Whether the
    /// claim is kept, and whether this call is what armed the grace.
    ///
    /// The claim is changed in place and not placed again ([`Self::set_supervisor`]): a closing
    /// connection is not a claim, so [`Self::claim_placements`] does not move (the restart
    /// watch reads a placement as the harness having claimed again), and the revision and the
    /// authority stand, the closed connection's serial included, which no later connection
    /// takes. A grace already armed is not armed again: a second close of the same serial does
    /// not extend it.
    pub(crate) fn keep_through_grace(&mut self, conn: u64, now_us: u64, grace_us: u64) -> Grace {
        let Some(claim) = self
            .supervisor
            .as_mut()
            .filter(|claim| claim.expires_us.is_none_or(|at| at > now_us))
            .filter(|claim| claim.conn == Some(conn))
            .filter(|claim| {
                claim.scoped.as_ref().is_some_and(|scoped| {
                    scoped.claim.contract == aterm_agent::harness::contract::UNIVERSAL
                })
            })
        else {
            return Grace::None;
        };
        if claim.expires_us.is_some() {
            return Grace::Standing;
        }
        claim.expires_us = Some(now_us.saturating_add(grace_us));
        Grace::Armed
    }

    /// The claim a seamless update carries at `now_us`, as `(row, claim_known)` for the handoff
    /// record.
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
        if claim.is_proven() {
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
            .filter(|(owner, _, _)| !self.attention_owners.held_under_a_claim(owner))
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
            return Some(self.set_bare_attention(value).effective);
        }
        if field == "autopilot" {
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
    /// The one writer of the Autopilot pair (§9.3.6, item 1): `land_at` is kept only under Cruise.
    pub(crate) fn set_autopilot(&mut self, word: Option<String>, land_at: Option<i64>) -> bool {
        let word = word.and_then(|value| sanitize_metadata_value("autopilot", &value));
        let cruise = word.as_deref().and_then(normalize_autopilot_value)
            == Some(alab_policy::config::Autopilot::Cruise.word());
        let land_at = land_at.filter(|_| cruise);
        let changed = self.autopilot != word || self.land_at != land_at;
        self.autopilot = word;
        self.land_at = land_at;
        changed
    }

    /// Store `set_by` as the Autopilot pair's setter ([`Self::autopilot_set_by`]) and step
    /// [`Self::autopilot_revision`].
    pub(crate) const fn note_autopilot_set_by(&mut self, set_by: AutopilotSetBy) {
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
    /// The proven claim a `kind=wait` entry was written under, on the connection that claim was
    /// bound to ([`SessionMeta::claimant_conn`]): the wait is its claimant's own, so it lapses
    /// when that claim ends or that connection closes ([`AttentionOwners::lapse_under`]) and never
    /// crosses an update, which no proven claim does. Only a wait carries one
    /// ([`AttentionOwners::put`]).
    under: Option<crate::self_claim::Proven>,
}

impl AttentionEntry {
    /// Whether the entry asks for someone: every kind but `kind=wait` (no kind is an ask).
    fn asks(&self) -> bool {
        self.kind != Some(aterm_types::needs::AttentionKind::Wait)
    }
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
    pub(crate) const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many owners hold an entry.
    #[must_use]
    pub(crate) const fn len(&self) -> usize {
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

    /// `owner`'s entry, if it holds one that asks ([`AttentionEntry::asks`]): a `kind=wait`
    /// entry asks nothing.
    #[must_use]
    pub(crate) fn ask(&self, owner: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.owner == owner && entry.asks())
            .map(|entry| entry.text.as_str())
    }

    /// The most recently written entry's text that asks ([`AttentionEntry::asks`]).
    #[must_use]
    pub(crate) fn newest_ask(&self) -> Option<&str> {
        self.entries
            .iter()
            .filter(|entry| entry.asks())
            .max_by_key(|entry| entry.stamp)
            .map(|entry| entry.text.as_str())
    }

    /// Remove every entry written under `ended` ([`AttentionEntry::under`]), whose claim ended or
    /// whose claim connection closed: whether any was.
    fn lapse_under(&mut self, ended: &crate::self_claim::Proven) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.under.as_ref() != Some(ended));
        self.entries.len() != before
    }

    /// Remove every entry written under a proven claim ([`AttentionEntry::under`]) other than
    /// `keep`: whether any was.
    fn lapse_except(&mut self, keep: Option<&crate::self_claim::Proven>) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.under.is_none() || entry.under.as_ref() == keep);
        self.entries.len() != before
    }

    /// Whether `owner`'s entry was written under a proven claim ([`AttentionEntry::under`]).
    #[cfg(any(unix, test))]
    fn held_under_a_claim(&self, owner: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.owner == owner && entry.under.is_some())
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

    /// The kind the effective entry's writer named (`kind=`), `None` when it named none.
    #[must_use]
    pub(crate) fn effective_kind(&self) -> Option<aterm_types::needs::AttentionKind> {
        self.top().and_then(|entry| entry.kind)
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
        under: Option<crate::self_claim::Proven>,
    ) -> Result<bool, AttentionOwnersFull> {
        let kind = if owner == BARE_ATTENTION_OWNER {
            Some(aterm_types::needs::AttentionKind::Ask)
        } else {
            kind
        };
        // The stored kind decides: an ask, the bare one included, outlives every claim.
        let under = under.filter(|_| kind == Some(aterm_types::needs::AttentionKind::Wait));
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
                self.entries[at].under = under;
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
                    under,
                });
                Ok(true)
            }
        }
    }
}

/// What a closed connection's claim is kept under ([`SessionMeta::keep_through_grace`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grace {
    /// No contract 4 claim is bound to the connection: whatever it held is released.
    None,
    /// This close armed the grace: the claim's expiry is now the grace's end.
    Armed,
    /// The claim is in a grace an earlier close armed.
    Standing,
}

/// The `state` record's kind, by its wire word, as [`StreamState::applied`] names it.
pub(crate) const STATE_KIND: &str = "state";
/// The `usage` record's kind: one account's limits and the context left.
pub(crate) const USAGE_KIND: &str = "usage";
/// The `account` record's kind: the account and its state.
pub(crate) const ACCOUNT_KIND: &str = "account";
/// The `progress` record's kind: the plan's items.
pub(crate) const PROGRESS_KIND: &str = "progress";
/// The `goal` record's kind: the active goal.
pub(crate) const GOAL_KIND: &str = "goal";
/// The `compact` record's kind: the vendor compacts its context.
pub(crate) const COMPACT_KIND: &str = "compact";
/// The `act` record's kind: what the harness did and by which rule, its continuation
/// observation (`wait`) and its own typing's stop (`held`); `meta set harness_act`,
/// `harness_activity` and `harness_held` stand down for it.
pub(crate) const ACT_KIND: &str = "act";
/// The `need` record's kind: a need the harness raised; `meta set harness_needs` stands down
/// for it.
pub(crate) const NEED_KIND: &str = "need";
/// The `clear` record's kind: a need the harness raised ended; `meta set harness_needs` stands
/// down for it as for `need`.
pub(crate) const CLEAR_KIND: &str = "clear";
/// The scopes a `need` or `clear` record is admitted under, any one of them: what the `meta set
/// harness_needs` stand-down asks the claim still holds, and what the needs the stream raised
/// stand under ([`SessionTimeline::settle_stream_claim`]).
pub(crate) const NEED_SCOPES: [aterm_agent::harness::contract::Scope; 3] = [
    aterm_agent::harness::contract::Scope::Approvals,
    aterm_agent::harness::contract::Scope::Questions,
    aterm_agent::harness::contract::Scope::Walls,
];
/// The `land` record's kind: Land's state; `meta set harness_land` stands down for it.
pub(crate) const LAND_KIND: &str = "land";
/// The `model` record's kind: the model, its effort and what is on offer.
pub(crate) const MODEL_KIND: &str = "model";
/// The `upgrade` record's kind: a move to a newer vendor build.
pub(crate) const UPGRADE_KIND: &str = "upgrade";

/// A `need` record the claim's stream applied and no `clear` has ended yet, kept by the record's
/// `seq` ([`StreamState::needs`]): what a `clear` naming that `seq` ends.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "only the Unix control stream applies a `need` record"
    )
)]
pub(crate) struct StreamNeed {
    /// The need it raised, as aterm's `mark need` words it (`review`, `decision`, `help`).
    pub(crate) word: &'static str,
    /// The entry it raised.
    pub(crate) entry: String,
    /// Each need the entry stands for, in the harness's order.
    pub(crate) needs: Vec<String>,
}

/// A claimant's record stream (`stream @<sid> v=1`, `control_stream.rs`), kept on the session
/// beside its claim and keyed on the claimant's proof. It names no wire type: every platform
/// builds this file, and the wire library is Unix only.
#[derive(Clone, Debug)]
pub(crate) struct StreamState {
    /// The claimant (pid and kernel start) whose stream this is.
    pub(crate) proven: crate::harness_claim::Proven,
    /// The aterm instance that answered the stream's `OK`.
    pub(crate) epoch: String,
    /// The last harness record acknowledged: every record up to it was applied or dropped.
    pub(crate) acked: u64,
    /// The last record aterm sent in this epoch (`in=`); an acknowledgement is not counted.
    pub(crate) sent: u64,
    /// The kinds, by their wire word, a record of which was applied. A kind acknowledged but
    /// not applied yet is not here, so its built-in source keeps feeding it.
    pub(crate) applied: std::collections::BTreeSet<&'static str>,
    /// Records acknowledged with a `dropped=` reason (`stream_dropped=`).
    pub(crate) dropped: u64,
    /// Records of a kind this aterm does not know (`stream_unknown=`).
    pub(crate) unknown: u64,
    /// Records of a known kind this aterm does not apply yet (`stream_unapplied=`).
    pub(crate) unapplied: u64,
    /// `title` records, acknowledged and written by nothing yet (`stream_titles=`).
    pub(crate) titles: u64,
    /// When the last `beat` arrived, on the awake clock ([`aterm_uds::process::awake_ms`]);
    /// HX-15e's watchdog reads it.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "kept by the unix-only `control_stream::judge`")
    )]
    pub(crate) last_beat_ms: Option<u64>,
    /// The open stream owes its harness a `policy` record ("read your grant again"): the
    /// person's policy moved ([`SessionMeta::owe_stream_policy`]), or a placement moved the
    /// claim's grant or revision under its own open stream ([`SessionMeta::set_supervisor`]).
    /// The stream's sender takes it (`control_stream.rs`); a stream that closes forgets it.
    pub(crate) policy_due: bool,
    /// The `need` records applied and not yet ended by a `clear`, by their `seq`: a `clear`
    /// names the need it ends by that `seq` (after a reconnect too, as this state outlives the
    /// connection), and the newest one left is the NEEDS YOU set the claimant's stream tells.
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "kept by the unix-only `control_stream::step`")
    )]
    pub(crate) needs: std::collections::BTreeMap<u64, StreamNeed>,
    /// The connection serving this stream now, `None` while no stream is open: only an open
    /// stream is owed aterm's records, and what was owed when it closed is never sent on a
    /// later one (D8).
    pub(crate) open: Option<u64>,
    /// The `restart` rows written while the stream was open and not yet sent as records, each
    /// as the row's own text (`cause=<cause>[ to=<label> home=<pct|->][ model=<word>]`):
    /// every platform builds this file, so nothing here names a wire type.
    pub(crate) restarts: std::collections::VecDeque<String>,
    /// aterm's records the harness answered `took` (`stream_took=`).
    pub(crate) took: u64,
    /// aterm's records the harness answered `refused` (`stream_refused=`), each logged as
    /// `mark - refused in=<n> text=<why>`.
    pub(crate) refused: u64,
}

impl StreamState {
    fn new(proven: crate::harness_claim::Proven, epoch: &str) -> Self {
        Self {
            proven,
            epoch: epoch.to_string(),
            acked: 0,
            sent: 0,
            applied: std::collections::BTreeSet::new(),
            dropped: 0,
            unknown: 0,
            unapplied: 0,
            titles: 0,
            last_beat_ms: None,
            policy_due: false,
            needs: std::collections::BTreeMap::new(),
            open: None,
            restarts: std::collections::VecDeque::new(),
            took: 0,
            refused: 0,
        }
    }

    /// The nine `stream_*` words `meta` prints for `stream` (`-` and zeros for none).
    pub(crate) fn meta_words(stream: Option<&Self>) -> String {
        let Some(s) = stream else {
            return "stream_epoch=- stream_acked=0 stream_in=0 stream_dropped=0 \
                    stream_unknown=0 stream_unapplied=0 stream_titles=0 stream_took=0 \
                    stream_refused=0"
                .to_string();
        };
        format!(
            "stream_epoch={} stream_acked={} stream_in={} stream_dropped={} stream_unknown={} \
             stream_unapplied={} stream_titles={} stream_took={} stream_refused={}",
            s.epoch,
            s.acked,
            s.sent,
            s.dropped,
            s.unknown,
            s.unapplied,
            s.titles,
            s.took,
            s.refused
        )
    }
}

/// A live supervisor's claim on a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorClaim {
    pub(crate) holder: String,
    pub(crate) conn: Option<u64>,
    pub(crate) expires_us: Option<u64>,
    /// A proven codex-alab self-claim (contract v2).
    pub(crate) proven: Option<crate::self_claim::Proven>,
    /// A proven registered harness's scoped claim (contract 3 or 4); never set with `proven`.
    pub(crate) scoped: Option<crate::harness_claim::Scoped>,
}

/// The open stream lane's one connection and claim proof. Its stop is latched under
/// the metadata lock when that binding ends, including a release followed by replacement.
#[cfg(unix)]
pub(crate) struct StreamBinding {
    pub(crate) conn: u64,
    pub(crate) proven: crate::harness_claim::Proven,
    pub(crate) revoked: aterm_uds::stop::Stop,
}

#[cfg(unix)]
impl StreamBinding {
    pub(crate) fn claim<'a>(
        &self,
        meta: &'a SessionMeta,
    ) -> Option<(&'a SupervisorClaim, &'a crate::harness_claim::Scoped)> {
        if self.revoked.is_set() {
            return None;
        }
        meta.stream_claim(self.conn)
            .filter(|(_, scoped)| scoped.proven == self.proven)
    }
}

/// An owned observation of one session's duty authority at one revision. A missing scope set
/// means a live legacy or v2 claim takes every duty; a missing holder means no live claim.
/// Its private fields keep the captured proof, connection, scope and revision together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorAuthority {
    holder: Option<String>,
    contract: Option<u8>,
    identity: Option<crate::self_claim::Proven>,
    conn: Option<u64>,
    scopes: Option<aterm_agent::harness::contract::Scopes>,
    revision: u64,
    builtin_relaunch_epoch: RelaunchEpoch,
    builtin_relaunch_kept: bool,
}

impl SupervisorAuthority {
    /// Passive evidence survives only uninterrupted built-in Relaunch ownership.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "ProveOwnContinuity",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "ProveAfterForeignReturn",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "ProveAfterForeignExpiry",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "ProveUnrelatedDuty",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "ProveAfterOverflow",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "NativeObservationContinuity",
            action = "CaptureAfterOverflow",
            project = "control::control_native_observe_tests::continuity_transition"
        )
    )]
    pub(crate) fn same_builtin_relaunch_continuity(&self, other: &Self) -> bool {
        self.builtin_relaunch_kept
            && other.builtin_relaunch_kept
            && self
                .builtin_relaunch_epoch
                .same(other.builtin_relaunch_epoch)
    }
    /// The live holder, or none once its lease has lapsed.
    #[must_use]
    pub(crate) fn holder(&self) -> Option<&str> {
        self.holder.as_deref()
    }

    /// The proven claim's contract (2, 3 or 4), absent for an unproven claim or no live claim.
    #[must_use]
    pub(crate) const fn contract(&self) -> Option<u8> {
        self.contract
    }

    /// The kernel identity captured by the claim's proof, absent for an unproven claim.
    #[must_use]
    pub(crate) fn identity(&self) -> Option<&crate::self_claim::Proven> {
        self.identity.as_ref()
    }

    /// The connection the live claim was bound to, absent for a TTL claim or no live claim.
    #[must_use]
    pub(crate) const fn connection(&self) -> Option<u64> {
        self.conn
    }

    /// The revision captured alongside the claim, retained even where no claim stands.
    #[must_use]
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    /// A v3 claim's explicit scope set; legacy and v2 claims have implicit full scope.
    #[must_use]
    pub(crate) fn scopes(&self) -> Option<&aterm_agent::harness::contract::Scopes> {
        self.scopes.as_ref()
    }

    /// Whether another holder takes `duty` from the built-in holder named by the caller.
    #[must_use]
    pub(crate) fn external_takes(
        &self,
        builtin_holder: &str,
        duty: aterm_agent::harness::contract::Scope,
    ) -> bool {
        self.holder().is_some_and(|holder| holder != builtin_holder)
            && self.scopes().is_none_or(|scopes| scopes.contains(duty))
    }

    /// Whether `duty` stays with the built-in holder: the complement of external ownership.
    #[must_use]
    pub(crate) fn builtin_keeps(
        &self,
        builtin_holder: &str,
        duty: aterm_agent::harness::contract::Scope,
    ) -> bool {
        !self.external_takes(builtin_holder, duty)
    }

    /// Additive status words describing the captured contract, effective scopes and revision.
    /// An absent claim has `-` scopes; a legacy or v2 claim lists every duty in canonical order.
    #[must_use]
    pub(crate) fn wire_fields(&self) -> String {
        use aterm_agent::harness::contract::Scope;
        let contract = self
            .contract()
            .map_or_else(|| "-".to_string(), |n| n.to_string());
        let scopes = match (self.holder(), self.scopes()) {
            (None, _) => "-".to_string(),
            (Some(_), Some(scopes)) => scopes.words(),
            (Some(_), None) => Scope::ALL.map(Scope::word).join(","),
        };
        format!(
            "supervisor_contract={contract} supervisor_scope={scopes} supervisor_revision={}",
            self.revision()
        )
    }
}

/// Tests of the typed input boundary and its drain model.
#[cfg(test)]
#[path = "session_scoped_input_tests.rs"]
pub(crate) mod scoped_input_tests;

/// The explicit duty and claim revision a coordinated input request names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorInputRequest {
    pub(crate) holder: String,
    pub(crate) revision: u64,
    pub(crate) duty: aterm_agent::harness::contract::Scope,
}

/// Facts read by the serving control lane, never supplied by the request's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorInputCaller {
    pub(crate) principal: crate::control::Principal,
    pub(crate) connection: Option<u64>,
    pub(crate) peer: Option<u32>,
    pub(crate) proof: Option<crate::self_claim::Proven>,
}

/// A direct kernel peer, preserving an unreadable birth as uncertainty rather than absence.
/// This classifies whether typed input is required; it grants no claimed duty or proof.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SupervisorInputPeer<'a> {
    pub(crate) pid: u32,
    pub(crate) start: Option<&'a str>,
}

impl<'a> From<&'a crate::self_claim::Proven> for SupervisorInputPeer<'a> {
    fn from(proven: &'a crate::self_claim::Proven) -> Self {
        Self {
            pid: proven.pid,
            start: Some(&proven.start),
        }
    }
}

/// The authority carried from control admission to actual queued input delivery. Legacy
/// callers keep their holder fence; a scoped request carries the entire captured authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SupervisorInputFence {
    Legacy(String),
    Scoped(Arc<ScopedInputFence>),
}

/// A scoped input reservation. Private fields prevent the receiver from rebuilding a grant
/// from a holder string or a caller's attribution label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScopedInputFence {
    session: aterm_session::SessionId,
    nonce: aterm_session::LaunchNonce,
    authority: SupervisorAuthority,
    request: SupervisorInputRequest,
    builtin: bool,
    caller: SupervisorInputCaller,
}

pub(crate) const SCOPED_INPUT_DENIED: &str = "ERR denied supervisor input authority\n";
pub(crate) const SCOPED_INPUT_BUSY: &str = "ERR busy supervisor input in flight\n";

#[derive(Clone, Debug, PartialEq, Eq)]
enum InputWriter {
    Legacy(String),
    Builtin(String),
    Claimant {
        holder: String,
        proof: Option<crate::self_claim::Proven>,
        connection: Option<u64>,
    },
}

impl InputWriter {
    fn same_actor(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Legacy(a) | Self::Builtin(a), Self::Legacy(b) | Self::Builtin(b)) => a == b,
            _ => self == other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveInput {
    writer: InputWriter,
    duty: Option<aterm_agent::harness::contract::Scope>,
    inline: bool,
    native: Option<String>,
}

impl ActiveInput {
    fn of(fence: &SupervisorInputFence, inline: bool) -> Self {
        match fence {
            SupervisorInputFence::Legacy(holder) => Self {
                writer: InputWriter::Legacy(holder.clone()),
                duty: None,
                inline: false,
                native: None,
            },
            SupervisorInputFence::Scoped(scoped) => Self {
                writer: if scoped.builtin {
                    InputWriter::Builtin(scoped.request.holder.clone())
                } else {
                    InputWriter::Claimant {
                        holder: scoped.request.holder.clone(),
                        proof: scoped.caller.proof.clone(),
                        connection: scoped.caller.connection,
                    }
                },
                duty: Some(scoped.request.duty),
                inline,
                native: None,
            },
        }
    }

    fn conflicts(&self, other: &Self) -> bool {
        // Ordered jobs serialize spill drain in their one writer. A guarded immediate write
        // uses no such queue, so its complete prefix/suffix must settle before another typed
        // batch can start (and it cannot start beside one already writing).
        self.inline
            || other.inline
            || (self.duty.zip(other.duty).is_none_or(|(a, b)| a == b)
                && !self.writer.same_actor(&other.writer))
    }
}

impl SupervisorInputFence {
    /// Capture a typed request only while its revision, actor, proof and binding agree under
    /// the session meta lock. This reserves no in-flight write until [`FencedWrite::enter_fence`].
    pub(crate) fn authorize(
        ctx: &crate::SessionCtx,
        request: SupervisorInputRequest,
        caller: &SupervisorInputCaller,
        now_us: u64,
    ) -> Result<Self, String> {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let authority = meta.supervisor_authority(now_us);
        use aterm_agent::harness::contract::Scope;
        let typing = matches!(
            request.duty,
            Scope::TurnEnd | Scope::Approvals | Scope::Questions | Scope::Walls
        );
        if !typing || authority.revision() != request.revision || caller.connection.is_none() {
            return Err(SCOPED_INPUT_DENIED.to_string());
        }
        let builtin = caller.principal == crate::control::Principal::Harness;
        if builtin {
            if request.holder != crate::harness_host::holder()
                || !authority.builtin_keeps(&request.holder, request.duty)
            {
                return Err(SCOPED_INPUT_DENIED.to_string());
            }
        } else {
            if !matches!(authority.contract(), Some(3 | 4))
                || authority.holder() != Some(request.holder.as_str())
                || !authority.scopes().is_some_and(|s| s.contains(request.duty))
                || caller.connection.is_none()
                || caller.connection != authority.connection()
                || caller.proof.as_ref() != authority.identity()
                || caller.peer != authority.identity().map(|p| p.pid)
            {
                return Err(SCOPED_INPUT_DENIED.to_string());
            }
        }
        Ok(Self::Scoped(Arc::new(ScopedInputFence {
            session: ctx.self_id.clone(),
            nonce: ctx.nonce,
            authority,
            request,
            builtin,
            caller: caller.clone(),
        })))
    }

    /// Check a turn before its wait without holding an in-flight reservation for that wait.
    pub(crate) fn check(&self, ctx: &crate::SessionCtx, now_us: u64) -> Result<(), String> {
        if let Self::Legacy(holder) = self {
            return supervisor_fence(ctx, holder, now_us).map_or(Ok(()), Err);
        }
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        self.refusal(ctx, &meta, now_us).map_or(Ok(()), Err)
    }

    fn refusal(&self, ctx: &crate::SessionCtx, meta: &SessionMeta, now_us: u64) -> Option<String> {
        match self {
            Self::Legacy(holder) => {
                if meta.live_scoped(now_us).is_some() {
                    Some(SCOPED_INPUT_DENIED.to_string())
                } else {
                    fence_refusal(meta, holder, now_us)
                }
            }
            Self::Scoped(scoped) => (ctx.self_id != scoped.session
                || ctx.nonce != scoped.nonce
                || meta.supervisor_authority(now_us) != scoped.authority)
                .then(|| SCOPED_INPUT_DENIED.to_string()),
        }
    }
}

/// A control batch before or after its first actual PTY write. Queued batches carry a ticket;
/// the first ordered-writer delivery admits one lease, which every continuation shares.
#[derive(Clone, Debug)]
pub(crate) enum SupervisorInputDelivery {
    Legacy(String),
    Scoped(Arc<SupervisorInputBatch>),
}

impl SupervisorInputDelivery {
    pub(crate) fn new(ctx: &Arc<crate::SessionCtx>, fence: SupervisorInputFence) -> Self {
        match fence {
            SupervisorInputFence::Legacy(holder) => Self::Legacy(holder),
            fence @ SupervisorInputFence::Scoped(_) => {
                Self::Scoped(Arc::new(SupervisorInputBatch {
                    ctx: Arc::downgrade(ctx),
                    fence,
                    state: std::sync::Mutex::new(InputBatchState::default()),
                }))
            }
        }
    }

    pub(crate) fn batch(&self) -> Option<Arc<SupervisorInputBatch>> {
        match self {
            Self::Legacy(_) => None,
            Self::Scoped(batch) => Some(Arc::clone(batch)),
        }
    }

    pub(crate) fn is_scoped(&self) -> bool {
        matches!(self, Self::Scoped(_))
    }

    pub(crate) fn producer(&self) -> Option<SupervisorInputProducer> {
        self.batch().map(SupervisorInputProducer)
    }

    pub(crate) fn check(&self, ctx: &crate::SessionCtx, now_us: u64) -> Result<(), String> {
        match self {
            Self::Legacy(holder) => supervisor_fence(ctx, holder, now_us).map_or(Ok(()), Err),
            Self::Scoped(batch) => batch.check_queued(ctx, now_us),
        }
    }
}

#[derive(Debug)]
pub(crate) struct SupervisorInputBatch {
    ctx: std::sync::Weak<crate::SessionCtx>,
    fence: SupervisorInputFence,
    state: std::sync::Mutex<InputBatchState>,
}

#[derive(Debug, Default)]
struct InputBatchState {
    sealed: bool,
    pending: usize,
    phase: InputBatchPhase,
}

#[derive(Debug, Default)]
enum InputBatchPhase {
    #[default]
    Queued,
    Writing(FencedWrite),
    Refused,
    Settled,
}

/// The one logical input producer. Sealing it is idempotent, including an early-return drop.
/// Read-only reply settlement may keep the batch's Arc but never keeps its writer lease.
pub(crate) struct SupervisorInputProducer(Arc<SupervisorInputBatch>);

impl Drop for SupervisorInputProducer {
    fn drop(&mut self) {
        self.0.seal();
    }
}

/// One queued/completing input lineage. Clones of a completion's mark share this reservation
/// through GUI dispatch and the actual sink worker; its last drop ends that pending work.
#[derive(Debug)]
pub(crate) struct SupervisorInputWork(Arc<SupervisorInputBatch>);

impl Drop for SupervisorInputWork {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.pending = state.pending.saturating_sub(1);
        SupervisorInputBatch::settle(&mut state);
    }
}

impl SupervisorInputBatch {
    fn check_queued(&self, ctx: &crate::SessionCtx, now_us: u64) -> Result<(), String> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match &state.phase {
            InputBatchPhase::Queued => self.fence.check(ctx, now_us),
            InputBatchPhase::Writing(lease) => {
                let SupervisorInputFence::Scoped(scoped) = lease.fence() else {
                    return Err(SCOPED_INPUT_DENIED.to_string());
                };
                (ctx.self_id == scoped.session && ctx.nonce == scoped.nonce)
                    .then_some(())
                    .ok_or_else(|| SCOPED_INPUT_DENIED.to_string())
            }
            InputBatchPhase::Refused | InputBatchPhase::Settled => {
                Err(SCOPED_INPUT_DENIED.to_string())
            }
        }
    }

    pub(crate) fn producer(self: &Arc<Self>) -> SupervisorInputProducer {
        SupervisorInputProducer(Arc::clone(self))
    }

    pub(crate) fn reserve(self: &Arc<Self>) -> Option<Arc<SupervisorInputWork>> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.sealed
            || matches!(
                state.phase,
                InputBatchPhase::Refused | InputBatchPhase::Settled
            )
        {
            return None;
        }
        state.pending = state.pending.checked_add(1)?;
        Some(Arc::new(SupervisorInputWork(Arc::clone(self))))
    }

    pub(crate) fn seal(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.sealed = true;
        Self::settle(&mut state);
    }

    fn settle(state: &mut InputBatchState) {
        if state.sealed && state.pending == 0 {
            // The owned lease drops here, independently of long-lived observation Arcs.
            state.phase = InputBatchPhase::Settled;
        }
    }

    /// Called immediately before this batch first writes. Once writing, a transition waits for
    /// its lease and never truncates later bytes or keys of the already admitted batch.
    pub(crate) fn start(&self) -> bool {
        self.start_with(false)
    }

    /// A guarded seam may already hold the terminal lock. It never waits for meta or another
    /// batch there: a busy first admission writes nothing and can be asked for again.
    pub(crate) fn start_inline(&self) -> bool {
        self.start_with(true)
    }

    fn start_with(&self, nonparking: bool) -> bool {
        let mut state = if nonparking {
            match self.state.try_lock() {
                Ok(state) => state,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return false,
            }
        } else {
            self.state.lock().unwrap_or_else(|p| p.into_inner())
        };
        match &state.phase {
            InputBatchPhase::Writing(_) => return true,
            InputBatchPhase::Refused | InputBatchPhase::Settled => return false,
            InputBatchPhase::Queued => {}
        }
        let lease = self.ctx.upgrade().and_then(|ctx| {
            if nonparking {
                FencedWrite::try_enter_fence(&ctx, &self.fence, crate::metrics::now_us()).ok()
            } else {
                FencedWrite::enter_fence(&ctx, &self.fence, crate::metrics::now_us()).ok()
            }
        });
        state.phase = lease.map_or(InputBatchPhase::Refused, InputBatchPhase::Writing);
        matches!(state.phase, InputBatchPhase::Writing(_))
    }
}

impl SupervisorClaim {
    /// The process a proof of this claim named, the contract 2 self-claim's or a scoped claim's
    /// (contract 3 or 4).
    #[must_use]
    pub(crate) fn identity(&self) -> Option<&crate::self_claim::Proven> {
        self.proven
            .as_ref()
            .or_else(|| self.scoped.as_ref().map(|s| &s.proven))
    }

    /// Whether the claim is proven ([`Self::identity`]).
    #[must_use]
    pub(crate) fn is_proven(&self) -> bool {
        self.identity().is_some()
    }

    /// THE PROOF UNDER WHICH THIS CLAIM WRITES WHAT REPORTS ON `turn-end` ([`Self::act_proof_for`]
    /// that duty): its stop, Land hold and NEEDS YOU (`meta set harness_held`, `harness_land`,
    /// `harness_needs`), the `meta prepare harness_act` lease and an act whose duty is `turn-end`.
    /// codex-alab's contract 2 self-claim takes every duty, so its proof is the one it always was;
    /// a proven scoped claim's (contract 3 or 4) is its scoped proof only where its scope takes
    /// `turn-end`; an unproven claim has none. Every other write is admitted under the proof for
    /// the duty it reports on (`control_session::self_claim_write`,
    /// `cmd_meta_harness_act_with_clock`: `harness_activity` under `state`, an act under its
    /// `acts::act_scope`). What was told under this proof is kept only while the standing claim
    /// writes under it again (`SessionMeta::set_supervisor` lets a stop, a Land hold and a NEEDS
    /// YOU set go when a claim drops `turn-end`); [`SessionMeta::self_needs`] and
    /// [`SessionMeta::say_needs_entry`] read through it, and [`SessionMeta::self_held`] and
    /// [`SessionMeta::self_land`] read what the standing claim's [`Self::identity`] told, so a
    /// write is read exactly where it was admitted.
    #[must_use]
    pub(crate) fn act_proof(&self) -> Option<&crate::self_claim::Proven> {
        self.act_proof_for(Some(aterm_agent::harness::contract::Scope::TurnEnd))
    }

    /// THE PROOF UNDER WHICH THIS CLAIM WRITES AN ACT THAT REPORTS ON `duty` (HX-06, one accessor
    /// for both writers): codex-alab's proven contract 2 self-claim writes every act, `duty` or
    /// none; a proven scoped claim (contract 3 or 4) writes one whose duty it
    /// [takes](Self::takes), and none whose act no duty names (`duty` `None`); an unproven claim
    /// writes nothing. Nothing admits on the v2 proof's presence alone ([`Self::proven`] stays
    /// codex-alab's: its admission as Codex, its record copies, the crash rule, the wall answer).
    #[must_use]
    pub(crate) fn act_proof_for(
        &self,
        duty: Option<aterm_agent::harness::contract::Scope>,
    ) -> Option<&crate::self_claim::Proven> {
        let proven = self.identity()?;
        if self.proven.is_some() {
            return Some(proven);
        }
        duty.filter(|duty| self.takes(*duty)).map(|_| proven)
    }

    /// Whether `next` retains this claim's authority: the same holder, connection, proof and
    /// scoped authority (its grant, versions and child), while its lease or requested words may
    /// move, so it names no new authority ([`SessionMeta::set_supervisor`]).
    fn renews_in_place(&self, next: &Self) -> bool {
        let Self {
            holder,
            conn,
            expires_us: _,
            proven,
            scoped,
        } = self;
        *holder == next.holder
            && *conn == next.conn
            && *proven == next.proven
            && crate::harness_claim::same_authority(scoped.as_ref(), next.scoped.as_ref())
    }

    /// Whether the claim takes `duty` on its session (`docs/DESIGN-harness-repos-2026-10-24.md`
    /// §8.1 item 4, §8.3): a registered harness's scoped claim (contract 3 or 4,
    /// [`Self::scoped`]) takes exactly the duties its scope names (under contract 4, its grant),
    /// and aterm keeps every other one; every other claim
    /// (codex-alab's v2 self-claim, any foreign holder's) takes them all, as it always has.
    #[must_use]
    pub(crate) fn takes(&self, duty: aterm_agent::harness::contract::Scope) -> bool {
        self.scoped
            .as_ref()
            .is_none_or(|s| s.claim.scopes.contains(duty))
    }

    /// Whether the claim speaks for its session's boxes and walls, so aterm raises no need for
    /// them while it stands ([`crate::needs::NeedFacts::other_supervisor`]): it [takes](Self::takes)
    /// `approvals`, `questions` and `walls` alike. One fact stands for all three, so a scope naming
    /// only some of them speaks for none (a person hears of a box a harness might press, never
    /// misses one nobody does); grok-alab's `relaunch,upgrade` speaks for none.
    #[must_use]
    pub(crate) fn speaks_for_boxes_and_walls(&self) -> bool {
        use aterm_agent::harness::contract::Scope;
        [Scope::Approvals, Scope::Questions, Scope::Walls]
            .into_iter()
            .all(|duty| self.takes(duty))
    }
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

    /// The field list an unknown-field refusal names (`meta set|unset <field>`), in
    /// [`Self::ALL`]'s order.
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

    /// The stable token [`SessionMeta::set`]/[`SessionMeta::get`] key on and the `meta-change`
    /// payload prints.
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
    TooLong {
        cap: usize,
    },
    /// A title whose percent-encoding is over `cap` bytes: its claim's text is never cut.
    TooLongEncoded {
        cap: usize,
    },
    /// A title from anyone but a person that reads whole as a credential.
    Credential,
    /// A title from anyone but a person in which the harness's turn-end readers read a head
    /// (`NEEDS YOU:`, `Nothing left:`, `Could not finish:`, `LANDED`).
    ProtocolLine,
    /// The title's author is short of a mark token for this long.
    Busy {
        retry_ms: u64,
    },
    NotAQuestionsPolicy,
    NotAnAutopilot,
    HungUp,
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

/// Apply one validated metadata mutation and, on an actual change, record the `meta-change`
/// timeline event (`field=<f> value=<pct|->`).
#[cfg(test)]
pub(crate) fn apply_meta_value(
    ctx: &crate::SessionCtx,
    field: MetaField,
    value: Option<String>,
    by: aterm_slog::Actor,
) -> bool {
    try_apply_meta_value(ctx, field, value, by).unwrap_or(false)
}

fn try_apply_meta_value(
    ctx: &crate::SessionCtx,
    field: MetaField,
    value: Option<String>,
    by: aterm_slog::Actor,
) -> Result<bool, MetaWriteError> {
    if field == MetaField::Autopilot {
        return Ok(apply_autopilot(ctx, value, None, by, None).moved);
    }
    if field == MetaField::Title {
        return title::apply_title_claim(ctx, value, by);
    }
    #[cfg(all(test, unix))]
    if matches!(field, MetaField::Attention | MetaField::Description) {
        attention_delivery_tests::before_effect();
    }
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if matches!(field, MetaField::Attention | MetaField::Description)
        && crate::control::caller_hung_up()
    {
        return Err(MetaWriteError::HungUp);
    }
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
        record_meta(ctx, change, &meta);
    }
    drop(meta);
    if changed && field == MetaField::Autopilot {
        crate::harness_host::ring();
    }
    Ok(moved)
}

/// What one write of the Autopilot pair did ([`apply_autopilot`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AutopilotWrite {
    pub(crate) moved: bool,
    pub(crate) word_moved: bool,
    pub(crate) revision: u64,
}

/// Who set a session's Autopilot pair, and when.
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
        let since = since.trim();
        if since.is_empty() || !since.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let since_ms = since.parse::<i64>().ok()?;
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

/// Write session `ctx`'s Autopilot pair (§9.3.6, item 1).
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

/// What the fire of a timed Cruise found ([`fire_land_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fired {
    Landed { revision: u64 },
    NotYet(i64),
    Moved(Option<i64>),
}

/// The fire is a compare-and-set (§9.3.6, item 5): it lands only while the stored pair is Cruise
/// at `expected_ms` and the wall has reached it.
pub(crate) fn fire_land_at(ctx: &crate::SessionCtx, expected_ms: i64, wall_ms: i64) -> Fired {
    use alab_policy::config::Autopilot;
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

/// The cancel is a compare-and-set too: it clears `land_at` only while a timer is armed
/// (§9.3.3, §9.3.4).
pub(crate) fn cancel_land_at(
    ctx: &crate::SessionCtx,
    surface: crate::autopilot::Surface,
) -> AutopilotWrite {
    use alab_policy::config::Autopilot;
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let armed = meta.autopilot_word() == Some(Autopilot::Cruise) && meta.land_at.is_some();
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

/// [`write_attention_owned`] with the entry's kind (`kind=`, `None` when the writer named none). A
/// move of the `owner=needs` entry settles aterm's own loop's outgoing NEEDS YOU set against it
/// ([`SessionTimeline::settle_harness_needs`]), as the meta settles a self-claim's
/// ([`SessionMeta::set_attention_under`]).
pub(crate) fn write_attention_kind(
    ctx: &crate::SessionCtx,
    owner: &str,
    edit: MetaEdit<'_>,
    kind: Option<aterm_types::needs::AttentionKind>,
    by: aterm_slog::Actor,
) -> Result<bool, AttentionWriteError> {
    let value = validated_attention_value(owner, edit).map_err(AttentionWriteError::Meta)?;
    #[cfg(all(test, unix))]
    attention_delivery_tests::before_effect();
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if crate::control::caller_hung_up() {
        return Err(AttentionWriteError::Meta(MetaWriteError::HungUp));
    }
    // A wait written on the connection a proven claim was bound to is its claimant's own, though
    // the claim ended first: it lapses with that claim or that connection
    // ([`AttentionOwners::lapse_under`]); `put` tags a wait alone.
    let under = meta
        .claimant_on(crate::control::serving_connection())
        .cloned();
    let moved = meta
        .set_attention_under(owner, value, kind, under)
        .map_err(|AttentionOwnersFull| AttentionWriteError::OwnersFull)?;
    let changed = moved.effective;
    meta.driver_writes |= MetaField::Attention.bit();
    let supervisor = crate::task_record::owner_moves_task(owner);
    let needs_moved = moved.entry && owner == crate::harness_host::NEEDS_ATTENTION_OWNER;
    let (mut task_moved, mut log_owed) = (false, false);
    if changed || supervisor || needs_moved {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        if needs_moved {
            timeline.settle_harness_needs(
                meta.attention_owners
                    .get(crate::harness_host::NEEDS_ATTENTION_OWNER),
            );
        }
        if changed {
            let change = MetaChange::written_by(
                MetaField::Attention.wire_name(),
                meta.attention.as_deref(),
                by,
            );
            timeline.record_meta_change(change, &meta);
        }
        if supervisor {
            let asks = crate::task_record::asks_standing(&meta, &timeline);
            task_moved = timeline.note_task_attention(asks);
        }
        log_owed = timeline.slog_take_wake();
    }
    drop(meta);
    Ok(moved.any() || task_moved || log_owed)
}

/// The supervisor's attention entry, written inside a task mark
/// (`task_record::mark_session_task_with`).
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
        let change = MetaChange::written_by(
            MetaField::Attention.wire_name(),
            meta.attention.as_deref(),
            aterm_slog::Actor::Aterm,
        );
        timeline.record_meta_change(change, meta);
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

/// What a granted claim did ([`publish_claim_supervisor_as`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claimed {
    pub(crate) changed: bool,
    pub(crate) replaced: Option<String>,
    pub(crate) revision: u64,
    /// [`SessionMeta::claim_placements`] after this claim: what its record read is ordered by.
    pub(crate) placement: u64,
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
#[cfg(test)]
pub(crate) fn claim_supervisor_as(
    ctx: &crate::SessionCtx,
    holder: &str,
    conn: Option<u64>,
    expires_us: Option<u64>,
    proven: Option<crate::self_claim::Proven>,
    now_us: u64,
    by: aterm_slog::Actor,
) -> Result<Claimed, String> {
    publish_claim_supervisor_as(ctx, holder, conn, expires_us, proven, now_us, by)
        .map(SupervisorTransition::acknowledge)
}

/// Publish a claim and capture the prior writers. Only the control worker acknowledging this
/// transition calls [`SupervisorTransition::acknowledge`]; GUI observers never wait on it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn publish_claim_supervisor_as(
    ctx: &crate::SessionCtx,
    holder: &str,
    conn: Option<u64>,
    expires_us: Option<u64>,
    proven: Option<crate::self_claim::Proven>,
    now_us: u64,
    by: aterm_slog::Actor,
) -> Result<SupervisorTransition<Claimed>, String> {
    claim_supervisor_with(ctx, holder, conn, expires_us, proven, None, now_us, by)
}

/// `meta set supervisor <id>@<pid> contract=3 scope=…`: a rostered harness's proven scoped claim
/// ([`crate::harness_claim`]), bound to `conn`, under the same [`claim_rule`] as a proof.
#[cfg(test)]
pub(crate) fn claim_supervisor_scoped(
    ctx: &crate::SessionCtx,
    holder: &str,
    conn: Option<u64>,
    scoped: crate::harness_claim::Scoped,
    now_us: u64,
    by: aterm_slog::Actor,
) -> Result<Claimed, String> {
    publish_claim_supervisor_scoped(ctx, holder, conn, scoped, now_us, by)
        .map(SupervisorTransition::acknowledge)
}

#[cfg(test)]
pub(crate) fn publish_claim_supervisor_scoped(
    ctx: &crate::SessionCtx,
    holder: &str,
    conn: Option<u64>,
    scoped: crate::harness_claim::Scoped,
    now_us: u64,
    by: aterm_slog::Actor,
) -> Result<SupervisorTransition<Claimed>, String> {
    publish_claim_supervisor_admitted(ctx, holder, conn, scoped, None, now_us, by)
}

/// [`publish_claim_supervisor_scoped`] with the version of the build the claim's proof admitted
/// (`admitted`, [`crate::harness_claim::ClaimProof::version`]), kept beside the claim in the
/// same hold that places it ([`SessionMeta::verified_version`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn publish_claim_supervisor_admitted(
    ctx: &crate::SessionCtx,
    holder: &str,
    conn: Option<u64>,
    scoped: crate::harness_claim::Scoped,
    admitted: Option<String>,
    now_us: u64,
    by: aterm_slog::Actor,
) -> Result<SupervisorTransition<Claimed>, String> {
    claim_supervisor_with(
        ctx,
        holder,
        conn,
        None,
        None,
        Some((scoped, admitted)),
        now_us,
        by,
    )
}

/// A published authority transition and the exact prior revisions its explicit ACK owes.
/// Dropping or observing a transition never parks a GUI/event/EOF lane.
#[must_use]
pub(crate) struct SupervisorTransition<T> {
    published: T,
    drain: FencedInputDrain,
}

impl<T> SupervisorTransition<T> {
    fn unchanged(published: T) -> Self {
        Self {
            published,
            drain: FencedInputDrain::default(),
        }
    }

    pub(crate) fn published(self) -> T {
        self.published
    }

    pub(crate) fn published_ref(&self) -> &T {
        &self.published
    }

    /// Called only by the explicit control reply lane, with no GUI-needed locks held.
    pub(crate) fn acknowledge(self) -> T {
        self.drain.wait();
        self.published
    }
}

#[allow(clippy::too_many_arguments)]
fn claim_supervisor_with(
    ctx: &crate::SessionCtx,
    holder: &str,
    conn: Option<u64>,
    expires_us: Option<u64>,
    proven: Option<crate::self_claim::Proven>,
    scoped: Option<(crate::harness_claim::Scoped, Option<String>)>,
    now_us: u64,
    by: aterm_slog::Actor,
) -> Result<SupervisorTransition<Claimed>, String> {
    // A scoped claim comes with the version its proof admitted, kept beside it below.
    let (scoped, verified) = match scoped {
        Some((scoped, admitted)) => {
            let verified = admitted.map(|version| (scoped.proven.clone(), version));
            (Some(scoped), verified)
        }
        None => (None, None),
    };
    let looked = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        meta.live_claim(now_us).and_then(|c| c.identity().cloned())
    };
    let looked_gone = looked.as_ref().is_some_and(|p| !p.alive());
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let standing = meta
        .live_claim(now_us)
        .map(|c| (c.holder.clone(), c.identity().cloned()));
    let gone = looked_gone
        && standing
            .as_ref()
            .is_some_and(|(_, p)| p.is_some() && *p == looked);
    let identity = proven
        .clone()
        .or_else(|| scoped.as_ref().map(|s| s.proven.clone()));
    let rule = claim_rule(
        standing.as_ref().map(|(h, p)| (h.as_str(), p.is_some())),
        gone,
        holder,
        identity.is_some(),
    );
    if rule == ClaimRule::Busy {
        return Err(standing.map(|(h, _)| h).unwrap_or_default());
    }
    let was_proven = standing.as_ref().is_some_and(|(_, p)| p.is_some());
    let now_proven = identity.is_some();
    let renewed_identity_changed =
        rule == ClaimRule::Renew && standing.as_ref().is_some_and(|(_, old)| *old != identity);
    let renewed_scoped_changed = rule == ClaimRule::Renew
        && !crate::harness_claim::same_authority(meta.live_scoped(now_us), scoped.as_ref());
    let stream_connection_changed = meta.live_claim(now_us).is_some_and(|claim| {
        claim.conn != conn
            && claim.scoped.as_ref().is_some_and(|scoped| {
                scoped.claim.contract == aterm_agent::harness::contract::UNIVERSAL
            })
    });
    let prior = meta.supervisor_revision;
    let drain = ctx.fenced_writes.capture_drain();
    let (revision, lapsed) = meta.set_supervisor(
        Some(SupervisorClaim {
            holder: holder.to_string(),
            conn,
            expires_us,
            proven,
            scoped,
        }),
        now_us,
    );
    meta.supervisor_verified = verified;
    let placement = meta.claim_placements;
    // A renewal in place keeps the revision and supersedes no admitted write, so its ACK owes no
    // drain; a drain of the revision it keeps would also wait on every write admitted after it.
    let drain = if revision == prior {
        FencedInputDrain::default()
    } else {
        drain
    };
    let supervisor_changed = match rule {
        ClaimRule::Renew => was_proven != now_proven,
        ClaimRule::Take | ClaimRule::Replace | ClaimRule::Busy => true,
    };
    // A lapse takes waits alone, which move no task record: only the effective text is recorded.
    if lapsed.effective {
        record_attention(ctx, &meta, by);
    }
    if supervisor_changed {
        record_supervisor(ctx, &meta, Some(holder), by);
    } else if renewed_identity_changed || renewed_scoped_changed || stream_connection_changed {
        // A renewal can end a stream or its stand-down without writing a `meta-change`.
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        if renewed_identity_changed {
            timeline.forget_holder_acts();
        }
        if timeline.settle_stream_claim(&meta) {
            timeline.ring();
        }
    }
    let changed =
        supervisor_changed || renewed_identity_changed || renewed_scoped_changed || lapsed.any();
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
    drop(meta);
    let replaced = (rule == ClaimRule::Replace)
        .then(|| standing.map(|(h, _)| h))
        .flatten();
    Ok(SupervisorTransition {
        published: Claimed {
            changed,
            replaced,
            revision,
            placement,
        },
        drain,
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
    if meta.live_scoped(now_us).is_some() {
        return Some(SCOPED_INPUT_DENIED.to_string());
    }
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
    state: Arc<FencedWriteState>,
    pub(crate) native: std::sync::Mutex<crate::native_act::Operations>,
}

#[derive(Debug, Default)]
struct FencedWriteState {
    inflight: std::sync::Mutex<std::collections::BTreeMap<u64, Vec<ActiveInput>>>,
    settled: std::sync::Condvar,
}

impl FencedWrites {
    /// Reserve the native position while MetaLock still excludes fresh input admission.
    /// The marker conflicts with every later writer, including the same actor/revision.
    pub(crate) fn reserve_native(
        &self,
        revision: u64,
        id: &str,
        boundary: aterm_types::update_boundary::Held<'static>,
    ) -> Result<NativeCustody, String> {
        let state = Arc::clone(&self.state);
        let mut inflight = state.inflight.lock().unwrap_or_else(|p| p.into_inner());
        if inflight
            .values()
            .flatten()
            .any(|active| active.native.is_some())
        {
            return Err("ERR busy native operation unresolved\n".into());
        }
        let active = ActiveInput {
            writer: InputWriter::Builtin(String::new()),
            duty: None,
            inline: true,
            native: Some(id.to_string()),
        };
        inflight.entry(revision).or_default().push(active.clone());
        drop(inflight);
        Ok(NativeCustody {
            state,
            revision,
            active,
            id: id.to_string(),
            boundary: std::sync::Mutex::new(Some(Arc::new(boundary))),
        })
    }

    /// Called with meta held, so no write can join the superseded revision after this capture.
    fn revisions(&self) -> Vec<u64> {
        self.state
            .inflight
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .copied()
            .collect()
    }

    /// Captured while MetaLock excludes admission to the superseded revision.
    fn capture_drain(&self) -> FencedInputDrain {
        FencedInputDrain {
            state: Some(Arc::clone(&self.state)),
            revisions: self.revisions(),
        }
    }
}

/// One native operation's exact position in the same ordering as fenced input. The owning
/// operation retains this after ACK loss and EOF, until its actual outcome is proved or its close
/// seals it `closed` once no input byte of it can follow.
#[derive(Debug)]
pub(crate) struct NativeCustody {
    state: Arc<FencedWriteState>,
    revision: u64,
    active: ActiveInput,
    id: String,
    /// The app handoff's hold, from admission until [`Self::release_handoff_guard`] or the
    /// custody's drop.
    boundary: std::sync::Mutex<Option<Arc<aterm_types::update_boundary::Held<'static>>>>,
}

impl NativeCustody {
    /// Observe earlier input without parking a cancellation owner on a cloned
    /// custody guard. Contention is not evidence that predecessors have drained.
    #[cfg(unix)]
    pub(crate) fn predecessors_drained(&self) -> bool {
        let inflight = match self.state.inflight.try_lock() {
            Ok(inflight) => inflight,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return false,
        };
        !inflight
            .values()
            .flatten()
            .any(|active| active != &self.active)
    }
    /// Keep the app handoff fenced after input custody drains, while a terminal
    /// receipt becomes durable. This retains the existing hold without racing
    /// a new admission against a pending handoff. None once it was released.
    pub(crate) fn handoff_guard(&self) -> Option<Arc<aterm_types::update_boundary::Held<'static>>> {
        self.boundary
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    /// Stop holding the app handoff for an operation that can start nothing more, while this
    /// custody keeps its input position: the handoff goes on once no clone of the guard is held
    /// ([`Self::handoff_guard`]), and a reader with none takes a fresh hold or answers Unknown.
    pub(crate) fn release_handoff_guard(&self) {
        let held = self
            .boundary
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        drop(held);
    }
    pub(crate) fn drain(&self) -> NativeDrain {
        NativeDrain {
            state: Arc::clone(&self.state),
            revision: self.revision,
            id: self.id.clone(),
        }
    }
    /// Only expendable control workers park here, holding no metadata, GUI or input locks.
    /// A departure before admission cancels the reservation; a started move never calls this.
    pub(crate) fn await_predecessors(&self, deadline_us: u64) -> bool {
        let mut inflight = self
            .state
            .inflight
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        while inflight
            .values()
            .flatten()
            .any(|active| active != &self.active)
        {
            let Some(left) = crate::native_act::remaining(deadline_us) else {
                return false;
            };
            if aterm_control::hangup::caller_hung_up() {
                return false;
            }
            let park = aterm_control::hangup::hangup_park(left);
            inflight = self
                .state
                .settled
                .wait_timeout(inflight, park)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        crate::native_act::remaining(deadline_us).is_some()
            && !aterm_control::hangup::caller_hung_up()
    }
}

/// A completion reader owns no lease and waits only this operation's marker, never later
/// revisions, terminal answers, response observation or a different operation's input.
#[derive(Clone, Debug)]
pub(crate) struct NativeDrain {
    state: Arc<FencedWriteState>,
    revision: u64,
    id: String,
}

impl NativeDrain {
    /// Observation never parks a request lane. Contention and pending actual custody both
    /// remain Unknown; only absence of this exact marker proves the terminal receipt ready.
    pub(crate) fn is_drained(&self) -> bool {
        let inflight = match self.state.inflight.try_lock() {
            Ok(inflight) => inflight,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return false,
        };
        !inflight.get(&self.revision).is_some_and(|active| {
            active
                .iter()
                .any(|active| active.native.as_deref() == Some(self.id.as_str()))
        })
    }
}

impl Drop for NativeCustody {
    fn drop(&mut self) {
        let mut inflight = self
            .state
            .inflight
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(active) = inflight.get_mut(&self.revision) {
            active.retain(|active| active != &self.active);
            if active.is_empty() {
                inflight.remove(&self.revision);
            }
        }
        drop(inflight);
        self.state.settled.notify_all();
    }
}

#[derive(Default)]
struct FencedInputDrain {
    state: Option<Arc<FencedWriteState>>,
    revisions: Vec<u64>,
}

impl FencedInputDrain {
    fn wait(self) {
        let Some(writes) = self.state else {
            return;
        };
        let inflight = writes.inflight.lock().unwrap_or_else(|p| p.into_inner());
        drop(
            writes
                .settled
                .wait_while(inflight, |inflight| {
                    self.revisions
                        .iter()
                        .any(|revision| inflight.contains_key(revision))
                })
                .unwrap_or_else(|p| p.into_inner()),
        );
    }
}

/// One admitted fenced write, counted in [`FencedWrites`] from the fence's read until it drops.
#[derive(Debug)]
pub(crate) struct FencedWrite {
    state: Arc<FencedWriteState>,
    revision: u64,
    fence: SupervisorInputFence,
    active: ActiveInput,
}

impl FencedWrite {
    /// Admit one write under `holder`'s fence.
    pub(crate) fn enter(
        ctx: &crate::SessionCtx,
        holder: &str,
        now_us: u64,
    ) -> Result<Self, String> {
        Self::enter_fence(
            ctx,
            &SupervisorInputFence::Legacy(holder.to_string()),
            now_us,
        )
    }

    /// Revalidate the carried grant and count this actual write at its admitted revision.
    pub(crate) fn enter_fence(
        ctx: &crate::SessionCtx,
        fence: &SupervisorInputFence,
        now_us: u64,
    ) -> Result<Self, String> {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        Self::enter_locked(ctx, fence, now_us, meta, false)
    }

    fn try_enter_fence(
        ctx: &crate::SessionCtx,
        fence: &SupervisorInputFence,
        now_us: u64,
    ) -> Result<Self, String> {
        let meta = match ctx.meta.try_lock() {
            Ok(meta) => meta,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return Err(SCOPED_INPUT_BUSY.to_string()),
        };
        Self::enter_locked(ctx, fence, now_us, meta, true)
    }

    fn enter_locked(
        ctx: &crate::SessionCtx,
        fence: &SupervisorInputFence,
        now_us: u64,
        meta: MetaGuard<'_>,
        nonparking: bool,
    ) -> Result<Self, String> {
        if let Some(refusal) = fence.refusal(ctx, &meta, now_us) {
            return Err(refusal);
        }
        let revision = meta.supervisor_revision;
        let active = ActiveInput::of(fence, nonparking);
        let state = Arc::clone(&ctx.fenced_writes.state);
        let mut inflight = if nonparking {
            match state.inflight.try_lock() {
                Ok(inflight) => inflight,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err(SCOPED_INPUT_BUSY.to_string());
                }
            }
        } else {
            state.inflight.lock().unwrap_or_else(|p| p.into_inner())
        };
        if inflight
            .values()
            .flatten()
            .any(|old| old.conflicts(&active))
        {
            return Err(SCOPED_INPUT_BUSY.to_string());
        }
        inflight.entry(revision).or_default().push(active.clone());
        drop(inflight);
        drop(meta);
        Ok(Self {
            state,
            revision,
            fence: fence.clone(),
            active,
        })
    }

    /// The immutable grant a queued receiver must revalidate before delivery.
    pub(crate) fn fence(&self) -> &SupervisorInputFence {
        &self.fence
    }
}

impl Drop for FencedWrite {
    fn drop(&mut self) {
        let writes = &self.state;
        let mut inflight = writes.inflight.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(writes) = inflight.get_mut(&self.revision) {
            if let Some(at) = writes.iter().position(|active| *active == self.active) {
                writes.remove(at);
            }
            if writes.is_empty() {
                inflight.remove(&self.revision);
            }
        }
        drop(inflight);
        writes.settled.notify_all();
    }
}

/// Release a proven claim when its holder process has exited or left the foreground.
pub(crate) fn publish_release_proven_if_gone(
    ctx: &crate::SessionCtx,
    foreground: i32,
    now_us: u64,
) -> SupervisorTransition<bool> {
    let proven = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        meta.live_claim(now_us).and_then(|c| c.identity().cloned())
    };
    let Some(proven) = proven else {
        return SupervisorTransition::unchanged(false);
    };
    if i64::from(foreground) == i64::from(proven.pid) && proven.alive() {
        return SupervisorTransition::unchanged(false);
    }
    release_supervisor_if(ctx, now_us, aterm_slog::Actor::Aterm, |claim| {
        claim.identity() == Some(&proven)
    })
}

#[cfg(test)]
pub(crate) fn release_proven_if_gone(
    ctx: &crate::SessionCtx,
    foreground: i32,
    now_us: u64,
) -> bool {
    publish_release_proven_if_gone(ctx, foreground, now_us).published()
}

/// Clear the session's supervisor claim.
pub(crate) fn publish_release_supervisor(
    ctx: &crate::SessionCtx,
    conn: Option<u64>,
    now_us: u64,
    by: aterm_slog::Actor,
) -> SupervisorTransition<bool> {
    release_supervisor_if(ctx, now_us, by, |claim| {
        conn.is_none() || claim.conn == conn
    })
}

#[cfg(test)]
pub(crate) fn release_supervisor(
    ctx: &crate::SessionCtx,
    conn: Option<u64>,
    now_us: u64,
    by: aterm_slog::Actor,
) -> bool {
    publish_release_supervisor(ctx, conn, now_us, by).acknowledge()
}

/// Connection `conn` closed at `now_us`: take the waits its proven claimant wrote on it and the
/// stop and the Land hold it told ([`SessionMeta::close_connection`]) and record the attention
/// `meta-change` when the shown entry moved; whether anything moved.
pub(crate) fn close_claim_connection(ctx: &crate::SessionCtx, conn: u64, now_us: u64) -> bool {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let (lapsed, held) = meta.close_connection(conn, now_us);
    if lapsed.effective {
        record_attention(ctx, &meta, aterm_slog::Actor::Aterm);
    }
    lapsed.any() || held
}

/// Release the session's claim only while it still holds the scoped authority `scoped` (a claim
/// whose proof no longer stands, [`crate::harness_claim::reprove`]).
pub(crate) fn publish_release_scoped(
    ctx: &crate::SessionCtx,
    scoped: &crate::harness_claim::Scoped,
    now_us: u64,
    by: aterm_slog::Actor,
) -> SupervisorTransition<bool> {
    release_supervisor_if(ctx, now_us, by, |claim| {
        crate::harness_claim::same_authority(claim.scoped.as_ref(), Some(scoped))
    })
}

/// Hand back what closed control connection `conn` held on `ctx`: its live claim and its
/// claimant binding. True when either moved. The control EOF does this at once unless a native
/// operation kept the claim ([`crate::native_act::retain_exec_claim`]); that operation does it
/// when its custody ends.
///
/// A proven contract 4 claim is not released here: it is kept through the re-exec grace
/// (`grace_us`, the wire contract's, [`crate::control::reexec_grace_us`]; `None` where this
/// platform proves no such claim), with the grace's end as its expiry
/// ([`SessionMeta::keep_through_grace`]). The connection's own bindings go as always, and what
/// its claimant told stands while that claim does ([`SessionMeta::close_connection`]). Arming
/// the grace counts as a move, so the window reads the new expiry and sets its lapse timer.
pub(crate) fn release_closed_connection(
    ctx: &crate::SessionCtx,
    conn: u64,
    now_us: u64,
    grace_us: Option<u64>,
) -> bool {
    let grace = grace_us.map_or(Grace::None, |grace_us| {
        let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let grace = meta.keep_through_grace(conn, now_us, grace_us);
        #[cfg(unix)]
        if grace == Grace::Armed {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .settle_stream_binding(&meta);
        }
        grace
    });
    let released = grace == Grace::None
        && publish_release_supervisor(ctx, Some(conn), now_us, aterm_slog::Actor::Aterm)
            .published();
    close_claim_connection(ctx, conn, now_us) || released || grace == Grace::Armed
}

/// `meta unset supervisor holder=<h>`.
pub(crate) fn publish_release_supervisor_held_by(
    ctx: &crate::SessionCtx,
    holder: &str,
    now_us: u64,
    by: aterm_slog::Actor,
) -> SupervisorTransition<bool> {
    release_supervisor_if(ctx, now_us, by, |claim| claim.holder == holder)
}

#[cfg(test)]
pub(crate) fn release_supervisor_held_by(
    ctx: &crate::SessionCtx,
    holder: &str,
    now_us: u64,
    by: aterm_slog::Actor,
) -> bool {
    publish_release_supervisor_held_by(ctx, holder, now_us, by).acknowledge()
}

fn release_supervisor_if(
    ctx: &crate::SessionCtx,
    now_us: u64,
    by: aterm_slog::Actor,
    ours: impl FnOnce(&SupervisorClaim) -> bool,
) -> SupervisorTransition<bool> {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let Some(claim) = meta.supervisor.as_ref() else {
        return SupervisorTransition::unchanged(false);
    };
    if !ours(claim) {
        return SupervisorTransition::unchanged(false);
    }
    let was_live = meta.live_supervisor(now_us).is_some();
    let proven = meta
        .supervisor
        .as_ref()
        .and_then(|claim| claim.proven.clone());
    let drain = ctx.fenced_writes.capture_drain();
    let (revision, lapsed) = meta.set_supervisor(None, now_us);
    if lapsed.effective {
        record_attention(ctx, &meta, by);
    }
    if was_live {
        record_supervisor(ctx, &meta, None, by);
    } else {
        // A claim whose `ttl=` had passed is released with no `meta-change`, and no lapse
        // record follows a claim that is gone: its stream's stand-down ends here.
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        if timeline.settle_stream_claim(&meta) {
            timeline.ring();
        }
    }
    drop(meta);
    if let Some(proven) = proven {
        crate::self_claim::released(ctx, &proven, revision);
    }
    crate::harness_host::note_claim_released();
    SupervisorTransition {
        published: was_live || lapsed.any(),
        drain,
    }
}

/// Remove a claim whose expiry passed at `now_us` (a `ttl=` lease, or the re-exec grace of a
/// contract 4 claim whose connection closed, [`SessionMeta::keep_through_grace`]) and record
/// `meta-change field=supervisor value=-`, as a release does. The record ends a stream's
/// stand-down and wakes the session's readers.
pub(crate) fn publish_lapse_supervisor(
    ctx: &crate::SessionCtx,
    now_us: u64,
) -> SupervisorTransition<bool> {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if meta.supervisor_expiry().is_none_or(|at| at > now_us) {
        return SupervisorTransition::unchanged(false);
    }
    let proven = meta
        .supervisor
        .as_ref()
        .and_then(|claim| claim.proven.clone());
    let drain = ctx.fenced_writes.capture_drain();
    let (revision, lapsed) = meta.set_supervisor(None, now_us);
    if lapsed.effective {
        record_attention(ctx, &meta, aterm_slog::Actor::Aterm);
    }
    record_supervisor(ctx, &meta, None, aterm_slog::Actor::Aterm);
    drop(meta);
    if let Some(proven) = proven {
        crate::self_claim::released(ctx, &proven, revision);
    }
    crate::harness_host::note_claim_released();
    SupervisorTransition {
        published: true,
        drain,
    }
}

#[cfg(test)]
pub(crate) fn lapse_supervisor(ctx: &crate::SessionCtx, now_us: u64) -> bool {
    publish_lapse_supervisor(ctx, now_us).published()
}

/// The longest `ttl=` a `meta set supervisor` accepts (ms).
pub(crate) const SUPERVISOR_TTL_MAX_MS: u64 = 600_000;

/// A carried supervisor row (`SessionRecord::supervisor`, `"<holder> <remaining_ms>"`).
#[must_use]
pub(crate) fn parse_carried_supervisor(row: &str) -> Option<(&str, u64)> {
    let (holder, ms) = row.split_once(' ')?;
    if holder == "-" || !valid_owner_token(holder) || !ms.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ms = ms.parse::<u64>().ok().filter(|ms| *ms > 0)?;
    Some((holder, ms.min(SUPERVISOR_TTL_MAX_MS)))
}

/// A carried attention row (`SessionRecord::attention_owners`,
/// `"<owner> [kind=<ask|wait>] <pct-text>"`).
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

/// Put back the claim and escalations a seamless update carried onto an adopted session's fresh
/// meta.
pub(crate) fn seed_carried_claims(
    meta: &MetaLock,
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
        meta.set_supervisor(
            Some(SupervisorClaim {
                holder: holder.to_string(),
                conn: None,
                expires_us: Some(now_us.saturating_add(ms.saturating_mul(1000))),
                proven: None,
                scoped: None,
            }),
            now_us,
        );
    }
    let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
    let by = aterm_slog::Actor::Aterm;
    if attention_moved {
        let change = MetaChange::written_by(
            MetaField::Attention.wire_name(),
            meta.attention.as_deref(),
            by,
        );
        timeline.record_meta_change(change, &meta);
    }
    if let Some((holder, _)) = claim {
        let change = MetaChange::written_by(SUPERVISOR_FIELD, Some(holder), by);
        timeline.record_meta_change(change, &meta);
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

/// The text a timeline holds of the folder `cwd` ([`SessionTimeline::cwd_text`]): bounded as the
/// log's `cwd=` is; `None` for none, an empty one, or one nothing of which survives the bound.
fn held_cwd(cwd: Option<&str>) -> Option<String> {
    cwd.filter(|c| !c.is_empty())
        .and_then(|c| aterm_slog::BoundedText::new(aterm_slog::Key::Cwd, c))
        .map(|c| c.as_str().to_owned())
}

/// The supervisor key's wire name, as its `meta-change` names it.
pub(crate) const SUPERVISOR_FIELD: &str = "supervisor";

/// The pin's wire name, as its `meta-change` names it (`pin`, `unpin`).
pub(crate) const PINNED_FIELD: &str = "pinned";

/// A pin as its `meta-change` says it: `1` pinned, none unpinned.
pub(crate) const fn pin_value(pinned: bool) -> Option<&'static str> {
    if pinned { Some("1") } else { None }
}

/// Pin (or with `pinned` false, unpin) `ctx`'s session, written by `by`; whether it moved. A pin
/// on a pinned session, or an unpin on an unpinned one, changes nothing and records nothing.
pub(crate) fn set_pinned(ctx: &crate::SessionCtx, pinned: bool, by: aterm_slog::Actor) -> bool {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.pin_written = true;
    if meta.pinned == pinned {
        return false;
    }
    meta.pinned = pinned;
    record_meta(
        ctx,
        MetaChange::written_by(PINNED_FIELD, pin_value(pinned), by),
        &meta,
    );
    true
}

/// One `meta-change`'s typed facts as its writer knows them, for the session log's mark
/// ([`SessionTimeline::record_meta_change`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct MetaChange<'a> {
    pub(crate) field: &'static str,
    pub(crate) value: Option<&'a str>,
    pub(crate) by: aterm_slog::Actor,
    pub(crate) land_at: Option<i64>,
    /// A closed word ([`aterm_slog::vocab::WHY_WORDS`]) saying why the field reads as it does now:
    /// the supervisor's `confirmed` or `unconfirmed` agent ([`record_supervisor_act`]).
    pub(crate) why: Option<&'static str>,
}

impl<'a> MetaChange<'a> {
    /// `field` now storing `value`, written by `by`.
    pub(crate) const fn written_by(
        field: &'static str,
        value: Option<&'a str>,
        by: aterm_slog::Actor,
    ) -> Self {
        MetaChange {
            field,
            value,
            by,
            land_at: None,
            why: None,
        }
    }

    /// This change with the Autopilot pair's timer, as its row says it.
    pub(crate) const fn with_land_at(self, land_at: Option<i64>) -> Self {
        MetaChange { land_at, ..self }
    }

    /// This change with its closed `why=` word.
    pub(crate) const fn with_why(self, why: &'static str) -> Self {
        MetaChange {
            why: Some(why),
            ..self
        }
    }
}

fn record_meta(ctx: &crate::SessionCtx, change: MetaChange<'_>, meta: &SessionMeta) {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record_meta_change(change, meta);
}

fn record_autopilot(ctx: &crate::SessionCtx, meta: &SessionMeta, by: aterm_slog::Actor) {
    let change = MetaChange::written_by(
        MetaField::Autopilot.wire_name(),
        meta.autopilot.as_deref(),
        by,
    )
    .with_land_at(meta.land_at);
    record_meta(ctx, change, meta);
}

fn record_attention(ctx: &crate::SessionCtx, meta: &SessionMeta, by: aterm_slog::Actor) {
    let change = MetaChange::written_by(
        MetaField::Attention.wire_name(),
        meta.attention.as_deref(),
        by,
    );
    record_meta(ctx, change, meta);
}

/// The longest re-exec gap, µs (`SessionTimeline::self_reexec_since_us`).
pub(crate) const SELF_REEXEC_GAP_US: u64 = 60_000_000;

fn record_supervisor(
    ctx: &crate::SessionCtx,
    meta: &SessionMeta,
    holder: Option<&str>,
    by: aterm_slog::Actor,
) {
    let change = MetaChange::written_by(SUPERVISOR_FIELD, holder, by);
    // This mark says no act: the next read under a claim tells the act again (H6 rule 3).
    crate::agent_act::forget_told(ctx.self_id.as_str());
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.forget_holder_acts();
    timeline.note_supervisor_moved(holder.is_some(), crate::metrics::now_us());
    timeline.record_meta_change(change, meta);
}

/// The session log's word on what the harness may do in `ctx`'s session now, as the host's read
/// of its agent's image moved to it (`why` is [`crate::agent_act::why`]'s): `mark - meta
/// field=supervisor value=<holder> why=confirmed|unconfirmed`, by the harness. Words only: the
/// image's path is never written.
pub(crate) fn record_supervisor_act(ctx: &crate::SessionCtx, why: &'static str) {
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let holder = meta.live_supervisor(crate::metrics::now_us());
    let change =
        MetaChange::written_by(SUPERVISOR_FIELD, holder, aterm_slog::Actor::Harness).with_why(why);
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record_meta_change(change, &meta);
}

/// Put a carried identity back onto a session already live here, which a reader may have seen
/// bare; a field a driver wrote since is kept.
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
        let change = MetaChange::written_by(name, meta.get(name), aterm_slog::Actor::Aterm);
        record_meta(ctx, change, &meta);
    }
    if !meta.pin_written && meta.pinned != carried.pinned {
        meta.pinned = carried.pinned;
        moved = true;
        let change = MetaChange::written_by(
            PINNED_FIELD,
            pin_value(meta.pinned),
            aterm_slog::Actor::Aterm,
        );
        record_meta(ctx, change, &meta);
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
    try_apply_meta_value(ctx, field, value, by)
}

/// A driver's commit of `field` that states nothing (a rename field committed as it opened,
/// `crate::app_rename::TabRenameEdit::seed`): no claim and no record, but the field counts as
/// written by a driver, as after any write, so the restore graft keeps its hands off it
/// ([`SessionMeta::driver_wrote`]).
pub(crate) fn note_unstated_meta_write(ctx: &crate::SessionCtx, field: MetaField) {
    ctx.meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .driver_writes |= field.bit();
}

/// What an adopted session's recorders say about the time before the seamless update that
/// adopted it.
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

/// Published foreground custody for an input decision, independent of ordinary phase or screen
/// revisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundStamp {
    generation: u64,
    pgid: i32,
    holder: Option<u64>,
    agent: aterm_phase::Program,
}

/// Foreground custody and accepted word state before an upgrade view reads records off-thread.
/// Unlike an input stamp this also names an unclassified foreground, and the last accepted word
/// rejects an older read even when requests repeat within the same recorded second. A local
/// revision distinguishes accepted operations without changing the process's stable owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpgradeStamp {
    generation: u64,
    pgid: i32,
    holder: Option<u64>,
    agent: Option<aterm_phase::Program>,
    conversation: Option<String>,
    word: Option<(aterm_agent::harness::upgrade::Request, u64)>,
    word_revision: u64,
}

/// The stable owner admitted with an upgrade row. Departing and returning to the foreground
/// changes the publication generation, but the unchanged process keeps its image token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UpgradeOwner {
    pgid: i32,
    holder: Option<u64>,
    agent: aterm_phase::Program,
}

impl UpgradeStamp {
    pub(crate) fn is_current(&self, timeline: &SessionTimeline) -> bool {
        self.generation < u64::MAX
            && self.word_revision < u64::MAX
            && *self == timeline.upgrade_stamp()
    }

    /// Bind a row only where this captured foreground actually names the lane's agent.
    pub(crate) fn owner(
        &self,
        row: &aterm_agent::harness::upgrade_drive::Row,
    ) -> Option<UpgradeOwner> {
        use aterm_agent::harness::upgrade_drive::NOT_FOREGROUND;
        (self.pgid > 0
            && self.agent == Some(row.agent.program())
            && self
                .conversation
                .as_deref()
                .is_none_or(|session| session == row.session)
            && row.aside != NOT_FOREGROUND
            && row.wait != NOT_FOREGROUND)
            .then_some(UpgradeOwner {
                pgid: self.pgid,
                holder: self.holder,
                agent: row.agent.program(),
            })
    }
}

impl UpgradeOwner {
    fn fronts(
        self,
        timeline: &SessionTimeline,
        row: &aterm_agent::harness::upgrade_drive::Row,
    ) -> bool {
        self.pgid == timeline.agent.program_pgid
            && self.holder.is_none_or(|token| {
                timeline
                    .current_holder_image()
                    .and_then(|image| image.token)
                    == Some(token)
            })
            && crate::harness_host::identified_agent(&timeline.agent)
                .is_none_or(|agent| agent == self.agent)
            && timeline
                .upgrade_conversation()
                .is_none_or(|session| row.session.is_empty() || session == row.session)
    }
}

/// How long the outputs after a holder's first look for Claude's effort hint keep reading the
/// live screen for it ([`SessionTimeline::effort_hint_looks`]).
pub(crate) const EFFORT_HINT_LOOK: std::time::Duration = std::time::Duration::from_secs(60);

/// What a worker's launcher holds its launch for ([`SessionTimeline::worker_pool_hold`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerPoolHold {
    /// When the first of the pool's rows opens, Unix seconds.
    pub(crate) until: i64,
    /// Why nothing can take the launch now, in this crate's own words.
    pub(crate) because: &'static str,
}

/// A session's lifecycle state under one lock: its session log, whose rows are the timeline
/// ([`crate::session_timeline_view`]), and what the session publishes beside it.
#[derive(Default)]
pub(crate) struct SessionTimeline {
    /// The view's head the session's watchers were last rung at ([`Self::ring_rows`]).
    rows_rung: u64,
    agent: AgentPublication,
    /// An open stream's wake, owned by its lane. It is registered under meta → timeline.
    #[cfg(unix)]
    stream_binding: std::sync::Weak<StreamBinding>,
    /// The needs the claim's stream raised (`mark need`) and has not lifted, by word, each with
    /// who raised it (the claim's holder, the tab's session): what [`Self::settle_stream_claim`]
    /// lifts once the stream holds no need of that word open under a claim that feeds `need`.
    stream_needs: std::collections::BTreeMap<&'static str, (String, aterm_slog::Actor)>,
    foreground_generation: u64,
    /// Earliest provider event this foreground stretch can own.
    foreground_since: Option<std::time::Instant>,
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
    /// The effort the footer last showed, with the conversation (its session id) and the model it
    /// was shown for: a relaunch of that conversation takes it ([`Self::carried_card`]).
    claude_effort: Option<(String, String, String)>,
    /// The holder's token and the level its effort hint named on the live screen at an output,
    /// frame or not ([`Self::note_effort_hint`]).
    effort_hint: Option<(u64, &'static str)>,
    /// The holder's token and when an output's read first looked for its effort hint
    /// ([`Self::effort_hint_looks`]).
    effort_hint_look: Option<(u64, std::time::Instant)>,
    path: Option<PathReading>,
    leader_copy: Option<(i32, &'static str)>,
    leader_no_harness: Option<i32>,
    leader_no_harness_applied: Option<i32>,
    /// The foreground leader's group while it is a build's front waiting for memory, and what its
    /// ledger says ([`Self::build_wait`]).
    leader_wait: Option<(i32, crate::session_status::program::BuildWait)>,
    upgrade: Option<(
        aterm_agent::harness::upgrade_drive::Row,
        Option<UpgradeOwner>,
    )>,
    /// The live-upgrade row the host last sent before it sent none, retaining the owner admitted
    /// with its publication ([`Self::upgrade_kept`]).
    upgrade_kept: Option<(
        aterm_agent::harness::upgrade_drive::Row,
        Option<UpgradeOwner>,
    )>,
    upgrade_word_revision: u64,
    resizes: crate::resize_ledger::ResizeLedger,
    #[cfg(any(unix, test))]
    handoff_ceiling: Option<u64>,
    task: Option<crate::task_record::TaskRecord>,
    /// An exit read before the agent publication cleared. A new task mark supersedes it;
    /// otherwise it takes effect only once a publication sees no agent in the tab.
    pending_task_exit: Option<crate::task_record::TaskMark>,
    /// THE RESULT FILES THIS TAB'S RECORD NAMED UNDER ANOTHER SESSION ID: the one its record
    /// named when a restore or a seamless update seeded it, where an earlier session ID of the
    /// tab names the file ([`crate::task_record::seed_restored`],
    /// [`crate::task_record::seed_carried`]), and each one a mark named since
    /// (`task mark … result_path=` takes any file in the tasks directory,
    /// [`crate::task_record::mark_session_task_with_timeline`]). Nothing that goes by this
    /// session's own ID finds them, so they are removed by these paths, but where another record
    /// still names one: when the tab closes, and at a quit whose layout does not name them
    /// (`crate::task_launch`). They stay while the tab lives, though a later mark leaves the
    /// record naming none or another: a mark made from an earlier reading of the record (the
    /// harness's exit mark) may name one again, and a mark holds no store to ask whether another
    /// record names the file. Every row that carries the record to another process carries them
    /// too ([`Self::push_carried_results`]), so a seamless update's successor removes them in
    /// turn and a restore's sweep clears those its record no longer names.
    carried_results: Vec<std::path::PathBuf>,
    worker_launch: Option<(std::time::Instant, String)>,
    /// The worker's launcher is installing its program or preparing its Google sign-in now
    /// (`task_launch::InstallingMark`).
    worker_installing: bool,
    /// How many times that mark has been set ([`Self::worker_install_mark`]).
    worker_install_mark: u64,
    /// The atpkg child that install runs, once the launcher has spawned it, while the mark stands.
    worker_install_child: Option<u32>,
    /// Why atpkg keeps the worker's program off this Mac, as its launcher's admission found.
    worker_kept_off: Option<String>,
    /// The worker's launcher is holding its launch for a pool now (`task_launch::PoolHoldMark`).
    worker_pool_hold: Option<WorkerPoolHold>,
    /// When its last hold ended, so a line typed then has a whole startup deadline to start in.
    worker_pool_hold_ended: Option<std::time::Instant>,
    claude_excerpts: Option<(i32, crate::survey::ClaudeExcerpts)>,
    codex_survey: Option<(i32, crate::survey::CodexSurvey)>,
    /// When each word of [`Self::claude_excerpts`] or [`Self::codex_survey`] last changed as read
    /// ([`crate::survey::WordsRead`]): what a worker's last topic post is newer than, or not, where
    /// the word's row carries no time ([`crate::survey::last_words`]).
    words_read: crate::survey::WordsRead,
    /// The folder the drift flush last read (OSC 7, [`Self::record_cwd_change`]), or an adopted
    /// session's hydrated engine held ([`Self::seed_adopted_cwd`]), held to `cwd=`'s bound: the
    /// session log keeps only its fingerprint, and aterm's default writer names the folder
    /// ([`crate::describe`]).
    cwd_text: Option<String>,
    /// Since a seamless update adopted this session ([`Self::note_unread_after_update`]), whether
    /// this process has yet to read its folder: the hydrated engine reported none, and the drift
    /// flush has not read it since, whatever it would find.
    cwd_unread: bool,
    /// Since a seamless update adopted this session, whether this process has yet to read its
    /// agent's words: neither the footer's resolver ([`Self::set_claude_excerpts`]) nor the
    /// rollout tap ([`Self::set_codex_survey`]) has answered.
    words_unread: bool,
    /// The newest seq of the log a seamless update adopted, as of the adoption (0 for a session
    /// never adopted): a claim at or below it is one the update carried.
    carried_seq: u64,
    /// What aterm's default writer keeps of this session between its passes
    /// ([`crate::describe::Held`]).
    described: crate::describe::Held,
    /// The conversation whose whole token counters this timeline last sent on a survey row, and
    /// since when (Unix ms) it has sent them unbroken ([`Self::token_epoch`]).
    token_epoch: Option<(String, u64)>,
    /// The rollout's authoritative counter event and its collection time, on the session clock.
    codex_token_clock: Option<(std::time::Instant, std::time::Instant)>,
    /// The background terminals codex-alab's own record counts for the codex process in group `.0`
    /// ([`Self::set_codex_terminals`]).
    codex_terminals: Option<(i32, u32)>,
    /// Where the agent in group `.0` works, as the harness host last read it ([`Self::set_work`]).
    work: Option<(i32, aterm_agent::harness::work_record::WorkRecord)>,
    program_shown: Option<ShownProgram>,
    /// Another program's report shown beside it (`ProgramApp::Other`), with no episode and no
    /// report sequence (both 0): only the words read it ([`Self::program_said`]).
    program_other: Option<ShownProgram>,
    program_episodes: u64,
    /// The ledger instant ([`now_ms`]) the program's own report last moved
    /// ([`Self::note_program_state`]), where [`Self::ab_word`] reads its word from.
    program_moved_ms: u64,
    claude_side: Option<(i32, std::sync::Arc<crate::claude_footer::FactSide>)>,
    claude_footer_read: Option<(i32, std::time::Instant)>,
    gauges: AgentGauges,
    screen_rung_at: Option<std::time::Instant>,
    slog: crate::session_log::SessionLog,
    need: crate::needs::Verdict,
    last_act: Option<String>,
    pending_self_tasks: VecDeque<aterm_agent::harness::acts::TaskFace>,
    /// Admitted self-claim acts the window has yet to put on its glass ([`Self::queue_self_act`]).
    pending_self_acts: VecDeque<SelfAct>,
    /// The last act a face that stands until another turn was told by ([`Self::self_act_resent`]):
    /// the act, its holder and the program's turns it arrived at.
    standing_self_act: Option<(String, String, u64)>,
    upgrade_recorded: bool,
    self_reexec_since_us: Option<u64>,
    landing_mark: Option<LandingMark>,
    typed_turn: Option<LandingMark>,
    task_face: Option<(aterm_agent::harness::acts::TaskFace, i64)>,
    watchers: Arc<crate::session_watch::Watchers>,
    task_face_held: Option<(aterm_agent::harness::acts::TaskFace, i64)>,
    /// The held face is a seamless update's carry ([`Self::seed_task_face_held`]), not yet told
    /// here.
    task_face_seeded: bool,
    pause: Option<crate::PausePlan>,
    harness_needs: crate::needs::ToldNeeds,
    land_held: Option<aterm_agent::harness::acts::LandHeld>,
    /// The hold's own words as the loop last told them ([`Self::land_said()`], slice 4b).
    land_said: Option<Arc<aterm_agent::harness::hold_said::HoldSaid>>,
    /// The words moved since the hold was last stored ([`Self::set_land_held`] says so once).
    land_said_untold: bool,
    land_receipt: Option<Arc<aterm_agent::harness::acts::LandReceipt>>,
    /// The why of the point its loop told standing that the held deadline judges
    /// ([`Self::loop_exception()`]).
    loop_exception: Option<String>,
    cwd_witness: Option<(u64, i32)>,
    holder_image: Option<std::sync::Arc<crate::holder_image::HolderImage>>,
    drawer_groups: crate::holder_image::DrawerGroups,
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
    /// The drawer card the probe saw: its token, its row, the pass count then, and the screen
    /// mutations before the slice that drew it.
    seen: Option<(u64, i64, u64, u64)>,
    /// Passes whose snapshot holds that slice, before the card was decorated.
    late: u32,
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

/// A Claude Code process whose registry row reports a build its Store or Native drawer image
/// does not name (card engine §3.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistryDrift {
    pub(crate) pgid: i32,
    pub(crate) image: String,
    pub(crate) source: atpkg::image::ImageSource,
    pub(crate) reported: String,
}

/// What an identified agent's screen counts ([`SessionTimeline::gauges`] adds codex-alab's own claim
/// where no screen count stands).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentGauges {
    pub(crate) ctx_pct: Option<u8>,
    pub(crate) shells: Option<u32>,
}

/// How many ids a seamless update leaves this process between the park's draw and its `_exit`
/// ([`SessionTimeline::handoff_id`]).
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

/// What the agent is doing, as a `state` record says it (the harness contract's six words).
/// Every platform builds this file and the wire library is Unix only, so the record's word is
/// kept here under its own type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "only the Unix control stream reads a `state` record into one"
    )
)]
pub(crate) enum StreamedState {
    Starting,
    Idle,
    Working,
    Asking,
    Walled,
    Ended,
}

impl StreamedState {
    /// The agent word and detail this state reads as (`docs/HARNESS-CONTRACT.md`, "The `state`
    /// record"), beside what the screen reader reads now. `asking` and `walled` name no kind:
    /// until `ask` and `wall` records are applied the kind is the screen's (its `prompt` with
    /// the box, its `question`, its `wall:<kind>`), else `prompt` and a bare `wall`, never a
    /// kind nothing showed.
    fn reads(
        self,
        screen_word: &'static str,
        screen_detail: Option<&String>,
    ) -> (&'static str, Option<String>) {
        match self {
            Self::Starting => ("busy", Some("starting".to_string())),
            Self::Working => ("busy", None),
            Self::Idle => ("idle", None),
            Self::Ended => ("-", None),
            Self::Asking if matches!(screen_word, "prompt" | "question") => {
                (screen_word, screen_detail.cloned())
            }
            Self::Asking => ("prompt", None),
            Self::Walled if screen_word.starts_with("wall:") => {
                (screen_word, screen_detail.cloned())
            }
            Self::Walled => ("wall", None),
        }
    }
}

/// The `state` record a claim's stream last applied: while the publication holds one, the
/// stream and not the screen reader decides the agent word and the composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamFed {
    pub(crate) state: StreamedState,
    /// What the record says the input box holds; a record that names none reads `-`.
    pub(crate) composer: aterm_messages::presence::ComposerState,
    /// The aterm instance whose stream applied it.
    pub(crate) epoch: String,
    /// The record's `seq`.
    pub(crate) hseq: u64,
    /// The claim's holder, stamped on every row the record writes.
    pub(crate) holder: String,
    /// The tab's own session, the rows' author.
    pub(crate) by: aterm_slog::Actor,
    /// The agent the claim's registry row names, for a tab no agent run is open in.
    pub(crate) agent: aterm_phase::Program,
    /// The record's line as the wire codec writes it, kept as text outside the log's retention.
    pub(crate) line: String,
}

/// What every row a stream record writes is stamped with, and where the record came from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "only the Unix control stream applies a record under one"
    )
)]
pub(crate) struct StreamStamp {
    /// The aterm instance whose stream applied the record.
    pub(crate) epoch: String,
    /// The record's `seq`.
    pub(crate) hseq: u64,
    /// The claim's holder.
    pub(crate) holder: String,
    /// The tab's own session, the rows' author.
    pub(crate) by: aterm_slog::Actor,
}

/// One limit of a `usage` record, as the record names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamLimit {
    /// Which, as the vendor names it (`session`, `weekly`, …).
    pub(crate) name: String,
    /// How much of it is used, in percent.
    pub(crate) used: Option<u8>,
    /// When it resets, in milliseconds since 1970.
    pub(crate) resets_ms: Option<u64>,
}

/// Where one item of a `progress` record's plan is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "only the Unix control stream reads a `progress` record into one"
    )
)]
pub(crate) enum StreamedItem {
    Pending,
    Active,
    Done,
}

/// A record of one of the kinds the stream applies beside `state`, as [`SessionTimeline::
/// publish_stream_record`] takes it: the record's own fields, under no wire type.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "only the Unix control stream reads a record into one"
    )
)]
pub(crate) enum StreamRecord {
    /// `usage`: the tab's own account's limits and the context left.
    Usage {
        account: String,
        limits: Vec<StreamLimit>,
        context_left: Option<u8>,
    },
    /// `account`: the tab's own account and its state, with the record's line as the wire
    /// codec writes it.
    Account {
        account: String,
        state: String,
        line: String,
    },
    /// `progress`: the plan's items, in the plan's order.
    Progress { items: Vec<(String, StreamedItem)> },
    /// `goal`: the active goal, `None` for none.
    Goal { text: Option<String> },
    /// `compact`: a compaction began, or (`done`) ended, with the context left.
    Compact {
        done: bool,
        context_left: Option<u8>,
    },
    /// `model`: the model, its effort, the top-effort key and what is on offer (each model with
    /// its levels), and the permission mode, each only as the vendor states it, with the
    /// record's line as the wire codec writes it.
    Model {
        model: Option<String>,
        effort: Option<String>,
        key: Option<bool>,
        offers: Vec<(String, Vec<String>)>,
        permission: Option<String>,
        line: String,
    },
    /// `upgrade`: where a move to a newer vendor build is (its phase's wire word: `seen`,
    /// `waiting`, `moving`, `done`, `abandoned`) and the target version, with the record's line
    /// as the wire codec writes it.
    Upgrade {
        phase: &'static str,
        to: String,
        line: String,
    },
}

impl StreamRecord {
    /// The record's kind, by its wire word, as [`StreamState::applied`] names it.
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "only the Unix control stream reads it")
    )]
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Usage { .. } => USAGE_KIND,
            Self::Account { .. } => ACCOUNT_KIND,
            Self::Progress { .. } => PROGRESS_KIND,
            Self::Goal { .. } => GOAL_KIND,
            Self::Compact { .. } => COMPACT_KIND,
            Self::Model { .. } => MODEL_KIND,
            Self::Upgrade { .. } => UPGRADE_KIND,
        }
    }
}

/// THE `model` RECORD the claim's stream last applied, kept in the current-state record while
/// that stream feeds the kind: from then `survey`'s `model=` and `effort=` and every reader of
/// the footer's model and effort ([`SessionTimeline::claude_footer_shown`]) read it, and a tab
/// whose stream never applied `model` keeps the footer's words. It writes no log row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamModel {
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    /// Whether the vendor's top-effort key is on; `None` where it did not say.
    pub(crate) key: Option<bool>,
    /// The models on offer, each with its effort levels.
    pub(crate) offers: Vec<(String, Vec<String>)>,
    pub(crate) permission: Option<String>,
    pub(crate) epoch: String,
    pub(crate) hseq: u64,
    /// The record as the wire codec writes it, kept as text.
    pub(crate) line: String,
}

/// THE `upgrade` RECORD the claim's stream last applied, kept in the current-state record while
/// that stream feeds the kind: from then `status upgrade=`, the session rows' and the survey's
/// read it ([`SessionTimeline::upgrade_column`]) and the engine-filled row stands down. It
/// writes no log row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamUpgrade {
    /// The phase's wire word.
    pub(crate) phase: &'static str,
    /// The target version.
    pub(crate) to: String,
    /// When it was applied, in unix seconds: the `upgrade=` word's age.
    pub(crate) at_s: u64,
    pub(crate) epoch: String,
    pub(crate) hseq: u64,
    /// The record as the wire codec writes it, kept as text.
    pub(crate) line: String,
}

impl StreamUpgrade {
    /// The `upgrade=` word, in the grammar the engine's row has
    /// (`<state>/<to>/<why>/<age>`, [`aterm_agent::harness::upgrade_drive::Row::column`]), so
    /// every reader of it splits it the same: `<phase>/<to>/-/<age since applied at now_s>`,
    /// `to` cut to the characters a harness word is made of (`-` for none of them).
    #[must_use]
    pub(crate) fn column(&self, now_s: u64) -> String {
        let keep =
            |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '(' | ')');
        let to: String = self.to.chars().filter(|c| keep(*c)).take(64).collect();
        format!(
            "{}/{}/-/{}",
            self.phase,
            if to.is_empty() { "-" } else { to.as_str() },
            aterm_agent::harness::upgrade::span(now_s.saturating_sub(self.at_s))
        )
    }
}

/// The `usage` reading the claim's stream last applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamUsage {
    pub(crate) account: String,
    pub(crate) limits: Vec<StreamLimit>,
    pub(crate) context_left: Option<u8>,
    pub(crate) epoch: String,
    pub(crate) hseq: u64,
}

impl StreamUsage {
    /// How much of the limit `name` is used, in percent.
    pub(crate) fn used(&self, name: &str) -> Option<u8> {
        self.limits
            .iter()
            .find(|limit| limit.name == name)
            .and_then(|limit| limit.used)
    }
}

/// The name a `usage` record gives the five-hour limit, and the one it gives the weekly limit:
/// the two `h5=` and `d7=` are read from.
pub(crate) const LIMIT_SESSION: &str = "session";
pub(crate) const LIMIT_WEEKLY: &str = "weekly";

/// THE KEPT `account` LINE: the latest `account` record a claim's stream applied, with its line
/// as the wire codec writes it, kept as text outside the log's retention. It outlives the claim
/// that sent it (`fed` then says the built-in readers feed `auth=` and `token=` again), so a
/// restart after its harness died still reads the account the tab last ran on; a newer applied
/// `account` record replaces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamAccount {
    pub(crate) account: String,
    /// Its state, as the harness words it.
    pub(crate) state: String,
    pub(crate) epoch: String,
    pub(crate) hseq: u64,
    pub(crate) line: String,
    /// The claim whose stream applied it still feeds the kind: the `auth=` and `token=`
    /// readers stand down.
    pub(crate) fed: bool,
}

/// The `progress` record the claim's stream last applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamProgress {
    pub(crate) items: Vec<(String, StreamedItem)>,
    pub(crate) epoch: String,
    pub(crate) hseq: u64,
}

/// The `goal` record the claim's stream last applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamGoal {
    pub(crate) text: Option<String>,
    pub(crate) epoch: String,
    pub(crate) hseq: u64,
}

/// The `compact` record the claim's stream last applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamCompact {
    pub(crate) done: bool,
    pub(crate) context_left: Option<u8>,
    pub(crate) epoch: String,
    pub(crate) hseq: u64,
}

/// The current-state record's kinds a claim's stream feeds beside `state`. Each of `usage`,
/// `progress`, `goal`, `compact`, `model` and `upgrade` holds a value only while its claim's
/// stream feeds that kind ([`SessionTimeline::settle_stream_claim`]), and the built-in source of
/// that kind stands down for exactly that long; `account` keeps its line past the claim
/// ([`StreamAccount`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StreamKinds {
    pub(crate) usage: Option<StreamUsage>,
    pub(crate) account: Option<StreamAccount>,
    pub(crate) progress: Option<StreamProgress>,
    pub(crate) goal: Option<StreamGoal>,
    pub(crate) compact: Option<StreamCompact>,
    pub(crate) model: Option<StreamModel>,
    pub(crate) upgrade: Option<StreamUpgrade>,
}

impl StreamKinds {
    /// The `account` record while its claim's stream feeds the kind.
    pub(crate) fn fed_account(&self) -> Option<&StreamAccount> {
        self.account.as_ref().filter(|account| account.fed)
    }
}

/// THE CURRENT-STATE RECORD of one session, per kind: its program's identity, the agent state
/// (`word`, `detail`, `rev`, since when, and its source: a built-in reader, or with `stream` set
/// the claim's stream, by its epoch and the record's `hseq`, with the record's line kept as
/// text), and the composer. `status` and `await agent` read it whichever source fed it.
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
    /// Unix milliseconds when the current screen word began, unchanged by details or an input-stall overlay.
    pub(crate) screen_word_unix_ms: u64,
    pub(crate) changed_continuous_ms: Option<u64>,
    pub(crate) stamp: Option<AgentStamp>,
    pub(crate) screen_word: &'static str,
    pub(crate) screen_detail: Option<String>,
    pub(crate) input: Option<crate::input_stall::InputStallFact>,
    pub(crate) text_moves: u64,
    pub(crate) grid: Option<GridRead>,
    pub(crate) screen_moves: u64,
    /// What the input box holds (`composer=`): the stream's word while its `state` stands,
    /// else the screen reader's.
    pub(crate) composer: aterm_messages::presence::ComposerState,
    /// The screen reader's own reading of the input box, kept while the stream decides.
    pub(crate) screen_composer: aterm_messages::presence::ComposerState,
    /// The latest `state` record the claim's stream applied, `None` while the screen reader
    /// feeds the state.
    pub(crate) stream: Option<StreamFed>,
    /// The other kinds the claim's stream feeds: `usage`, `account` (its kept line), `progress`,
    /// `goal` and `compact`.
    pub(crate) streamed: StreamKinds,
}

/// What the program in front last said about itself ([`SessionTimeline::note_program_state`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShownProgram {
    pub(crate) state: aterm_core::terminal::program_state::ProgramState,
    /// The episode: every move of the shown report is a new one, counted on this timeline (a
    /// repeat of the report shown is no move, so it stays one episode; a second box begun anew
    /// with the same words is one, `ProgramStateGate::observe`). A program-claim need's key
    /// ([`crate::needs::ProgramClaimFact::key`]).
    pub(crate) seq: u64,
    /// The engine's sequence where the report shown began (`ProgramStateReport::began`, a
    /// re-send of it looked past): where it stands in the program's output, which a
    /// program-claim need carries ([`crate::needs::ProgramClaimFact::report`]).
    pub(crate) report: u64,
    since: std::time::Instant,
    since_continuous_ms: Option<u64>,
}

impl ShownProgram {
    fn now(
        state: aterm_core::terminal::program_state::ProgramState,
        seq: u64,
        report: u64,
    ) -> Self {
        Self {
            state,
            seq,
            report,
            since: std::time::Instant::now(),
            since_continuous_ms: aterm_uds::process::continuous_ms(),
        }
    }

    /// How long it has said so, counted through sleep, as the survey reports it.
    pub(crate) fn age_ms(&self) -> u64 {
        match (
            self.since_continuous_ms,
            aterm_uds::process::continuous_ms(),
        ) {
            (Some(at), Some(now)) => now.saturating_sub(at),
            _ => u64::try_from(self.since.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// What a proven self-claim's landing was recorded over (Autopilot Land).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LandingMark {
    pub(crate) program_turns: u64,
}

/// AN ADMITTED SELF-CLAIM ACT, AS THE WINDOW'S GLASS TAKES IT (`HarnessFact::SelfAct`): the
/// chip's act it is ([`aterm_agent::harness::acts::self_claim_sort`]; `None` where the chip
/// lights nothing for it, which still owes Settings ▸ Harness a reading) and the holder of the
/// proven claim that recorded it, who did it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SelfAct {
    pub(crate) act: Option<aterm_agent::harness::acts::Act>,
    pub(crate) holder: String,
}

/// How long after the first read that finds a session's grid resized a text change counts as
/// the re-grid's.
pub(crate) const REGRID_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

/// The grid at a read of the screen ([`SessionTimeline::note_grid`]).
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
            screen_word_unix_ms: aterm_time::unix_millis().unwrap_or(0),
            changed_continuous_ms: aterm_uds::process::continuous_ms(),
            stamp: None,
            screen_word: "-",
            screen_detail: None,
            input: None,
            text_moves: 0,
            grid: None,
            screen_moves: 0,
            composer: aterm_messages::presence::ComposerState::Unknown,
            screen_composer: aterm_messages::presence::ComposerState::Unknown,
            stream: None,
            streamed: StreamKinds::default(),
        }
    }
}

impl AgentPublication {
    /// Milliseconds since the verdict last moved (`agent_since_ms=`).
    pub(crate) fn since_ms(&self) -> u64 {
        now_ms().saturating_sub(self.changed_ms)
    }

    /// [`Self::since_ms`] counted through sleep, as the survey reports it.
    pub(crate) fn age_ms(&self) -> u64 {
        match (
            self.changed_continuous_ms,
            aterm_uds::process::continuous_ms(),
        ) {
            (Some(at), Some(now)) => now.saturating_sub(at),
            _ => self.since_ms(),
        }
    }

    /// The word and detail to publish, and whether the stream's `state` decided them. aterm's
    /// own input-stall watch comes first, then the stream's state, then the screen.
    fn effective(&self) -> (&'static str, Option<String>, bool) {
        match (&self.input, &self.stream) {
            (Some(stall), _) if self.reader.is_some() => (
                "wall:unresponsive",
                stall.stopped.then(|| "stopped".to_string()),
                false,
            ),
            (_, Some(fed)) => {
                let (word, detail) = fed
                    .state
                    .reads(self.screen_word, self.screen_detail.as_ref());
                (word, detail, true)
            }
            _ => (self.screen_word, self.screen_detail.clone(), false),
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

/// How long a person's hands must be off a session before their next input opens a new burst
/// (one `EVENT <local> human` push, one `human` row).
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
    pub(crate) const fn watchers(&self) -> &Arc<crate::session_watch::Watchers> {
        &self.watchers
    }

    /// An act the session's proven self-claim's holder took (`meta set harness_act <act>`).
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
        self.record(HARNESS_ACT);
        Some(self.slog.row_head())
    }

    /// THE STREAM'S ARM of [`Self::note_harness_act`]: the act of record `hseq` the claim's
    /// stream applied, by its rule, written as `meta set harness_act` writes it but through the
    /// stream's write arm ([`crate::session_log::SessionLog::stream_self_claim_act`]: the four
    /// stamps, `hseq=`, and no mark limiter). `None`, with nothing kept, where the log refused
    /// the row.
    #[cfg(unix)]
    pub(crate) fn note_stream_act(
        &mut self,
        act: &str,
        holder: &str,
        by: aterm_slog::Actor,
        hseq: u64,
    ) -> Option<u64> {
        let at = self.slog.now();
        self.slog.stream_self_claim_act(act, holder, by, hseq, at)?;
        self.last_act = Some(act.to_string());
        if act == aterm_agent::harness::acts::RULE_UPGRADE {
            self.upgrade_recorded = true;
        }
        self.record(HARNESS_ACT);
        Some(self.slog.row_head())
    }

    /// The last act [`Self::note_harness_act`] kept, if any.
    pub(crate) fn last_act(&self) -> Option<&str> {
        self.last_act.as_deref()
    }

    /// Queue the face of an admitted act in the hold that commits its row.
    pub(crate) fn queue_self_task(&mut self, face: aterm_agent::harness::acts::TaskFace) {
        if self.pending_self_tasks.len() >= 64 {
            self.pending_self_tasks.pop_front();
        }
        self.pending_self_tasks.push_back(face);
    }

    /// Queue an admitted self-claim act for the window's glass (`HarnessFact::SelfAct`): what the
    /// chip and the trail show of it, and its holder, who did it.
    pub(crate) fn queue_self_act(&mut self, act: SelfAct) {
        if self.pending_self_acts.len() >= 64 {
            self.pending_self_acts.pop_front();
        }
        self.pending_self_acts.push_back(act);
    }

    /// Take the self-claim acts queued before this read, oldest first.
    pub(crate) fn take_self_acts(&mut self) -> VecDeque<SelfAct> {
        std::mem::take(&mut self.pending_self_acts)
    }

    /// A SELF-CLAIM'S ACT TOLD AGAIN IS ONE ACT: whether `act`, recorded by `holder` with the
    /// program at `turns` begun, repeats the face that stands until another turn the last such act
    /// told — the same act by the same holder at the same count, no act that reopens the work
    /// between — as a holder's retry does after a write whose answer it never read (codex-alab
    /// sends `left-to-person@v1` again after a failed or timed-out send). An act of a standing
    /// face is remembered; an act that opens the work forgets it; an act with no face (a re-exec)
    /// leaves it, and one whose count is unknown never repeats.
    pub(crate) fn self_act_resent(
        &mut self,
        act: &str,
        holder: &str,
        face: Option<aterm_agent::harness::acts::TaskFace>,
        turns: Option<u64>,
    ) -> bool {
        match face {
            Some(face) if crate::harness_task::holds_until_another_turn(face) => {
                let Some(turns) = turns else {
                    self.standing_self_act = None;
                    return false;
                };
                let resent = self
                    .standing_self_act
                    .as_ref()
                    .is_some_and(|(a, h, t)| a == act && h == holder && *t == turns);
                self.standing_self_act = Some((act.to_string(), holder.to_string(), turns));
                resent
            }
            Some(_) => {
                self.standing_self_act = None;
                false
            }
            None => false,
        }
    }

    /// Take the finite batch committed before this read.
    pub(crate) fn take_self_tasks(&mut self) -> VecDeque<aterm_agent::harness::acts::TaskFace> {
        std::mem::take(&mut self.pending_self_tasks)
    }

    /// A proven self-claim's holder recorded its landing over `mark`, replacing any mark before it.
    pub(crate) const fn mark_landing(&mut self, mark: LandingMark) {
        self.landing_mark = Some(mark);
    }

    /// The latest landing's mark ([`Self::mark_landing`]), `None` before one.
    pub(crate) const fn landing_mark(&self) -> Option<LandingMark> {
        self.landing_mark
    }

    /// Record a turn-starting act typed by a proven self-claim holder: the program's turns it
    /// stood at, when it came with the program at rest.
    pub(crate) const fn note_typed_act(&mut self, at_rest: Option<LandingMark>) {
        self.typed_turn = at_rest;
    }

    /// The counts [`Self::note_typed_act`] kept, taken by the landing or the point left to a
    /// person that follows it.
    pub(crate) const fn take_typed_turn(&mut self) -> Option<LandingMark> {
        self.typed_turn.take()
    }

    /// The claim moved at `now_us`, taken (`taken`) or given back; a give-back after a recorded
    /// upgrade act opens the re-exec gap ([`Self::in_self_reexec`]).
    pub(crate) fn note_supervisor_moved(&mut self, taken: bool, now_us: u64) {
        self.self_reexec_since_us = (!taken && self.upgrade_recorded).then_some(now_us);
        self.upgrade_recorded = false;
    }

    /// Whether `now_us` falls in the re-exec's gap.
    pub(crate) fn in_self_reexec(&self, now_us: u64) -> bool {
        self.self_reexec_since_us
            .is_some_and(|at| now_us.saturating_sub(at) < SELF_REEXEC_GAP_US)
    }

    /// The claim moved, or a renewal changed its proof: drop the queued acts and the typed turn.
    pub(crate) fn forget_holder_acts(&mut self) {
        self.pending_self_tasks.clear();
        self.standing_self_act = None;
        self.typed_turn = None;
    }

    /// A move of what the program in front says about itself: `shown` and the engine's sequence
    /// where that report began (`ProgramStateReport::began`), a new episode
    /// ([`ShownProgram::seq`]) and, on a codex run, its box's identity
    /// ([`crate::session_log::SessionLog::observe_program`]).
    pub(crate) fn note_program_state(
        &mut self,
        shown: Option<aterm_core::terminal::program_state::ProgramState>,
        report: u64,
    ) {
        self.program_episodes = self.program_episodes.wrapping_add(1);
        let seq = self.program_episodes;
        self.program_shown = shown.map(|state| ShownProgram::now(state, seq, report));
        self.program_moved_ms = now_ms();
        let at = self.slog.now();
        self.slog.observe_program(shown, report, at);
        self.record(PROGRAM_STATE);
    }

    /// A move of another program's report shown (`ProgramApp::Other`,
    /// [`crate::program_state::ProgramStateGate::observe_other`]): kept for the words alone, it
    /// writes no mark, episode or row and moves nothing [`Self::note_program_state`] moves.
    pub(crate) fn note_other_program(
        &mut self,
        other: Option<aterm_core::terminal::program_state::ProgramState>,
    ) {
        self.program_other = other.map(|state| ShownProgram::now(state, 0, 0));
    }

    /// THE EDGES OF THE PROGRAM'S TURNS PAST THE MOVE (`read`, the sweep's
    /// [`crate::session_log::ProgramRead`]), after the block read, the bucket's tick and the
    /// foreground move, into the log's claimed turns at their own times
    /// ([`crate::session_log::SessionLog::feed_front`]), judged by the program in front now (a
    /// group with no name yet holds them): no episode, shown report or `program-state` row moves,
    /// and the watchers are rung when a turn record was written, so `await log close=turn` hears
    /// it on this wake.
    pub(crate) fn feed_program_turns(&mut self, read: &mut crate::session_log::ProgramRead) {
        use crate::session_log::Front;
        let front = match self.agent.program.as_deref() {
            None => Front::Unnamed,
            Some(name) => {
                let codex = crate::program_state::honoured_for(
                    Some(name),
                    Some(aterm_core::terminal::program_state::ProgramApp::Codex),
                    true, // The job flag is read only for Other; this judgement names Codex.
                );
                if codex { Front::Codex } else { Front::Other }
            }
        };
        let at = self.slog.now();
        if self.slog.feed_front(read, front, at) {
            self.ring();
        }
    }

    /// A `kind` row's writer wrote its session log mark in this hold (the mark is the row,
    /// [`crate::session_timeline_view::rows_of`]): wake the session's watchers.
    pub(crate) fn record(&mut self, kind: &'static str) {
        debug_assert_ne!(kind, META_CHANGE, "a meta-change is record_meta_change's");
        debug_assert!(
            !crate::session_timeline_view::ROW_ONLY.contains(&kind),
            "a {kind} row is record_row's: the log holds it as its row alone"
        );
        self.ring();
    }

    /// The one writer of a row-only kind ([`crate::session_timeline_view::ROW_ONLY`]): the
    /// session log's `row` mark, which is the row, then the watchers rung, in this one hold.
    pub(crate) fn record_row(&mut self, kind: &'static str, payload: String) {
        debug_assert!(
            crate::session_timeline_view::ROW_ONLY.contains(&kind),
            "{kind} is no row-only kind: its writer writes its own mark"
        );
        let at = self.slog.now();
        self.slog.observe_row(kind, &payload, at);
        self.ring();
    }

    /// A row a test writes by hand, of any kind, as [`Self::record_row`] writes it: a `row`
    /// mark. Returns the view's head.
    #[cfg(test)]
    pub(crate) fn record_hand_row(&mut self, kind: &'static str, payload: String) -> u64 {
        let at = self.slog.now();
        self.slog.observe_row(kind, &payload, at);
        self.ring();
        self.slog.row_head()
    }

    /// Wake the session's watchers for a record, at the view's head now.
    fn ring(&mut self) {
        self.rows_rung = self.slog.row_head();
        self.watchers.ring_record();
    }

    /// Wake the session's watchers iff the view's head moved since they were last rung, as it
    /// does for a row a mark wrote outside this timeline's writers (a person's burst,
    /// [`crate::session_log::SessionLog::person_input`]; a program state the status sweep's tick
    /// wrote, [`crate::session_log::SessionLog::program_tick`]).
    pub(crate) fn ring_rows(&mut self) {
        if self.slog.row_head() != self.rows_rung {
            self.ring();
        }
    }

    /// The view past `after`, its newest `keep` rows (0: every one), copied under this lock for
    /// the `timeline` verb to format after it.
    pub(crate) fn view_copy(
        &self,
        after: Option<u64>,
        keep: usize,
    ) -> crate::session_timeline_view::ViewCopy {
        crate::session_timeline_view::ViewCopy::of(self.slog.log(), after, keep)
    }

    /// The view past `after` (`None`: from the first row), with the row ids it no longer holds
    /// (the `events` digest's read).
    pub(crate) fn view_since(
        &self,
        after: Option<u64>,
    ) -> crate::session_timeline_view::TimelineView {
        crate::session_timeline_view::timeline_rows_since(self.slog.log(), after)
    }

    /// The view's head: the last row id the log passed, `None` before any (the digest's early-out
    /// and seed, the chrome's cache key).
    pub(crate) fn view_head(&self) -> Option<u64> {
        Some(self.slog.row_head()).filter(|&h| h > 0)
    }

    /// The view's rows newest first, as `(kind, process-clock ms)`: the tab chrome's tail.
    pub(crate) fn view_newest_first(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        crate::session_timeline_view::newest_first(self.slog.log())
    }

    /// The one `meta-change` writer, under the meta → timeline hold its caller already takes
    /// (`meta` is the store as the write left it).
    pub(crate) fn record_meta_change(&mut self, change: MetaChange<'_>, meta: &SessionMeta) {
        let owners = &meta.attention_owners;
        let escalated = crate::session_log::observed::Escalated::of(
            owners.effective_owner() == Some(aterm_agent::supervise::run::ATTENTION_OWNER),
            owners.effective_kind(),
        );
        let at = self.slog.now();
        self.slog.observe_meta(change, escalated, at);
        self.settle_stream_claim(meta);
        self.ring();
    }

    /// THE END OF THE STAND-DOWN, under the meta → timeline hold of whoever moved the claim
    /// (`meta` is the store as the move left it). The stream's state stands only under its own
    /// live claim holding the scope `state`: a claim that was released (live or with its
    /// `ttl=` already past), lapsed, taken by another claimant, renewed by another process
    /// under the same holder, or renewed without `state` hands the state and the composer back
    /// to the screen reader. A claim kept live through a grace keeps the reader down. Every
    /// claim move calls this: through [`Self::record_meta_change`] where it writes a
    /// `meta-change`, and directly where it writes none. Returns whether the stand-down ended.
    ///
    /// The other kinds settle first: a `compact` step the stream opened is ended as aterm's
    /// ([`crate::session_log::SessionLog::end_stream_compact`]) before the state's end hands
    /// the word back to the screen, whose verdict may close the turn or run the step sits
    /// under and would otherwise close the step with it.
    pub(crate) fn settle_stream_claim(&mut self, meta: &SessionMeta) -> bool {
        #[cfg(unix)]
        self.settle_stream_binding(meta);
        self.settle_stream_kinds(meta);
        // A placement that moved the claim's grant or revision under its open stream owes it a
        // `policy` record ([`SessionMeta::set_supervisor`]): the stream looks.
        if meta.stream.as_ref().is_some_and(|stream| stream.policy_due) {
            self.watchers.ring_stream();
        }
        let ended = self.agent.stream.as_ref().is_some_and(|fed| {
            !meta.stream_applied(
                STATE_KIND,
                aterm_agent::harness::contract::Scope::State,
                &fed.epoch,
                crate::metrics::now_us(),
            )
        });
        if ended {
            self.end_stream_state();
        }
        ended
    }

    /// [`Self::settle_stream_claim`] for the kinds beside `state`: each stands only under its
    /// own live claim holding the scope the kind is admitted under (`walls` for `usage`,
    /// `accounts` for `account`, `state` for `progress`, `goal` and `compact`). A kind whose
    /// claim no longer feeds it hands its words back to its built-in source; the `account`
    /// line is kept as text and only stops standing its readers down.
    ///
    /// `usage` and `account` are the tab's own account's alone, and that account is the live
    /// claim's `login=` as it stands now, not as it stood when the record arrived: a claim
    /// placed again under another `login=` (or none) keeps neither, so the old account's
    /// reading is not shown for the new one and its built-in readers feed again until the
    /// stream sends a record for the account the claim now names.
    ///
    /// A `compact` step the stream opened and did not close ends with the feed
    /// ([`crate::session_log::SessionLog::end_stream_compact`]).
    fn settle_stream_kinds(&mut self, meta: &SessionMeta) {
        use aterm_agent::harness::contract::Scope;
        let now = crate::metrics::now_us();
        let feeds =
            |kind: &str, scope: Scope, epoch: &str| meta.stream_applied(kind, scope, epoch, now);
        let own = meta
            .live_scoped(now)
            .and_then(|scoped| scoped.claim.login.as_deref());
        let kinds = &mut self.agent.streamed;
        let before = kinds.clone();
        kinds.usage.take_if(|fed| {
            !feeds(USAGE_KIND, Scope::Walls, &fed.epoch) || own != Some(fed.account.as_str())
        });
        if let Some(account) = kinds.account.as_mut() {
            account.fed = account.fed
                && feeds(ACCOUNT_KIND, Scope::Accounts, &account.epoch)
                && own == Some(account.account.as_str());
        }
        kinds
            .progress
            .take_if(|fed| !feeds(PROGRESS_KIND, Scope::State, &fed.epoch));
        kinds
            .goal
            .take_if(|fed| !feeds(GOAL_KIND, Scope::State, &fed.epoch));
        kinds
            .compact
            .take_if(|fed| !feeds(COMPACT_KIND, Scope::State, &fed.epoch));
        // `model` and `upgrade` need no scope beyond the open stream.
        kinds
            .model
            .take_if(|fed| !meta.stream_applied_under(MODEL_KIND, &[], &fed.epoch, now));
        kinds
            .upgrade
            .take_if(|fed| !meta.stream_applied_under(UPGRADE_KIND, &[], &fed.epoch, now));
        let compact_ended = before.compact.is_some() && kinds.compact.is_none();
        if *kinds != before {
            self.settle_stream_down();
        }
        if compact_ended {
            let at = self.slog.now();
            if self.slog.end_stream_compact(at) {
                self.ring_rows();
            }
        }
        self.settle_stream_needs(meta);
    }

    /// [`Self::settle_stream_claim`] for the needs the stream raised ([`Self::stream_needs`]): a
    /// need stands while the stream holds a need of its word open ([`StreamState::needs`]) under
    /// a claim that feeds `need` (one of [`NEED_SCOPES`]). Once it does not (its claim was
    /// released, lapsed, placed by another claimant or renewed without the scope, each of which
    /// ends the open needs, [`SessionMeta::settle_stream_run`]), the need is lifted by the clear
    /// the stream owed, `mark - clear word=<its word>` through the stream's arm under the stamp
    /// that raised it and with no `hseq=` (no record sent it): the fold's clear rule lets only a
    /// `src=harness` clear by the same session lift a need the stream raised, and no later
    /// claimant's stream holds the record that raised it.
    fn settle_stream_needs(&mut self, meta: &SessionMeta) {
        if self.stream_needs.is_empty() {
            return;
        }
        let now = crate::metrics::now_us();
        let open: std::collections::BTreeSet<&str> = meta
            .stream
            .as_ref()
            .filter(|stream| meta.stream_applied_under(NEED_KIND, &NEED_SCOPES, &stream.epoch, now))
            .map(|stream| stream.needs.values().map(|need| need.word).collect())
            .unwrap_or_default();
        let lifted: Vec<(&'static str, (String, aterm_slog::Actor))> = self
            .stream_needs
            .iter()
            .filter(|(word, _)| !open.contains(*word))
            .map(|(word, raised)| (*word, raised.clone()))
            .collect();
        if lifted.is_empty() {
            return;
        }
        self.stream_needs.retain(|word, _| open.contains(word));
        let at = self.slog.now();
        for (word, (holder, by)) in lifted {
            if self.slog.stream_lift_need(word, &holder, by, at).is_none() {
                aterm_log::info!(
                    "stream: the log refused the lift of the need {word} the stream raised"
                );
            }
        }
        self.ring_rows();
    }

    /// Tell the session log which of its built-in writers the stream stands down: the usage
    /// tap's mark and the context-left mark under `usage`, the goal edge under `goal`, the
    /// transcript's compact step under `compact`. Each stands down by kind, not by the parts
    /// the latest record states (D29): a `usage` record with no `context_left` still stands the
    /// context-left mark down, so the kind never has two writers.
    fn settle_stream_down(&mut self) {
        let kinds = &self.agent.streamed;
        let down = crate::session_log::StreamDown {
            usage: kinds.usage.is_some(),
            goal: kinds.goal.is_some(),
            compact: kinds.compact.is_some(),
        };
        self.slog.set_stream_down(down);
    }

    /// Register the lane while it still holds the metadata lock that admitted its claim.
    #[cfg(unix)]
    pub(crate) fn watch_stream_binding(&mut self, binding: &Arc<StreamBinding>) {
        if let Some(previous) = self.stream_binding.upgrade() {
            let _ = previous.revoked.set();
        }
        self.stream_binding = Arc::downgrade(binding);
    }

    #[cfg(unix)]
    fn settle_stream_binding(&mut self, meta: &SessionMeta) {
        if let Some(binding) = self.stream_binding.upgrade() {
            if binding.claim(meta).is_some() {
                return;
            }
            let _ = binding.revoked.set();
        }
        self.stream_binding = std::sync::Weak::new();
    }

    /// A raw `meta-change` row for a test that pins the timeline's own shapes and not a writer's.
    #[cfg(test)]
    pub(crate) fn record_raw_meta_change(&mut self, payload: String) -> u64 {
        self.record_hand_row(META_CHANGE, payload)
    }

    /// The one `cwd-change` writer: a row iff the session log's `cwd` mark moved; returns whether
    /// it recorded one.
    pub(crate) fn record_cwd_change(&mut self, cwd: Option<&str>) -> bool {
        // Held before the mark's own test: a carried log already holds the folder's
        // fingerprint, and the text is wanted after an update too.
        let text = held_cwd(cwd);
        let moved = self.cwd_unread || self.cwd_text != text;
        if moved {
            self.cwd_text = text;
            self.cwd_unread = false;
        }
        let at = self.slog.now();
        if !self.slog.observe_cwd(cwd, at) {
            if moved {
                // What aterm's default writer reads of the folder moved and no record says so (as
                // where a carried log already holds the folder's fingerprint): it is told.
                crate::describe::ring();
            }
            return false;
        }
        // The mark's append rings the default writer through the log's one callback.
        self.record(CWD_CHANGE);
        true
    }

    /// THE ADOPTED SESSION'S FOLDER, as its hydrated engine reports it, before the session is
    /// registered ([`crate::spawn::fit_adopted_log`]): held as [`Self::record_cwd_change`] holds
    /// it, with no mark (a carried log already holds its fingerprint, and the drift flush's
    /// later read of the same folder moves nothing) and no ring (no pass reads a session the
    /// store does not hold). An engine that reports none leaves the folder unread.
    pub(crate) fn seed_adopted_cwd(&mut self, cwd: Option<&str>) {
        if let Some(text) = held_cwd(cwd) {
            self.cwd_text = Some(text);
            self.cwd_unread = false;
        }
    }

    /// A SEAMLESS UPDATE ADOPTED THIS SESSION'S LOG ([`crate::spawn::mark_handoff_gap`], after
    /// the adoption): the folder and an agent's words aterm's default writer derives from live
    /// only in memory, so this process has not read them yet, and the claims the log carried, up
    /// to its newest seq now, are the update's ([`crate::describe`]).
    pub(crate) fn note_unread_after_update(&mut self) {
        self.cwd_unread = true;
        self.words_unread = true;
        self.carried_seq = self.slog.log().last_seq();
    }

    /// The one `title-change` writer: a row iff the session log's title mark moved; returns
    /// whether it recorded one. For a command line (`stands`) the mark, and so the row, keeps
    /// the title's fingerprint alone ([`crate::session_log::observed::TitleStands`]).
    pub(crate) fn record_title(
        &mut self,
        shown: &str,
        stands: crate::session_log::observed::TitleStands,
    ) -> bool {
        let at = self.slog.now();
        if !self.slog.observe_title(shown, stands, at) {
            return false;
        }
        self.record(TITLE_CHANGE);
        true
    }

    /// The resize ledger, for its readers (`resizes`, the `events` digest).
    pub(crate) const fn resizes(&self) -> &crate::resize_ledger::ResizeLedger {
        &self.resizes
    }

    /// Book a resize path's journal copy ([`crate::resize_ledger::ResizeLedger::ingest`]) and
    /// record any verdict transition as a `render` event.
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

    /// Evaluate the resize ledger against `probe` and record any verdict transition as a
    /// `render` event.
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

    /// Tests: the presence classifier's published reader, `reader`.
    #[cfg(test)]
    pub(crate) fn set_agent_reader_program_for_test(
        &mut self,
        reader: Option<aterm_phase::Program>,
    ) {
        self.set_agent_reader(reader);
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

    /// The repaint nudge's attempt for epoch `id` gave up, unanswered
    /// ([`crate::resize_epoch::EpochBook::note_unrepaired`]).
    pub(crate) fn note_resize_unrepaired(&mut self, id: u64) {
        self.resizes.note_unrepaired(id);
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
                | aterm_phase::Program::Grok
                | aterm_phase::Program::Kimi
                | aterm_phase::Program::Agy
                | aterm_phase::Program::Muse
                | aterm_phase::Program::Generic,
            )
            | None => false,
        });
    }

    /// Only entry into and exit from `desync-risk` reach the timeline, each a `render` mark.
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
            let at = self.slog.now();
            self.slog.observe_render(transition, at);
            self.record("render");
        }
        !transitions.is_empty()
    }

    /// A test's read of the timeline: the view's rows with `id > after` (all with `None`, or with
    /// an id past the view's head), oldest first.
    #[cfg(test)]
    pub(crate) fn since(&self, after: Option<u64>) -> std::vec::IntoIter<TimelineEvent> {
        self.view_since(after).rows.into_iter()
    }

    /// The id a seamless update's manifest carries (`SessionRecord::timeline_id`).
    #[cfg(any(unix, test))]
    pub(crate) fn handoff_id(&mut self) -> u64 {
        let carried = self
            .slog
            .row_head()
            .saturating_add(HANDOFF_ID_RESERVE)
            .min(i64::MAX as u64);
        self.handoff_ceiling = Some(carried);
        carried
    }

    /// Return whether this process has allocated a session id past the handoff ceiling.
    #[cfg(any(unix, test))]
    pub(crate) fn past_handoff_ceiling(&self) -> bool {
        self.handoff_ceiling
            .is_some_and(|ceiling| self.slog.row_head() > ceiling)
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

    /// The handoff worker's take of the session log.
    #[cfg(any(unix, test))]
    pub(crate) fn slog_carry_source(&mut self) -> crate::session_log::SlogCarrySource {
        self.slog.carry_source()
    }

    /// The session log ([`crate::session_log`]).
    pub(crate) const fn slog(&self) -> &crate::session_log::SessionLog {
        &self.slog
    }

    /// A timeline on the test constructor's session log, for a unit test that reads epochs by
    /// hand.
    #[cfg(test)]
    pub(crate) fn for_test() -> SessionTimeline {
        SessionTimeline {
            slog: crate::session_log::SessionLog::for_test(),
            ..SessionTimeline::default()
        }
    }

    /// A fixture timeline whose distinct epoch comes from one explicit
    /// process allocator and whose subsequent rows read one explicit clock.
    #[cfg(all(test, unix))]
    pub(crate) fn for_fixture(
        epochs: std::sync::Arc<crate::session_log::Epochs>,
        at: aterm_slog::Stamp,
    ) -> SessionTimeline {
        let mut slog = crate::session_log::SessionLog::starting(epochs, at);
        slog.set_clock(crate::session_log::Clock::reading(move || at));
        SessionTimeline {
            slog,
            ..SessionTimeline::default()
        }
    }

    /// The session log, to write.
    pub(crate) const fn slog_mut(&mut self) -> &mut crate::session_log::SessionLog {
        &mut self.slog
    }

    /// When the session log wants its settle tick (monotonic ms), if one is owed
    /// ([`crate::session_log::SessionLog::wake`]).
    pub(crate) fn slog_wake(&self) -> Option<u64> {
        self.slog.wake()
    }

    /// The event loop's settle tick.
    pub(crate) fn slog_tick(&mut self) -> Option<u64> {
        let at = self.slog.now();
        self.slog.tick(at)
    }

    /// Whether the session log's readers were owed a wake
    /// ([`crate::session_log::SessionLog::take_log_wake`]); rings them when they were.
    pub(crate) fn slog_take_wake(&mut self) -> bool {
        let owed = self.slog.take_log_wake();
        if owed {
            self.watchers.ring_watchers();
        }
        owed
    }

    /// Count on from a seamless update.
    pub(crate) fn continue_ids(&mut self, carried: u64) {
        self.slog.continue_rows(carried.min(i64::MAX as u64));
    }

    /// The newest row's id in the view, or `None` when it holds none (a test's).
    #[cfg(test)]
    pub(crate) fn high_id(&self) -> Option<u64> {
        self.since(None).next_back().map(|e| e.id)
    }

    /// The oldest row's id in the view, or `None` when it holds none (a test's).
    #[cfg(test)]
    pub(crate) fn low_id(&self) -> Option<u64> {
        self.since(None).next().map(|e| e.id)
    }

    /// How many rows the view holds (a test's).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.since(None).len()
    }

    /// The server's published program and agent verdict for this session.
    pub(crate) const fn agent(&self) -> &AgentPublication {
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

    /// The cached foreground and accepted word state captured before reading upgrade records.
    pub(crate) fn upgrade_stamp(&self) -> UpgradeStamp {
        UpgradeStamp {
            generation: self.foreground_generation,
            pgid: self.agent.program_pgid,
            holder: self.current_holder_image().and_then(|image| image.token),
            agent: crate::harness_host::identified_agent(&self.agent),
            conversation: self.upgrade_conversation().map(str::to_owned),
            word: self.upgrade_word(),
            word_revision: self.upgrade_word_revision,
        }
    }

    fn upgrade_word(&self) -> Option<(aterm_agent::harness::upgrade::Request, u64)> {
        self.upgrade
            .as_ref()
            .or(self.upgrade_kept.as_ref())
            .filter(|(row, _)| {
                row.request_at != 0 || row.request != aterm_agent::harness::upgrade::Request::None
            })
            .map(|(row, _)| (row.request.clone(), row.request_at))
    }

    /// An already admitted footer can identify a different conversation within the same image.
    fn upgrade_conversation(&self) -> Option<&str> {
        if let Some(image) = self
            .current_holder_image()
            .filter(|image| image.drawer.is_some())
        {
            return self
                .registry_owner(image)
                .map(|owner| owner.session_id.as_str());
        }
        self.claude_footer()
            .and_then(|facts| facts.owner.as_ref())
            .map(|owner| owner.session_id.as_str())
    }

    fn advance_foreground_generation(&mut self) {
        self.foreground_since = Some(std::time::Instant::now());
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
    pub(crate) const fn task(&self) -> Option<&crate::task_record::TaskRecord> {
        self.task.as_ref()
    }

    /// Note what the worker's launcher did with its line ([`Self::worker_launch`]).
    pub(crate) fn note_worker_launch(&mut self, words: String) {
        self.worker_launch = Some((std::time::Instant::now(), words));
    }

    /// What the worker's launcher did with its line, and when.
    pub(crate) const fn worker_launch(&self) -> Option<&(std::time::Instant, String)> {
        self.worker_launch.as_ref()
    }

    /// Note whether the worker's launcher is installing its program or preparing its Google sign-in
    /// now (`task_launch::InstallingMark`).
    pub(crate) fn set_worker_installing(&mut self, installing: bool) {
        if installing {
            self.worker_install_mark = self.worker_install_mark.wrapping_add(1);
        }
        self.worker_installing = installing;
        self.worker_install_child = None;
    }

    /// Which of the launcher's install marks stands or stood last. Two marks can follow each other
    /// between two looks of the startup deadline, which would read one `installing` from them;
    /// this tells them apart, so each has its quiet limit whole
    /// (`harness_host::INSTALL_QUIET_LIMIT`).
    pub(crate) const fn worker_install_mark(&self) -> u64 {
        self.worker_install_mark
    }

    /// Note the atpkg child the launcher's install just spawned (`task_launch::install_with`).
    pub(crate) fn set_worker_install_child(&mut self, mark: u64, pid: u32) -> bool {
        if !self.worker_installing_at(mark) {
            return false;
        }
        self.worker_install_child = Some(pid);
        true
    }

    /// Whether this exact launcher episode still owns the installation state.
    pub(crate) const fn worker_installing_at(&self, mark: u64) -> bool {
        self.worker_installing && self.worker_install_mark == mark
    }

    /// Release an episode without clearing a replacement launcher's installation.
    pub(crate) fn end_worker_install(&mut self, mark: u64) -> bool {
        if !self.worker_installing_at(mark) {
            return false;
        }
        self.set_worker_installing(false);
        true
    }

    /// The atpkg child the worker's launcher is installing its program by, whose pass on atpkg's
    /// `progress.json` is that install's work (`harness_host::InstallWork`). `None` before the
    /// spawn returns, under a launcher's Google preparation, which runs no such child, and once the
    /// mark is gone.
    pub(crate) const fn worker_install_child(&self) -> Option<u32> {
        self.worker_install_child
    }

    /// Whether the worker's launcher is installing its program or preparing its Google sign-in now.
    /// While it is, the startup deadline judges the worker by whether that work moves, not by the
    /// deadline's own 120 s (`harness_host::look_at_startups`, `harness_host::INSTALL_QUIET_LIMIT`).
    pub(crate) const fn worker_installing(&self) -> bool {
        self.worker_installing
    }

    /// Note why atpkg keeps the worker's program off this Mac, or that it does not, as the launcher's
    /// admission found.
    pub(crate) fn set_worker_kept_off(&mut self, why: Option<String>) {
        self.worker_kept_off = why;
    }

    /// Why atpkg keeps the worker's program off this Mac, which a stall names if no copy of the
    /// shell's own starts (`harness_host::kept_off_held`).
    pub(crate) fn worker_kept_off(&self) -> Option<&str> {
        self.worker_kept_off.as_deref()
    }

    /// Note the hold the worker's launcher has its launch under now, or that it ended
    /// (`task_launch::PoolHoldMark`).
    pub(crate) fn set_worker_pool_hold(&mut self, hold: Option<WorkerPoolHold>) {
        if hold.is_none() && self.worker_pool_hold.is_some() {
            self.worker_pool_hold_ended = Some(std::time::Instant::now());
        }
        self.worker_pool_hold = hold;
    }

    /// What the worker's launcher is holding its launch for now. The startup deadline judges a
    /// worker through this (`harness_host::StartupHold::LaunchHeld`): what holds it may stand for
    /// days, and whoever started the worker is owed that news.
    pub(crate) const fn worker_pool_hold(&self) -> Option<&WorkerPoolHold> {
        self.worker_pool_hold.as_ref()
    }

    /// When the worker's launcher last stopped holding its launch for a pool.
    pub(crate) const fn worker_pool_hold_ended(&self) -> Option<std::time::Instant> {
        self.worker_pool_hold_ended
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
    pub(crate) const fn need(&self) -> &crate::needs::Verdict {
        &self.need
    }

    /// Apply `mark` to the task record with `asks` standing
    /// ([`crate::task_record::TaskMark::apply`]) at `now`.
    pub(crate) fn mark_task(
        &mut self,
        mark: &crate::task_record::TaskMark,
        asks: &crate::task_record::Asks,
        now: std::time::Instant,
        now_unix_ms: u64,
    ) -> (crate::task_record::TaskRecord, bool) {
        // An exit belongs to the process that left, not a replacement already in this tab.
        // Serialize this decision with agent publication, so a late host worker cannot close
        // the replacement's task between a status read and this mark.
        if mark.state == crate::task_record::TaskState::Exited
            && mark.why == Some(crate::task_record::WHY_EXIT)
            && (self.task_agent_present() || self.agent.word == "busy")
            && let Some(task) = self.task.as_ref()
            && !task.runs_a_program()
            && matches!(
                task.state,
                crate::task_record::TaskState::Working | crate::task_record::TaskState::Stalled
            )
        {
            self.pending_task_exit = Some(mark.clone());
            return (task.clone(), false);
        }
        self.pending_task_exit = None;
        let (next, changed) = mark.apply(self.task.as_ref(), asks, now, now_unix_ms);
        if changed {
            let at = self.slog.now();
            self.slog.observe_task(&next, at);
            self.record("task-change");
        }
        self.task = Some(next.clone());
        self.tell_task_creator();
        (next, changed)
    }

    /// Tell the session log who created the session, as its task record now names it
    /// ([`crate::session_log::SessionLog::set_desc_creator`]): called wherever the record is
    /// set, so the creator the log ranks a description claim by is the record's `from`.
    fn tell_task_creator(&mut self) {
        let from = self.task.as_ref().map(|task| task.from.as_str());
        self.slog.set_desc_creator(from);
    }

    /// A worker's agent is currently identified in this tab. Program workers have their own
    /// block/exit-status lifecycle and are never reopened by an agent's screen.
    fn task_agent_present(&self) -> bool {
        self.task.as_ref().is_some_and(|task| {
            !task.runs_a_program() && crate::harness_host::identified_agent(&self.agent).is_some()
        })
    }

    /// A new agent observation supersedes an unfinished exit of its predecessor. A deferred exit
    /// takes effect when a fresh screen read sees the agent gone (`screen_read`). A resolver's
    /// name change alone can precede that screen read. Keep completed work and explicit failures intact.
    fn reconcile_task_agent(&mut self, screen_read: bool) -> bool {
        use crate::task_record::{TaskMark, TaskState, WHY_EXIT};
        if !self.task_agent_present() {
            if !screen_read {
                return false;
            }
            let Some(exit) = self.pending_task_exit.take() else {
                return false;
            };
            let asks = self
                .task
                .as_ref()
                .map(|task| task.asks.clone())
                .unwrap_or_default();
            return self
                .mark_task(
                    &exit,
                    &asks,
                    std::time::Instant::now(),
                    aterm_time::unix_millis().unwrap_or(0),
                )
                .1;
        }
        let Some(task) = self.task.as_ref().filter(|task| {
            task.state == TaskState::Exited && task.why == Some(WHY_EXIT) && task.unfinished_exit
        }) else {
            return false;
        };
        let asks = task.asks.clone();
        let mut working = TaskMark::new(TaskState::Working);
        working.why = Some("moved");
        let (_, moved) = self.mark_task(
            &working,
            &asks,
            std::time::Instant::now(),
            aterm_time::unix_millis().unwrap_or(0),
        );
        moved
    }

    /// A person's input into an ended tab begins their exchange, not the old worker's task.
    pub(crate) fn person_reached_task(&mut self) {
        if let Some(task) = self
            .task
            .as_mut()
            .filter(|task| task.state == crate::task_record::TaskState::Exited)
        {
            task.unfinished_exit = false;
        }
    }

    /// The asks standing under the task owners moved ([`crate::task_record::supervisor_asks`]):
    /// the record's rev moves when they raise or clear its attention or one joins them
    /// ([`crate::task_record::Asks::joins`]), never when one of several is taken down or the same
    /// ask is written again.
    pub(crate) fn note_task_attention(&mut self, asks: crate::task_record::Asks) -> bool {
        self.note_task_asks(asks, true)
    }

    /// [`Self::note_task_attention`] after words that restate another owner's ask
    /// (`task_record::AttentionEdit::Restate`): the record keeps `asks`, its rev moving only
    /// where they raise or clear its attention.
    pub(crate) fn restate_task_attention(&mut self, asks: crate::task_record::Asks) -> bool {
        self.note_task_asks(asks, false)
    }

    /// The record keeps `asks`; its rev moves where they raise or clear its attention or, with
    /// `joined` news, one joins them.
    fn note_task_asks(&mut self, asks: crate::task_record::Asks, joined: bool) -> bool {
        let Some(task) = self.task.as_mut() else {
            return false;
        };
        let raised = !asks.is_empty();
        let news = task.attention != raised || (joined && asks.joins(&task.asks));
        task.asks = asks;
        if !news {
            return false;
        }
        task.attention = raised;
        task.rev = task.rev.saturating_add(1);
        let at = self.slog.now();
        self.slog.observe_task(task, at);
        self.record("task-change");
        true
    }

    /// Edit the task record's fact word ([`crate::task_record::fact_edit`]): the outcome, and
    /// whether the record moved (its `task-change` recorded).
    pub(crate) fn note_task_fact(
        &mut self,
        word: Option<aterm_agent::supervise::occupancy::FactWord>,
    ) -> (aterm_agent::supervise::occupancy::NoteOutcome, bool) {
        let Some(task) = self.task.as_ref() else {
            return (
                aterm_agent::supervise::occupancy::NoteOutcome::NoTask,
                false,
            );
        };
        let (outcome, next) = crate::task_record::fact_edit(task, word);
        let Some(next) = next else {
            return (outcome, false);
        };
        let at = self.slog.now();
        self.slog.observe_task(&next, at);
        self.record("task-change");
        self.task = Some(next);
        self.tell_task_creator();
        (outcome, true)
    }

    /// Seed the record a seamless update carried, before the adopted session is registered
    /// ([`crate::task_record::seed_carried`]).
    pub(crate) fn seed_task(&mut self, record: crate::task_record::TaskRecord) {
        self.pending_task_exit = None;
        self.task = Some(record);
        self.tell_task_creator();
    }

    /// Note the result files the record just seeded came with under other session IDs
    /// ([`Self::carried_results`]), in place of any noted before.
    pub(crate) fn seed_carried_results(&mut self, mut paths: Vec<std::path::PathBuf>) {
        paths.truncate(CARRIED_RESULTS_MAX);
        self.carried_results = paths;
    }

    /// Note one more result file this tab's record named under another session ID
    /// ([`Self::carried_results`]), once, while fewer than [`CARRIED_RESULTS_MAX`] are noted. One
    /// past the bound is not noted: it stays when the tab ends, as a file nothing notes does,
    /// rather than growing every row that carries the list.
    pub(crate) fn note_carried_result(&mut self, path: std::path::PathBuf) {
        if self.carried_results.len() < CARRIED_RESULTS_MAX && !self.carried_results.contains(&path)
        {
            self.carried_results.push(path);
        }
    }

    /// The result files this tab's record named under other session IDs, to be removed with its
    /// tab.
    pub(crate) fn carried_results(&self) -> &[std::path::PathBuf] {
        &self.carried_results
    }

    /// ` carried=<name>,…` of a row that carries the record to another process
    /// ([`Self::task_carry_row`], [`crate::task_record::restore_row`]): the file names of
    /// [`Self::carried_results`], each directly in the tasks directory and shaped as a done edge
    /// writes it, so no comma or space is in one; nothing where there are none. An older build
    /// reads past the word.
    pub(crate) fn push_carried_results(&self, out: &mut String) {
        let names: Vec<&str> = self
            .carried_results
            .iter()
            .filter_map(|path| path.file_name()?.to_str())
            .filter(|name| crate::task_launch::snapshot_sid(name).is_some())
            .take(CARRIED_RESULTS_MAX)
            .collect();
        if !names.is_empty() {
            out.push_str(" carried=");
            out.push_str(&names.join(","));
        }
    }

    /// Note the shell block a program worker's launcher typed its program into
    /// ([`crate::task_record::note_program_block`]): taken where the record is a program worker's,
    /// still `working` or `stalled`, with none noted. Moves no rev.
    pub(crate) fn note_program_block(&mut self, block: u64) -> bool {
        use crate::task_record::TaskState;
        let Some(task) = self.task.as_mut().filter(|task| {
            task.runs_a_program()
                && task.program_block.is_none()
                && matches!(task.state, TaskState::Working | TaskState::Stalled)
        }) else {
            return false;
        };
        task.program_block = Some(block);
        true
    }

    /// Note that the startup deadline judged the worker of the task record at revision `rev`
    /// started ([`crate::task_record::note_task_took_up`]): taken where the record is still at
    /// `rev` and `working` or `stalled`. Moves no rev. Whether that record carries it now.
    pub(crate) fn note_task_took_up(&mut self, rev: u64) -> bool {
        use crate::task_record::TaskState;
        let Some(task) = self.task.as_mut().filter(|task| {
            task.rev == rev && matches!(task.state, TaskState::Working | TaskState::Stalled)
        }) else {
            return false;
        };
        task.took_up = true;
        true
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

    /// Name the program a program worker's spawn started on its task record
    /// ([`crate::task_record::TaskRecord::program`]); `None` for an agent's worker.
    pub(crate) fn set_task_program(&mut self, program: Option<&str>) {
        if let Some(task) = self.task.as_mut() {
            task.program = program.map(str::to_owned);
        }
    }

    /// Name the task's first line on its record ([`crate::task_record::TaskRecord::line`]), where
    /// a record exists. Moves no rev: no waiter reads it.
    pub(crate) fn set_task_line(&mut self, line: Option<String>) {
        if let Some(task) = self.task.as_mut() {
            task.line = line;
        }
        // No record says so: aterm's default writer is told.
        crate::describe::ring();
    }

    /// The Mac the task's manager runs on, where that is another Mac
    /// ([`crate::task_record::TaskRecord::manager_node`]).
    pub(crate) fn set_task_manager_node(&mut self, node: Option<String>) {
        if let Some(task) = self.task.as_mut() {
            task.manager_node = node;
        }
    }

    /// What the worker's mail told its manager of the task begun at `started_unix_ms`
    /// ([`crate::task_record::TaskRecord::told`]), where that is still the task: moves no rev,
    /// since nothing a waiter reads changed.
    pub(crate) fn task_told_mut(
        &mut self,
        started_unix_ms: u64,
    ) -> Option<&mut crate::worker_mail::Told> {
        self.task
            .as_mut()
            .filter(|task| task.started_unix_ms == started_unix_ms)
            .map(|task| &mut task.told)
    }

    /// The row a seamless update carries for the record, `None` with no task: the record's carry
    /// row and the results its tab named under other session IDs ([`Self::carried_results`]).
    #[cfg(any(unix, test))]
    pub(crate) fn task_carry_row(&self) -> Option<String> {
        let mut row = self.task.as_ref()?.carry_row(std::time::Instant::now());
        self.push_carried_results(&mut row);
        Some(row)
    }

    /// The published program for foreground group `pgid`, or `None` when the publication names
    /// another group.
    pub(crate) fn program_of(&self, pgid: i32) -> Option<&str> {
        if pgid > 0 && self.agent.program_pgid == pgid {
            self.agent.program.as_deref()
        } else {
            None
        }
    }

    /// THE STREAM'S ARM of the state: a `state` record the claim's stream applied. From here
    /// until the claim stops feeding it ([`Self::settle_stream_claim`]) the record decides the agent word and
    /// the composer, and the screen reader's reading only supplies the kind `asking` and
    /// `walled` do not name. Returns whether the published word or detail moved.
    #[cfg(any(unix, test))]
    pub(crate) fn publish_stream_state(&mut self, fed: StreamFed) -> bool {
        let old_word = self.agent.word;
        self.agent.stream = Some(fed);
        self.settle_composer();
        let changed = self.settle_agent_word(false);
        // No screen was read: an exit a screen read would confirm stays pending.
        let task_changed = self.reconcile_task_agent(false);
        if self.agent.word != old_word {
            crate::harness_host::ring_phase();
        }
        changed || task_changed
    }

    /// THE HARNESS REFUSED aterm's record `of` (`refused`), saying `why`: the stream's write arm
    /// logs it at the session node, whatever run is open, as `mark - refused in=<of> text=<why>`
    /// under the claim's `holder`, by the tab `by`
    /// ([`crate::session_log::SessionLog::stream_mark`]). The harness's words go in the
    /// host-tier `text=`, sanitized and clamped to its bound, never in the closed `why=`; words
    /// that leave nothing write no `text=`. Whether the mark was written.
    #[cfg(unix)]
    pub(crate) fn stream_refused(
        &mut self,
        of: u64,
        why: &str,
        holder: &str,
        by: aterm_slog::Actor,
    ) -> bool {
        let mut fields = aterm_slog::Fields::new().u(aterm_slog::Key::In, of);
        if let Some(text) = aterm_slog::BoundedText::new(aterm_slog::Key::Text, why) {
            fields =
                fields.and_then(|f| f.with(aterm_slog::Key::Text, aterm_slog::Val::Text(text)));
        }
        let Ok(fields) = fields else {
            return false;
        };
        let at = self.slog.now();
        let written = self
            .slog
            .stream_mark(
                aterm_slog::Kind::Refused,
                Some(aterm_slog::Node::SESSION),
                fields,
                holder,
                by,
                at,
            )
            .is_some();
        self.ring_rows();
        written
    }

    /// THE STREAM'S ARM of the kinds beside `state`: a `usage`, `account`, `progress`, `goal`,
    /// `compact`, `model` or `upgrade` record the claim's stream applied. The record is kept in
    /// the current-state record and its row is written under the kind's own word, stamped
    /// `holder=`, `src=harness`, `conf=claim` and `by=<the tab's session>` ([`crate::session_log::
    /// SessionLog::stream_mark`]); `account`, `model` and `upgrade` write no row (D31). From here
    /// until the claim stops feeding the kind ([`Self::settle_stream_claim`]) its built-in
    /// source stands down.
    ///
    /// Returns whether the record was applied. Where the log refused its row (a closed session
    /// log, a holder that is no ID) it was not: nothing of the record is kept, what the stream
    /// kept of that kind before is dropped, and the kind's built-in source feeds, so a kind is
    /// never left with its built-in writer silenced and no row from the stream. The caller
    /// takes the kind out of the applied set and acknowledges the record as refused.
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "only the Unix control stream applies a record")
    )]
    #[must_use]
    pub(crate) fn publish_stream_record(
        &mut self,
        stamp: StreamStamp,
        record: StreamRecord,
    ) -> bool {
        let StreamStamp {
            epoch,
            hseq,
            holder,
            by,
        } = stamp;
        let at = self.slog.now();
        let kinds = &mut self.agent.streamed;
        let written = match record {
            StreamRecord::Usage {
                account,
                limits,
                context_left,
            } => {
                let usage = StreamUsage {
                    account,
                    limits,
                    context_left,
                    epoch,
                    hseq,
                };
                let written = self.slog.stream_usage(&usage, &holder, by, at).is_some();
                kinds.usage = written.then_some(usage);
                written
            }
            StreamRecord::Account {
                account,
                state,
                line,
            } => {
                kinds.account = Some(StreamAccount {
                    account,
                    state,
                    epoch,
                    hseq,
                    line,
                    fed: true,
                });
                true
            }
            StreamRecord::Progress { items } => {
                let done = items
                    .iter()
                    .filter(|(_, state)| *state == StreamedItem::Done)
                    .count();
                let written = self
                    .slog
                    .stream_progress(items.len(), done, hseq, &holder, by, at)
                    .is_some();
                kinds.progress = written.then_some(StreamProgress { items, epoch, hseq });
                written
            }
            StreamRecord::Goal { text } => {
                let written = self
                    .slog
                    .stream_goal(text.as_deref(), hseq, &holder, by, at)
                    .is_some();
                kinds.goal = written.then_some(StreamGoal { text, epoch, hseq });
                written
            }
            StreamRecord::Compact { done, context_left } => {
                let written = self
                    .slog
                    .stream_compact(done, context_left, hseq, &holder, by, at);
                kinds.compact = written.then_some(StreamCompact {
                    done,
                    context_left,
                    epoch,
                    hseq,
                });
                written
            }
            // D31: `model` and `upgrade` write no log row of their own; each is kept here.
            StreamRecord::Model {
                model,
                effort,
                key,
                offers,
                permission,
                line,
            } => {
                kinds.model = Some(StreamModel {
                    model,
                    effort,
                    key,
                    offers,
                    permission,
                    epoch,
                    hseq,
                    line,
                });
                true
            }
            StreamRecord::Upgrade { phase, to, line } => {
                kinds.upgrade = Some(StreamUpgrade {
                    phase,
                    to,
                    at_s: aterm_time::unix_millis().unwrap_or(0) / 1000,
                    epoch,
                    hseq,
                    line,
                });
                true
            }
        };
        self.settle_stream_down();
        self.ring_rows();
        written
    }

    /// A `need` (`clear` false) or `clear` RECORD's row, `mark - need|clear word=<the need's
    /// word> [text=<its entry>] hseq= holder=`, through the stream's write arm
    /// ([`crate::session_log::SessionLog::stream_need`]): whether the log took it. A need taken
    /// is kept by its word with the stamp that raised it ([`Self::stream_needs`]) until a clear
    /// of that word is taken or the stream stops holding it ([`Self::settle_stream_claim`]).
    #[cfg(unix)]
    pub(crate) fn note_stream_need(
        &mut self,
        clear: bool,
        word: &'static str,
        entry: Option<&str>,
        stamp: &StreamStamp,
    ) -> bool {
        let at = self.slog.now();
        let written = self
            .slog
            .stream_need(clear, word, entry, stamp, at)
            .is_some();
        if written {
            if clear {
                self.stream_needs.remove(word);
            } else {
                self.stream_needs
                    .insert(word, (stamp.holder.clone(), stamp.by));
            }
        }
        self.ring_rows();
        written
    }

    /// THE CLAIM'S STREAM APPLIED A RECORD of any kind (D15): where no agent run is open, one
    /// opens for `agent`, the agent the claim's registry row names. The one exception is a
    /// stream whose standing `state` is `ended`: the record said the agent is gone, and a
    /// `beat` after it opens nothing. Called after the record itself was applied, so a `state`
    /// that moved the word has opened the run already, with its mark on it.
    #[cfg(unix)]
    pub(crate) fn stream_applied_record(
        &mut self,
        agent: aterm_phase::Program,
        by: aterm_slog::Actor,
    ) -> bool {
        if self
            .agent
            .stream
            .as_ref()
            .is_some_and(|fed| fed.state == StreamedState::Ended)
        {
            return false;
        }
        let at = self.slog.now();
        let opened = self
            .slog
            .stream_applied(agent, by, self.agent.program_pgid, at);
        if opened {
            self.ring_rows();
        }
        opened
    }

    /// The stream's state no longer stands: the screen reader feeds the state and the composer
    /// again, from what it reads now.
    fn end_stream_state(&mut self) {
        let old_word = self.agent.word;
        self.agent.stream = None;
        self.settle_composer();
        self.settle_agent_word(false);
        if self.agent.word != old_word {
            crate::harness_host::ring_phase();
        }
    }

    /// THE READER'S ARM of the composer: what the screen reader reads in the input box. It is
    /// kept, and published unless the stream's state stands.
    pub(crate) fn set_composer(&mut self, composer: aterm_messages::presence::ComposerState) {
        self.agent.screen_composer = composer;
        self.settle_composer();
    }

    /// Publish the composer: the stream's while its state stands, else the screen reader's. A
    /// move rings the open record stream, whose `hand` carries it.
    fn settle_composer(&mut self) {
        let composer = self
            .agent
            .stream
            .as_ref()
            .map_or(self.agent.screen_composer, |fed| fed.composer);
        if std::mem::replace(&mut self.agent.composer, composer) != composer {
            self.watchers.ring_stream();
        }
    }

    /// THE READER'S ARM of the state: an agent verdict read from the screen `stamp` names.
    /// While the stream's state stands the reading is kept (it names the kind `asking` and
    /// `walled` do not) and the published word is the stream's.
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
        self.set_agent_reader(reader);
        if self.agent.screen_word != word {
            self.agent.screen_word_unix_ms = aterm_time::unix_millis().unwrap_or(0);
        }
        self.agent.screen_word = word;
        self.agent.screen_detail = detail;
        let changed = self.settle_agent_word(regrid);
        let task_changed = self.reconcile_task_agent(true);
        self.settle_regrid(regrid, changed);
        if reader_changed || self.agent.word != old_word {
            // One wake after publication, even if both reader and phase moved.
            crate::harness_host::ring_phase();
        } else if self.note_screen_move(std::time::Instant::now()) {
            crate::harness_host::ring_phase();
        }
        changed || task_changed
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

    /// Publish (`Some`) or clear (`None`) the input stall ([`AgentPublication::input`]) and
    /// re-settle the effective word.
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
        let (word, detail, streamed) = self.agent.effective();
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
        let at = self.slog.now();
        let moved = crate::session_log::AgentMove {
            word,
            detail: self.agent.detail.as_deref(),
            rev: self.agent.rev,
            stamp: self.agent.stamp,
            reader: self.agent.reader,
            pgid: self.agent.program_pgid,
            stream: self.agent.stream.as_ref().filter(|_| streamed).map(|fed| {
                crate::session_log::StreamMove {
                    hseq: fed.hseq,
                    holder: &fed.holder,
                    by: fed.by,
                    agent: fed.agent,
                }
            }),
        };
        let _ = self.slog.agent_moved(moved, at);
        // A box the verdict closed with no answer and opened again keeps the supervisor's
        // standing ask (fix E3 review).
        self.slog.carry_escalation(at);
        self.record("agent-change");
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

    /// The grid at this read of the screen.
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
        self.leader_no_harness_applied = None;
        self.leader_wait = None;
        if self.agent.program.take().is_some() || had_reader {
            crate::harness_host::ring();
        }
        let at = self.slog.now();
        self.slog.foreground_moved(pgid, at);
        true
    }

    /// The status sweep saw the engine's cwd report `seq` (`Terminal::reported_cwd_seq`) with
    /// group `pgid` in front.
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
    pub(crate) const fn cwd_witness(&self) -> Option<(u64, i32)> {
        self.cwd_witness
    }

    /// The session's own process group (its shell's pid), for the session log.
    pub(crate) fn note_root_group(&mut self, pgid: i32) {
        self.slog.note_root(pgid);
    }

    /// The session-log step with no turn edges read ([`Self::sweep_slog_turns`]), as a restore's
    /// adoption sweep takes it: a restored engine's ring holds no `clear` and at most the resumed
    /// entry of a report restored in a turn (turns are never carried), so the next status sweep's
    /// first reading feeds it to the program then in front.
    #[cfg(any(unix, test))]
    pub(crate) fn sweep_slog(
        &mut self,
        blocks: &crate::session_log::BlockRead,
        seen: bool,
        root: i32,
        pgid: i32,
    ) -> Option<i32> {
        let mut none = crate::session_log::ProgramRead::default();
        self.sweep_slog_turns(blocks, seen, root, pgid, &mut none)
    }

    /// The status sweep's session-log step, in one order under one hold of this lock: on a
    /// foreground move, the program's turn edges before the leaving program's `clear` first
    /// ([`crate::session_log::SessionLog::feed_leaving`]), judged by the name of the group that
    /// left (the root shell, or none, is no codex), before the block read can close its run; then
    /// the block read and the move. The edges past them wait in `program` for
    /// [`Self::feed_program_turns`].
    pub(crate) fn sweep_slog_turns(
        &mut self,
        blocks: &crate::session_log::BlockRead,
        seen: bool,
        root: i32,
        pgid: i32,
        program: &mut crate::session_log::ProgramRead,
    ) -> Option<i32> {
        use crate::session_log::Leaving;
        let at = self.slog.now();
        if pgid > 0 && pgid != self.agent.program_pgid {
            let left = self.agent.program_pgid;
            let leaving = match self.agent.program.as_deref() {
                _ if left <= 0 || left == root => Leaving::Nothing,
                None => Leaving::MaybeCodex,
                Some(name) => {
                    if crate::program_state::honoured_for(
                        Some(name),
                        Some(aterm_core::terminal::program_state::ProgramApp::Codex),
                        true, // The job flag is read only for Other; this is Codex.
                    ) {
                        Leaving::MaybeCodex
                    } else {
                        Leaving::Other
                    }
                }
            };
            // A turn record the move's edges wrote is heard on this wake, as the front's are
            // ([`Self::feed_program_turns`]).
            if self.slog.feed_leaving(program, leaving, at) {
                self.ring();
            }
        }
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

    /// The holder image's one writer.
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
    pub(crate) const fn holder_image(
        &self,
    ) -> Option<&std::sync::Arc<crate::holder_image::HolderImage>> {
        self.holder_image.as_ref()
    }

    /// The group each holder-fence mark's drawer leads ([`crate::holder_image::DrawerGroups`]).
    #[cfg(unix)]
    pub(crate) const fn drawer_groups(&self) -> &crate::holder_image::DrawerGroups {
        &self.drawer_groups
    }

    /// See [`Self::drawer_groups`].
    pub(crate) const fn drawer_groups_mut(&mut self) -> &mut crate::holder_image::DrawerGroups {
        &mut self.drawer_groups
    }

    fn current_holder_image(&self) -> Option<&crate::holder_image::HolderImage> {
        self.holder_image
            .as_deref()
            .filter(|image| image.pgid > 0 && image.pgid == self.agent.program_pgid)
    }

    /// The vendor build the foreground agent's image runs, as its store directory names it
    /// (`2.1.305`, `1.4.13-R6996.1`): read when the image was classified, never on this call.
    pub(crate) fn running_build(&self) -> Option<atpkg::vendor_direct::Version> {
        self.current_holder_image()?.build.version()
    }

    /// Whether the current holder image is a card drawer with a token.
    pub(crate) fn card_drawer_current(&self) -> bool {
        self.current_holder_image()
            .is_some_and(|image| image.drawer.is_some() && image.token.is_some())
    }

    /// The card's gate (card engine §3.8), read under the splice's one timeline lock per pane.
    pub(crate) fn card_gate(&self) -> Option<crate::holder_image::CardGate> {
        let image = self.current_holder_image()?;
        image.gate(|| self.registry_build(image))
    }

    fn registry_build(
        &self,
        image: &crate::holder_image::HolderImage,
    ) -> Option<atpkg::vendor_direct::Version> {
        self.registry_owner(image)?;
        self.claude_footer_build
    }

    fn registry_owner(
        &self,
        image: &crate::holder_image::HolderImage,
    ) -> Option<&aterm_agent::harness::footer::FactsOwner> {
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
        Some(owner)
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

    /// The drift (card engine §3.10), once per drawer token and reported build: the registry row
    /// reports a build the Store or Native Claude drawer image does not name.
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
        from: u64,
    ) -> bool {
        if self.card_seen == Some(token)
            || !self.holder_image.as_ref().is_some_and(|image| {
                image.token == Some(token) && image.matches_card(rows_kind, version)
            })
        {
            return false;
        }
        self.card_seen = Some(token);
        self.card_frames.seen = Some((token, row, self.card_frames.composed, from));
        self.card_frames.late = 0;
        self.record_row(
            "card-seen",
            format!(
                "token={} rows_kind={rows_kind} row={row} after={} from={from} evidence=matching-screen {}",
                crate::holder_image::token_word(Some(token)),
                self.card_frames.composed,
                crate::holder_image::dev_tail()
            ),
        );
        true
    }

    /// Record dev diagnostic rows for one launch-card compose pass, whose snapshot counted
    /// `mutations` screen mutations: after the slice the probe saw the card in, it shows that card.
    pub(crate) fn note_card_frame(
        &mut self,
        outcome: crate::claude_card::CardOutcome,
        composed: u64,
        mutations: u64,
    ) {
        use crate::claude_card::{Bare, CardOutcome};
        let token_word = |t: u64| crate::holder_image::token_word((t != 0).then_some(t));
        self.card_frames.composed = composed;
        let shows_seen = self
            .card_frames
            .seen
            .filter(|&(t, _, _, from)| mutations > from && self.card_frames.decorated != Some(t))
            .map(|(t, ..)| t);
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
                        Some((t, r, ..)) if t == token && r == row => self.card_frames.late,
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
                    && ((refused && row.is_some()) || shows_seen == Some(token))
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
        if shows_seen.is_some_and(|t| self.card_frames.decorated != Some(t)) {
            self.card_frames.late = self.card_frames.late.saturating_add(1);
        }
    }

    /// The token and drawer of the current holder image when it is a drawer whose card has not
    /// been seen yet.
    pub(crate) fn card_unseen(&self) -> Option<(u64, crate::holder_image::CardProgram)> {
        let image = self
            .holder_image
            .as_ref()
            .filter(|i| i.build.version().is_some())?;
        let token = image.token.filter(|t| self.card_seen != Some(*t))?;
        Some((token, image.drawer?))
    }

    /// The resolver's answer for `pgid`, applied only while that group is in front (a late answer
    /// for a finished job is dropped).
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
            self.reconcile_task_agent(false);
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

    /// The footer resolver's answer for `pgid` and the process's kernel start time (when read),
    /// applied only while that group is in front.
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
        if let (Some((np, ns, new)), Some((op, os, old))) =
            (next.as_mut(), self.claude_footer.as_ref())
            && np == op
            && ns == os
            && new.owner == old.owner
        {
            new.account_progress = old.account_progress.clone();
            new.account_progress_busy = old.account_progress_busy;
        }
        let is_muse_or_agy = match self
            .agent
            .program
            .as_deref()
            .and_then(aterm_phase::program_of)
        {
            Some(aterm_phase::Program::Muse | aterm_phase::Program::Agy) => true,
            Some(
                aterm_phase::Program::Claude
                | aterm_phase::Program::Codex
                | aterm_phase::Program::Grok
                | aterm_phase::Program::Kimi
                | aterm_phase::Program::Generic,
            )
            | None => false,
        };
        let mut switched = false;
        if is_muse_or_agy
            && let Some(email) = next
                .as_ref()
                .and_then(|(_, _, facts)| facts.subscription.as_ref()?.email.as_deref())
            && self.muse_card_account.as_deref() != Some(email)
        {
            switched = self.muse_card_account.is_some();
            self.muse_cards_need_account |= switched;
            self.muse_card_account = Some(email.to_owned());
        }
        if is_muse_or_agy
            && self.muse_cards_need_account
            && let Some(sub) = next
                .as_mut()
                .and_then(|(_, _, facts)| facts.subscription.as_mut())
        {
            if sub.weekly_figure.is_none() {
                sub.weekly = None;
                sub.weekly_reset = None;
            }
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
        // A process that takes up the conversation the footer last showed an effort for (the
        // relaunch an update or a restart makes resumes it with the same settings) runs at that
        // effort until its own word: where its facts leave the effort open for the same model,
        // the level is its card's, so `◆` keeps it where no frame read Claude's start-up hint
        // (a background tab, an occluded window, another Space).
        if self.claude_card.is_none()
            && !is_muse_or_agy
            && let Some((np, ns, facts)) = &next
            && let Some((owner, card)) = self.carried_card(facts)
        {
            self.claude_card = Some((*np, *ns, owner, card));
        }
        if let Some((pgid, started, facts)) = &next {
            self.slog.note_agent_usage(
                agent_run(*pgid, *started, facts),
                whole_tokens(facts),
                facts.account.clone(),
            );
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
        self.note_shown_effort();
        true
    }

    /// The launch card that carries the effort the footer last showed for `facts`' conversation
    /// ([`Self::note_shown_effort`]) to the process `facts` were read for, and whose it is:
    /// `None` unless they leave the effort open for the model it was shown with.
    fn carried_card(
        &self,
        facts: &aterm_agent::harness::footer::FooterFacts,
    ) -> Option<(
        aterm_agent::harness::footer::FactsOwner,
        aterm_phase::LaunchCard,
    )> {
        let (conversation, model, effort) = self.claude_effort.as_ref()?;
        let owner = facts.owner.as_ref()?;
        let to = facts.effort_for.as_ref().or(facts.model.as_ref())?;
        (facts.effort_open && owner.session_id == *conversation && to == model).then(|| {
            (
                owner.clone(),
                aterm_phase::LaunchCard {
                    version: facts.version.clone().unwrap_or_default(),
                    model: model.clone(),
                    effort: Some(effort.clone()),
                },
            )
        })
    }

    /// Keep the effort the footer shows for the facts in front, with their conversation and the
    /// model it is shown for ([`Self::carried_card`]); what it kept stands while none is shown.
    fn note_shown_effort(&mut self) {
        let shown = self.claude_footer().and_then(|facts| {
            let owner = facts.owner.as_ref()?;
            let (model, effort) = facts.filled_model_effort(self.claude_card());
            Some((
                owner.session_id.clone(),
                model?.to_owned(),
                effort?.to_owned(),
            ))
        });
        if shown.is_some() {
            self.claude_effort = shown;
        }
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
        self.note_shown_effort();
        true
    }

    /// Keep `level`, the effort Claude's own hint names on a row the holder `token` drew, read off
    /// the live screen at an output (`App::read_claude_effort_hint`), frame or not: the footer
    /// pass takes it for that holder where no frame read the hint.
    pub(crate) const fn note_effort_hint(&mut self, token: u64, level: &'static str) {
        self.effort_hint = Some((token, level));
    }

    /// Whether an output at `now` still looks for holder `token`'s effort hint on the live screen:
    /// for [`EFFORT_HINT_LOOK`] from the first output that looked for it, the time Claude's
    /// start-up hint is up in (it shows some 10 s after a launch). A holder whose hint was never
    /// read (a mode row too narrow for it, a launch that draws none) is looked for no longer.
    pub(crate) fn effort_hint_looks(&mut self, token: u64, now: std::time::Instant) -> bool {
        match self.effort_hint_look {
            Some((looked, since)) if looked == token => {
                now.saturating_duration_since(since) < EFFORT_HINT_LOOK
            }
            _ => {
                self.effort_hint_look = Some((token, now));
                true
            }
        }
    }

    /// The level [`Self::note_effort_hint`] kept for holder `token`.
    pub(crate) fn effort_hint(&self, token: u64) -> Option<&'static str> {
        self.effort_hint
            .filter(|(kept, _)| *kept == token)
            .map(|(_, level)| level)
    }

    /// The process the stored footer facts were read for.
    pub(crate) fn claude_footer_identity(&self) -> Option<(i32, Option<u64>)> {
        self.claude_footer
            .as_ref()
            .map(|(pgid, started, _)| (*pgid, *started))
    }

    /// A known switch or carried glass cannot attribute an anonymous Muse card to the current
    /// login.
    pub(crate) const fn muse_cards_need_account(&self) -> bool {
        self.muse_cards_need_account
    }

    /// An adopted terminal may still show another account's cards, and its predecessor need not
    /// have carried an account observation.
    pub(crate) const fn note_carried_glass(&mut self) {
        self.muse_cards_need_account = true;
    }

    /// Forget the footer facts.
    pub(crate) fn clear_claude_footer(&mut self) -> bool {
        self.claude_card = None;
        self.claude_effort = None;
        self.claude_excerpts = None;
        self.claude_side = None;
        self.claude_footer_read = None;
        self.claude_footer_build = None;
        self.claude_footer_image = None;
        self.claude_footer.take().is_some()
    }

    /// The footer resolver read `pgid`'s facts at `at`, kept only while that group is in front.
    pub(crate) const fn note_claude_footer_read(&mut self, pgid: i32, at: std::time::Instant) {
        if self.agent.program_pgid == pgid {
            self.claude_footer_read = Some((pgid, at));
        }
    }

    /// When the facts [`Self::claude_footer`] gives were last read, while they are the front
    /// group's.
    pub(crate) fn claude_footer_read_at(&self) -> Option<std::time::Instant> {
        self.claude_footer_read
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, at)| at)
    }

    /// The footer resolver's excerpts for `pgid`, applied only while that group is in front:
    /// whether what aterm's default writer reads of them moved ([`crate::describe::excerpts_seen`]),
    /// which tells it. A re-read that found what it held tells it nothing.
    pub(crate) fn set_claude_excerpts(
        &mut self,
        pgid: i32,
        excerpts: Option<crate::survey::ClaudeExcerpts>,
    ) -> bool {
        if self.agent.program_pgid != pgid {
            return false;
        }
        let before = crate::describe::excerpts_seen(self);
        let held = self
            .claude_excerpts
            .as_ref()
            .filter(|(held, _)| *held == pgid)
            .map(|(_, e)| crate::survey::excerpt_words(&e.excerpts));
        self.words_read.note(
            held,
            excerpts
                .as_ref()
                .map(|e| crate::survey::excerpt_words(&e.excerpts)),
            aterm_gui_timing::wall_ms(),
        );
        self.claude_excerpts = excerpts.map(|e| (pgid, e));
        self.words_unread = false;
        let moved = crate::describe::excerpts_seen(self) != before;
        if moved {
            // No record says so: aterm's default writer is told.
            crate::describe::ring();
        }
        moved
    }

    /// When each of the agent's words this timeline holds last changed as read
    /// ([`Self::words_read`]).
    pub(crate) const fn words_read(&self) -> crate::survey::WordsRead {
        self.words_read
    }

    /// The folder the drift flush last read ([`Self::record_cwd_change`]) or the adopted engine
    /// held ([`Self::seed_adopted_cwd`]); `None` where it read none, or has yet to read one after
    /// an update ([`Self::cwd_unread`]).
    pub(crate) fn cwd_text(&self) -> Option<&str> {
        self.cwd_text.as_deref()
    }

    /// Whether this process has yet to read the folder of a session a seamless update adopted.
    pub(crate) const fn cwd_unread(&self) -> bool {
        self.cwd_unread
    }

    /// Whether this process has yet to read the words of the agent of a session a seamless
    /// update adopted.
    pub(crate) const fn words_unread(&self) -> bool {
        self.words_unread
    }

    /// The newest seq of the log a seamless update adopted, as of the adoption; 0 for a session
    /// never adopted.
    pub(crate) const fn carried_seq(&self) -> u64 {
        self.carried_seq
    }

    /// What aterm's default writer keeps of this session between its passes.
    pub(crate) const fn described(&self) -> &crate::describe::Held {
        &self.described
    }

    /// [`Self::described`], for the writer's pass.
    pub(crate) const fn described_mut(&mut self) -> &mut crate::describe::Held {
        &mut self.described
    }

    /// SINCE WHEN THE ROW'S TOKEN COUNTERS HAVE COUNTED `conversation` UNBROKEN (a survey row's
    /// `tok_for_ms`, as Unix ms): the first `now_ms` this timeline sent whole counters for it.
    /// Counters that are not whole (`whole == false`) clear it, and another conversation (a new
    /// one, or the same one in a restarted process: its key names the process) starts it again,
    /// so an observer never takes a rate across a restart's catch-up or a resumed conversation.
    /// `None` while not whole. An unread clock (`now_ms == 0`, the survey's word for it) starts
    /// no epoch: a start of 0 would outlive it, and every later row would say the counters had
    /// counted since 1970. Its conversation's epoch then starts at the first read clock, which
    /// is later than the truth and never earlier.
    pub(crate) fn token_epoch(
        &mut self,
        conversation: &str,
        whole: bool,
        now_ms: u64,
    ) -> Option<u64> {
        if !whole {
            self.token_epoch = None;
            return None;
        }
        match &self.token_epoch {
            Some((held, since)) if held == conversation => Some(*since),
            _ if now_ms == 0 => {
                self.token_epoch = None;
                None
            }
            _ => {
                self.token_epoch = Some((conversation.to_owned(), now_ms));
                Some(now_ms)
            }
        }
    }

    /// The survey's excerpts for the current foreground group, or `None` when the program in front
    /// is not the one they were read for.
    pub(crate) fn claude_excerpts(&self) -> Option<&crate::survey::ClaudeExcerpts> {
        self.claude_excerpts
            .as_ref()
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, excerpts)| excerpts)
    }

    /// What the program in front last said about itself (OSC 7501), and since when.
    pub(crate) const fn program_shown(&self) -> Option<ShownProgram> {
        self.program_shown
    }

    /// What the words say the program in front says about itself: [`Self::program_shown`], else
    /// another program's report ([`Self::note_other_program`]). Only the words read it
    /// ([`crate::program_state::survey_word`]); nothing that decides does.
    pub(crate) const fn program_said(&self) -> Option<ShownProgram> {
        match self.program_shown {
            Some(shown) => Some(shown),
            None => self.program_other,
        }
    }

    /// The agent word a surface shows and how long it has held, in ms: the reader's, or where the
    /// reader names no state (`unknown`, codex's) and the program says itself (OSC 7501), the
    /// program's word ([`crate::program_state::survey_word`]) and its report's age. Render-time
    /// only: `agent=` stays the reader's.
    pub(crate) fn shown_agent_word(&self) -> (&'static str, u64) {
        match self.program_said().filter(|_| self.agent.word == "unknown") {
            Some(shown) => (
                crate::program_state::survey_word(shown.state),
                shown.age_ms(),
            ),
            None => (self.agent.word, self.agent.age_ms()),
        }
    }

    /// The word the A/B book charges (`HarnessScreen::note_phase`) and the ledger instant it
    /// moved: the reader's, or where the reader names no state (`unknown`, codex's) the program's
    /// own report's ([`Self::shown_agent_word`]'s rule; none shown is `unknown`), so the busy
    /// time of a codex-alab whose screen aterm reads no phase from counts toward its landing's
    /// work floor (`harness_task::END_WORK_MS`). An `error` report (a turn an API or connection
    /// error ended) is `unknown` here, no phase: a system message is a wait, not the agent's turn
    /// end (design §14.17.5), as Claude Code's walls charge no phase.
    pub(crate) fn ab_word(&self) -> (&'static str, u64) {
        use aterm_core::terminal::program_state::ProgramStateWord;
        if self.agent.word != "unknown" {
            return (self.agent.word, self.agent.changed_ms);
        }
        let word = self
            .program_shown
            .filter(|shown| shown.state.state != ProgramStateWord::Error)
            .map_or(self.agent.word, |shown| {
                crate::program_state::survey_word(shown.state)
            });
        (word, self.agent.changed_ms.max(self.program_moved_ms))
    }

    /// Whether the program's own report is the word the A/B book charges ([`Self::ab_word`]).
    pub(crate) fn program_speaks(&self) -> bool {
        self.agent.word == "unknown"
    }

    /// The rollout tap's facts for the codex process in group `pgid` ([`crate::codex_rollout_tap`]),
    /// taken only while that group is in front: whether they were.
    pub(crate) fn set_codex_survey(
        &mut self,
        pgid: i32,
        survey: crate::survey::CodexSurvey,
    ) -> bool {
        if self.agent.program_pgid != pgid {
            return false;
        }
        let previous = self
            .codex_survey()
            .filter(|old| old.thread == survey.thread);
        let same_count = previous.is_some_and(|old| {
            old.facts.tok_at_ms == survey.facts.tok_at_ms
                && old.facts.tok_in == survey.facts.tok_in
                && old.facts.tok_out == survey.facts.tok_out
        });
        if !same_count {
            self.codex_token_clock = survey.facts.tok_at_ms.and_then(|ms| {
                let now = std::time::Instant::now();
                // Keep the provider's exact intervals, including two revisions with one stamp.
                let at = if let Some((prior, (anchored, _))) = previous
                    .and_then(|v| v.facts.tok_at_ms)
                    .zip(self.codex_token_clock)
                {
                    let delta = std::time::Duration::from_millis(ms.abs_diff(prior));
                    if ms >= prior {
                        anchored.checked_add(delta)?
                    } else {
                        anchored.checked_sub(delta)?
                    }
                } else {
                    let source = std::time::Duration::from_millis(u64::try_from(ms).ok()?);
                    now.checked_sub(aterm_time::since_epoch()?.checked_sub(source)?)?
                };
                (at <= now).then_some((at, now))
            });
        }
        let before = crate::describe::excerpts_seen(self);
        let (ctx, goal) = (survey.facts.ctx_left_pct, survey.goal.is_some());
        let held = self
            .codex_survey
            .as_ref()
            .filter(|(held, _)| *held == pgid)
            .map(|(_, held)| crate::survey::excerpt_words(&held.facts.excerpts));
        self.words_read.note(
            held,
            Some(crate::survey::excerpt_words(&survey.facts.excerpts)),
            aterm_gui_timing::wall_ms(),
        );
        self.codex_survey = Some((pgid, survey));
        self.words_unread = false;
        let at = self.slog().now();
        self.slog_mut().observe_codex_facts(ctx, Some(goal), at);
        if crate::describe::excerpts_seen(self) != before {
            // The excerpts move with no record of their own: aterm's default writer is told.
            crate::describe::ring();
        }
        true
    }

    /// Forget the rollout tap's facts and the background terminals it read off the record: its
    /// tap went. Returns whether any were cleared.
    pub(crate) fn clear_codex_survey(&mut self) -> bool {
        self.codex_token_clock = None;
        let facts = self.codex_survey.take().is_some();
        self.codex_terminals.take().is_some() || facts
    }

    /// The codex facts the survey shows, or `None` when the program in front is not the one they
    /// were read for.
    pub(crate) fn codex_survey(&self) -> Option<&crate::survey::CodexSurvey> {
        self.codex_survey
            .as_ref()
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, survey)| survey)
    }

    /// The token source in this terminal now, including agent replacement without shell replacement.
    pub(crate) fn panel_token_owner(
        &self,
        launch: aterm_session::LaunchNonce,
    ) -> Option<aterm_vitals::pane_tokens::Owner> {
        use aterm_phase::Program;
        let pgid = self.agent.program_pgid;
        if pgid <= 0 || self.foreground_generation == u64::MAX {
            return None;
        }
        let (conversation, started, floor) = match self.agent.reader? {
            Program::Claude => {
                let owner = self.claude_footer()?.owner.as_ref()?;
                if owner.session_id.is_empty() {
                    return None;
                }
                (
                    format!("claude {}", owner.session_id),
                    self.claude_footer_identity()
                        .and_then(|(_, started)| started),
                    owner.floor,
                )
            }
            Program::Codex => {
                let thread = &self.codex_survey()?.thread;
                if thread.is_empty() {
                    return None;
                }
                (
                    format!("codex {thread}"),
                    self.current_holder_image().and_then(|image| image.token),
                    None,
                )
            }
            _ => return None,
        };
        Some(aterm_vitals::pane_tokens::Owner {
            launch: launch.to_hex(),
            generation: self.foreground_generation,
            pgid,
            conversation,
            started,
            floor,
        })
    }

    /// Collect the provider's own whole-counter evidence and timestamp without polling a provider.
    pub(crate) fn panel_token_observation(
        &self,
        pane: u64,
        launch: aterm_session::LaunchNonce,
    ) -> Option<aterm_vitals::pane_tokens::Observation> {
        use aterm_vitals::pane_tokens::{Counters, Observation};
        let owner = self.panel_token_owner(launch)?;
        let counters = (|| {
            let whole = crate::survey::whole_tokens(
                self.agent.reader,
                self.agent.program_pgid,
                self.claude_footer()
                    .and_then(|f| Some((f.usage.as_ref()?, f.owner.as_ref()?))),
                self.codex_survey(),
            )?;
            let (at, collected) = match self.agent.reader? {
                aterm_phase::Program::Claude => {
                    let read = self.claude_footer_read_at()?;
                    (read, read)
                }
                aterm_phase::Program::Codex => self.codex_token_clock?,
                _ => return None,
            };
            if at <= self.foreground_since? {
                return None;
            }
            let epoch = aterm_session::terminal_stream::epoch();
            let ns = |time: std::time::Instant| {
                u64::try_from(time.checked_duration_since(epoch)?.as_nanos()).ok()
            };
            Some(Counters {
                at_ns: ns(at)?,
                collected_ns: ns(collected)?,
                input: whole.tok_in,
                output: whole.tok_out,
            })
        })();
        Some(Observation {
            pane,
            owner,
            counters,
        })
    }

    /// Where the agent in group `pgid` works ([`aterm_agent::harness::work_record::WorkRecord`]),
    /// or nothing to show (`None`), taken only while that group is in front and the task's face is
    /// not landed: whether it was. A landing clears it ([`Self::set_task_face`]).
    pub(crate) fn set_work(
        &mut self,
        pgid: i32,
        record: Option<aterm_agent::harness::work_record::WorkRecord>,
    ) -> bool {
        let landed = matches!(
            self.task_face,
            Some((aterm_agent::harness::acts::TaskFace::Landed, _))
        );
        if self.agent.program_pgid != pgid || (landed && record.is_some()) {
            return false;
        }
        self.work = record.map(|record| (pgid, record));
        true
    }

    /// The work record the survey shows, or `None` when the program in front is not the one it
    /// was read for.
    pub(crate) fn work(&self) -> Option<&aterm_agent::harness::work_record::WorkRecord> {
        self.work
            .as_ref()
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, record)| record)
    }

    /// The background terminals codex-alab's own record counts for the codex process in group
    /// `pgid` ([`crate::codex_rollout_tap`]), taken only while that group is in front: whether it
    /// was. Apart from [`Self::codex_survey`]: a count alone
    /// publishes no survey facts, so a tab with no rollout keeps its screen's `cmd=` and `rows=`.
    pub(crate) fn set_codex_terminals(&mut self, pgid: i32, count: Option<u32>) -> bool {
        if self.agent.program_pgid != pgid {
            return false;
        }
        self.codex_terminals = count.map(|n| (pgid, n));
        true
    }

    /// The count [`Self::set_codex_terminals`] took, or `None` when the program in front is not the
    /// one it was read for.
    pub(crate) fn codex_terminals(&self) -> Option<u32> {
        self.codex_terminals
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, n)| n)
    }

    /// The footer resolver's side for `pgid`, applied only while that group is in front.
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

    /// The footer's side for the current foreground group, or `None` when the program in front is
    /// not the one it was read for.
    pub(crate) fn claude_side(&self) -> Option<&std::sync::Arc<crate::claude_footer::FactSide>> {
        self.claude_side
            .as_ref()
            .filter(|(pgid, _)| *pgid == self.agent.program_pgid)
            .map(|(_, side)| side)
    }

    /// The status sweep's latest [`AgentGauges`] for the agent in front.
    pub(crate) const fn set_gauges(&mut self, gauges: AgentGauges) {
        self.gauges = gauges;
    }

    /// The gauges as last read while an identified agent is in front (none otherwise), with
    /// codex-alab's own count of its background terminals ([`Self::codex_terminals`], taken only
    /// while its group is in front, identified or not, as [`Self::goal`] reads the codex slot)
    /// where the screen gave none.
    pub(crate) fn gauges(&self) -> AgentGauges {
        let screen = if self.agent.reader.is_some() {
            self.gauges
        } else {
            AgentGauges::default()
        };
        AgentGauges {
            shells: screen.shells.or_else(|| self.codex_terminals()),
            ..screen
        }
    }

    /// The footer facts for the current foreground group, or `None` when the program in front is
    /// not the one they were read for.
    pub(crate) fn claude_footer(&self) -> Option<&aterm_agent::harness::footer::FooterFacts> {
        self.claude_footer
            .as_ref()
            .filter(|(pgid, _, _)| *pgid == self.agent.program_pgid)
            .map(|(_, _, facts)| facts)
    }

    /// Apply an in-memory supervisor observation only to its current process.
    pub(crate) fn set_account_progress(
        &mut self,
        pgid: i32,
        started: Option<u64>,
        conversation: Option<&str>,
        progress: Option<aterm_agent::harness::account_progress::AccountProgress>,
    ) -> bool {
        let Some(facts) = self.account_progress_facts_mut(pgid, started, conversation) else {
            return false;
        };
        if facts.account_progress == progress && !facts.account_progress_busy {
            return false;
        }
        facts.account_progress = progress;
        facts.account_progress_busy = false;
        true
    }

    /// Withdraw current recovery claims without replacing the retained observation.
    pub(crate) fn set_account_progress_busy(
        &mut self,
        pgid: i32,
        started: Option<u64>,
        conversation: Option<&str>,
    ) -> bool {
        let Some(facts) = self.account_progress_facts_mut(pgid, started, conversation) else {
            return false;
        };
        let changed = !facts.account_progress_busy;
        facts.account_progress_busy = true;
        changed
    }

    fn account_progress_facts_mut(
        &mut self,
        pgid: i32,
        started: Option<u64>,
        conversation: Option<&str>,
    ) -> Option<&mut aterm_agent::harness::footer::FooterFacts> {
        let (pid, birth, facts) = self.claude_footer.as_mut()?;
        (self.agent.program_pgid == pgid
            && *pid == pgid
            && *birth == started
            && facts.owner.as_ref().map(|owner| owner.session_id.as_str()) == conversation)
            .then_some(facts)
    }

    /// The active goal of the agent in front, one line ([`aterm_agent::harness::goal`]): what
    /// its own records say (Claude Code's `/goal` and Muse Code's goal store through the footer
    /// resolver's facts, codex-alab's session record through the rollout tap's), or `None` while it
    /// has none or keeps none aterm reads.
    pub(crate) fn goal(&self) -> Option<&str> {
        if let Some(fed) = &self.agent.streamed.goal {
            // The stream feeds the goal: the built-in readers stand down, and a record that
            // names none says there is none.
            return fed.text.as_deref();
        }
        self.claude_footer()
            .and_then(|facts| facts.goal.as_deref())
            .or_else(|| self.codex_survey().and_then(|codex| codex.goal.as_deref()))
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

    /// What the footer shows for the group in front. While the claim's stream feeds `model`
    /// (D29) the footer's model and effort readers stand down: the model and effort are the
    /// record's ([`StreamModel`]), whatever the footer read, and every other fact the footer's.
    pub(crate) fn claude_footer_shown(&self) -> Option<aterm_agent::harness::footer::FooterFacts> {
        self.claude_footer().map(|facts| {
            let mut shown = facts.filled_from(self.claude_card());
            if let Some(fed) = &self.agent.streamed.model {
                shown.model.clone_from(&fed.model);
                shown.effort.clone_from(&fed.effort);
            }
            shown
        })
    }

    /// The leader read's copy and PATH answers for `pgid`
    /// (`session_status::program::leader_facts`), applied only while that group is in front.
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
    pub(crate) fn set_leader_no_harness(&mut self, pgid: i32, no_harness: bool) -> bool {
        if self.agent.program_pgid != pgid {
            return false;
        }
        let next = no_harness.then_some(pgid);
        let changed = self.leader_no_harness != next;
        self.leader_no_harness = next;
        changed
    }

    /// The resolver's reading of `pgid`'s leader as a build's front waiting for memory
    /// ([`crate::session_status::program::LeaderFacts::build_wait`]), applied only while that group is in
    /// front; whether it changed what [`Self::build_wait`] says.
    pub(crate) fn set_build_wait(
        &mut self,
        pgid: i32,
        wait: Option<crate::session_status::program::BuildWait>,
    ) -> bool {
        if self.agent.program_pgid != pgid {
            return false;
        }
        let next = wait.map(|wait| (pgid, wait));
        let changed = self.leader_wait != next;
        self.leader_wait = next;
        changed
    }

    /// The foreground leader's wait for memory, while it is a build's front whose claim waits
    /// (`status wait=memory`, the survey's `wait=`): `None` for any other leader.
    pub(crate) fn build_wait(&self) -> Option<&crate::session_status::program::BuildWait> {
        self.leader_wait
            .as_ref()
            .filter(|(pgid, _)| *pgid > 0 && *pgid == self.agent.program_pgid)
            .map(|(_, wait)| wait)
    }

    /// Whether the current foreground leader was started with `--no-harness` / `--no-alab-harness`.
    pub(crate) fn leader_no_harness(&self) -> bool {
        self.leader_no_harness
            .is_some_and(|pgid| pgid > 0 && pgid == self.agent.program_pgid)
    }

    /// Whether the current foreground leader requested `--no-harness` and `Autopilot::Off` is not
    /// yet applied for its process group.
    pub(crate) fn unapplied_leader_no_harness(&self) -> bool {
        self.leader_no_harness() && self.leader_no_harness_applied != self.leader_no_harness
    }

    /// Claim the current foreground leader's `--no-harness` request once per process group.
    pub(crate) fn take_leader_no_harness(&mut self) -> bool {
        if !self.unapplied_leader_no_harness() {
            return false;
        }
        self.leader_no_harness_applied = self.leader_no_harness;
        true
    }

    /// `(path word, age in ms, settled)` of the last PATH measurement, or `None`.
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

    /// Synchronous fixture publication. Older layout fixtures deliberately leave the process
    /// or program unnamed; production publications must supply their captured custody.
    #[cfg(test)]
    pub(crate) fn set_upgrade(
        &mut self,
        row: Option<aterm_agent::harness::upgrade_drive::Row>,
    ) -> bool {
        let row = row.map(|row| {
            let owner = UpgradeOwner {
                pgid: self.agent.program_pgid,
                holder: self.current_holder_image().and_then(|image| image.token),
                agent: crate::harness_host::identified_agent(&self.agent)
                    .unwrap_or(row.agent.program()),
            };
            (row, Some(owner))
        });
        self.set_upgrade_owned(row)
    }

    /// Replace the row with its admitted producer's stable owner; a clear retains that owner,
    /// never the foreground program at delivery time.
    pub(crate) fn set_upgrade_owned(
        &mut self,
        row: Option<(
            aterm_agent::harness::upgrade_drive::Row,
            Option<UpgradeOwner>,
        )>,
    ) -> bool {
        if self.upgrade == row {
            return false;
        }
        let previous_word = self.upgrade_word();
        if row.is_some() {
            self.upgrade_kept = None;
        } else {
            self.upgrade_kept = self.upgrade.take();
        }
        self.upgrade = row;
        if self.upgrade_word() != previous_word {
            self.upgrade_word_revision = self.upgrade_word_revision.saturating_add(1);
        }
        true
    }

    /// An admitted successful reply is a new operation even when its value was already cached.
    pub(crate) fn upgrade_word_accepted(&mut self, row: &aterm_agent::harness::upgrade_drive::Row) {
        if self
            .upgrade
            .as_ref()
            .or(self.upgrade_kept.as_ref())
            .is_some_and(|(current, _)| crate::upgrade_host::same_move(current, row))
        {
            self.upgrade_word_revision = self.upgrade_word_revision.saturating_add(1);
        }
    }

    /// This tab's live-upgrade row, or `None`.
    pub(crate) fn upgrade(&self) -> Option<&aterm_agent::harness::upgrade_drive::Row> {
        self.upgrade
            .as_ref()
            .filter(|(row, owner)| owner.is_some_and(|owner| owner.fronts(self, row)))
            .map(|(row, _)| row)
    }

    /// Reapply a record with its accepted run's owner. A target, phase, wait or source change
    /// grants no new custody; a newly recorded process birth can admit a new foreground owner.
    pub(crate) fn accepted_upgrade_owner(
        &self,
        row: &aterm_agent::harness::upgrade_drive::Row,
    ) -> Option<UpgradeOwner> {
        self.upgrade
            .as_ref()
            .or(self.upgrade_kept.as_ref())
            .filter(|(old, _)| crate::upgrade_host::same_upgrade_run(old, row))
            .and_then(|(_, owner)| *owner)
    }

    /// The row the host last sent for this tab before it sent none (a host stood down, its
    /// harness switched off): what the top rule's ⬆️ still says of a build waiting while the
    /// harness is off. A live row clears it, and it is none while another foreground group than
    /// the one in front when it was kept is: the program it was minted for has left the front.
    pub(crate) fn upgrade_kept(&self) -> Option<&aterm_agent::harness::upgrade_drive::Row> {
        self.upgrade_kept
            .as_ref()
            .filter(|(row, owner)| owner.is_some_and(|owner| owner.fronts(self, row)))
            .map(|(row, _)| row)
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
        self.task_face_seeded = false;
        if matches!(
            face,
            Some(
                aterm_agent::harness::acts::TaskFace::Open
                    | aterm_agent::harness::acts::TaskFace::Theirs
                    | aterm_agent::harness::acts::TaskFace::Failed
            )
        ) {
            // A RECEIPT NEVER OUTLIVES ITS LANDING (§14.9, slice 9b).
            self.land_receipt = None;
        }
        if face == Some(aterm_agent::harness::acts::TaskFace::Landed) {
            // A landed session works nowhere until its task opens again.
            self.work = None;
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

    /// The task's face as this window's host last heard it, and the wall time (unix ms) it moved
    /// there.
    pub(crate) const fn task_face(&self) -> Option<(aterm_agent::harness::acts::TaskFace, i64)> {
        self.task_face
    }

    /// Hold a seamless update's carried end, or a proven self-claim's carried face, while no face
    /// is shown or held; whether it was seeded.
    pub(crate) const fn seed_task_face_held(
        &mut self,
        face: aterm_agent::harness::acts::TaskFace,
        at_ms: i64,
    ) -> bool {
        if self.task_face.is_some() || self.task_face_held.is_some() {
            return false;
        }
        self.task_face_held = Some((face, at_ms));
        self.task_face_seeded = true;
        true
    }

    /// The face a seamless update carried ([`Self::seed_task_face_held`]) and its wall time (unix
    /// ms), while nothing has been told here since; a face held only because the shown one went
    /// unknown is no carry.
    pub(crate) const fn carried_task_face(
        &self,
    ) -> Option<(aterm_agent::harness::acts::TaskFace, i64)> {
        match (self.task_face, self.task_face_seeded) {
            (None, true) => self.task_face_held,
            _ => None,
        }
    }

    /// The wall time (unix ms) the task's face moved to `face`, when that is the face this
    /// timeline holds.
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
    pub(crate) const fn pause(&self) -> Option<crate::PausePlan> {
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

    /// Settle aterm's own loop's NEEDS YOU sets against `standing`, the `owner=needs` entry just
    /// written ([`crate::needs::ToldNeeds::settle`]): the outgoing set goes the moment that entry
    /// moves off the text it was told with or is cleared.
    pub(crate) fn settle_harness_needs(&mut self, standing: Option<&str>) {
        self.harness_needs.settle(standing);
    }

    /// The worker's NEEDS YOU sets as its loop told this window's host, whole, each with the
    /// entry's text it was told with: aterm's own loop's only, which a reader reads through
    /// [`crate::needs::told_of`] (a proven self-claim's own sets in their place while it stands).
    pub(crate) fn harness_needs(&self) -> crate::needs::ToldNeeds {
        self.harness_needs.clone()
    }

    /// Replace Land's hold as the session's loop told it ([`Self::land_held()`]): whether it
    /// moved, or the hold's words did since it was last stored ([`Self::set_land_said`]), so the
    /// one fact the hold's tell sends carries a reworded quote too (slice 4b).
    pub(crate) fn set_land_held(
        &mut self,
        held: Option<aterm_agent::harness::acts::LandHeld>,
    ) -> bool {
        let words = std::mem::take(&mut self.land_said_untold);
        (std::mem::replace(&mut self.land_held, held) != held) | words
    }

    /// Replace the hold's own words as the session's loop told them ([`Self::land_said()`]):
    /// whether they moved. The loop tells the hold right after them, and that store says they
    /// moved ([`Self::set_land_held`]).
    pub(crate) fn set_land_said(
        &mut self,
        said: Option<Arc<aterm_agent::harness::hold_said::HoldSaid>>,
    ) -> bool {
        let moved = self.land_said.as_deref() != said.as_deref();
        self.land_said = said;
        self.land_said_untold |= moved;
        moved
    }

    /// The hold's own words as the session's loop last told this window's host (§14.17 item 1,
    /// slice 4b): what the agent said is left and the files git found, sanitized at store.
    pub(crate) fn land_said(&self) -> Option<&Arc<aterm_agent::harness::hold_said::HoldSaid>> {
        self.land_said.as_ref()
    }

    /// Land's hold as the session's loop last told this window's host (§12.3.3 item 3).
    pub(crate) const fn land_held(&self) -> Option<aterm_agent::harness::acts::LandHeld> {
        self.land_held
    }

    /// Replace what aterm checked at the landing, as the session's loop told it
    /// ([`Self::land_receipt()`]): whether it moved.
    pub(crate) fn set_land_receipt(
        &mut self,
        receipt: Option<Arc<aterm_agent::harness::acts::LandReceipt>>,
    ) -> bool {
        let same = match (&self.land_receipt, &receipt) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        };
        self.land_receipt = receipt;
        !same
    }

    /// What aterm checked at the landing that stands, as the session's loop last told this
    /// window's host (§14.9, slice 9b), until the task's face moves to open or theirs. Whether
    /// the landing answered the land check is its `check` (`mode_words::said_words` words it).
    pub(crate) fn land_receipt(&self) -> Option<&Arc<aterm_agent::harness::acts::LandReceipt>> {
        self.land_receipt.as_ref()
    }

    /// Replace the point the session's loop told standing ([`Self::loop_exception()`]).
    pub(crate) fn set_loop_exception(&mut self, why: Option<String>) {
        self.loop_exception = why;
    }

    /// The why of the point only a person can answer that the session's loop last told this
    /// window's host standing (`HostNote::ResumeException`), where it is one the held deadline
    /// judges at any verdict (`harness_host::held_judges_exception`): a fact for later told once
    /// the loop's own tries at it are spent. `None` once the loop lifts it, ends, or tells one
    /// the deadline does not judge.
    pub(crate) fn loop_exception(&self) -> Option<&str> {
        self.loop_exception.as_deref()
    }

    /// `(path word, "path_evidence= copy= upgrade=")` for `status` and roster rows, after
    /// `supervisor=`.
    pub(crate) fn owner_columns(&self, mark: bool, now_s: u64) -> (&'static str, String) {
        self.owner_columns_with_awake(
            mark,
            now_s,
            aterm_agent::harness::upgrade_drive::awake_since(),
        )
    }

    /// [`Self::owner_columns`] judged against an explicit awake-clock reading `awake_s`.
    pub(crate) fn owner_columns_with_awake(
        &self,
        mark: bool,
        now_s: u64,
        awake_s: u64,
    ) -> (&'static str, String) {
        let (path, evidence) = path_columns(mark, self.path_reading());
        (
            path,
            format!(
                "path_evidence={evidence} copy={} upgrade={}",
                self.copy().unwrap_or("-"),
                self.upgrade_column(now_s, awake_s)
            ),
        )
    }

    /// The one `upgrade=` word `status`, roster rows and the survey's session rows carry: the
    /// live-upgrade row the host last sent, judged again at `now_s` against the awake-clock
    /// reading `awake_s` (`<state>/<to>/<why>/<age>`,
    /// [`aterm_agent::harness::upgrade_drive::Row::column`]), `-` where it sent none. While the
    /// claim's stream feeds `upgrade` (D29) the engine-filled row stands down and the word is
    /// the record's ([`StreamUpgrade::column`]); the engine's row is kept, so the word is its
    /// again once the stream stops feeding the kind, and a tab whose stream never applied
    /// `upgrade` keeps it throughout.
    pub(crate) fn upgrade_column(&self, now_s: u64, awake_s: u64) -> String {
        if let Some(fed) = &self.agent.streamed.upgrade {
            return fed.column(now_s);
        }
        self.upgrade.as_ref().map_or_else(
            || "-".to_string(),
            |(row, _)| {
                let mut row = row.clone();
                row.reread_with_awake(now_s, awake_s);
                row.column(now_s)
            },
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

#[path = "session_timeline_title.rs"]
mod title;

#[cfg(test)]
#[path = "session_timeline_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "session_timeline_write_api_tests.rs"]
mod write_api_tests;

#[cfg(test)]
#[path = "session_timeline_questions_field_tests.rs"]
mod questions_field_tests;

#[cfg(test)]
#[path = "session_timeline_account_progress_tests.rs"]
mod account_progress_tests;

#[cfg(test)]
#[path = "session_timeline_autopilot_field_tests.rs"]
mod autopilot_field_tests;

/// A worker an older build started (a task not yet exited) with no Autopilot word and no driver
/// write of one takes [`crate::task_launch::WORKER_AUTOPILOT`]; whether the pair moved.
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
#[path = "session_timeline_worker_autopilot_tests.rs"]
mod worker_autopilot_tests;

#[cfg(test)]
#[path = "session_timeline_carried_meta_tests.rs"]
mod carried_meta_tests;

#[cfg(test)]
#[path = "session_timeline_meta_atomicity_tests.rs"]
mod meta_atomicity_tests;

#[cfg(all(test, unix))]
#[path = "attention_delivery_tests.rs"]
mod attention_delivery_tests;

#[cfg(test)]
#[path = "session_timeline_keyed_attention_tests.rs"]
mod keyed_attention_tests;

#[cfg(test)]
#[path = "session_timeline_input_stall_tests.rs"]
mod input_stall_tests;

#[cfg(test)]
#[path = "session_timeline_screen_move_tests.rs"]
mod screen_move_tests;

/// The agent run footer facts speak for, as a tab's spend keys it
/// (`crate::session_log::SessionLog::note_agent_usage`): its conversation where the registry
/// names one (a process that takes up a conversation goes on its run), else its process.
fn agent_run(
    pgid: i32,
    started: Option<u64>,
    facts: &aterm_agent::harness::footer::FooterFacts,
) -> String {
    facts.owner.as_ref().map_or_else(
        || {
            format!(
                "p:{pgid}:{}",
                started.map_or_else(|| "-".to_string(), |s| s.to_string())
            )
        },
        |owner| format!("c:{}", owner.session_id),
    )
}

/// The input and output tokens `facts` count, from a usage read that saw the whole run alone.
fn whole_tokens(facts: &aterm_agent::harness::footer::FooterFacts) -> Option<u64> {
    facts
        .usage
        .as_ref()
        .filter(|u| u.complete)
        .map(|u| u.input_total().saturating_add(u.output_total()))
}

#[cfg(test)]
#[path = "session_timeline_registry_drift_tests.rs"]
mod registry_drift_tests;

#[cfg(all(test, any(debug_assertions, feature = "dev-seams")))]
#[path = "session_timeline_card_engine_dev_row_tests.rs"]
mod card_engine_dev_row_tests;
