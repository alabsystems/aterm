// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A waiting account lineage owns its frontier and one successor slot. These
//! data-only credits neither extend an execution deadline nor hold app/input custody.

use super::*;

#[derive(Debug, Default)]
pub(super) struct AccountLineages {
    // Frontier -> reserved or published candidate. None is an unused slot,
    // charged to the same global bound as operations and preparations.
    entries: BTreeMap<String, Option<String>>,
}

#[derive(Debug)]
pub(super) enum AccountSlot {
    Plain,
    Root,
    Successor(String),
}

impl AccountSlot {
    pub(super) fn for_request(
        request: &Prepare,
        caller: &SupervisorInputCaller,
        renewal: bool,
        account: Option<&AccountContinuation>,
    ) -> Self {
        if let Some(account) = account {
            return Self::Successor(account.proof().predecessor.id.clone());
        }
        let stored = |command: &RestartCommand| {
            matches!(
                command.account_intent.as_deref(),
                Some(aterm_agent::harness::accounts_pool::ExecutionIntent::Stored { .. })
            )
        };
        if !renewal
            && caller.principal == crate::control::Principal::Harness
            && matches!(&request.plan, NativePlan::Restart {
                forward, rollback, cold: None, observation: None, ..
            } if stored(forward) || rollback.as_deref().is_some_and(stored))
        {
            Self::Root
        } else {
            Self::Plain
        }
    }
}

impl AccountLineages {
    pub(super) fn spares(&self) -> usize {
        self.entries
            .values()
            .filter(|candidate| candidate.is_none())
            .count()
    }

    pub(super) fn pins(&self, id: &str) -> bool {
        self.entries.contains_key(id)
            || self
                .entries
                .values()
                .any(|candidate| candidate.as_deref() == Some(id))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLineageBudget",
            action = "Root",
            project = "native_act::account_retention::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLineageBudget",
            action = "Other",
            project = "native_act::account_retention::tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLineageBudget",
            action = "Child",
            project = "native_act::account_retention::tests::transition"
        )
    )]
    pub(super) fn reserve(
        &mut self,
        kind: &AccountSlot,
        id: &str,
        occupied: usize,
    ) -> Result<(), String> {
        let cost = match kind {
            AccountSlot::Plain => 1,
            AccountSlot::Root => 2,
            AccountSlot::Successor(parent) => {
                let Some(candidate) = self.entries.get_mut(parent) else {
                    return Err(UNKNOWN.into());
                };
                if candidate.is_some() {
                    return Err("ERR busy native operation prepare\n".into());
                }
                *candidate = Some(id.to_owned());
                return Ok(());
            }
        };
        if occupied
            .checked_add(cost)
            .is_none_or(|total| total > MAX_COMPLETED)
        {
            return Err("ERR busy native operation history\n".into());
        }
        if matches!(kind, AccountSlot::Root) {
            self.entries.insert(id.to_owned(), None);
        }
        Ok(())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLineageBudget",
            action = "Cancel",
            project = "native_act::account_retention::tests::transition"
        )
    )]
    pub(super) fn cancel(&mut self, kind: &AccountSlot, id: &str) {
        match kind {
            AccountSlot::Plain => {}
            AccountSlot::Root => {
                self.entries.remove(id);
            }
            AccountSlot::Successor(parent) => {
                if let Some(candidate) = self.entries.get_mut(parent)
                    && candidate.as_deref() == Some(id)
                {
                    *candidate = None;
                }
            }
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLineageBudget",
            action = "Transfer",
            project = "native_act::account_retention::tests::transition"
        )
    )]
    fn advance(&mut self, parent: &str, candidate: String) {
        self.entries.remove(parent);
        self.entries.insert(candidate, None);
    }
}

impl Operations {
    /// Transfer only after acknowledged successor publication. The journal
    /// remains authoritative for the old receipt; a missing operation never
    /// recreates its proof. Return owners for destruction outside the table lock.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "AccountFence",
            action = "Reclaim",
            project = "native_act::tests::account_continuation_tests::fence_transaction::fence_transition"
        )
    )]
    pub(super) fn compact_account_lineages(&mut self) -> Vec<Arc<Mutex<Operation>>> {
        let mut retired = Vec::new();
        let frontiers = self
            .account_lineages
            .entries
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in frontiers {
            let Some(parent) = self.by_id.get(&id) else {
                continue;
            };
            let Ok(parent) = parent.try_lock() else {
                continue;
            };
            if !matches!(parent.phase, Phase::Completed(_)) {
                continue;
            }
            let stored = command_for(&parent, parent.resume_phase.unwrap_or(ImagePhase::Forward))
                .is_some_and(|command| {
                    matches!(
                        command.account_intent.as_deref(),
                        Some(aterm_agent::harness::accounts_pool::ExecutionIntent::Stored { .. })
                    )
                });
            let Some(proof) = parent
                .closed_restart
                .as_ref()
                .filter(|proof| proof.noexec() && stored)
            else {
                // A committed, uncertain, aborted or previously executed root
                // cannot authorize an account replacement. Neither can an active
                // command without a stored account. Keep ordinary history.
                self.account_lineages.entries.remove(&id);
                continue;
            };
            let Some(candidate) = self.account_lineages.entries.get(&id).cloned().flatten() else {
                if !proof.keeps_account_frontier() {
                    self.account_lineages.entries.remove(&id);
                }
                continue;
            };
            if self.receipt_flights.contains_key(&id)
                || parent
                    .completion_drain
                    .as_ref()
                    .is_some_and(|drain| !drain.is_drained())
            {
                continue;
            }
            let (request, binding, proof) = (
                parent.request.clone(),
                parent.binding.clone(),
                Arc::clone(proof),
            );
            drop(parent);
            let Some(child) = self.by_id.get(&candidate) else {
                continue;
            };
            let Ok(child) = child.try_lock() else {
                continue;
            };
            if !child.preparation.transfers(&request, &binding)
                || !proof.confirmed_successor(&child.request)
                || !child
                    .authority
                    .account
                    .as_ref()
                    .is_some_and(|account| Arc::ptr_eq(account.proof(), &proof))
            {
                continue;
            }
            let retain_child =
                !matches!(child.phase, Phase::Completed(_)) || child.closed_restart.is_some();
            drop(child);
            if retain_child {
                self.account_lineages.advance(&id, candidate);
            } else {
                // An acknowledged terminal child without a live closure proof
                // cannot be another account frontier; release its credit now.
                self.account_lineages.entries.remove(&id);
            }
            self.completed.retain(|old| old != &id);
            if let Some(parent) = self.by_id.remove(&id) {
                retired.push(parent);
            }
        }
        retired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_spec::interp::State;

    pub(crate) fn transition(state: &State, action: &str) -> State {
        let mut result = state.clone();
        let mut ledger = AccountLineages::default();
        if state["root"] == 1 {
            ledger
                .entries
                .insert("root".into(), (state["child"] == 1).then(|| "child".into()));
        }
        let occupied =
            (state["root"] + state["child"] + state["others"]) as usize + ledger.spares();
        match action {
            "Root" => {
                let _ = ledger.reserve(&AccountSlot::Root, "root", occupied);
            }
            "Other" => {
                if ledger
                    .reserve(&AccountSlot::Plain, "other", occupied)
                    .is_ok()
                {
                    result.insert("others", state["others"] + 1);
                }
            }
            "Child" => {
                let accepted = ledger
                    .reserve(&AccountSlot::Successor("root".into()), "child", occupied)
                    .is_ok();
                result.insert("attempted", 1);
                result.insert("accepted", i64::from(accepted));
            }
            "Cancel" => {
                ledger.cancel(&AccountSlot::Successor("root".into()), "child");
                result.insert("attempted", 0);
                result.insert("accepted", 0);
            }
            "Transfer" => {
                ledger.advance("root", "child".into());
                result.insert("attempted", 0);
                result.insert("accepted", 0);
                result.insert("hops", state["hops"] + 1);
            }
            _ => unreachable!(),
        }
        result.insert("root", i64::from(!ledger.entries.is_empty()));
        result.insert(
            "child",
            i64::from(ledger.entries.values().any(Option::is_some)),
        );
        result.insert("spare", ledger.spares() as i64);
        result
    }

    #[test]
    fn bounded_ledger_refines_production_and_catches_lost_spare() {
        let model = aterm_models_harness::account_lineage_budget_model();
        assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
        for state in aterm_spec::interp::reachable_states(&model) {
            for action in &model.actions {
                if model.action_enabled(action.name, &state) {
                    let actual = transition(&state, action.name);
                    assert!(
                        model.successors(action.name, &state).contains(&actual),
                        "{}: {state:?} -> {actual:?}",
                        action.name
                    );
                }
            }
        }
        let mut state = model.init_state();
        assert!(model.fire("Root", &mut state));
        assert!(model.fire("Child", &mut state));
        let mut lost_spare = transition(&state, "Transfer");
        lost_spare.insert("spare", 0);
        assert!(!model.check_invariant("Reserved", &lost_spare));
    }
}
