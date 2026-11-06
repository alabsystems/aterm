// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One observation's reads of this Mac's vault: an explicit value made once a read starts and
//! passed down, so each file it holds is read at most once and each value derived from them is
//! derived once, whichever check asks first. A failed read answers every later ask with its error,
//! and nothing outlives the value, so the next observation reads the files as they are then.
//!
//! The device files and the lock are read outside it, the journal holds its own files, and a pinned
//! records layout opens its data file twice, validated and then read.
//!
//! A writer's own files change under it, so a [`Reads`] never lives across a write.

use super::{
    Config, Paths, State, chain_cache, config_with, envelope, fsx, load_records_for, load_state,
    log_entries, pin_gives_way, read_identity, retained_history, source_bound,
};
use crate::Result;
use crate::age::Identity;
use crate::envelope::Envelope;
use crate::membership_log::{Log, Store};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

pub(super) struct Reads<'a> {
    paths: &'a Paths,
    config: OnceCell<Result<Option<Config>>>,
    state: OnceCell<Result<State>>,
    identity: OnceCell<Result<Option<Identity>>>,
    vault: OnceCell<Result<Option<Arc<chain_cache::Parsed>>>>,
    enrollment: OnceCell<Result<retained_history::Enrollment>>,
    lineage: RefCell<Vec<(Config, KeyLine)>>,
    acknowledged: RefCell<Vec<(Config, Result<Option<HeldLog>>)>>,
    verified: OnceCell<Result<bool>>,
    seeded: OnceCell<Result<Option<HeldLog>>>,
    configured: OnceCell<Result<Option<HeldLog>>>,
    custody: OnceCell<Result<Option<HeldLog>>>,
}

/// The signer keys of one configuration's line.
type KeyLine = Rc<[[u8; 32]]>;

/// A store holding a log, with the digest of the log's heads.
#[derive(Clone)]
pub(super) struct HeldLog {
    store: Rc<Store>,
    digest: [u8; 32],
}

impl HeldLog {
    /// `store`'s log, where it holds one.
    pub(super) fn of(store: Store) -> Option<Self> {
        let digest = log_entries::records_heads_digest(store.log()?);
        Some(Self {
            store: Rc::new(store),
            digest,
        })
    }

    pub(super) fn store(&self) -> &Store {
        &self.store
    }

    pub(super) fn log(&self) -> &Log {
        self.store
            .log()
            .expect("HeldLog::of holds only a store with a log")
    }

    pub(super) const fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// The log and its digest, for a caller that keeps them.
    pub(super) fn into_owned(self) -> (Log, [u8; 32]) {
        (self.log().clone(), self.digest)
    }
}

impl<'a> Reads<'a> {
    pub(super) fn new(paths: &'a Paths) -> Self {
        Self {
            paths,
            config: OnceCell::new(),
            state: OnceCell::new(),
            identity: OnceCell::new(),
            vault: OnceCell::new(),
            enrollment: OnceCell::new(),
            lineage: RefCell::new(Vec::new()),
            acknowledged: RefCell::new(Vec::new()),
            verified: OnceCell::new(),
            seeded: OnceCell::new(),
            configured: OnceCell::new(),
            custody: OnceCell::new(),
        }
    }

    /// A read that judges `config` and `state`, which the caller already holds, against the
    /// files.
    pub(super) fn under(paths: &'a Paths, config: &Config, state: &State) -> Self {
        let reads = Self::new(paths);
        let _ = reads.config.set(Ok(Some(config.clone())));
        let _ = reads.state.set(Ok(state.clone()));
        reads
    }

    pub(super) const fn paths(&self) -> &'a Paths {
        self.paths
    }

    /// `source.toml`, whose absent `followed` is the vault's chain ([`Self::vault`]).
    pub(super) fn config(&self) -> Result<Option<&Config>> {
        self.config
            .get_or_init(|| {
                config_with(self.paths, |pin| {
                    self.vault()
                        .ok()
                        .flatten()
                        .map_or_else(Vec::new, |vault| vault.chain(pin))
                })
            })
            .as_ref()
            .map(Option::as_ref)
            .map_err(Clone::clone)
    }

    /// `state.toml`.
    pub(super) fn state(&self) -> Result<&State> {
        self.state
            .get_or_init(|| load_state(self.paths))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// The machine key.
    pub(super) fn identity(&self) -> Result<Option<&Identity>> {
        self.identity
            .get_or_init(|| read_identity(self.paths))
            .as_ref()
            .map(Option::as_ref)
            .map_err(Clone::clone)
    }

    /// The vault file's bytes, and what they parse to.
    pub(super) fn vault(&self) -> Result<Option<&chain_cache::Parsed>> {
        self.vault
            .get_or_init(|| {
                Ok(
                    fsx::read_private(&self.paths.cache_file(), envelope::MAX_ENVELOPE)?
                        .map(chain_cache::parsed),
                )
            })
            .as_ref()
            .map(|vault| vault.as_deref())
            .map_err(Clone::clone)
    }

    /// The enrollment claim and the journal it establishes.
    pub(super) fn enrollment(&self) -> Result<&retained_history::Enrollment> {
        self.enrollment
            .get_or_init(|| retained_history::read_enrollment(self.paths))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// [`retained_history::lineage_of`] `config`, once for each configuration asked about.
    pub(super) fn lineage(&self, config: &Config) -> KeyLine {
        if let Some((_, keys)) = self.lineage.borrow().iter().find(|(c, _)| c == config) {
            return keys.clone();
        }
        let keys: KeyLine = retained_history::lineage_of(self, config).into();
        self.lineage
            .borrow_mut()
            .push((config.clone(), keys.clone()));
        keys
    }

    /// The membership store under `config`, projected on the history this Mac acknowledged
    /// ([`retained_history::project_store`]), where it holds a log.
    pub(super) fn acknowledged(&self, config: &Config) -> Result<Option<HeldLog>> {
        if let Some((_, held)) = self.acknowledged.borrow().iter().find(|(c, _)| c == config) {
            return held.clone();
        }
        let held = load_records_for(self, config)
            .and_then(|store| retained_history::project_store(self, config, store))
            .map(HeldLog::of);
        self.acknowledged
            .borrow_mut()
            .push((config.clone(), held.clone()));
        held
    }

    /// The vault's envelope, where it verifies under `config` and is the version `state` applied.
    pub(super) fn verified_under(
        &self,
        config: &Config,
        state: &State,
    ) -> Result<Option<&Envelope>> {
        let Some(pin) = config.signer else {
            return Ok(None);
        };
        if !source_bound(config, state) || pin_gives_way(config) {
            return Ok(None);
        }
        let Some(vault) = self.vault()? else {
            return Ok(None);
        };
        let Ok((envelope, _, _)) =
            vault.verify_for_reader(&config.name, &pin, &config.followed, None)
        else {
            return Ok(None);
        };
        Ok(
            (envelope.seq == state.seq && Some(envelope.digest()) == state.digest)
                .then_some(envelope),
        )
    }

    /// [`Self::verified_under`] this read's own configuration and state; none with no
    /// configuration.
    pub(super) fn verified(&self) -> Result<Option<&Envelope>> {
        let verified = self
            .verified
            .get_or_init(|| {
                let Some(config) = self.config()? else {
                    return Ok(false);
                };
                Ok(self.verified_under(config, self.state()?)?.is_some())
            })
            .clone()?;
        if !verified {
            return Ok(None);
        }
        Ok(self.vault()?.and_then(|vault| vault.envelope().ok()))
    }

    /// [`log_entries::seeded_under`] this read's own configuration and the vault's signer, once
    /// the caller has verified that envelope.
    pub(super) fn seeded(&self) -> Result<Option<HeldLog>> {
        self.seeded
            .get_or_init(|| {
                let Some(config) = self.config()? else {
                    return Ok(None);
                };
                let Some(vault) = self.vault()? else {
                    return Ok(None);
                };
                let envelope = vault.envelope()?;
                log_entries::seeded_under(self, config, &envelope.signer, &envelope.handoff)
            })
            .clone()
    }

    /// [`log_entries::configured_under`] this read's own configuration.
    pub(super) fn configured(&self) -> Result<Option<HeldLog>> {
        self.configured
            .get_or_init(|| {
                let Some(config) = self.config()? else {
                    return Ok(None);
                };
                log_entries::configured_under(self, config)
            })
            .clone()
    }

    /// The authenticated log this read's own configuration and state hold in custody
    /// ([`log_entries::authenticated_custody`]).
    pub(super) fn custody(&self) -> Result<Option<HeldLog>> {
        self.custody
            .get_or_init(|| {
                if self.config()?.is_none() {
                    return Ok(None);
                }
                self.state()?;
                if self.verified()?.is_some()
                    && let Some(seeded) = self.seeded()?
                {
                    return Ok(Some(seeded));
                }
                self.configured()
            })
            .clone()
    }
}
