// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's `aterm secrets` vault as the registry's secret store
//! ([`crate::typed::secret::Store`]): a secret cell's value is put into and removed from the
//! vault entry its reference names (`aterm_secrets::client::put_entry`, `remove_entry`), by the
//! routes `aterm secrets set` and `unset` take, and read in place
//! (`aterm_secrets::client::read_in_place`), never from a file. A value goes in only once every
//! Mac reading the vault writes no vault entry to a file
//! (`aterm_secrets::client::fleet_in_place`, judged once per store and again once that finding
//! is old), since a build that still does would put it in plain text under `~/.secrets`.

use crate::{
    Error, Result,
    typed::secret::{Secret, Store},
};
use aterm_secrets::client::{self, Paths};
use std::collections::BTreeSet;

/// How long a put or a removal waits for this Mac's vault pass to let go of its state.
const WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The vault of the Mac whose secrets client lives at `paths`.
pub struct Vault {
    paths: Paths,
    /// This store's finding that the fleet writes no vault entry to a file, once judged.
    writes: std::sync::Mutex<Option<client::InPlace>>,
}

impl Vault {
    /// The vault this Mac's secrets client at `paths` writes.
    pub const fn at(paths: Paths) -> Self {
        Self {
            paths,
            writes: std::sync::Mutex::new(None),
        }
    }

    /// [`Self::at`] for a test's vault, which has no Commons to judge: it takes values at once.
    ///
    /// # Errors
    /// No vault is configured there.
    #[cfg(test)]
    pub(crate) fn admitted(paths: Paths) -> Result<Self> {
        let writes = client::InPlace::for_test(&paths).map_err(|e| Error::new(&e.to_string()))?;
        Ok(Self {
            paths,
            writes: std::sync::Mutex::new(Some(writes)),
        })
    }

    /// Run `then` with this store's standing finding, judging the fleet first where it has none
    /// or the one it has is old.
    fn with_writes<T>(&self, then: impl FnOnce(&client::InPlace) -> Result<T>) -> Result<T> {
        let mut writes = self
            .writes
            .lock()
            .map_err(|_| Error::new("the registry's secret store is poisoned"))?;
        let standing = match writes.take().filter(client::InPlace::stands) {
            Some(standing) => standing,
            None => {
                let now_ms = aterm_time::unix_millis()
                    .ok_or_else(|| Error::new("the system clock reads no time"))?;
                client::fleet_in_place(&self.paths, now_ms)
                    .map_err(|error| Error::new(&error.to_string()))?
            }
        };
        let out = then(&standing);
        *writes = Some(standing);
        out
    }
}

/// A put as stored only where the vault holds it now ([`client::EntryWrite::Here`]): a write
/// signed on this Mac and not yet posted, or a draft its vault pass may still drop
/// ([`client::EntryWrite::Pending`]), is no copy any other Mac reads, so a caller that writes a
/// reference to it, or lets a sealed copy go, would name a value that may never arrive.
fn stored(entry: &str, write: client::EntryWrite) -> Result<()> {
    match write {
        client::EntryWrite::Here => Ok(()),
        client::EntryWrite::Pending => Err(Error::new(&format!(
            "the secret store took {entry} but it is not in the vault yet; this Mac's vault \
             pass posts it, and the write that needs it waits for that"
        ))),
    }
}

/// A registry cell's entry name (`alab-…`): the vault entries this store reaches. Any other
/// entry (an API key, a release credential) is never put, read or removed through it.
fn owned(entry: &str) -> Result<()> {
    if crate::typed::secret::is_entry_name(entry) {
        Ok(())
    } else {
        Err(Error::new(
            "the registry's secret store reaches only its own alab- entries",
        ))
    }
}

impl Store for Vault {
    fn vault(&self) -> Result<String> {
        client::vault_name(&self.paths).map_err(|error| Error::new(&error.to_string()))
    }

    fn admit(&self) -> Result<()> {
        self.with_writes(|_| Ok(()))
    }

    fn put(&self, entry: &str, value: &Secret) -> Result<()> {
        owned(entry)?;
        self.with_writes(|writes| {
            let bytes = aterm_secrets::SecretBytes::new(value.plaintext().as_bytes().to_vec());
            client::put_entry(
                &self.paths,
                writes,
                entry,
                bytes,
                std::time::Instant::now() + WAIT,
            )
            .map_err(|error| Error::new(&format!("the secret store refused {entry}: {error}")))
            .and_then(|write| stored(entry, write))
        })
    }

    fn get(&self, entry: &str) -> Result<Option<Secret>> {
        owned(entry)?;
        let Some(bytes) = client::read_in_place(&self.paths, entry)
            .map_err(|error| Error::new(&format!("the secret store: {error}")))?
        else {
            return Ok(None);
        };
        let text = std::str::from_utf8(bytes.as_slice())
            .map_err(|_| Error::new("a secret-store entry a cell names is not UTF-8"))?;
        Ok(Some(Secret::new(text.to_owned())))
    }

    fn remove(&self, entry: &str) -> Result<()> {
        owned(entry)?;
        client::remove_entry(&self.paths, entry, std::time::Instant::now() + WAIT)
            .map(drop)
            .map_err(|error| Error::new(&format!("the secret store refused {entry}: {error}")))
    }

    fn owe(&self, entries: &[String]) -> Result<()> {
        let dir = self.owed_dir();
        aterm_journal::create_private_dir_durable(&dir).map_err(|e| owed_error(&e))?;
        let owner = owner(std::process::id())
            .ok_or_else(|| Error::new("this run's process start cannot be read"))?;
        for entry in entries {
            owned(entry)?;
            aterm_journal::write_private_durable(&dir, entry, owner.as_bytes())
                .map_err(|e| owed_error(&e))?;
        }
        Ok(())
    }

    fn orphaned(&self) -> Result<BTreeSet<String>> {
        let dir = self.owed_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeSet::new());
            }
            Err(error) => return Err(owed_error(&error)),
        };
        Ok(entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| crate::typed::secret::is_entry_name(name))
            .filter(|name| {
                // An owner that cannot be read, or one whose process is gone or is another
                // process now, has ended; a record that cannot be read is kept for a later look.
                std::fs::read_to_string(dir.join(name)).is_ok_and(|recorded| !live(&recorded))
            })
            .collect())
    }

    fn paid(&self, entries: &BTreeSet<String>) -> Result<()> {
        let me = owner(std::process::id());
        self.clear(entries, |recorded| Some(recorded) == me.as_deref())
    }

    fn cleared(&self, entries: &BTreeSet<String>) -> Result<()> {
        self.clear(entries, |recorded| !live(recorded))
    }
}

impl Vault {
    /// Remove the owed record of each of `entries` whose recorded owner `whose` accepts; one
    /// already gone is fine, and one that cannot be read is kept.
    fn clear(&self, entries: &BTreeSet<String>, whose: impl Fn(&str) -> bool) -> Result<()> {
        let dir = self.owed_dir();
        for entry in entries {
            owned(entry)?;
            let path = dir.join(entry);
            if !std::fs::read_to_string(&path).is_ok_and(|recorded| whose(recorded.trim())) {
                continue;
            }
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(owed_error(&error));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// This Mac's record of the registry entries it owes removal: one file per entry name beside
    /// the vault's own state, holding its owning run ([`owner`]), so two runs owing at once never
    /// lose each other's and neither collects the other's.
    fn owed_dir(&self) -> std::path::PathBuf {
        self.paths.dir.join("registry-owed")
    }
}

/// A run as its owed record names it: its process id and the kernel's record of its start, so a
/// later process given the same id is never taken for it.
fn owner(pid: u32) -> Option<String> {
    let start = aterm_uds::process::read_process_start(i32::try_from(pid).ok()?)?;
    Some(format!("{pid} {}", start.boot.0))
}

/// Whether the run `recorded` names ([`owner`]) is still running.
fn live(recorded: &str) -> bool {
    recorded
        .split_once(' ')
        .and_then(|(pid, _)| pid.parse::<u32>().ok())
        .and_then(owner)
        .is_some_and(|now| now == recorded.trim())
}

fn owed_error(error: &std::io::Error) -> Error {
    Error::new(&format!(
        "the record of owed secret-store removals: {error}"
    ))
}

#[cfg(test)]
#[path = "vault_tests.rs"]
mod tests;
