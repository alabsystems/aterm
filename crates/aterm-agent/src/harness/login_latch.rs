// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE SIGN-IN PER LOGIN, HOWEVER MANY TABS SHARE IT (the review of the login
//! wall, 2026-10-19, F7). Claude Code's sessions on one machine sign in with
//! one stored login unless their environment names another, so one expiry
//! walls every tab working on it. Decided tab by tab, each typed its own
//! `/login` and answered its own picker: the owner met as many browser
//! sign-ins, and once one was finished the others still asked for theirs.
//!
//! A host keeps ONE [`LoginLatches`] for all its tabs, keyed by the stored
//! login a process signs in with ([`login_key`]):
//!
//! * the first tab whose loop is about to type `/login` reserves its sign-in
//!   ([`LoginLatches::carry_for`], under its worker's stop handle); a tab
//!   that finds it taken types nothing;
//! * every other tab at that login's wall reads that sign-in standing
//!   ([`LoginShared::Carried`]) and waits on it;
//! * once the login is back in any tab ([`LoginLatches::back`]: `Login
//!   successful`, or a turn worked past the wall), each tab that waited reads
//!   it back ([`LoginShared::Back`]) and delivers its turn again — the tab
//!   that carried the sign-in too, where the login came back elsewhere (its
//!   own `/login` left, the owner signed in in another tab);
//! * an unacknowledged attempt, a relaunch of the tab onto another login, or
//!   an ended worker releases its reservation
//!   ([`LoginLatches::abandon_for`]); a tab that closes
//!   ([`LoginLatches::gone`]) likewise leaves its sign-in to
//!   whichever tab takes it next.
//!
//! Nothing here reads a screen or a clock: the host asks it, under one lock,
//! for what each tab's loop asks of its host.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::accounts::{HOME_ENV, ProcessLogin};
use crate::supervise::policy::login::LoginShared;

/// THE STORED LOGIN A PROCESS SIGNS IN WITH, from its exec environment
/// (`KEY=value` entries, as the kernel hands them back): the keychain item
/// Claude Code reads — named by [`super::accounts::LOGIN_ENV`] where it is
/// set, else by the Claude home [`HOME_ENV`] moves it to, else the default
/// item. Two processes with one key share one login: one expiry walls both,
/// and one sign-in brings both back. `None`: an environment that was not read,
/// or a process that authenticates otherwise (a credential, a provider, a
/// profile or a route of its own: [`ProcessLogin::stored_login`]), whose
/// lost login is its own.
#[must_use]
pub fn login_key(env: &[String]) -> Option<String> {
    let who = ProcessLogin::of_exec_env(env)?;
    if !who.stored_login {
        return None;
    }
    if let Some(dir) = who.login {
        return Some(format!("dir:{dir}"));
    }
    if who.default_home {
        return Some("default".to_string());
    }
    env.iter()
        .filter_map(|entry| entry.split_once('='))
        .find(|(name, value)| *name == HOME_ENV && !value.is_empty())
        .map(|(_, home)| format!("dir:{home}"))
}

/// THE SIGN-INS OF THE LOGINS ONE HOST'S TABS SHARE ([module docs](self)),
/// by [`login_key`]. Every call takes one lock and holds it for a few map
/// operations; nothing is waited on under it.
#[derive(Debug, Default)]
pub struct LoginLatches {
    logins: Mutex<BTreeMap<String, Latch>>,
}

/// One login's sign-in.
#[derive(Debug, Default)]
struct Latch {
    /// The tab whose `/login` is the login's sign-in, until the login is
    /// back, its worker ends, or the tab closes.
    carrier: Option<String>,
    /// A worker's existing stop handle identifies its reservation. Its value
    /// is not read here; an older worker cannot release a successor's.
    carrier_owner: Option<Arc<AtomicBool>>,
    /// The roster row whose login the carrier's sign-in is, as its host
    /// named it ([`LoginLatches::carried_rows`]).
    row: Option<String>,
    /// The tabs that read that sign-in standing at their own walls.
    waiting: BTreeSet<String>,
    /// The tabs whose wait the login's return answered, each with the tab it
    /// came back in: read back until the tab's own loss ends, or it takes a
    /// sign-in of its own.
    back: BTreeMap<String, String>,
}

impl Latch {
    fn idle(&self) -> bool {
        self.carrier.is_none() && self.waiting.is_empty() && self.back.is_empty()
    }
}

impl LoginLatches {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Latch>> {
        self.logins.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What tab `sid` reads of login `key`'s sign-in at its own lost login:
    /// another tab's standing ([`LoginShared::Carried`]: `sid` is counted
    /// among the tabs waiting on it), the login back in another tab since
    /// `sid` waited on it ([`LoginShared::Back`]), or nothing — `sid`'s own
    /// `/login` is the login's.
    pub fn shared(&self, key: &str, sid: &str) -> Option<LoginShared> {
        let mut logins = self.lock();
        let latch = logins.get_mut(key)?;
        if let Some(by) = latch.carrier.clone().filter(|by| by != sid) {
            latch.waiting.insert(sid.to_string());
            return Some(LoginShared::Carried { by });
        }
        latch
            .back
            .get(sid)
            .map(|by| LoginShared::Back { by: by.clone() })
    }

    /// Tab `sid` takes login `key`'s one sign-in, as its loop types `/login`:
    /// `Ok` — it carries it (again, or first); `Err(by)` — tab `by` took it
    /// first, and `sid` waits on it. A sign-in `sid` carried of another login
    /// is nobody's now: its process signs in with this one. The tests' form
    /// of [`Self::carry_for`], with no worker's handle.
    ///
    /// # Errors
    ///
    /// The tab that carries the sign-in already.
    #[cfg(test)]
    pub fn carry(&self, key: &str, sid: &str) -> Result<(), String> {
        self.carry_owned(key, sid, None, None)
    }

    /// Reserve the sign-in for one worker, identified by its own stop handle.
    /// The handle is an identity only, independent of its boolean value.
    /// `row`: the roster row whose login this is, where one is
    /// ([`Self::carried_rows`]).
    ///
    /// # Errors
    ///
    /// The tab that already carries the sign-in.
    pub fn carry_for(
        &self,
        key: &str,
        sid: &str,
        owner: &Arc<AtomicBool>,
        row: Option<&str>,
    ) -> Result<(), String> {
        self.carry_owned(key, sid, Some(Arc::clone(owner)), row)
    }

    /// THE ROSTER ROWS WHOSE LOGIN A TAB'S SIGN-IN CARRIES NOW: each login
    /// one tab's `/login` stands for, by the row its host named at the carry.
    /// The tab's own notice is that loss's one step, so the accounts' digest
    /// holds back its own line for those rows (the review of the login
    /// carry, acct3-3: the tab asked for a browser sign-in while the digest
    /// asked for a renewal of the same loss).
    #[must_use]
    pub fn carried_rows(&self) -> BTreeSet<String> {
        self.lock()
            .values()
            .filter(|latch| latch.carrier.is_some())
            .filter_map(|latch| latch.row.clone())
            .collect()
    }

    fn carry_owned(
        &self,
        key: &str,
        sid: &str,
        owner: Option<Arc<AtomicBool>>,
        row: Option<&str>,
    ) -> Result<(), String> {
        let mut logins = self.lock();
        for (other, latch) in logins.iter_mut() {
            if other != key && latch.carrier.as_deref() == Some(sid) {
                latch.carrier = None;
                latch.carrier_owner = None;
            }
        }
        let latch = logins.entry(key.to_string()).or_default();
        if let Some(by) = latch.carrier.clone().filter(|by| by != sid) {
            latch.waiting.insert(sid.to_string());
            return Err(by);
        }
        latch.carrier = Some(sid.to_string());
        latch.carrier_owner = owner;
        latch.row = row.map(str::to_string);
        latch.waiting.remove(sid);
        latch.back.remove(sid);
        Ok(())
    }

    /// An unacknowledged attempt, a relaunch of the tab's process onto
    /// another login (the move the roster made, a renewal followed: the
    /// sign-in it carried is of a login it no longer signs in with — the
    /// review of the login carry, r3h-1), or an ended worker releases only
    /// its own reservation. Waiting tabs see no successful sign-in and may
    /// take one; a newer worker on the same tab keeps its reservation.
    ///
    /// Whether a carry that named a roster row ended: that row is off
    /// [`Self::carried_rows`] now, and the accounts' digest has its lines
    /// again where their cause stands.
    pub fn abandon_for(&self, sid: &str, owner: &Arc<AtomicBool>) -> bool {
        let mut logins = self.lock();
        let mut row = false;
        for latch in logins.values_mut() {
            if latch.carrier.as_deref() == Some(sid)
                && latch
                    .carrier_owner
                    .as_ref()
                    .is_some_and(|held| Arc::ptr_eq(held, owner))
            {
                latch.carrier = None;
                latch.carrier_owner = None;
                row |= latch.row.is_some();
            }
        }
        logins.retain(|_, latch| !latch.idle());
        row
    }

    /// Login `key` is back in tab `sid` (its own loss ended with no wall):
    /// its sign-in is nobody's, and every other tab that waited on it reads
    /// it back in `sid` until its own loss ends — the tab that carried it
    /// too, where that is another (the review of the login carry,
    /// 2026-10-20: its own `/login` left, or its dialog still up, it never
    /// learned the owner had signed in elsewhere, and its turn stayed
    /// stopped with the owner's notice up). A sign-in `sid` carried of
    /// another login is nobody's either: its process left that login (a
    /// relaunch onto a renewed one, say), and the tabs that waited on it take
    /// their own.
    pub fn back(&self, key: &str, sid: &str) {
        let mut logins = self.lock();
        for (other, latch) in logins.iter_mut() {
            if other != key && latch.carrier.as_deref() == Some(sid) {
                latch.carrier = None;
                latch.carrier_owner = None;
            }
        }
        logins.retain(|_, latch| !latch.idle());
        let Some(latch) = logins.get_mut(key) else {
            return;
        };
        if let Some(carrier) = latch.carrier.take().filter(|c| c != sid) {
            latch.back.insert(carrier, sid.to_string());
        }
        latch.carrier_owner = None;
        for waited in std::mem::take(&mut latch.waiting) {
            if waited != sid {
                latch.back.insert(waited, sid.to_string());
            }
        }
        latch.back.remove(sid);
        if latch.idle() {
            logins.remove(key);
        }
    }

    /// Tab `sid` closed, or its loss ended on a login no key names: a
    /// sign-in it carried is the next tab's to take, and nothing waits on its
    /// behalf.
    pub fn gone(&self, sid: &str) {
        let mut logins = self.lock();
        for latch in logins.values_mut() {
            if latch.carrier.as_deref() == Some(sid) {
                latch.carrier = None;
                latch.carrier_owner = None;
            }
            latch.waiting.remove(sid);
            latch.back.remove(sid);
        }
        logins.retain(|_, latch| !latch.idle());
    }
}

#[cfg(test)]
#[path = "login_latch_tests.rs"]
mod tests;
