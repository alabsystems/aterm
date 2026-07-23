// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Domain registry for managing multiple domains.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use super::{Domain, DomainId, DomainType};

/// Domain registry for managing multiple domains.
// Skip (propagates to the derive-generated impl): the derived `Default`
// constructs `RwLock<HashMap<..>>` — the HashMap ctor's absent std body plus
// the lock wrapper. Field construction only; no logic of our own.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Default)]
pub struct DomainRegistry {
    domains: RwLock<HashMap<DomainId, Arc<dyn Domain>>>,
    default_domain: RwLock<Option<DomainId>>,
}

impl DomainRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a domain.
    pub fn register(&self, domain: Arc<dyn Domain>) {
        let id = domain.domain_id();
        let mut domains = self.domains.write().unwrap_or_else(PoisonError::into_inner);
        let mut default = self
            .default_domain
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        domains.insert(id, domain);
        if default.is_none() {
            *default = Some(id);
        }
    }

    /// Unregister a domain.
    #[must_use]
    pub fn unregister(&self, id: DomainId) -> Option<Arc<dyn Domain>> {
        let mut domains = self.domains.write().unwrap_or_else(PoisonError::into_inner);
        let mut default = self
            .default_domain
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let domain = domains.remove(&id);
        if *default == Some(id) {
            *default = domains.keys().next().copied();
        }
        domain
    }

    // Trust: the accessors below use explicit `match`/`for` instead of the
    // former iterator/Option combinator chains (`.cloned()`, `.and_then(..)`,
    // `.filter(..).cloned().collect()`, `.find(..)`). Each adapter closure
    // lowers to an opaque environment the Trust gate cannot model; the loops
    // visit the same elements in the same order and clone the same `Arc`s
    // (`Option::cloned` on `Option<&Arc<_>>` IS `Arc::clone`), so every
    // returned value is identical.

    /// Get a domain by ID.
    #[must_use]
    pub fn get(&self, id: DomainId) -> Option<Arc<dyn Domain>> {
        let domains = self.domains.read().unwrap_or_else(PoisonError::into_inner);
        match domains.get(&id) {
            Some(domain) => Some(Arc::clone(domain)),
            None => None,
        }
    }

    /// Get the default domain.
    #[must_use]
    pub fn default_domain(&self) -> Option<Arc<dyn Domain>> {
        // Copy the id out and DROP the `default_domain` guard BEFORE `get()` takes
        // the `domains` lock. `register`/`unregister` lock domains → default_domain,
        // so holding default_domain here while acquiring domains (via get) is the
        // opposite nesting — a lock-order inversion that can deadlock under a
        // concurrent register(). The two locks are never held at once now.
        let id = {
            let default = self
                .default_domain
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            *default
        };
        match id {
            Some(id) => self.get(id),
            None => None,
        }
    }

    /// Set the default domain.
    pub fn set_default(&self, id: DomainId) {
        let mut default = self
            .default_domain
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        *default = Some(id);
    }

    /// List all registered domains.
    #[must_use]
    pub fn list(&self) -> Vec<Arc<dyn Domain>> {
        let domains = self.domains.read().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::with_capacity(domains.len());
        for domain in domains.values() {
            out.push(Arc::clone(domain));
        }
        out
    }

    /// List registered domains of a given type.
    #[must_use]
    pub fn list_by_type(&self, domain_type: DomainType) -> Vec<Arc<dyn Domain>> {
        let domains = self.domains.read().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        for domain in domains.values() {
            if domain.domain_type() == domain_type {
                out.push(Arc::clone(domain));
            }
        }
        out
    }

    /// List domains that advertise remote execution.
    #[must_use]
    pub fn list_remote(&self) -> Vec<Arc<dyn Domain>> {
        let domains = self.domains.read().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        for domain in domains.values() {
            if domain.capabilities().remote {
                out.push(Arc::clone(domain));
            }
        }
        out
    }

    /// List domains that advertise pane multiplexing.
    #[must_use]
    pub fn list_multiplexers(&self) -> Vec<Arc<dyn Domain>> {
        let domains = self.domains.read().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        for domain in domains.values() {
            if domain.capabilities().multiplexed {
                out.push(Arc::clone(domain));
            }
        }
        out
    }

    /// Get a domain by name.
    #[must_use]
    pub fn get_by_name(&self, name: &str) -> Option<Arc<dyn Domain>> {
        let domains = self.domains.read().unwrap_or_else(PoisonError::into_inner);
        for domain in domains.values() {
            if domain.domain_name() == name {
                return Some(Arc::clone(domain));
            }
        }
        None
    }
}
