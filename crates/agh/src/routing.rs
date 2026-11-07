// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Configured identity routing shared by the CLI and library hosts. File-backed
//! publication grants stay separate from synthesized vault profiles and owner rows.

use crate::config::Dirs;
use crate::identity::EnvSource;
use crate::identity::vault::{self, Adoption};
use crate::policy::Router;
use crate::{Github, Result};

/// One configuration snapshot and its judged adopted credentials, before client binding.
pub struct Configured {
    router: Result<Router>,
    adoption: Option<Adoption>,
}

impl Configured {
    /// The composed route, or the vault identity refusal that prevents routing.
    /// # Errors
    /// A vault record refusal or an identity that changed since adoption.
    pub fn router(&self) -> Result<&Router> {
        self.router.as_ref().map_err(Clone::clone)
    }

    /// Adoption metadata, including why a vault token was left out.
    #[must_use]
    pub const fn adoption(&self) -> Option<&Adoption> {
        self.adoption.as_ref()
    }

    /// Bind this snapshot, preserving the base transport, caller, journal and retry policy.
    /// Every adopted token is pinned and taught to the redactor before a call can use it.
    /// # Errors
    /// The same identity refusal as [`Self::router`].
    pub fn bind(&self, base: &Github) -> Result<Github> {
        let client = base.with_router(self.router()?.clone());
        if let Some(adoption) = &self.adoption {
            client.absorb(adoption.spent());
            for credential in adoption.pinned() {
                client.pin(credential.clone());
            }
        }
        Ok(client)
    }

    /// Retain the adoption metadata in a calling CLI after binding the client.
    #[must_use]
    pub fn into_adoption(self) -> Option<Adoption> {
        self.adoption
    }
}

fn resolve(
    dirs: &Dirs,
    env: &dyn EnvSource,
    live: Option<&Github>,
    honour_loose: bool,
) -> Result<Configured> {
    let snapshot = match crate::config::load_routing_snapshot(dirs, honour_loose)
        .map_err(|refusal| refusal.error)?
    {
        Some((validated, _)) if !validated.joins_vault() => {
            return Ok(Configured {
                router: Ok(Router::from_validated(validated)),
                adoption: None,
            });
        }
        snapshot => snapshot,
    };
    let adoption = vault::run_with_rows(dirs, env, live, snapshot.as_ref().map(|(_, rows)| rows));
    let router = match &adoption {
        Some(adoption) => adoption.router(),
        None => Ok(snapshot
            .as_ref()
            .map_or_else(Router::empty, |(validated, _)| {
                Router::from_validated(validated.clone())
            })),
    }
    .map(|router| match &snapshot {
        Some((validated, _)) => router.with_file_authority(validated),
        None => router,
    });
    Ok(Configured { router, adoption })
}

/// Resolve standard CLI routing, learning an unrecorded vault login through the base client.
/// # Errors
/// Refusal of a malformed, unreadable or unsafe explicit configuration file.
pub fn adopt(base: &Github, dirs: &Dirs) -> Result<Configured> {
    resolve(dirs, base.env(), Some(base), false)
}

/// Resolve standard CLI routing from recorded identities without requests or state writes.
/// Unrecorded or changed credentials cannot acquire identity or grant authority here.
/// # Errors
/// Refusal of a malformed, unreadable or unsafe explicit configuration file.
pub fn view(dirs: &Dirs, env: &dyn EnvSource) -> Result<Configured> {
    resolve(dirs, env, None, false)
}

/// [`adopt`] for a library host: retain its existing explicit `legacy_loose` file policy.
/// Vault credentials are always checked by the strict vault reader.
/// # Errors
/// As [`adopt`].
pub fn adopt_for_host(base: &Github, dirs: &Dirs) -> Result<Configured> {
    resolve(dirs, base.env(), Some(base), true)
}

/// [`view`] with the library host's existing explicit-file policy, still without requests
/// or state writes.
/// # Errors
/// As [`view`].
pub fn view_for_host(dirs: &Dirs, env: &dyn EnvSource) -> Result<Configured> {
    resolve(dirs, env, None, true)
}

#[cfg(all(test, unix))]
mod tests;
