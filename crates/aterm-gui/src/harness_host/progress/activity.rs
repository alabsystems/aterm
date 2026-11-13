// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's bounded account metadata read, independent of recovery decisions.

use super::*;

/// The owner of a background metadata read, captured without process or store I/O.
pub(in crate::harness_host) struct ActivityCapture {
    snapshot: Snapshot,
    generation: Arc<Mutex<()>>,
    policy: Arc<()>,
}

struct ActivityRows {
    state: PathBuf,
    labels: BTreeSet<String>,
    rows: Arc<[accounts::Row]>,
}

impl ActivityRows {
    fn read(seat: &AccountSeat) -> Result<Self, String> {
        let records = accounts::Store::at(&seat.state)
            .bounded_by(seat.ladder)
            .read_account_records(seat.roster.as_deref(), seat.now)
            .map_err(|_| "account activity metadata is unavailable".to_owned())?;
        if !records.notes.is_empty() {
            return Err("account activity metadata is incomplete".into());
        }
        Ok(Self {
            state: seat.state.clone(),
            labels: records
                .roster
                .rows
                .iter()
                .map(|row| row.label.clone())
                .collect(),
            rows: records.roster.rows.into(),
        })
    }
}

impl ActivityCapture {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountExecutionVisibility",
            action = "ReplaceContext",
            reason = "The environment can replace the retained process or account generation while metadata is read."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountExecutionVisibility",
            action = "WorkStarts",
            reason = "The existing upkeep worker independently begins attributed sign-in work."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountExecutionVisibility",
            action = "ActivityBusy",
            reason = "The activity guard may be held by its existing owner during an observation."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountExecutionVisibility",
            action = "CaptureContext",
            project = "harness_host::tests::account_advice::execution_transition"
        )
    )]
    pub(in crate::harness_host) fn capture(kept: &Kept) -> Option<Self> {
        let snapshot = kept.snapshot.as_ref()?;
        if !observes(Some(snapshot.agent().program())) {
            return None;
        }
        Some(Self {
            snapshot: snapshot.clone(),
            generation: Arc::clone(&kept.account.landing),
            policy: Arc::clone(&kept.account.progress_policy),
        })
    }

    fn current(&self, kept: &Kept) -> bool {
        progress_context_current(&self.snapshot, &self.generation, kept)
            && Arc::ptr_eq(&self.policy, &kept.account.progress_policy)
    }

    fn context(&self, metadata: &ActivityRows, born: u64) -> AccountActivityContext {
        AccountActivityContext {
            snapshot: self.snapshot.clone(),
            generation: Arc::clone(&self.generation),
            born,
            state: metadata.state.clone(),
            labels: metadata.labels.clone(),
            rows: Arc::clone(&metadata.rows),
            policy: Arc::clone(&self.policy),
        }
    }

    #[cfg(test)]
    fn read_with_birth(
        &self,
        seat: &AccountSeat,
        birth: &dyn Fn(&Snapshot) -> Option<u64>,
    ) -> Result<AccountActivityContext, String> {
        let born = birth(&self.snapshot).ok_or("account activity process changed")?;
        Ok(self.context(&ActivityRows::read(seat)?, born))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountExecutionVisibility",
            action = "PublishContext",
            project = "harness_host::tests::account_advice::execution_transition"
        )
    )]
    fn publish_with_birth(
        &self,
        kept: &Mutex<Kept>,
        hooks: &Hooks,
        seat: &AccountSeat,
        observed: Result<AccountActivityContext, String>,
        birth: &dyn Fn(&Snapshot) -> Option<u64>,
    ) -> bool {
        let context = observed.ok().filter(|context| {
            context.snapshot == self.snapshot
                && Arc::ptr_eq(&context.generation, &self.generation)
                && Arc::ptr_eq(&context.policy, &self.policy)
                && context.state == seat.state
                && current_account_seat(hooks, seat).is_ok()
                && birth(&self.snapshot) == Some(context.born)
        });
        let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.current(&kept) {
            return false;
        }
        let changed = match (&kept.account.activity_context, &context) {
            (Some(old), Some(new)) => {
                !old.current(&kept)
                    || !Arc::ptr_eq(&old.policy, &new.policy)
                    || old.born != new.born
                    || old.state != new.state
                    || old.labels != new.labels
                    || old.rows != new.rows
            }
            (None, None) => false,
            _ => true,
        };
        kept.account.activity_context = context;
        changed
    }

    #[cfg(test)]
    pub(in crate::harness_host) fn read_for_test(
        &self,
        seat: &AccountSeat,
        born: u64,
    ) -> Result<AccountActivityContext, String> {
        self.read_with_birth(seat, &|_| Some(born))
    }

    #[cfg(test)]
    pub(in crate::harness_host) fn publish_for_test(
        &self,
        kept: &Mutex<Kept>,
        hooks: &Hooks,
        seat: &AccountSeat,
        observed: Result<AccountActivityContext, String>,
        born: Option<u64>,
    ) -> bool {
        self.publish_with_birth(kept, hooks, seat, observed, &|_| born)
    }
}

/// A generic wake refreshes metadata once; normal frame/settle timers reuse current context.
/// All retained Claude snapshots participate, including those whose harness is switched Off.
pub(in crate::harness_host) fn refresh_kept(shared: &Shared, hooks: &Hooks, full_wake: bool) {
    refresh_kept_with_birth(shared, hooks, full_wake, &Snapshot::current_start_seconds);
}

fn refresh_kept_with_birth(
    shared: &Shared,
    hooks: &Hooks,
    full_wake: bool,
    birth: &dyn Fn(&Snapshot) -> Option<u64>,
) {
    let retained: Vec<_> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .cloned()
        .collect();
    let mut changed = false;
    let mut pending = Vec::new();
    for kept in retained {
        let capture = {
            let mut held = kept.lock().unwrap_or_else(PoisonError::into_inner);
            match ActivityCapture::capture(&held) {
                None => {
                    changed |= held.account.activity_context.take().is_some();
                    None
                }
                Some(capture) => {
                    let cached = held
                        .account
                        .activity_context
                        .as_ref()
                        .is_some_and(|context| {
                            context.current(&held)
                                && Arc::ptr_eq(&context.policy, &held.account.progress_policy)
                        });
                    (full_wake || !cached).then_some(capture)
                }
            }
        };
        if let Some(capture) = capture {
            let born = birth(&capture.snapshot);
            pending.push((kept, capture, born));
        }
    }
    if !pending.is_empty() {
        // A single coherent store image supplies every snapshot in this pass.
        let seat = (hooks.acts.account_seat)();
        let metadata = seat.as_ref().map(ActivityRows::read);
        for (kept, capture, born) in pending {
            let observed = metadata
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .zip(born)
                .map(|(metadata, born)| capture.context(metadata, born));
            if let Some(seat) = &seat {
                changed |= capture.publish_with_birth(
                    &kept,
                    hooks,
                    seat,
                    observed.ok_or_else(|| "account activity metadata is unavailable".into()),
                    birth,
                );
            } else {
                let mut held = kept.lock().unwrap_or_else(PoisonError::into_inner);
                if capture.current(&held) {
                    changed |= held.account.activity_context.take().is_some();
                }
            }
        }
    }
    if changed {
        (hooks.note)(HostNote::AccountProgress);
    }
}

#[cfg(test)]
#[path = "activity_own_tests.rs"]
pub(super) mod tests;
