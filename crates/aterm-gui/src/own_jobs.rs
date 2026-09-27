// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's OWN JOBS' PROCESSES: the children aterm spawns for work that has a
//! row of its own — today the `atpkg` pass child of the ALab tools lane (the
//! launch pass, the update loop's ticks, the Settings Check), registered from
//! spawn to reap by [`register`] in `run_pass_child`.
//!
//! The strain engine (`aterm_messages::strain`, design ruling 211) groups a
//! loaded machine's processes; a process whose parent chain reaches one of
//! these pids is that job's, and the job's live row (looked up by its key,
//! [`App::strain_own_jobs`]) explains it: the episode's record says
//! `explained by <title>`, and no strain row doubles the work row. Without it
//! the chain reached aterm and the record read `aterm itself` (design ruling
//! 214). A job with no live row — a silent routine pass — is not passed, so
//! it still reads as aterm's: it is not explained by a row nobody sees.
//!
//! Process-wide because the children are spawned on lane threads; the list is
//! a handful of entries at most, read once per strain sweep.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// One registered child: its pid, the key of the row that describes its job,
/// and the registration's own token (so a guard removes only its own entry,
/// even if the kernel reuses the pid).
static JOBS: Mutex<Vec<(u32, &'static str, u64)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

/// A registration: the entry lives until this is dropped (the child reaped).
#[must_use = "the job is registered only while the guard lives"]
#[derive(Debug)]
pub(crate) struct Registration {
    token: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        JOBS.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(_, _, token)| *token != self.token);
    }
}

/// Register `pid` as the process of the job whose row is keyed `key`.
pub(crate) fn register(pid: u32, key: &'static str) -> Registration {
    let token = NEXT.fetch_add(1, Ordering::Relaxed);
    JOBS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((pid, key, token));
    Registration { token }
}

/// Every registered child now: `(pid, row key)`.
pub(crate) fn snapshot() -> Vec<(u32, &'static str)> {
    JOBS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|(pid, key, _)| (*pid, *key))
        .collect()
}

/// The jobs the strain engine is told about: each registered child whose
/// job has a live row (`row` answers a key with that row's id and title),
/// as `(pid, id, title)`. Pure, for the test.
pub(crate) fn with_rows(
    jobs: &[(u32, &'static str)],
    row: impl Fn(&str) -> Option<(aterm_messages::MessageId, String)>,
) -> Vec<(u32, aterm_messages::MessageId, String)> {
    jobs.iter()
        .filter_map(|(pid, key)| row(key).map(|(id, title)| (*pid, id, title)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guard registers its child until it drops, and removes only its own
    /// entry: two registrations of one pid are two entries.
    #[test]
    fn a_registration_lives_as_long_as_its_guard() {
        let pid = 0x7fff_fff0;
        let a = register(pid, "toolchain.pass");
        let b = register(pid, "toolchain.pass");
        let mine = || snapshot().into_iter().filter(|(p, _)| *p == pid).count();
        assert_eq!(mine(), 2);
        drop(a);
        assert_eq!(mine(), 1);
        drop(b);
        assert_eq!(mine(), 0);
    }

    /// Only a job with a live row is passed on, with that row's id and title.
    #[test]
    fn only_a_job_with_a_live_row_explains_its_load() {
        let id = aterm_messages::MessageId::from_raw(7).expect("an id");
        let jobs = [(41, "toolchain.pass"), (42, "update.progress")];
        let got = with_rows(&jobs, |key| {
            (key == "toolchain.pass").then(|| (id, "Installing ALab tools".to_string()))
        });
        assert_eq!(got, vec![(41, id, "Installing ALab tools".to_string())]);
    }
}
