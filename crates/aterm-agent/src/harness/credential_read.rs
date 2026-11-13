// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S RESOLUTION OF CREDENTIAL WANTS (`IdleHost::credentials`), the one path every
//! credential need takes ([`crate::supervise::policy::credential_need`]): each want is looked up
//! in this Mac's secrets registry (`aterm_secrets::registry::read`: `state.toml` alone, never the
//! identity or a value) by the entries the need catalog says it arrives as (`Want::entries`), and
//! one it holds no entry for is filed in the need ledger (`aterm_secrets::need::raise`, by hand),
//! which asks the lab's Macs for it. A registry that cannot be read is no empty one: the want is
//! filed, and the ledger answers from what this Mac holds.

use crate::supervise::policy::credential_need::CredentialRead;
#[cfg(unix)]
use crate::supervise::policy::credential_need::{Resolution, VaultEntry};

/// A read whose thread could not start, or that panicked: each want unresolved, so the agent
/// files it itself.
impl crate::harness::repo_work::ProbeAnswer for CredentialRead {
    fn unread() -> Self {
        Self::NotAsked
    }
}

/// HOW `wants` RESOLVE under `home`: held where the registry holds an entry the catalog names for
/// the want, else filed (or why it could not be).
#[cfg(unix)]
#[must_use]
pub fn resolve(home: &std::path::Path, wants: &[String]) -> CredentialRead {
    use aterm_secrets::need::{self, Purpose, Want};
    let registry =
        aterm_secrets::registry::read(&aterm_secrets::client::Paths::locate_for_home(home));
    let resolved = wants
        .iter()
        .map(|spelled| {
            let resolution = match Want::parse(spelled) {
                Err(why) => Resolution::Failed(why),
                Ok(want) => {
                    let names = want.entries();
                    let held: Vec<VaultEntry> = registry
                        .iter()
                        .flatten()
                        .filter(|e| names.contains(&e.name))
                        .map(|e| VaultEntry {
                            name: e.name.clone(),
                            pointer: e.pointer.clone(),
                            purpose: e.purpose.clone(),
                        })
                        .collect();
                    if held.is_empty() {
                        match need::raise(
                            &aterm_secrets::client::Paths::for_home(home),
                            want,
                            Purpose::ByHand,
                        ) {
                            Ok(_) => Resolution::Filed,
                            Err(why) => Resolution::Failed(why.to_string()),
                        }
                    } else {
                        Resolution::Held(held)
                    }
                }
            };
            (spelled.clone(), resolution)
        })
        .collect();
    CredentialRead::Resolved(resolved)
}

/// [`resolve`] under this user's home; a platform with no vault resolves nothing.
#[must_use]
pub fn resolve_live(wants: &[String]) -> CredentialRead {
    #[cfg(unix)]
    {
        aterm_types::dirs::home_dir().map_or(CredentialRead::NotAsked, |home| resolve(&home, wants))
    }
    #[cfg(not(unix))]
    {
        let _ = wants;
        CredentialRead::NotAsked
    }
}

#[cfg(all(test, unix))]
#[path = "credential_read_tests.rs"]
mod tests;
