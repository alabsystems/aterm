// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared per-login `/login` reservation latch across tabs on one host.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::accounts::{HOME_ENV, ProcessLogin};
use crate::supervise::policy::login::LoginShared;

/// Stored-login key (`config:<dir>` or `service:<name>`) from a process's `KEY=value` env.
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

/// Per-host table of shared login sign-ins, keyed by [`login_key`].
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

    /// Shared sign-in state for tab `sid` on login `key` (`Carried` by another tab or `Back`).
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

    /// Reserve login `key`'s sign-in for tab `sid` (`None` owner).
    ///
    /// # Errors
    /// Returns the carrier tab's SID if another tab already holds the sign-in.
    #[cfg(test)]
    pub fn carry(&self, key: &str, sid: &str) -> Result<(), String> {
        self.carry_owned(key, sid, None, None)
    }

    /// Reserve login `key`'s sign-in for tab `sid` under worker `owner` and roster `row`.
    ///
    /// # Errors
    /// Returns the carrier tab's SID if another tab already holds the sign-in.
    pub fn carry_for(
        &self,
        key: &str,
        sid: &str,
        owner: &Arc<AtomicBool>,
        row: Option<&str>,
    ) -> Result<(), String> {
        self.carry_owned(key, sid, Some(Arc::clone(owner)), row)
    }

    /// Roster rows whose login a tab's sign-in currently carries.
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

    /// Release any sign-in or wait held by `sid` when `owner` matches the current carrier owner.
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

    /// Mark login `key` restored in tab `sid` and notify waiting tabs via [`LoginShared::Back`].
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

    /// Release any sign-in or wait held by closed tab `sid`.
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
