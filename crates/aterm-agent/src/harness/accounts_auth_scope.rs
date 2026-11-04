// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Legacy harness observations retain history when an admitted edit replaces the login.
//! Codex instead uses its credential snapshots and staged authentication publication.

use super::*;

#[path = "accounts_lifetime_scope.rs"]
mod lifetime;
pub use lifetime::LifetimeScope;
pub(in crate::harness) use lifetime::note_served_login_for_observed_row;
pub(super) use lifetime::{
    Clock, clear_absent_lifetime, clock_current, current_for_row as lifetime_current_for_row,
    ended_current as lifetime_ended, forecast_current as lifetime_forecast, prepare_lifetime,
    replace_lifetime, served_current as lifetime_served, stamp_lifetime,
};

/// Whether a legacy harness's retained observation still describes its managed login.
/// Its transaction witness is private, transient and contains no credential bytes.
#[derive(Clone, Default)]
pub struct ManagedScope {
    retired: bool,
    witness: Option<Box<Witness>>,
}

impl std::fmt::Debug for ManagedScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedScope")
            .field("retired", &self.retired)
            .finish()
    }
}

impl PartialEq for ManagedScope {
    fn eq(&self, other: &Self) -> bool {
        self.retired == other.retired
    }
}

impl Eq for ManagedScope {}

/// Entry login contexts held only while an account transaction edits its admitted images.
/// The cache has no durable representation and is excluded from record equality.
#[derive(Clone, Default)]
pub struct ManagedContexts {
    entries: BTreeMap<String, Context>,
    contextual: bool,
}

impl std::fmt::Debug for ManagedContexts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedContexts")
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl PartialEq for ManagedContexts {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for ManagedContexts {}

#[derive(Clone)]
struct Witness {
    entry: Option<Context>,
    retired_at_entry: bool,
    observed_entry: bool,
    fresh: Option<Context>,
    invalidated: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct RawContext {
    dir: Option<String>,
    identity: Option<String>,
    region: Option<Region>,
    flow: Option<Flow>,
}

impl RawContext {
    fn of(row: &Row) -> Self {
        Self {
            dir: row.dir.clone(),
            identity: row.expected_identity_email().map(str::to_ascii_lowercase),
            region: row.vendor.region,
            flow: row.vendor.flow,
        }
    }

    fn same_policy(&self, other: &Self) -> bool {
        self.identity == other.identity && self.region == other.region && self.flow == other.flow
    }
}

#[derive(Clone)]
struct Context {
    raw: Option<RawContext>,
    dir: Option<String>,
    renewed_at: Option<i64>,
    proved_email: Option<String>,
    proved_dir: Option<String>,
    google_principal: Option<[u8; 32]>,
}

impl Context {
    fn of(raw: Option<RawContext>, logins: &Logins, label: &str) -> Self {
        let binding = logins.get(label);
        let dir = binding
            .and_then(|binding| binding.dir.clone())
            .or_else(|| raw.as_ref().and_then(|row| row.dir.clone()));
        let proved =
            binding.filter(|binding| binding.proved_dir.is_some() && binding.proved_dir == dir);
        Self {
            dir,
            raw,
            renewed_at: binding.and_then(|binding| binding.renewed_at),
            proved_email: proved
                .and_then(|binding| binding.proved_email.as_deref())
                .map(str::to_ascii_lowercase),
            proved_dir: proved.and_then(|binding| binding.proved_dir.clone()),
            google_principal: proved.and_then(|binding| binding.google_principal),
        }
    }

    fn on_default(mut self) -> Self {
        self.dir = None;
        if let Some(raw) = &mut self.raw {
            raw.dir = None;
        }
        // A named directory's retained proof does not describe the native default login.
        self.proved_email = None;
        self.proved_dir = None;
        self.google_principal = None;
        self
    }

    fn of_observed_row(row: &Row, logins: &Logins) -> Self {
        let mut context = Self::of(Some(RawContext::of(row)), logins, &row.label);
        context.dir.clone_from(&row.dir);
        let proved = logins
            .get(&row.label)
            .filter(|binding| binding.proved_dir.is_some() && binding.proved_dir == context.dir);
        context.proved_email = proved
            .and_then(|binding| binding.proved_email.as_deref())
            .map(str::to_ascii_lowercase);
        context.proved_dir = proved.and_then(|binding| binding.proved_dir.clone());
        context.google_principal = proved.and_then(|binding| binding.google_principal);
        context
    }

    fn matches(&self, current: &Self, fresh: bool, strengthen: bool) -> bool {
        let raw_matches = match (&self.raw, &current.raw) {
            (Some(was), Some(now)) => was.same_policy(now),
            (None, None) => true,
            (None, Some(_)) => fresh,
            (Some(_), None) => false,
        };
        fn evidence<T: PartialEq>(was: &Option<T>, now: &Option<T>, strengthen: bool) -> bool {
            was == now || (strengthen && was.is_none())
        }
        raw_matches
            && self.dir == current.dir
            && self.renewed_at == current.renewed_at
            && (!fresh
                || current.proved_email.as_ref().is_none_or(|email| {
                    current
                        .raw
                        .as_ref()
                        .and_then(|raw| raw.identity.as_ref())
                        .is_none_or(|expected| expected == email)
                }))
            && (!fresh
                || current
                    .proved_dir
                    .as_ref()
                    .is_none_or(|dir| current.dir.as_ref() == Some(dir)))
            && evidence(&self.proved_email, &current.proved_email, strengthen)
            && evidence(&self.proved_dir, &current.proved_dir, strengthen)
            && evidence(
                &self.google_principal,
                &current.google_principal,
                strengthen,
            )
    }
}

impl ManagedScope {
    pub(super) fn from_retired(retired: bool) -> Self {
        Self {
            retired,
            witness: None,
        }
    }

    pub(super) const fn is_retired(&self) -> bool {
        self.retired
    }

    /// Retire the old observation without losing the entry context needed by a later result.
    pub(super) fn retire(&mut self) {
        self.retired = true;
        if let Some(witness) = &mut self.witness {
            witness.fresh = None;
        }
    }

    /// A state-only observation is of the transaction's entry login, not a later replacement.
    pub(super) fn observe_entry(&mut self) {
        let retired_at_entry = self.retired;
        self.retired = false;
        match &mut self.witness {
            Some(witness) => {
                witness.observed_entry = true;
                witness.fresh = None;
            }
            None => {
                self.witness = Some(Box::new(Witness {
                    entry: None,
                    retired_at_entry,
                    observed_entry: true,
                    fresh: None,
                    invalidated: false,
                }));
            }
        }
    }
}

impl Account {
    /// The managed-login word used only by legacy harness transaction mechanics.
    pub(super) fn managed_auth(&self) -> Auth {
        if self.auth_scope.is_retired() {
            Auth::Unseen
        } else {
            self.auth
        }
    }

    /// Managed retirement is independent of Codex's actual credential-context authority.
    pub(super) fn managed_auth_is_current(&self) -> bool {
        !self.auth_scope.is_retired()
    }
}

/// Capture the login each existing observation describes before the transaction's edit runs.
pub(super) fn begin(state: &mut State, raw: Option<&Roster>, logins: &mut Logins) {
    if state.contextual_auth {
        logins.auth_contexts = ManagedContexts {
            contextual: true,
            ..ManagedContexts::default()
        };
        return;
    }
    let labels: BTreeSet<_> = raw
        .into_iter()
        .flat_map(|raw| raw.rows.iter().map(|row| row.label.clone()))
        .chain(logins.labels.keys().cloned())
        .chain(state.accounts.keys().cloned())
        .collect();
    let entries = labels
        .into_iter()
        .map(|label| {
            let context = Context::of(
                raw.and_then(|raw| raw.row(&label)).map(RawContext::of),
                logins,
                &label,
            );
            (label, context)
        })
        .collect();
    logins.auth_contexts = ManagedContexts {
        entries,
        contextual: false,
    };
    for (label, account) in &mut state.accounts {
        account.auth_scope.witness = Some(Box::new(Witness {
            entry: logins.auth_contexts.entries.get(label).cloned(),
            retired_at_entry: account.auth_scope.retired,
            observed_entry: false,
            fresh: None,
            invalidated: false,
        }));
    }
    lifetime::begin(logins);
}

/// Carry entry snapshots with the same simultaneous, already validated account moves.
pub(super) fn move_contexts(logins: &mut Logins, moves: &[(&str, &str)]) {
    let moved: Vec<_> = moves
        .iter()
        .filter(|(from, to)| from != to)
        .map(|(from, to)| (*to, logins.auth_contexts.entries.remove(*from)))
        .collect();
    for (to, _) in &moved {
        logins.auth_contexts.entries.remove(*to);
    }
    for (to, context) in moved {
        if let Some(context) = context {
            logins.auth_contexts.entries.insert(to.to_owned(), context);
        }
    }
}

/// Consume the witnesses before serializing the state. Pure attempt fences and relabels do
/// not replace a login; an explicit renewal does even at the same path and clock instant.
/// Returns whether applicability changed, independently of any new journal event.
/// A newly observed word for a different final context refuses the entire publication.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRenewalGeneration",
        action = "Commit",
        project = "aterm_agent::harness::accounts::generation_tests::generation_transition"
    )
)]
pub(super) fn finish(
    state: &mut State,
    before: &Logins,
    raw: Option<&Roster>,
    logins: &mut Logins,
    events: &[Event],
) -> Result<bool, String> {
    if state.contextual_auth {
        lifetime::clear_witnesses(logins);
        logins.auth_contexts = ManagedContexts::default();
        return Ok(false);
    }
    let renewed: BTreeSet<_> = events
        .iter()
        .filter(|event| event.kind == Kind::Renewed)
        .map(|event| event.account.as_str())
        .collect();
    let verified: BTreeSet<_> = events
        .iter()
        .filter(|event| matches!(event.kind, Kind::Renewed | Kind::SignedIn))
        .map(|event| event.account.as_str())
        .collect();
    let mut changed = false;
    for (label, account) in &mut state.accounts {
        let current = Context::of(
            raw.and_then(|raw| raw.row(label)).map(RawContext::of),
            logins,
            label,
        );
        let witness = account.auth_scope.witness.take();
        let retired_before = witness
            .as_ref()
            .map_or(account.auth_scope.retired, |witness| {
                witness.retired_at_entry
            });
        let entry = witness
            .as_ref()
            .and_then(|witness| witness.entry.as_ref())
            .or_else(|| logins.auth_contexts.entries.get(label));
        let fresh_observation = witness
            .as_ref()
            .is_some_and(|witness| witness.observed_entry || witness.fresh.is_some());
        let matches = match witness.as_ref().and_then(|witness| witness.fresh.as_ref()) {
            Some(fresh) => fresh.matches(
                &current,
                true,
                verified.contains(label.as_str()) && account.auth == Auth::Ok,
            ),
            None => {
                !renewed.contains(label.as_str())
                    && entry.map_or_else(
                        || {
                            Context::of(None, before, label).matches(
                                &current,
                                !fresh_observation,
                                false,
                            )
                        },
                        |entry| entry.matches(&current, false, false),
                    )
            }
        };
        if account.auth != Auth::Unseen && !matches {
            if fresh_observation {
                logins.auth_contexts.entries.clear();
                return Err("authentication observation no longer matches the final login; nothing recorded".into());
            }
            account.auth_scope.retired = true;
        }
        if account.auth_scope.retired
            && let Some(binding) = logins.labels.get_mut(label)
        {
            binding.last_success_generation = 0;
        }
        changed |= retired_before != account.auth_scope.retired
            || (fresh_observation
                && entry.is_some_and(|entry| !entry.matches(&current, false, false)));
    }
    changed |= lifetime::finish(before, raw, logins, &renewed, &verified)?;
    logins.auth_contexts = ManagedContexts::default();
    Ok(changed)
}

/// State-only transactions cannot change bindings and retain no transaction witness.
pub(super) fn clear(state: &mut State) -> bool {
    let contextual = state.contextual_auth;
    let mut changed = false;
    for account in state.accounts.values_mut() {
        changed |= !contextual
            && account
                .auth_scope
                .witness
                .as_ref()
                .is_some_and(|witness| witness.retired_at_entry != account.auth_scope.retired);
        account.auth_scope.witness = None;
    }
    changed
}

fn note_in_context(
    state: &mut State,
    logins: &Logins,
    label: &str,
    auth: Auth,
    at: i64,
    default: bool,
) -> Vec<Event> {
    if state.contextual_auth {
        return note_auth(state, label, auth, at);
    }
    if auth == Auth::Unseen {
        return Vec::new();
    }
    let observed = state
        .accounts
        .get(label)
        .map(|account| observed_context(account, logins, label, default))
        .unwrap_or_else(|| {
            let raw = logins
                .auth_contexts
                .entries
                .get(label)
                .and_then(|entry| entry.raw.clone());
            let context = Context::of(raw, logins, label);
            if default {
                context.on_default()
            } else {
                context
            }
        });
    note_captured(state, logins, label, auth, at, observed)
}

fn note_captured(
    state: &mut State,
    logins: &Logins,
    label: &str,
    auth: Auth,
    at: i64,
    observed: Context,
) -> Vec<Event> {
    retire_against(state, logins, label, &observed);
    if state.accounts.get(label).is_some_and(|account| {
        account.managed_auth_is_current() && account.auth_at.is_some_and(|held| held > at)
    }) {
        return Vec::new();
    }
    let events = note_auth(state, label, auth, at);
    if let Some(account) = state.accounts.get_mut(label)
        && account.auth == auth
        && account.auth_at == Some(at)
        && account.managed_auth_is_current()
    {
        bind_captured(account, logins, label, observed);
    }
    events
}

fn retire_against(state: &mut State, logins: &Logins, label: &str, observed: &Context) {
    if let Some(account) = state.accounts.get_mut(label)
        && account.auth_scope.witness.as_ref().is_some_and(|witness| {
            witness.fresh.as_ref().map_or_else(
                || {
                    witness
                        .entry
                        .as_ref()
                        .or_else(|| logins.auth_contexts.entries.get(label))
                        .is_some_and(|entry| !entry.matches(observed, false, false))
                },
                |fresh| !fresh.matches(observed, true, account.auth == Auth::Ok),
            )
        })
    {
        // A timestamp learned on a different login cannot reject this login's result.
        account.auth_scope.retire();
    }
}

/// Withdraw a held word of another login before an enclosing authentication timestamp guard.
pub(super) fn retire_changed_context(
    state: &mut State,
    logins: &Logins,
    label: &str,
    default: bool,
) {
    if state.contextual_auth {
        return;
    }
    if let Some(account) = state.accounts.get(label) {
        let observed = observed_context(account, logins, label, default);
        retire_against(state, logins, label, &observed);
    }
}

fn observed_context(account: &Account, logins: &Logins, label: &str, default: bool) -> Context {
    let raw = account
        .auth_scope
        .witness
        .as_ref()
        .and_then(|witness| witness.entry.as_ref())
        .or_else(|| logins.auth_contexts.entries.get(label))
        .and_then(|entry| entry.raw.clone());
    let fresh = Context::of(raw, logins, label);
    if default { fresh.on_default() } else { fresh }
}

fn bind_captured(account: &mut Account, logins: &Logins, label: &str, fresh: Context) {
    match &mut account.auth_scope.witness {
        Some(witness) => {
            if witness.entry.is_none() {
                witness.entry = logins.auth_contexts.entries.get(label).cloned();
            }
            witness.fresh = Some(fresh);
            witness.invalidated = false;
        }
        None => {
            account.auth_scope.witness = Some(Box::new(Witness {
                entry: logins.auth_contexts.entries.get(label).cloned(),
                retired_at_entry: account.auth_scope.retired,
                observed_entry: true,
                fresh: Some(fresh),
                invalidated: false,
            }));
        }
    }
}

/// Attach the current login context to an already accepted direct observation. This changes
/// no authentication word, timestamp or event; the caller owns that observation's admission.
pub(super) fn bind_observation(state: &mut State, logins: &Logins, label: &str, default: bool) {
    if state.contextual_auth {
        return;
    }
    if let Some(account) = state.accounts.get_mut(label) {
        let observed = observed_context(account, logins, label, default);
        bind_captured(account, logins, label, observed);
    }
}

/// Record a legacy result against its current binding and the entry row's identity.
/// Contextual Codex stores retain history here; their staged observations supply authority.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRenewalGeneration",
        action = "RecordFailure",
        project = "aterm_agent::harness::accounts::generation_tests::generation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRenewalGeneration",
        action = "FreshFailure",
        project = "aterm_agent::harness::accounts::generation_tests::generation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountLifetimeScope",
        action = "AuthOnly",
        project = "aterm_agent::harness::accounts::lifetime_scope_tests::lifetime_transition"
    )
)]
pub fn note_auth_in_context(
    state: &mut State,
    logins: &Logins,
    label: &str,
    auth: Auth,
    at: i64,
) -> Vec<Event> {
    note_in_context(state, logins, label, auth, at, false)
}

/// Record a legacy result on the native default login, not a future raw target. Contextual
/// Codex stores retain history here and use their staged credential observations instead.
pub fn note_auth_on_default(
    state: &mut State,
    logins: &Logins,
    label: &str,
    auth: Auth,
    at: i64,
) -> Vec<Event> {
    note_in_context(state, logins, label, auth, at, true)
}

/// Record a result for the row and effective login the producer actually inspected. Unlike
/// the ordinary scoped helper, this accepts newly discovered row policy as observation
/// evidence; a later edit to that policy or login still rejects the whole publication.
pub(in crate::harness) fn note_auth_for_observed_row(
    state: &mut State,
    logins: &Logins,
    row: &Row,
    auth: Auth,
    at: i64,
) -> Vec<Event> {
    if state.contextual_auth {
        return note_auth(state, &row.label, auth, at);
    }
    if auth == Auth::Unseen {
        return Vec::new();
    }
    let observed = Context::of_observed_row(row, logins);
    note_captured(state, logins, &row.label, auth, at, observed)
}
