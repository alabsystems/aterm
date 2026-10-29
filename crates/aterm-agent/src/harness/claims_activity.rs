// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Positive activity evidence shared by account stores under one home. Nothing here is
//! replicated: the Commons row remains the only published claim. A store cannot retract
//! another store's evidence, and a crash after saving evidence can only delay release.

use super::{IDLE_S, LeaseKey, Row, SESSION_FRESH_S, accounts};
#[cfg(unix)]
use aterm_json::{Value, json};
#[cfg(unix)]
use aterm_uds::private_file::{self, PrivateFileError, PrivateLock};
use std::collections::BTreeMap;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
const FILE: &str = "activity.json";
const MAX_ENTRIES: usize = 4096;
#[cfg(unix)]
const MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Book {
    machine: String,
    at: i64,
    entries: BTreeMap<LeaseKey, i64>,
}

impl Book {
    #[cfg(unix)]
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let value: Value = aterm_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if value["version"].as_u64() != Some(1) {
            return Err("unsupported claim activity version".into());
        }
        let machine = value["machine"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("claim activity has no machine")?
            .to_owned();
        let at = value["at"]
            .as_i64()
            .ok_or("claim activity has no observation time")?;
        let rows = value["entries"]
            .as_array()
            .filter(|rows| rows.len() <= MAX_ENTRIES)
            .ok_or("claim activity entries are missing or full")?;
        let mut entries = BTreeMap::new();
        for row in rows {
            let pool = row["pool"]
                .as_str()
                .ok_or("claim activity has no provider")?;
            let email = row["email"]
                .as_str()
                .ok_or("claim activity has no identity")?;
            let key = LeaseKey::new(pool, email)?;
            let until = row["until"]
                .as_i64()
                .ok_or("claim activity has no expiry")?;
            if until <= at || until > at.saturating_add(SESSION_FRESH_S) {
                return Err("claim activity has an invalid expiry".into());
            }
            if key.pool != pool || key.email != email || entries.insert(key, until).is_some() {
                return Err("claim activity has a noncanonical or duplicate identity".into());
            }
        }
        Ok(Self {
            machine,
            at,
            entries,
        })
    }

    #[cfg(unix)]
    fn encode(&self) -> Result<Vec<u8>, String> {
        aterm_json::to_vec(&json!({
            "version": 1, "machine": self.machine, "at": self.at,
            "entries": self.entries.iter().map(|(key, until)| json!({
                "pool": key.pool, "email": key.email, "until": until
            })).collect::<Vec<_>>()
        }))
        .map_err(|e| e.to_string())
    }

    /// Union of positive evidence; an older or empty observer never lowers a deadline.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimActivity",
            action = "ObserveA",
            project = "aterm_agent::harness::claims::activity::tests::activity_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimActivity",
            action = "ObserveB",
            project = "aterm_agent::harness::claims::activity::tests::activity_transition"
        )
    )]
    fn note(&mut self, key: LeaseKey, until: i64) -> Result<(), String> {
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_ENTRIES {
            return Err("local claim activity is full".into());
        }
        self.entries
            .entry(key)
            .and_modify(|held| *held = (*held).max(until))
            .or_insert(until);
        Ok(())
    }

    fn live(&self, key: &LeaseKey, now: i64) -> bool {
        self.entries.get(key).is_some_and(|until| *until > now)
    }
}

/// The lock spans the activity snapshot, its durable save, and Commons publication.
pub(super) struct Held {
    #[cfg(unix)]
    dir: PathBuf,
    book: Option<Book>,
    #[cfg(unix)]
    _lock: PrivateLock,
}

impl Held {
    #[cfg(unix)]
    pub(super) fn take(home: &Path) -> Result<Self, String> {
        let parent = home.join(".aterm");
        let _ = private_file::ensure_private_dir(&parent).map_err(|e| e.to_string())?;
        let dir = parent.join("claim-activity");
        private_file::create_private_dir(&dir).map_err(|e| e.to_string())?;
        let lock = PrivateLock::take(
            &dir,
            "activity.lock",
            Duration::from_secs(1),
            "local claim activity is busy",
        )
        .map_err(|e| e.to_string())?;
        let book = match private_file::read_private(&dir, FILE, MAX_BYTES) {
            Ok(bytes) => Some(Book::decode(&bytes)?),
            Err(PrivateFileError::Missing) => None,
            Err(e) => return Err(format!("{}: {e}", dir.join(FILE).display())),
        };
        Ok(Self {
            dir,
            book,
            _lock: lock,
        })
    }

    #[cfg(not(unix))]
    pub(super) fn take(_: &Path) -> Result<Self, String> {
        Err("private claim activity is not available on this platform".into())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimActivity",
            action = "Empty",
            project = "aterm_agent::harness::claims::activity::tests::activity_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimActivity",
            action = "Tick",
            project = "aterm_agent::harness::claims::activity::tests::activity_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimActivity",
            action = "Delayed",
            project = "aterm_agent::harness::claims::activity::tests::activity_transition"
        )
    )]
    pub(super) fn observe(
        &mut self,
        machine: &str,
        current: Option<&Row>,
        state: &accounts::State,
        keys: &[(String, LeaseKey)],
        now: i64,
    ) -> Result<i64, String> {
        // An existing row predates this ledger, or belongs to a newly joined Commons.
        // Give it one bounded grace period; never reseed missing entries on later looks.
        if self
            .book
            .as_ref()
            .is_none_or(|book| book.machine != machine)
        {
            let mut book = Book {
                machine: machine.to_owned(),
                at: now,
                entries: BTreeMap::new(),
            };
            if let Some(row) = current.filter(|row| row.until > now) {
                for claim in &row.claims {
                    book.note(
                        claim.key.clone(),
                        row.until.min(now.saturating_add(SESSION_FRESH_S)),
                    )?;
                }
            }
            self.book = Some(book);
        }
        let book = self.book.as_mut().expect("initialized above");
        if book.at.saturating_sub(now) > accounts::STAMP_SKEW_S {
            return Err("local claim activity clock exceeds the allowed skew".into());
        }
        // Callers capture time before taking the lock. A delayed observer, or a host
        // clock correction, must neither rewind expiry nor acquire older seniority.
        let now = now.max(book.at);
        book.at = now;
        book.entries.retain(|_, until| *until > now);
        for (label, key) in keys {
            let until = state
                .claimed_at
                .get(label)
                .map(|at| at.min(&now).saturating_add(IDLE_S))
                .into_iter()
                .chain(
                    state
                        .sessions
                        .values()
                        .filter(|row| &row.account == label)
                        .map(|row| row.at.min(now).saturating_add(SESSION_FRESH_S)),
                )
                .max();
            if let Some(until) = until.filter(|until| *until > now) {
                book.note(key.clone(), until)?;
            }
        }
        Ok(now)
    }

    pub(super) fn live(&self, key: &LeaseKey, now: i64) -> bool {
        self.book.as_ref().is_some_and(|book| book.live(key, now))
    }

    pub(super) fn claim(&mut self, key: LeaseKey, now: i64) -> Result<(), String> {
        self.book
            .as_mut()
            .ok_or("local claim activity was not initialized")?
            .note(key, now.saturating_add(IDLE_S))
    }

    #[cfg(unix)]
    pub(super) fn save(&self) -> Result<(), String> {
        let bytes = self
            .book
            .as_ref()
            .ok_or("local claim activity was not initialized")?
            .encode()?;
        private_file::write_private(&self.dir, FILE, &bytes).map_err(|e| e.to_string())
    }
}

#[cfg(not(unix))]
impl Held {
    pub(super) fn save(&self) -> Result<(), String> {
        Err("private claim activity is not available on this platform".into())
    }
}

#[cfg(all(test, unix))]
#[path = "claims_activity_tests.rs"]
mod tests;
