// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Names the wait of an agent's quiet call once per suspect episode, by the same identity-bound
//! sample `sys name` takes ([`crate::sys_naming::command`]), before anything is said of it.
//!
//! The kernel names few waits on macOS: a process asleep in `nanosleep` (a `sleep` the agent ran)
//! or on a mutex shows no descriptor, knote or wait channel, so the engine sees only a quiet call
//! and would, at φ ≥ 8, call it one at no progress. One stack sample settles it: a timer is a
//! wait the call declared (`TierB::Timer`, legitimate, never at no progress), and a lock or mutex
//! is said with its word (and a lock with its holder) where the agent is told of it. One sample
//! runs at a time, on its own thread, never the UI's or the probe's; a call is sampled once an
//! episode (its job and the engine's episode number), and again only once progress opens another.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use aterm_vitals::jobs::{Class, JobKind, JobsView, Standing, Verdict};
use aterm_vitals::sys::query::NameTarget;

/// The episodes sampled, each by its tab, job id and engine episode, and whether a sample runs.
pub(crate) struct Tried {
    seen: Mutex<BTreeSet<(u64, String, u32)>>,
    running: AtomicBool,
}

/// This process's.
pub(crate) static TRIED: Tried = Tried::new();

impl Tried {
    pub(crate) const fn new() -> Self {
        Self {
            seen: Mutex::new(BTreeSet::new()),
            running: AtomicBool::new(false),
        }
    }

    /// The next episode of `view` to sample, marked sampled: an agent's live call, quiet or stuck,
    /// waiting or polling, whose wait counts against it (`Standing::Capable | Capped`) and that
    /// no name holds, not sampled in this episode before. Episodes of jobs `view` no longer names
    /// are forgotten.
    pub(crate) fn next_due(&self, view: &JobsView) -> Option<(u64, String)> {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        seen.retain(|(tab, id, _)| view.rows.iter().any(|r| r.tab == *tab && r.id == *id));
        let row = view.rows.iter().find(|r| {
            r.kind == JobKind::AgentCall
                && matches!(r.class, Class::Waiting | Class::Polling)
                && matches!(r.verdict, Verdict::Quiet | Verdict::Stuck)
                && matches!(r.standing, Some(Standing::Capable | Standing::Capped))
                && r.named.is_none()
                && !seen.contains(&(r.tab, r.id.clone(), r.episode))
        })?;
        seen.insert((row.tab, row.id.clone(), row.episode));
        Some((row.tab, row.id.clone()))
    }

    /// Forgets `job` of `tab`, so a later view samples it again (a sample another request held
    /// off).
    fn forget(&self, tab: u64, job: &str) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(t, id, _)| !(*t == tab && id == job));
    }
}

/// Samples, on its own thread, the next call of `view` due ([`Tried::next_due`]) unless a sample
/// runs; macOS only, where the sampler is.
pub(crate) fn name_quiet_calls(view: &JobsView) {
    if !cfg!(target_os = "macos") || TRIED.running.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some((tab, job)) = TRIED.next_due(view) else {
        TRIED.running.store(false, Ordering::Release);
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("aterm-auto-name".to_string())
        .spawn(move || {
            let said = crate::sys_naming::command(
                &crate::sys_host::SYS,
                tab,
                NameTarget::Job(job.clone()),
                true,
                false,
            );
            if said.contains("why=busy") {
                TRIED.forget(tab, &job);
            }
            aterm_log::debug!("auto-name tab={tab} job={job}: {}", said.trim_end());
            TRIED.running.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        TRIED.running.store(false, Ordering::Release);
    }
}

#[cfg(test)]
#[path = "auto_naming_tests.rs"]
mod tests;
