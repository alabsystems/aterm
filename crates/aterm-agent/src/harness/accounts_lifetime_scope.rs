// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent applicability of managed lifetime observations, sharing auth's context witness.

use super::*;

/// Applicability of retained managed lifetime observations. Transient witnesses never persist.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LifetimeScope {
    renewal: ManagedScope,
    notice: ManagedScope,
    served: ManagedScope,
    ended: ManagedScope,
}

#[derive(Clone, Copy)]
pub(in crate::harness::accounts) enum Clock {
    Renewal,
    Notice,
    Served,
    Ended,
}

const CLOCKS: [Clock; 4] = [Clock::Renewal, Clock::Notice, Clock::Served, Clock::Ended];

impl Clock {
    fn key(self) -> &'static str {
        match self {
            Self::Renewal => "renewal_retired",
            Self::Notice => "notice_retired",
            Self::Served => "served_retired",
            Self::Ended => "ended_retired",
        }
    }

    fn present(self, binding: &Binding) -> bool {
        match self {
            Self::Renewal => binding.renewed_at.is_some(),
            Self::Notice => binding.notice_end.is_some(),
            Self::Served => binding.served_at.is_some(),
            Self::Ended => binding.ended_at.is_some(),
        }
    }
}

impl LifetimeScope {
    fn get(&self, clock: Clock) -> &ManagedScope {
        match clock {
            Clock::Renewal => &self.renewal,
            Clock::Notice => &self.notice,
            Clock::Served => &self.served,
            Clock::Ended => &self.ended,
        }
    }

    fn get_mut(&mut self, clock: Clock) -> &mut ManagedScope {
        match clock {
            Clock::Renewal => &mut self.renewal,
            Clock::Notice => &mut self.notice,
            Clock::Served => &mut self.served,
            Clock::Ended => &mut self.ended,
        }
    }

    pub(in crate::harness::accounts) fn write_json(&self, object: &mut Map) {
        for clock in CLOCKS {
            if self.get(clock).retired {
                object.insert(clock.key().into(), Value::Bool(true));
            }
        }
    }

    pub(in crate::harness::accounts) fn from_json(value: &Value) -> Self {
        let mut scope = Self::default();
        for clock in CLOCKS {
            scope.get_mut(clock).retired = value
                .get(clock.key())
                .is_some_and(|value| value.as_bool() != Some(false));
        }
        scope
    }
}

fn captured_context(logins: &Logins, label: &str, observed: Option<&Row>) -> Context {
    if let Some(row) = observed {
        return Context::of_observed_row(row, logins);
    }
    let raw = logins
        .auth_contexts
        .entries
        .get(label)
        .and_then(|entry| entry.raw.clone());
    Context::of(raw, logins, label)
}

fn applies(scope: &ManagedScope, entry: Option<&Context>, current: &Context) -> bool {
    if scope.retired {
        return false;
    }
    let Some(witness) = &scope.witness else {
        return entry.is_none_or(|entry| entry.matches(current, false, false));
    };
    if witness.invalidated {
        return false;
    }
    match witness.fresh.as_ref() {
        Some(fresh) => fresh.matches(current, true, false),
        None => witness
            .entry
            .as_ref()
            .or(entry)
            .is_none_or(|entry| entry.matches(current, false, false)),
    }
}

pub(super) fn begin(logins: &mut Logins) {
    for (label, binding) in &mut logins.labels {
        let entry = logins.auth_contexts.entries.get(label);
        for clock in CLOCKS {
            let scope = binding.lifetime_scope.get_mut(clock);
            scope.witness = Some(Box::new(Witness {
                entry: entry.cloned(),
                retired_at_entry: scope.retired,
                observed_entry: false,
                fresh: None,
                invalidated: false,
            }));
        }
    }
}

pub(super) fn clear_witnesses(logins: &mut Logins) {
    for binding in logins.labels.values_mut() {
        for clock in CLOCKS {
            binding.lifetime_scope.get_mut(clock).witness = None;
        }
    }
}

/// Withdraw old inputs before a new observation combines timestamps or learns a duration.
pub(in crate::harness::accounts) fn prepare_lifetime(logins: &mut Logins, label: &str) {
    if logins.auth_contexts.contextual {
        return;
    }
    let current = captured_context(logins, label, None);
    let entry = logins.auth_contexts.entries.get(label).cloned();
    if let Some(binding) = logins.labels.get_mut(label) {
        for clock in CLOCKS {
            if clock.present(binding)
                && !applies(binding.lifetime_scope.get(clock), entry.as_ref(), &current)
            {
                // Keep a fresh witness: a later different result must replace it explicitly,
                // otherwise publication still refuses the stale observation as a whole.
                binding.lifetime_scope.get_mut(clock).retired = true;
            }
        }
    }
}

/// A verified replacement invalidates old observations even at the same path and clock instant.
pub(in crate::harness::accounts) fn replace_lifetime(logins: &mut Logins, label: &str) {
    if logins.auth_contexts.contextual {
        return;
    }
    if let Some(binding) = logins.labels.get_mut(label) {
        for clock in CLOCKS {
            let present = clock.present(binding);
            let scope = binding.lifetime_scope.get_mut(clock);
            scope.retired = present;
            if let Some(witness) = &mut scope.witness {
                witness.invalidated |= witness.observed_entry || witness.fresh.is_some();
            }
        }
    }
}

/// Absent observations have no durable authority; retain transient publication intent.
pub(in crate::harness::accounts) fn clear_absent_lifetime(logins: &mut Logins, label: &str) {
    if logins.auth_contexts.contextual {
        return;
    }
    if let Some(binding) = logins.labels.get_mut(label) {
        for clock in CLOCKS {
            if !clock.present(binding) {
                binding.lifetime_scope.get_mut(clock).retired = false;
            }
        }
    }
}

pub(in crate::harness::accounts) fn stamp_lifetime(
    logins: &mut Logins,
    label: &str,
    clock: Clock,
    observed: Option<&Row>,
) {
    if logins.auth_contexts.contextual {
        return;
    }
    let current = captured_context(logins, label, observed);
    let entry = logins.auth_contexts.entries.get(label).cloned();
    if let Some(binding) = logins.labels.get_mut(label) {
        let scope = binding.lifetime_scope.get_mut(clock);
        let retired_at_entry = scope
            .witness
            .as_ref()
            .map_or(scope.retired, |witness| witness.retired_at_entry);
        let held_entry = scope
            .witness
            .as_ref()
            .and_then(|witness| witness.entry.clone())
            .or(entry);
        scope.retired = false;
        scope.witness = Some(Box::new(Witness {
            entry: held_entry,
            retired_at_entry,
            observed_entry: true,
            fresh: Some(current),
            invalidated: false,
        }));
    }
}

pub(in crate::harness::accounts) fn clock_current(
    logins: &Logins,
    label: &str,
    clock: Clock,
) -> bool {
    logins.auth_contexts.contextual
        || logins
            .get(label)
            .is_none_or(|binding| !binding.lifetime_scope.get(clock).retired)
}

pub(in crate::harness::accounts) fn current_for_row(
    row: &Row,
    logins: &Logins,
    clock: Clock,
) -> bool {
    let Some(binding) = logins.get(&row.label) else {
        return false;
    };
    let current = Context::of(Some(RawContext::of(row)), logins, &row.label);
    applies(
        binding.lifetime_scope.get(clock),
        logins.auth_contexts.entries.get(&row.label),
        &current,
    )
}

pub(in crate::harness::accounts) fn forecast_current(row: &Row, logins: &Logins) -> Option<i64> {
    let binding = logins.get(&row.label)?;
    let renewal = current_for_row(row, logins, Clock::Renewal)
        .then_some(binding.renewed_at)
        .flatten()
        .map(|at| at.saturating_add(binding.lifetime()));
    let notice = current_for_row(row, logins, Clock::Notice)
        .then_some(binding.notice_end)
        .flatten();
    [renewal, notice].into_iter().flatten().min()
}

pub(in crate::harness::accounts) fn served_current(row: &Row, logins: &Logins) -> Option<i64> {
    current_for_row(row, logins, Clock::Served)
        .then(|| logins.get(&row.label)?.served_at)
        .flatten()
}

pub(in crate::harness::accounts) fn ended_current(row: &Row, logins: &Logins) -> Option<i64> {
    current_for_row(row, logins, Clock::Ended)
        .then(|| logins.get(&row.label)?.ended_at)
        .flatten()
}

/// Capture a served turn against the exact effective row the producer actually inspected.
pub(in crate::harness) fn note_served_login_for_observed_row(
    logins: &mut Logins,
    row: &Row,
    at: i64,
) -> bool {
    prepare_lifetime(logins, &row.label);
    let current = captured_context(logins, &row.label, Some(row));
    let entry = logins.auth_contexts.entries.get(&row.label);
    let keep = logins.auth_contexts.contextual
        || logins
            .get(&row.label)
            .is_none_or(|binding| applies(&binding.lifetime_scope.served, entry, &current));
    let Some(binding) = logins.entry(&row.label) else {
        return false;
    };
    let served = if keep {
        binding.served_at.map_or(at, |held| held.max(at))
    } else {
        at
    };
    let changed = binding.served_at != Some(served) || !keep;
    binding.served_at = Some(served);
    stamp_lifetime(logins, &row.label, Clock::Served, Some(row));
    changed
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountLifetimeScope",
        action = "Transform",
        project = "aterm_agent::harness::accounts::lifetime_scope_tests::lifetime_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountLifetimeScope",
        action = "StaleServed",
        project = "aterm_agent::harness::accounts::lifetime_scope_tests::lifetime_transition"
    )
)]
pub(super) fn finish(
    before: &Logins,
    raw: Option<&Roster>,
    logins: &mut Logins,
    renewed: &BTreeSet<&str>,
    verified: &BTreeSet<&str>,
) -> Result<bool, String> {
    let mut changed = false;
    let labels: Vec<_> = logins.labels.keys().cloned().collect();
    for label in labels {
        let current = Context::of(
            raw.and_then(|raw| raw.row(&label)).map(RawContext::of),
            logins,
            &label,
        );
        let fallback = Context::of(None, before, &label);
        let cached_entry = logins.auth_contexts.entries.get(&label).cloned();
        let binding = logins
            .labels
            .get_mut(&label)
            .ok_or("lifetime binding disappeared during finalization")?;
        for clock in CLOCKS {
            let present = clock.present(binding);
            let scope = binding.lifetime_scope.get_mut(clock);
            let witness = scope.witness.take();
            let retired_before = witness
                .as_ref()
                .map_or(scope.retired, |witness| witness.retired_at_entry);
            if !present {
                scope.retired = false;
                changed |= retired_before;
                continue;
            }
            let entry = witness
                .as_ref()
                .and_then(|witness| witness.entry.as_ref())
                .or(cached_entry.as_ref());
            let fresh = witness
                .as_ref()
                .is_some_and(|witness| witness.observed_entry || witness.fresh.is_some());
            let matches = !witness.as_ref().is_some_and(|witness| witness.invalidated)
                && match witness.as_ref().and_then(|witness| witness.fresh.as_ref()) {
                    Some(observed) => {
                        observed.matches(&current, true, verified.contains(label.as_str()))
                    }
                    None => {
                        !renewed.contains(label.as_str())
                            && entry.unwrap_or(&fallback).matches(&current, false, false)
                    }
                };
            if !matches {
                if fresh {
                    return Err(
                        "lifetime observation no longer matches the final login; nothing recorded"
                            .into(),
                    );
                }
                scope.retired = true;
            }
            changed |= retired_before != scope.retired
                || (fresh && entry.is_some_and(|entry| !entry.matches(&current, false, false)));
        }
    }
    Ok(changed)
}
