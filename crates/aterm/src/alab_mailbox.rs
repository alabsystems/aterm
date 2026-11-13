// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What becomes of a Proton mailbox's address list once the harness has the mailbox open to read a
//! sign-in code (docs/DESIGN-alab-lab-model-2026-11-01.md §4.10, step 10): the code-fetch path
//! hands it to `aterm_agent::harness::mailbox_seen`, and in a release build this hook runs
//! `aterm_db::alab::Registry::observe_mailbox` on it, on a thread of its own so a sign-in never
//! waits, at most once a day per mailbox and sign-in address in a process once a pass settled, and
//! `main` waits a bounded while for a running pass before it returns. A test, a debug build or a checkout's
//! `targo run` never installs it, so nothing reaches a live registry from there.

use aterm_agent::harness::mailbox_seen::Seen;
use aterm_db::alab::{
    Discovered,
    discover::{Listed, Listing},
};
use std::{
    collections::BTreeMap,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

/// A process takes up one mailbox's list for one sign-in address at most this often once a pass
/// settled; the registry row itself changes only as `discover::plan` allows.
const REST: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a process that is ending waits for its passes still running.
const SETTLE: Duration = Duration::from_secs(10);

/// The registry's reading of what the harness noted.
fn listing(seen: &Seen) -> Listing {
    Listing {
        login: seen.login.clone(),
        target: seen.target.clone(),
        listed: seen
            .addresses
            .iter()
            .map(|address| Listed {
                addr: address.email.clone(),
                receives: address.receives,
            })
            .collect(),
        complete: seen.complete,
    }
}

/// Where a mailbox's list stands in this process, per mailbox and sign-in address.
#[derive(Clone, Copy)]
enum Rest {
    /// A pass is running.
    Running,
    /// A pass settled at this instant.
    Settled(Instant),
}

/// Each (mailbox, sign-in address) pair's last pass in this process, by a digest of both, and how
/// many passes are running, which an ending process waits on.
struct Rests {
    by: Mutex<BTreeMap<[u8; 32], Rest>>,
    running: Mutex<usize>,
    done: Condvar,
}

impl Rests {
    const fn new() -> Self {
        Self {
            by: Mutex::new(BTreeMap::new()),
            running: Mutex::new(0),
            done: Condvar::new(),
        }
    }

    fn key(login: &str, target: Option<&str>) -> [u8; 32] {
        fn update_lower(hasher: &mut aterm_digest::Sha256, s: &str) {
            let s = s.trim();
            if !s.bytes().any(|b| b.is_ascii_uppercase()) {
                hasher.update(s.as_bytes());
            } else {
                let mut buf = [0u8; 64];
                for chunk in s.as_bytes().chunks(buf.len()) {
                    for (dst, &src) in buf.iter_mut().zip(chunk) {
                        *dst = src.to_ascii_lowercase();
                    }
                    hasher.update(&buf[..chunk.len()]);
                }
            }
        }
        let mut hasher = aterm_digest::Sha256::new();
        update_lower(&mut hasher, login);
        hasher.update([0]);
        if let Some(target) = target {
            update_lower(&mut hasher, target);
        }
        hasher.finalize()
    }

    /// Whether a pass for `login` and `target` may start now, marking it running if so: not while
    /// one runs, nor within [`REST`] of one that settled. Each sign-in address is its own pair, so
    /// the second address signed in on a day still reaches its birth.
    #[cfg(test)]
    fn begin(&self, login: &str, target: Option<&str>, now: Instant) -> bool {
        self.begin_key(Self::key(login, target), now)
    }

    fn begin_key(&self, key: [u8; 32], now: Instant) -> bool {
        let Ok(mut by) = self.by.lock() else {
            return false;
        };
        match by.get(&key) {
            Some(Rest::Running) => return false,
            Some(Rest::Settled(at)) if now.saturating_duration_since(*at) < REST => return false,
            _ => {}
        }
        by.insert(key, Rest::Running);
        drop(by);
        if let Ok(mut running) = self.running.lock() {
            *running += 1;
        }
        true
    }

    /// A pass ended: one that `settled` rests its pair for [`REST`]; one that did not (the
    /// registry not caught up here, the mailbox not yet known, an error) leaves the pair due at
    /// the next code fetch.
    #[cfg(test)]
    fn end(&self, login: &str, target: Option<&str>, settled: bool, now: Instant) {
        self.end_key(Self::key(login, target), settled, now);
    }

    fn end_key(&self, key: [u8; 32], settled: bool, now: Instant) {
        if let Ok(mut by) = self.by.lock() {
            if settled {
                by.insert(key, Rest::Settled(now));
            } else {
                by.remove(&key);
            }
        }
        if let Ok(mut running) = self.running.lock() {
            *running = running.saturating_sub(1);
            self.done.notify_all();
        }
    }

    /// Wait up to `bound` for every running pass to end.
    fn settle(&self, bound: Duration) {
        let Ok(running) = self.running.lock() else {
            return;
        };
        let _ = self
            .done
            .wait_timeout_while(running, bound, |running| *running > 0);
    }
}

static RESTS: Rests = Rests::new();

/// Whether a pass's outcome stands for the rest period: it wrote, found nothing to write, or
/// reached a state only the owner or a table roll changes. A registry still catching up here, a
/// mailbox nothing names yet, and an error are tried again at the next code fetch.
fn settled(pass: &Result<Discovered, String>) -> bool {
    use aterm_db::alab::discover::Outcome;
    matches!(
        pass,
        Ok(Discovered {
            outcome: Outcome::Excluded
                | Outcome::Held
                | Outcome::Empty
                | Outcome::Unchanged
                | Outcome::Rested { .. }
                | Outcome::Observed { .. }
                | Outcome::QuotaSpent,
            ..
        })
    )
}

/// Whether a pass's result is worth a line a shipped build keeps: its log filter stops at `Info`.
/// A pass with findings, held, out of quota or in error speaks; one that wrote or had nothing to
/// do stays at debug, and `aterm db alab check` answers why at any time.
fn speaks(pass: &Result<Discovered, String>) -> bool {
    use aterm_db::alab::discover::Outcome;
    match pass {
        Err(_) => true,
        Ok(done) => {
            !done.findings.is_empty() || matches!(done.outcome, Outcome::Held | Outcome::QuotaSpent)
        }
    }
}

/// One pass's line, by outcome word and key, never by value.
fn log_pass(pass: &Result<Discovered, String>) {
    let spoken = speaks(pass);
    match pass {
        Ok(done) if spoken => aterm_log::info!(
            "lab mailbox discovery: {}, {} record(s), findings {:?}",
            done.outcome.word(),
            done.records,
            done.findings
        ),
        Ok(done) => aterm_log::debug!(
            "lab mailbox discovery: {}, {} record(s)",
            done.outcome.word(),
            done.records
        ),
        Err(why) => aterm_log::info!("lab mailbox discovery: {why}"),
    }
}

fn pass(listing: &Listing) -> Result<Discovered, String> {
    let paths = aterm_secrets::client::Paths::live()
        .map_err(|_| "no vault state on this Mac".to_owned())?;
    let config =
        aterm_types::dirs::aterm_config_path().ok_or_else(|| "no aterm config path".to_owned())?;
    let now_ms = aterm_time::unix_millis()
        .filter(|ms| *ms > 0)
        .ok_or_else(|| "no valid clock on this Mac".to_owned())?;
    aterm_db::alab::Registry::at(paths)
        .observe_mailbox(&config, listing, now_ms)
        .map_err(|why| why.to_string())
}

/// Fill the code-fetch path's hook, where this process holds a lab ([`crate::alab_view::holds`]),
/// the holds the Commons runtime keeps.
pub(crate) fn install() {
    if !crate::alab_view::holds() {
        return;
    }
    aterm_agent::harness::mailbox_seen::install(Box::new(|seen: &Seen| {
        take_up(
            &RESTS,
            seen,
            |body| {
                std::thread::Builder::new()
                    .name("aterm-lab-mailbox".into())
                    .spawn(body)
                    .map(drop)
            },
            pass,
        );
    }));
}

/// The hook's take-up of what `seen` noted: where its pair is due in `rests`, a pass on the thread
/// `spawn` starts, which ends the pair by whether the pass settled; where no thread starts, the
/// pair is due again at once.
fn take_up(
    rests: &'static Rests,
    seen: &Seen,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    pass: fn(&Listing) -> Result<Discovered, String>,
) {
    let key = Rests::key(&seen.login, seen.target.as_deref());
    if !rests.begin_key(key, Instant::now()) {
        return;
    }
    let listing = listing(seen);
    let spawned = spawn(Box::new(move || {
        let done = pass(&listing);
        log_pass(&done);
        rests.end_key(key, settled(&done), Instant::now());
    }));
    if spawned.is_err() {
        rests.end_key(key, false, Instant::now());
        aterm_log::debug!("lab mailbox discovery: no thread");
    }
}

/// Before the process returns from `main`: wait, a bounded while, for passes still running, so
/// a short-lived CLI that read a code still records what its mailbox listed.
pub(crate) fn settle() {
    RESTS.settle(SETTLE);
}

#[cfg(test)]
#[path = "alab_mailbox_tests.rs"]
mod tests;
